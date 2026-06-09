//! peer-l0-dial — tiny CLI for the L0 connectivity spike.
//!
//! Two modes:
//!   --mode listen   (the "receiver" side)
//!   --mode dial     (the "sender" side, needs --node <NodeId>)
//!
//! Both modes can write a ConnectivitySample JSON (the artifact we collect
//! across real networks to decide whether to green-light L1).
//!
//! This binary has its own Cargo.toml precisely so it can depend on iroh
//! without pulling the main aeroftp crate (and its russh aead pin) into the
//! same resolution graph.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use iroh::NodeId;
use iroh_blobs::Hash;
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tracing::info;

use aeroftp_peer_l0::{
    decode_secret, derive_drive_key, derive_session_key, encode_secret, encrypt_blob, decrypt_blob,
    generate_pairing_secret, recv_encrypted_blob, recv_offer, send_encrypted_blob, send_offer,
    ConnectivitySample,
};

#[derive(Parser, Debug)]
#[command(name = "peer-l0-dial", version, about = "AeroFTP Peer L0 spike dialer (isolated)")]
struct Cli {
    #[command(subcommand)]
    mode: Mode,

    /// Where to write the ConnectivitySample JSON report (one per run).
    #[arg(long)]
    report: Option<PathBuf>,

    /// Free-form note for this sample (network conditions, "home CGNAT", "office", etc.).
    #[arg(long)]
    note: Option<String>,

    /// Pairing secret for E2EE (L0 "vault encryption" simulation).
    /// Listen side: if omitted, a fresh secret is generated and printed.
    /// Dial side: must match the secret shown by the listener.
    /// Format: base64url (no padding), 16 bytes recommended.
    /// This is the mechanism that will later be replaced by proper per-user identity keys
    /// once MU-VAULT lands.
    #[arg(long)]
    secret: Option<String>,

    /// Comma-separated list of relay URLs to use with `RelayMode::Custom`
    /// (e.g. a self-hosted relay). If omitted or empty, the research default
    /// (Staging) is used. Both peers MUST use the SAME relay set to interoperate.
    /// Example: --custom-relay-urls "https://my-relay.example:443,https://backup:443"
    #[arg(long, value_delimiter = ',')]
    custom_relay_urls: Option<Vec<String>>,
}

#[derive(Subcommand, Debug)]
enum Mode {
    /// Listen for one incoming peer connection and receive a blob (after explicit approval).
    Listen {
        /// Optional explicit port (0 = random). Printed together with the NodeID.
        #[arg(long, default_value_t = 0)]
        port: u16,

        /// How many transfers to accept before exiting (0 = until Ctrl-C).
        /// Very useful for measurement campaigns: keep one receiver up and send many samples.
        #[arg(long, default_value_t = 0)]
        count: u32,
    },

    /// Dial a remote NodeID (the whole point) and send a small test blob.
    Dial {
        /// The target NodeID (hex, as printed by the listen side).
        #[arg(long)]
        node: String,

        /// Size of the random blob to send (bytes). Keep small for the gate tests.
        #[arg(long, default_value_t = 128 * 1024)]
        size: usize,
    },

    /// Analyze collected ConnectivitySample reports (JSON files or directory).
    /// Extremely useful after a measurement campaign to get quick stats.
    Summarize {
        /// One or more report files (JSON single sample or array of samples).
        /// If a directory is passed, all *.json inside are read recursively (shallow).
        #[arg(required = true)]
        inputs: Vec<PathBuf>,
    },

    /// L1 docs-publish (Node A): create namespace + ONE signed entry + its *encrypted* blob, print DocTicket, stay running.
    /// E2EE layered on the drive key (from --secret + NamespaceId). Requires --secret.
    DocsPublish {
        /// Entry key (path-like). Default chosen to match the task's "hello.txt" example.
        #[arg(long, default_value = "hello.txt")]
        key: String,
    },

    /// L1 docs-replicate (Node B): given ticket from publish, import+sync, read the entry,
    /// fetch the ciphertext blob, decrypt with --secret + ns, verify plaintext + author sig.
    DocsReplicate {
        /// DocTicket string exactly as printed by the publish side (contains Namespace + addrs).
        ticket: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "info,aeroftp_peer_l0=debug".to_string()),
        )
        .init();

    let cli = Cli::parse();

    // Support env var for secret — extremely convenient when scripting many cross-network measurements.
    let effective_secret = cli.secret.or_else(|| std::env::var("AEROFTP_PEER_SECRET").ok());

    // Shared endpoint config: carries the optional custom-relay override (bind_addr is
    // set per-mode for the listener). Built once and threaded into listen/dial.
    // Clone so later docs- arms can also read it (L0 paths consume one copy).
    let custom_relay = cli.custom_relay_urls.clone();
    let cfg = aeroftp_peer_l0::endpoint::PeerEndpointConfig {
        bind_addr: None,
        secret_key_path: None,
        custom_relay_urls: custom_relay,
    };

    match cli.mode {
        Mode::Listen { port, count } => {
            let samples = run_listen_multi(port, cli.note.clone(), effective_secret, count, cfg).await;

            // For campaign use: if --report is given with listen --count, write all samples as JSON array.
            if let Some(path) = cli.report {
                let json = serde_json::to_string_pretty(&samples)?;
                std::fs::write(&path, json)?;
                info!(path = %path.display(), count = samples.len(), "multi-sample report written");
            }

            // Print a tiny summary for the operator.
            let ok = samples.iter().filter(|s| s.success).count();
            let fail = samples.len() - ok;
            println!("\n=== LISTEN SESSION SUMMARY ===");
            println!("Total: {} | OK: {} | FAIL: {}", samples.len(), ok, fail);
            return Ok(());
        }
        Mode::Dial { node, size } => {
            let sample = run_dial(&node, size, cli.note.clone(), effective_secret, cfg).await;

            // Always print a one-line summary (easy for log collection).
            if sample.success {
                println!(
                    "SAMPLE_OK path={} total={}ms connect={}ms xfer={}ms note={:?}",
                    sample.path, sample.total_duration_ms, sample.connect_duration_ms, sample.transfer_duration_ms, sample.network_note
                );
            } else {
                println!("SAMPLE_FAIL error={:?} note={:?}", sample.error, sample.network_note);
            }

            if let Some(path) = cli.report {
                let json = serde_json::to_string_pretty(&sample)?;
                std::fs::write(&path, json)?;
                info!(path = %path.display(), "report written");
            }

            if !sample.success {
                std::process::exit(2);
            }
        }
        Mode::Summarize { inputs } => {
            let all_samples = load_samples_from_inputs(&inputs);
            if all_samples.is_empty() {
                eprintln!("No samples found in the provided inputs.");
                std::process::exit(1);
            }
            print_campaign_summary(&all_samples);
            return Ok(());
        }
        Mode::DocsPublish { key } => {
            let docs_cfg = aeroftp_peer_l0::endpoint::PeerEndpointConfig {
                bind_addr: None,
                secret_key_path: None,
                custom_relay_urls: cli.custom_relay_urls.clone(),
            };
            let secret_bytes = effective_secret.as_deref().map(decode_secret).transpose()?;
            run_docs_publish(key, docs_cfg, secret_bytes).await?;
            return Ok(());
        }
        Mode::DocsReplicate { ticket } => {
            let docs_cfg = aeroftp_peer_l0::endpoint::PeerEndpointConfig {
                bind_addr: None,
                secret_key_path: None,
                custom_relay_urls: cli.custom_relay_urls.clone(),
            };
            let secret_bytes = effective_secret.as_deref().map(decode_secret).transpose()?;
            run_docs_replicate(ticket, docs_cfg, secret_bytes).await?;
            return Ok(());
        }
    }

    Ok(())
}

/// Load ConnectivitySample(s) from files or directories.
/// Supports single object or array per file.
fn load_samples_from_inputs(inputs: &[PathBuf]) -> Vec<ConnectivitySample> {
    let mut out = Vec::new();
    for input in inputs {
        if input.is_dir() {
            if let Ok(entries) = std::fs::read_dir(input) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.extension().map_or(false, |e| e == "json") {
                        if let Ok(samples) = load_one_report(&p) {
                            out.extend(samples);
                        }
                    }
                }
            }
        } else if let Ok(samples) = load_one_report(input) {
            out.extend(samples);
        }
    }
    out
}

fn load_one_report(path: &PathBuf) -> Result<Vec<ConnectivitySample>, anyhow::Error> {
    let content = std::fs::read_to_string(path)?;
    // Try as array first, then as single object.
    if let Ok(arr) = serde_json::from_str::<Vec<ConnectivitySample>>(&content) {
        return Ok(arr);
    }
    if let Ok(single) = serde_json::from_str::<ConnectivitySample>(&content) {
        return Ok(vec![single]);
    }
    anyhow::bail!("{} is not a valid ConnectivitySample or array of them", path.display())
}

fn print_campaign_summary(samples: &[ConnectivitySample]) {
    let total = samples.len();
    let successes: Vec<_> = samples.iter().filter(|s| s.success).collect();
    let fails = total - successes.len();
    let success_rate = if total > 0 { (successes.len() as f64 / total as f64) * 100.0 } else { 0.0 };

    println!("=== L0 CAMPAIGN SUMMARY ===");
    println!("Total samples: {}", total);
    println!("Successes: {} ({:.1}%)", successes.len(), success_rate);
    println!("Failures:  {}", fails);

    if !successes.is_empty() {
        let mut connect_times: Vec<u64> = successes.iter().map(|s| s.connect_duration_ms).collect();
        let mut total_times: Vec<u64> = successes.iter().map(|s| s.total_duration_ms).collect();
        connect_times.sort_unstable();
        total_times.sort_unstable();

        let avg_connect = connect_times.iter().sum::<u64>() as f64 / connect_times.len() as f64;
        let med_connect = connect_times[connect_times.len() / 2];
        let avg_total = total_times.iter().sum::<u64>() as f64 / total_times.len() as f64;
        let med_total = total_times[total_times.len() / 2];

        println!("\nConnect time (successes):");
        println!("  avg: {:.1} ms   median: {} ms", avg_connect, med_connect);
        println!("Total time (successes):");
        println!("  avg: {:.1} ms   median: {} ms", avg_total, med_total);
    }

    // Simple path breakdown (even if many "unknown")
    use std::collections::HashMap;
    let mut by_path: HashMap<String, usize> = HashMap::new();
    for s in samples {
        *by_path.entry(s.path.clone()).or_default() += 1;
    }
    println!("\nBy path:");
    for (p, c) in by_path {
        println!("  {}: {}", p, c);
    }

    // Failures by error (top level)
    let mut errors: HashMap<String, usize> = HashMap::new();
    for s in samples.iter().filter(|s| !s.success) {
        if let Some(e) = &s.error {
            let key = e.split(':').next().unwrap_or(e).trim().to_string();
            *errors.entry(key).or_default() += 1;
        } else {
            *errors.entry("unknown".to_string()).or_default() += 1;
        }
    }
    if !errors.is_empty() {
        println!("\nFailure reasons (top):");
        let mut errs: Vec<_> = errors.into_iter().collect();
        errs.sort_by_key(|(_, c)| std::cmp::Reverse(*c));
        for (reason, count) in errs.into_iter().take(8) {
            println!("  {}: {}", reason, count);
        }
    }

    // B: Keyword grouping from network_note (useful for real campaign analysis)
    let mut by_keyword: HashMap<String, usize> = HashMap::new();
    let keywords = ["CGNAT", "mobile", "home", "office", "hotel", "VPN", "Starlink", "4G", "5G", "double NAT", "residential"];
    for s in samples {
        if let Some(note) = &s.network_note {
            let note_lower = note.to_lowercase();
            for kw in &keywords {
                if note_lower.contains(&kw.to_lowercase()) {
                    *by_keyword.entry(kw.to_string()).or_default() += 1;
                }
            }
        }
    }
    if !by_keyword.is_empty() {
        println!("\nBy network keyword (from --note):");
        let mut kws: Vec<_> = by_keyword.into_iter().collect();
        kws.sort_by_key(|(_, c)| std::cmp::Reverse(*c));
        for (kw, count) in kws {
            println!("  {}: {}", kw, count);
        }
    }

    // Simple CSV block for easy export / further analysis in spreadsheet
    println!("\n--- CSV (copy-paste friendly) ---");
    println!("success,connect_ms,total_ms,path,note");
    for s in samples {
        let note = s.network_note.as_deref().unwrap_or("").replace(',', ";");
        println!("{},{},{},{},{}", 
            if s.success { "1" } else { "0" },
            s.connect_duration_ms,
            s.total_duration_ms,
            s.path,
            note
        );
    }

    println!("\nTip: use --note with rich network description (include CGNAT, mobile, home, etc.) for better post-analysis grouping.");
}

/// Core of one listen transfer. Returns one ConnectivitySample.
async fn run_one_listen(
    ep: &aeroftp_peer_l0::endpoint::PeerEndpoint,
    secret_bytes: &[u8],
    note: &Option<String>,
) -> ConnectivitySample {
    let total_start = Instant::now();

    let node = ep.node_id();

    let connect_start = Instant::now();
    let conn = match ep.accept().await {
        Ok(c) => c,
        Err(e) => return ConnectivitySample::failure(e, note.clone()),
    };
    let connect_duration = connect_start.elapsed().as_millis() as u64;

    let mut path = "unknown".to_string();

    let remote_node_str = conn.remote_node_id().map(|id| id.to_string()).unwrap_or_default();
    let _remote_fp = if remote_node_str.len() > 12 { &remote_node_str[..12] } else { &remote_node_str };

    let offer = match recv_offer(&conn).await {
        Ok(o) => o,
        Err(e) => {
            let _ = conn.close(1u32.into(), b"bad-offer");
            return ConnectivitySample::failure(e, note.clone());
        }
    };

    let remote_node_str2 = conn.remote_node_id().map(|id| id.to_string()).unwrap_or_else(|_| "unknown".to_string());
    let _remote_fp = if remote_node_str2.len() > 12 { &remote_node_str2[..12] } else { &remote_node_str2 };

    println!("\n--- Incoming peer offer ---");
    println!("From NodeID: {} (fingerprint: {})", remote_node_str, remote_node_str2);
    println!("Hash: {}", offer.hash);
    println!("Size: {} bytes", offer.size);
    println!("Name hint: {}", offer.name_hint);
    if let Some(n) = &offer.note {
        println!("Note: {}", n);
    }
    println!("(Transfer will be E2EE using the pairing secret)");
    println!("---------------------------");

    // B: Pairing UX - explicit confirmation of the peer identity (fingerprint).
    print!("Does the fingerprint above match the expected peer? Accept this transfer? [y/N]: ");
    io::stdout().flush().ok();
    let mut answer = String::new();
    if io::stdin().read_line(&mut answer).is_err() || !answer.trim().eq_ignore_ascii_case("y") {
        let _ = conn.close(0u32.into(), b"rejected-by-user");
        println!("Rejected by user.");
        return ConnectivitySample::failure("user rejected", note.clone());
    }

    let local_node_str = node.to_string();
    let key = derive_session_key(secret_bytes, &local_node_str, &remote_node_str);

    let xfer_start = Instant::now();
    let received = match recv_encrypted_blob(&conn, &offer, &key).await {
        Ok(plain) => plain,
        Err(e) => {
            let _ = conn.close(1u32.into(), b"recv-failed");
            return ConnectivitySample::failure(e, note.clone());
        }
    };
    let xfer_duration = xfer_start.elapsed().as_millis() as u64;

    // Snapshot the path type (direct/mixed/relay) while the connection is still
    // open, for the L0 gate's hole-punch-vs-relay accounting.
    if let Ok(rid) = conn.remote_node_id() {
        path = conn_type_label(ep, rid).await;
    }

    println!("Received and decrypted {} bytes. BLAKE3 verified after decryption.", received.len());

    // C: Controlled inbox + basic guards (for the spike; real version will be under per-user private storage).
    let inbox_dir = std::path::Path::new("l0-peer-inbox");
    let _ = std::fs::create_dir_all(inbox_dir);

    // Simple guard (offer.size is the claimed plaintext size).
    if offer.size > 256 * 1024 * 1024 {
        let _ = conn.close(1u32.into(), b"too-large");
        return ConnectivitySample::failure("file exceeds spike safety cap (256MB)", note.clone());
    }

    let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
    let safe_name = offer.name_hint.replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|'], "_");
    let final_name = format!("{}_{}", ts, safe_name);
    let file_path = inbox_dir.join(&final_name);

    if let Ok(_) = std::fs::write(&file_path, &received) {
        let meta = serde_json::json!({
            "sender_node": remote_node_str,
            "sender_fingerprint": remote_node_str2,
            "received_at": ts,
            "original_name_hint": offer.name_hint,
            "note": offer.note,
            "plaintext_hash": offer.hash.to_string(),
            "size": received.len(),
        });
        let _ = std::fs::write(file_path.with_extension("meta.json"), serde_json::to_string_pretty(&meta).unwrap_or_default());
        println!("Saved to controlled inbox: {}", file_path.display());
    } else {
        println!("Warning: failed to persist to inbox (still counted as success for measurement).");
    }

    println!("L0 receive complete (E2EE + inbox).");

    let _ = conn.close(0u32.into(), b"l0-ok");

    let total_duration = total_start.elapsed().as_millis() as u64;

    let mut sample = ConnectivitySample::success(
        total_duration,
        connect_duration,
        xfer_duration,
        path,
        note.clone(),
    );
    // Enhanced diagnostics for real data collection.
    // Note: in iroh 0.92 used here, rich path info (direct vs relayed, hole-punch success details)
    // is not directly exposed without additional features. We capture what we can and rely on
    // the human-provided --note for network conditions.
    sample.diagnostics = Some(format!(
        "remote_node={remote_node_str} local_node={local_node_str}; connection=quic; iroh_path={}",
        sample.path
    ));
    sample
}

/// Best-effort classification of the iroh path actually in use to `node`
/// (direct hole-punch vs relay-only vs mixed), snapshot right after a successful
/// transfer. Recorded into the ConnectivitySample's `path` so the L0 gate can
/// compute hole-punch-vs-relay rates — the "is a relay mandatory?" question (§8).
///
/// Phase 2 refinement (dialer conn-type): the watcher often starts as `None` (or
/// "unknown") for the first few hundred ms after `connect()`. We now await the first
/// non-None `ConnectionType` (bounded ~2000ms timeout) before falling back to a
/// snapshot `get()`. This makes the *dialer* (call site ~625) report "direct"/"mixed"/"relay"
/// instead of "unknown" on fast local connects. Listener side (which waited in accept)
/// was already reliable; we call the same awaited helper from both for consistency.
/// Keep the exact Debug-prefix mapping for Direct/Mixed/Relay/None.
async fn conn_type_label(ep: &aeroftp_peer_l0::endpoint::PeerEndpoint, node: NodeId) -> String {
    use iroh::Watcher;

    let Some(mut w) = ep.raw().conn_type(node) else {
        return "unknown".to_string();
    };

    // Fast path: if already non-None (typical for listener after accept, or warm dial), use it.
    let initial = format!("{:?}", w.get());
    if !initial.starts_with("None") {
        return classify_conn_type_debug(&initial);
    }

    // Await first non-None ConnectionType. On sub-second connects the initial get() is still
    // None because iroh has not yet classified (or updated the watcher). Bounded timeout
    // prevents hanging the sample collection; on timeout we fall back to current get()
    // (which may legitimately still be None/"unknown" or "none").
    let timeout = Duration::from_millis(2000);
    let fut = async {
        loop {
            match w.updated().await {
                Ok(val) => {
                    let s = format!("{:?}", val);
                    if !s.starts_with("None") {
                        return classify_conn_type_debug(&s);
                    }
                    // still None after this update; wait for the next one
                }
                Err(_) => break, // watcher disconnected; use whatever we have
            }
        }
        classify_conn_type_debug(&format!("{:?}", w.get()))
    };

    match ::tokio::time::timeout(timeout, fut).await {
        Ok(s) => s,
        Err(_) => {
            // timeout: honest fallback to latest snapshot (may be "none" or "unknown")
            classify_conn_type_debug(&format!("{:?}", w.get()))
        }
    }
}

fn classify_conn_type_debug(s: &str) -> String {
    if s.starts_with("Direct") {
        "direct".to_string()
    } else if s.starts_with("Mixed") {
        "mixed".to_string()
    } else if s.starts_with("Relay") {
        "relay".to_string()
    } else if s.starts_with("None") {
        "none".to_string()
    } else {
        "unknown".to_string()
    }
}

async fn run_listen_multi(
    port: u16,
    note: Option<String>,
    secret_opt: Option<String>,
    count: u32,
    mut cfg: aeroftp_peer_l0::endpoint::PeerEndpointConfig,
) -> Vec<ConnectivitySample> {
    if port != 0 {
        cfg.bind_addr = Some(([0, 0, 0, 0], port).into());
    }
    let ep = match aeroftp_peer_l0::endpoint::PeerEndpoint::new(cfg)
    .await
    {
        Ok(e) => e,
        Err(e) => {
            eprintln!("Failed to bind endpoint: {e}");
            std::process::exit(1);
        }
    };

    let secret_bytes: Vec<u8> = if let Some(s) = secret_opt {
        match decode_secret(&s) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("Bad secret: {e}");
                std::process::exit(1);
            }
        }
    } else {
        let fresh = generate_pairing_secret();
        let encoded = encode_secret(&fresh);
        println!("\n=== PAIRING SECRET (copy this to all senders) ===");
        println!("{}", encoded);
        println!("================================================\n");
        fresh.to_vec()
    };

    let node = ep.node_id();
    println!("=== AERO FTP PEER L0 LISTEN ===");
    println!("NodeID: {}", node);
    println!("Short fingerprint: {}", &node.to_string()[..12.min(node.to_string().len())]);
    if count > 0 {
        println!("Will accept up to {} transfers then exit.", count);
    } else {
        println!("Will accept transfers until Ctrl-C (ideal for measurement campaigns).");
    }
    println!("(share NodeID + pairing secret with senders)");

    let mut samples = Vec::new();
    let mut accepted = 0u32;

    loop {
        if count > 0 && accepted >= count {
            break;
        }

        println!("\n--- Waiting for connection #{} ---", accepted + 1);

        let sample = run_one_listen(&ep, &secret_bytes, &note).await;

        if sample.success {
            println!(
                "SAMPLE_OK path={} total={}ms connect={}ms xfer={}ms note={:?}",
                sample.path, sample.total_duration_ms, sample.connect_duration_ms, sample.transfer_duration_ms, sample.network_note
            );
        } else {
            println!("SAMPLE_FAIL error={:?} note={:?}", sample.error, sample.network_note);
        }

        samples.push(sample);
        accepted += 1;

        if count > 0 && accepted >= count {
            println!("Reached --count ({}). Exiting listener.", count);
            break;
        }
    }

    println!("\nListener finished. Accepted {} transfer(s).", accepted);
    samples
}

async fn run_dial(node_str: &str, blob_size: usize, note: Option<String>, secret_opt: Option<String>, cfg: aeroftp_peer_l0::endpoint::PeerEndpointConfig) -> ConnectivitySample {
    let total_start = Instant::now();

    let remote: NodeId = match node_str.parse() {
        Ok(n) => n,
        Err(e) => return ConnectivitySample::failure(format!("bad NodeId: {e}"), note),
    };

    let secret_bytes: Vec<u8> = match secret_opt {
        Some(s) => match decode_secret(&s) {
            Ok(b) => b,
            Err(e) => return ConnectivitySample::failure(e, note),
        },
        None => {
            return ConnectivitySample::failure(
                "L0 E2EE requires --secret (copy the one printed by the listen side)",
                note,
            );
        }
    };

    let ep = match aeroftp_peer_l0::endpoint::PeerEndpoint::new(cfg)
    .await
    {
        Ok(e) => e,
        Err(e) => return ConnectivitySample::failure(e, note),
    };

    println!("Dialing {} ...", remote);

    // Measure connection establishment time separately — this is the critical part for the L0 gate.
    let connect_start = Instant::now();
    let conn = match ep.connect(remote, aeroftp_peer_l0::PEER_L0_ALPN).await {
        Ok(c) => c,
        Err(e) => return ConnectivitySample::failure(e, note),
    };
    let connect_duration = connect_start.elapsed().as_millis() as u64;

    // Path type (direct/mixed/relay) is captured after the transfer succeeds below.

    // Build test data (this is the "plaintext" that will be vault-encrypted in real life).
    let data: Vec<u8> = (0..blob_size).map(|i| (i % 251) as u8).collect();
    let hash = Hash::new(&data);

    let offer = aeroftp_peer_l0::endpoint::PeerBlobOffer::new(hash, data.len() as u64, "l0-spike-test.bin")
        .with_note(note.clone().unwrap_or_default());

    // Send the (still plaintext) offer so the receiver can see size/name/note before deciding.
    if let Err(e) = send_offer(&conn, &offer).await {
        let _ = conn.close(1u32.into(), b"offer-failed");
        return ConnectivitySample::failure(e, note);
    }

    // Derive session key using both NodeIDs for binding.
    let local_node_str = ep.node_id().to_string();
    let remote_node_str = remote.to_string();
    let key = derive_session_key(&secret_bytes, &local_node_str, &remote_node_str);

    // Encrypt.
    let (nonce, ciphertext) = match aeroftp_peer_l0::encrypt_blob(&key, &data) {
        Ok(v) => v,
        Err(e) => {
            let _ = conn.close(1u32.into(), b"encrypt-failed");
            return ConnectivitySample::failure(e, note);
        }
    };

    // Measure actual transfer time.
    let xfer_start = Instant::now();
    if let Err(e) = send_encrypted_blob(&conn, &nonce, &ciphertext).await {
        let _ = conn.close(1u32.into(), b"send-failed");
        return ConnectivitySample::failure(e, note);
    }
    let xfer_duration = xfer_start.elapsed().as_millis() as u64;

    println!(
        "Encrypted blob sent and acknowledged by receiver ({} bytes plaintext, {} bytes ciphertext, hash {}).",
        data.len(),
        ciphertext.len(),
        hash
    );

    // Snapshot the path type (direct/mixed/relay) while the connection is still
    // open, for the L0 gate's hole-punch-vs-relay accounting.
    let path = conn_type_label(&ep, remote).await;

    // send_encrypted_blob only returns Ok after the receiver ACKed a successful
    // decrypt + BLAKE3 verify, so the transfer is genuinely complete. Close cleanly;
    // the old fixed 200ms-then-close truncated the stream over relayed paths and
    // caused the L0 RUN#1 "connection lost" failures.
    let _ = conn.close(0u32.into(), b"l0-ok");

    let total_duration = total_start.elapsed().as_millis() as u64;

    let mut sample = ConnectivitySample::success(
        total_duration,
        connect_duration,
        xfer_duration,
        path,
        note,
    );

    // Enhanced diagnostics for real data collection (same limitation note as listen side).
    sample.diagnostics = Some(format!(
        "local_node={local_node_str} remote_node={remote_node_str} connect_time_ms={connect_duration}; connection=quic; iroh_path={}",
        sample.path
    ));

    sample
}

/// L1 Stage 3: docs-publish side with E2EE (layered AES-GCM over the drive key derived from
/// the pairing secret + NamespaceId). The *value* bytes passed to set_bytes (and thus the
/// iroh-blobs content) are now nonce||ciphertext; the entry still carries the BLAKE3 of that
/// ciphertext. Plaintext is never exposed to the docs/iroh layer.
async fn run_docs_publish(key: String, cfg: aeroftp_peer_l0::endpoint::PeerEndpointConfig, secret_bytes: Option<Vec<u8>>) -> Result<()> {
    use bytes::Bytes;
    use iroh::protocol::Router;
    use iroh_blobs::BlobsProtocol;
    use iroh_docs::api::protocol::{AddrInfoOptions, ShareMode};
    use iroh_docs::protocol::Docs;
    use iroh_gossip::net::Gossip;

    let endpoint = aeroftp_peer_l0::endpoint::build_base_endpoint(cfg).await?;
    let node_id = endpoint.node_id();

    println!("=== AERO FTP PEER L1 DOCS PUBLISH (bare, no E2EE) ===");
    println!("NodeID: {}", node_id);
    println!("(this is the listener side; share the ticket below with the replicate side)");

    // Verified setup from iroh-docs 0.92 crate docs + examples (see L1-DESIGN and task links).
    // Note: scaffold in task used slightly different accept args; the real 0.92 surface
    // requires wrapping the store in BlobsProtocol for the blobs ALPN (as shown in the
    // official "getting started" example in the crate root docs).
    let blobs = iroh_blobs::store::mem::MemStore::default();
    let gossip = Gossip::builder().spawn(endpoint.clone());
    let docs = Docs::memory()
        .spawn(endpoint.clone(), (*blobs).clone(), gossip.clone())
        .await?;

    let router = Router::builder(endpoint.clone())
        .accept(iroh_blobs::ALPN, BlobsProtocol::new(&blobs, endpoint.clone(), None))
        .accept(iroh_gossip::ALPN, gossip)
        .accept(iroh_docs::ALPN, docs.clone())
        .spawn();

    let api = docs.api();

    // Create a fresh author (the signer) and a new document (the "drive" = namespace).
    let author = api.author_create().await?;
    let doc = api.create().await?;
    let ns = doc.id();

    println!("AuthorId: {}", author);
    println!("NamespaceId: {}", ns);

    // Write ONE entry, E2EE-layered.
    // The plaintext content is encrypted with a drive key derived from the pairing secret + ns.
    // We store nonce||ciphertext as the "content bytes" under the key; iroh-docs/iroh-blobs
    // only ever see the ciphertext (RBSR works on the encrypted value + its hash).
    let secret = secret_bytes.context("docs-publish requires --secret (16-byte pairing secret for L1 drive E2EE)")?;
    let drive_key = derive_drive_key(&secret, &ns.to_string());

    let content_str = format!("hi from L1 {}", chrono::Utc::now().to_rfc3339());
    let content: Vec<u8> = content_str.into_bytes();

    let (nonce, ct) = encrypt_blob(&drive_key, &content)?;
    let mut blob = nonce.clone();
    blob.extend_from_slice(&ct);

    // key: String moved into Bytes...
    let written_hash = doc.set_bytes(author, Bytes::from(key.clone()), Bytes::from(blob.clone())).await?;

    println!("Wrote entry: key={} content_hash={} size={}", key, written_hash, blob.len());
    println!("stored ciphertext blob len={} (nonce {}B + ct) (E2EE; plaintext never leaves this process; entry signed by author)", blob.len(), nonce.len());

    // Produce a ticket that tells the other side the NamespaceId + where to find us.
    // Read mode is sufficient for replication (the other side only pulls).
    let ticket = doc.share(ShareMode::Read, AddrInfoOptions::RelayAndAddresses).await?;
    println!("\n=== DOC TICKET (copy/paste to docs-replicate side) ===");
    println!("{}", ticket);
    println!("==================================================\n");

    println!("Publish side ready. Waiting for replicators (Ctrl-C to stop)...");
    // Keep the router (and thus the docs/gossip/blobs handlers) alive.
    tokio::signal::ctrl_c().await.ok();
    println!("Shutting down publish side.");
    // router drops will shutdown
    drop(router);
    Ok(())
}

/// L1 Stage 3: docs-replicate side with E2EE.
/// Imports the ticket, runs replication, fetches the (ciphertext) blob for the entry,
/// derives the drive key from --secret + doc.id(), decrypts, and verifies the plaintext.
async fn run_docs_replicate(ticket_str: String, cfg: aeroftp_peer_l0::endpoint::PeerEndpointConfig, secret_bytes: Option<Vec<u8>>) -> Result<()> {
    use bytes::Bytes;
    use futures_lite::stream::StreamExt;
    use iroh::protocol::Router;
    use iroh_blobs::BlobsProtocol;
    use iroh_docs::protocol::Docs;
    use iroh_gossip::net::Gossip;

    let ticket: iroh_docs::DocTicket = ticket_str.parse().context("failed to parse DocTicket")?;

    let endpoint = aeroftp_peer_l0::endpoint::build_base_endpoint(cfg).await?;

    println!("=== AERO FTP PEER L1 DOCS REPLICATE (bare, no E2EE) ===");
    println!("Importing ticket and syncing...");

    let blobs = iroh_blobs::store::mem::MemStore::default();
    let gossip = Gossip::builder().spawn(endpoint.clone());
    let docs = Docs::memory()
        .spawn(endpoint.clone(), (*blobs).clone(), gossip.clone())
        .await?;

    let _router = Router::builder(endpoint.clone())
        .accept(iroh_blobs::ALPN, BlobsProtocol::new(&blobs, endpoint.clone(), None))
        .accept(iroh_gossip::ALPN, gossip)
        .accept(iroh_docs::ALPN, docs.clone())
        .spawn();

    // import joins the peers listed in the ticket and starts sync.
    let doc = docs.api().import(ticket).await?;
    let ns = doc.id();
    println!("Imported + opened doc: NamespaceId={}", ns);

    // Read the entry we expect (key="hello.txt" by default from publish).
    // Replication (RBSR + blob xfer) is async even on localhost; retry a few times.
    let key = "hello.txt";
    let mut entry = None;
    for attempt in 0..20 {
        let q = iroh_docs::store::Query::key_exact(key.as_bytes());
        let mut stream = Box::pin(doc.get_many(q).await?);
        if let Some(res) = stream.next().await {
            entry = Some(res.context("entry stream item error")?);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        if attempt % 5 == 0 {
            println!("(waiting for replicated entry, attempt {})", attempt);
        }
    }
    let entry = entry.context("no entries returned after sync (timeout)")?;

    let entry_author = entry.author();
    let entry_key = String::from_utf8_lossy(entry.key()).to_string();
    let content_hash = entry.content_hash();
    let content_size = entry.content_len();

    println!("R1: received entry key={} author={} hash={} size={}", entry_key, entry_author, content_hash, content_size);

    // Fetch the *ciphertext* blob (the value stored under the entry is nonce||ct, not plaintext).
    // The entry (RBSR metadata) syncs first; the CONTENT blob is downloaded asynchronously by the
    // docs live engine (default DownloadPolicy = EverythingExcept([])). On loopback the download wins
    // the race before we read; over a real relay/cross-net path it is still in flight when the entry
    // appears, so a single get_bytes reads a partial blob and fails bao verification with
    // LeafHashMismatch(0). Retry until the download completes (same pattern as the entry wait above).
    let mut fetched: Option<Bytes> = None;
    let mut last_err: Option<anyhow::Error> = None;
    for attempt in 0..100 {
        match blobs.blobs().get_bytes(content_hash).await {
            Ok(b) => {
                fetched = Some(b);
                break;
            }
            Err(e) => {
                last_err = Some(e.into());
                if attempt % 10 == 0 {
                    println!("(waiting for content blob download to complete, attempt {})", attempt);
                }
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        }
    }
    let fetched: Bytes = fetched
        .with_context(|| format!(
            "blob content not available after docs replication (timeout; last error: {:?})",
            last_err
        ))?;

    // E3: show raw bytes are ciphertext (not the readable "hi from L1...").
    let raw_preview: String = fetched.iter().take(16).map(|b| format!("{:02x}", b)).collect();
    println!("E3: raw fetched blob (first 16 bytes hex): {} (should not start with '68 69 20 66 72 6f 6d' = 'hi from')", raw_preview);

    // Derive drive key and decrypt (this is the E2EE layer; iroh-docs only saw ciphertext).
    let secret = secret_bytes.context("docs-replicate requires --secret (16-byte pairing secret for L1 drive E2EE)")?;
    let drive_key = derive_drive_key(&secret, &ns.to_string());

    let n = 12; // AES-GCM nonce length (see encrypt_blob in crypto.rs)
    if fetched.len() < n {
        anyhow::bail!("fetched blob too short to contain nonce");
    }
    let (nonce, ct) = fetched.split_at(n);
    let plaintext = decrypt_blob(&drive_key, nonce, ct)?;

    println!("R2 (decrypted): plaintext len={} : {:?}", plaintext.len(), String::from_utf8_lossy(&plaintext));

    // The blake3 in the entry is over the *ciphertext* we stored; keep the check for the blob integrity.
    let local_hash = iroh_blobs::Hash::new(&fetched);
    println!("     local blake3 (of ct): {}", local_hash);
    println!("     entry content_hash  : {}", content_hash);
    if local_hash != content_hash {
        anyhow::bail!("BLAKE3 mismatch after fetch (ct)");
    }
    println!("     BLAKE3 (ct) match: PASS");

    // R3 unchanged in spirit.
    println!("R3: entry author = {} (docs sync verifies the author signature; no error above means sig OK)", entry_author);

    println!("\nL1 E2EE replication SUCCESS (entry + ciphertext blob replicated over the network; decrypted with drive key).");
    Ok(())
}
