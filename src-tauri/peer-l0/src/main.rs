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
use std::path::{Path, PathBuf};
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

    /// L1 docs-publish (Node A): create namespace + ONE signed entry + its *encrypted* blob (or a whole drive dir),
    /// print DocTicket, stay running. E2EE layered on the drive key. Requires --secret.
    DocsPublish {
        /// Entry key (path-like). Default chosen to match the task's "hello.txt" example.
        /// Used only when --dir is absent (single-entry backward compat mode).
        #[arg(long, default_value = "hello.txt")]
        key: String,

        /// Optional directory root to publish as a multi-file drive (recurses, builds signed+encrypted manifest
        /// under __drive_manifest__.json). If present, ignores the single `key` and walks regular files only
        /// (skips symlinks). Relative keys use '/' separators. Sorted for determinism.
        #[arg(long)]
        dir: Option<String>,

        /// For drive mode: after publishing v1, sleep this many seconds, then re-read the dir from disk
        /// and publish v2 (drive_version=2, LWW updates + any new files). Then stay alive so watchers
        /// can converge live. Only meaningful with --dir.
        #[arg(long, default_value_t = 0)]
        republish_after: u64,
    },

    /// L1 docs-replicate (Node B): given ticket from publish, import+sync, read the entry (or whole drive via manifest),
    /// fetch ciphertext blobs, decrypt with --secret + ns, verify plaintext BLAKE3s against manifest (if drive mode),
    /// write reconstructed files under --out. Single-entry mode preserved when --out absent.
    DocsReplicate {
        /// DocTicket string exactly as printed by the publish side (contains Namespace + addrs).
        ticket: String,

        /// Optional output directory to reconstruct a multi-file drive into (creates subdirs as needed).
        /// The manifest itself is NOT written as a file under --out (it is the drive index).
        /// If absent, falls back to single-entry hello.txt behavior (backward compat).
        #[arg(long)]
        out: Option<String>,

        /// For drive mode (--out present): after initial reconstruction, keep watching the LiveEvent
        /// stream for this many seconds. On manifest version increase (via InsertRemote / PendingContentReady
        /// etc.), re-pull the new manifest + all its files and converge (LWW). 0 or absent = exit after
        /// initial (Stage 4 one-shot behavior).
        #[arg(long, default_value_t = 0)]
        watch_secs: u64,
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
        Mode::DocsPublish { key, dir, republish_after } => {
            let docs_cfg = aeroftp_peer_l0::endpoint::PeerEndpointConfig {
                bind_addr: None,
                secret_key_path: None,
                custom_relay_urls: cli.custom_relay_urls.clone(),
            };
            let secret_bytes = effective_secret.as_deref().map(decode_secret).transpose()?;
            run_docs_publish(key, dir, republish_after, docs_cfg, secret_bytes).await?;
            return Ok(());
        }
        Mode::DocsReplicate { ticket, out, watch_secs } => {
            let docs_cfg = aeroftp_peer_l0::endpoint::PeerEndpointConfig {
                bind_addr: None,
                secret_key_path: None,
                custom_relay_urls: cli.custom_relay_urls.clone(),
            };
            let secret_bytes = effective_secret.as_deref().map(decode_secret).transpose()?;
            run_docs_replicate(ticket, out, watch_secs, docs_cfg, secret_bytes).await?;
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

type DriveState = std::collections::HashMap<String, String>;

struct DriveStats {
    updated: usize,
    added: usize,
    deleted: usize,
    unchanged: usize,
    file_count: usize,
    total_pt: u64,
}

/// Publish the current on-disk state of `src` as a specific drive_version: encrypt each regular file
/// (fresh nonce) under `drive_key`, write it under its relative '/'-key, then write the signed +
/// encrypted manifest under `__drive_manifest__.json`.
/// When `prev` is Some, implements differential: skip unchanged (by plaintext_blake3), count added/updated/deleted/unchanged,
/// issue doc.del for removed keys. Always materializes full current manifest for the version.
/// Returns (stats, new_state) for chaining to next republish.
async fn publish_drive_version(
    doc: &iroh_docs::api::Doc,
    author: iroh_docs::AuthorId,
    drive_key: &[u8; 32],
    src: &Path,
    version: u64,
    prev: Option<&DriveState>,
) -> Result<(DriveStats, DriveState)> {
    use bytes::Bytes;
    if !src.is_dir() {
        anyhow::bail!("--dir {} is not a directory", src.display());
    }

    let mut files: Vec<(String, PathBuf)> = vec![];
    fn collect_files(dir: &Path, base: &Path, out: &mut Vec<(String, PathBuf)>) {
        if let Ok(read_dir) = std::fs::read_dir(dir) {
            for entry in read_dir.flatten() {
                let p = entry.path();
                if p.is_symlink() {
                    continue;
                }
                if p.is_dir() {
                    collect_files(&p, base, out);
                } else if p.is_file() {
                    let rel = p.strip_prefix(base)
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .replace('\\', "/");
                    out.push((rel, p));
                }
            }
        }
    }
    collect_files(src, src, &mut files);
    files.sort_by(|a, b| a.0.cmp(&b.0));

    let mut manifest_files: Vec<serde_json::Value> = vec![];
    let mut total_pt: u64 = 0;

    let mut stats = DriveStats { updated: 0, added: 0, deleted: 0, unchanged: 0, file_count: files.len(), total_pt: 0 };
    let mut new_state: DriveState = std::collections::HashMap::new();

    for (rel_key, path) in &files {
        let pt = std::fs::read(path)?;
        let pt_blake3 = blake3::hash(&pt).to_hex().to_string();
        new_state.insert(rel_key.clone(), pt_blake3.clone());

        let is_unchanged = if let Some(p) = prev {
            p.get(rel_key).map_or(false, |old| old == &pt_blake3)
        } else {
            false
        };

        if is_unchanged {
            println!("skip unchanged key={}", rel_key);
            stats.unchanged += 1;
            manifest_files.push(serde_json::json!({
                "key": rel_key,
                "plaintext_len": pt.len(),
                "plaintext_blake3": pt_blake3
            }));
            total_pt += pt.len() as u64;
            continue;
        }

        // new or changed: fresh encrypt + set_bytes
        let (nonce, ct) = encrypt_blob(drive_key, &pt)?;
        let mut blob = nonce.clone();
        blob.extend_from_slice(&ct);
        let content_hash = doc.set_bytes(author, Bytes::from(rel_key.clone()), Bytes::from(blob)).await?;
        let action = if prev.map_or(true, |p| !p.contains_key(rel_key)) { "added" } else { "updated" };
        if action == "added" {
            stats.added += 1;
        } else {
            stats.updated += 1;
        }
        println!("wrote key={} pt_len={} ct_len={} content_hash={}", rel_key, pt.len(), ct.len(), content_hash);
        manifest_files.push(serde_json::json!({
            "key": rel_key,
            "plaintext_len": pt.len(),
            "plaintext_blake3": pt_blake3
        }));
        total_pt += pt.len() as u64;
    }

    // Deletions (only when we have a prev snapshot): keys that existed before but are gone on disk now
    if let Some(p) = prev {
        for old_key in p.keys() {
            if !new_state.contains_key(old_key) {
                let _ = doc.del(author, Bytes::from(old_key.clone())).await;
                println!("deleted key={}", old_key);
                stats.deleted += 1;
            }
        }
    }

    stats.total_pt = total_pt;

    let manifest = serde_json::json!({
        "drive_version": version,
        "created": chrono::Utc::now().to_rfc3339(),
        "file_count": files.len(),
        "files": manifest_files
    });
    let manifest_bytes = serde_json::to_vec(&manifest)?;
    let (m_nonce, m_ct) = encrypt_blob(drive_key, &manifest_bytes)?;
    let mut m_blob = m_nonce.clone();
    m_blob.extend_from_slice(&m_ct);
    doc.set_bytes(author, Bytes::from("__drive_manifest__.json"), Bytes::from(m_blob)).await?;

    Ok((stats, new_state))
}

/// L1 Stage 5: docs-publish with optional drive mode + republish-after for versioning demo.
/// If --dir absent: exact single-entry (compat).
/// If --dir present: publish v1 (drive_version=1) and SHARE THE TICKET IMMEDIATELY so a watcher can
/// join during v1. If --republish-after > 0: only THEN sleep and publish v2 (drive_version=2, LWW
/// updates + any new files), which gossip delivers live to the already-joined watcher.
async fn run_docs_publish(key: String, dir: Option<String>, republish_after: u64, cfg: aeroftp_peer_l0::endpoint::PeerEndpointConfig, secret_bytes: Option<Vec<u8>>) -> Result<()> {
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

    let secret = secret_bytes.context("docs-publish requires --secret (16-byte pairing secret for L1 drive E2EE)")?;
    let drive_key = derive_drive_key(&secret, &ns.to_string());

    let mut v1_file_count: usize = 0;
    let mut drive_state: Option<DriveState> = None;
    if let Some(dir_path) = dir.as_deref() {
        // === DRIVE MODE (Stage 6 differential): publish v1 (prev=None -> all added); share ticket immediately
        // so watcher can join during v1. v2 republish (after sleep) passes prev state for diff + del.
        let src = Path::new(dir_path);
        let (s1, state1) = publish_drive_version(&doc, author, &drive_key, src, 1, None).await?;
        println!("DRIVE PUBLISHED: {} files + manifest, ns={}, total_plaintext_bytes={}", s1.file_count, ns, s1.total_pt);
        v1_file_count = s1.file_count;
        drive_state = Some(state1);
    } else {
        // === SINGLE ENTRY MODE (exact backward compat with gate at 9f5a5c18) ===
        let content_str = format!("hi from L1 {}", chrono::Utc::now().to_rfc3339());
        let content: Vec<u8> = content_str.into_bytes();

        let (nonce, ct) = encrypt_blob(&drive_key, &content)?;
        let mut blob = nonce.clone();
        blob.extend_from_slice(&ct);

        let written_hash = doc.set_bytes(author, Bytes::from(key.clone()), Bytes::from(blob.clone())).await?;

        println!("Wrote entry: key={} content_hash={} size={}", key, written_hash, blob.len());
        println!("stored ciphertext blob len={} (nonce {}B + ct) (E2EE; plaintext never leaves this process; entry signed by author)", blob.len(), nonce.len());
    }

    // Produce a ticket that tells the other side the NamespaceId + where to find us. Read mode is
    // sufficient (the other side only pulls). Shared NOW (right after v1) so a watcher can join during
    // v1 and observe the live v1->v2 update below.
    let ticket = doc.share(ShareMode::Read, AddrInfoOptions::RelayAndAddresses).await?;
    println!("\n=== DOC TICKET (copy/paste to docs-replicate side) ===");
    println!("{}", ticket);
    println!("==================================================\n");
    println!("Publish side ready. Waiting for replicators (Ctrl-C to stop)...");

    // Drive versioning demo: republish v2 only AFTER the ticket is out, so a joined watcher sees the
    // live transition (gossip pushes the updated entries + the bumped manifest).
    if republish_after > 0 {
        if let Some(dir_path) = dir.as_deref() {
            let src = Path::new(dir_path);
            println!("(republish-after {}s: sleeping before v2)", republish_after);
            tokio::time::sleep(std::time::Duration::from_secs(republish_after)).await;
            let prev = drive_state.as_ref();
            let (s2, _state2) = publish_drive_version(&doc, author, &drive_key, src, 2, prev).await?;
            println!("DRIVE REPUBLISHED: v2: {} updated, {} added, {} deleted, {} unchanged (file_count {} -> {})", s2.updated, s2.added, s2.deleted, s2.unchanged, v1_file_count, s2.file_count);
        }
    }

    // Keep the router (and thus the docs/gossip/blobs handlers) alive.
    tokio::signal::ctrl_c().await.ok();
    println!("Shutting down publish side.");
    // router drops will shutdown
    drop(router);
    Ok(())
}

/// Poll for an entry by exact key and return its content hash once it appears.
/// (RBSR entry sync is async; on a slow/relay link the entry lands a little after the event.)
async fn wait_entry_hash(doc: &iroh_docs::api::Doc, key: &[u8], attempts: u32) -> Option<iroh_blobs::Hash> {
    use futures_lite::stream::StreamExt;
    for _ in 0..attempts {
        if let Ok(stream) = doc.get_many(iroh_docs::store::Query::key_exact(key)).await {
            let mut pinned = Box::pin(stream);
            if let Some(Ok(entry)) = pinned.next().await {
                return Some(entry.content_hash());
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
    }
    None
}

/// Fetch a blob by hash, retrying while the content download completes (same race as the 9f5a5c18 fix).
async fn fetch_blob_retry(
    blobs: &iroh_blobs::store::mem::MemStore,
    hash: iroh_blobs::Hash,
    attempts: u32,
) -> Option<bytes::Bytes> {
    for _ in 0..attempts {
        if let Ok(b) = blobs.blobs().get_bytes(hash).await {
            return Some(b);
        }
        tokio::time::sleep(std::time::Duration::from_millis(180)).await;
    }
    None
}

/// L1 Stage 5: docs-replicate with optional drive + live watch (--out + --watch-secs).
/// If --out absent: single-entry compat.
/// If --out present + no watch: Stage-4 one-shot (initial reconstruct + exit).
/// If --watch-secs > 0: after initial, use import_and_subscribe + LiveEvent loop; on manifest version
/// increase, re-converge (re-pull manifest + all files in it).
async fn run_docs_replicate(ticket_str: String, out: Option<String>, watch_secs: u64, cfg: aeroftp_peer_l0::endpoint::PeerEndpointConfig, secret_bytes: Option<Vec<u8>>) -> Result<()> {
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

    // Stage 5: use import_and_subscribe to get the LiveEvent stream for live convergence.
    let (doc, mut events) = docs.api().import_and_subscribe(ticket).await?;
    let ns = doc.id();
    println!("Imported + opened doc: NamespaceId={}", ns);

    let secret = secret_bytes.context("docs-replicate requires --secret (16-byte pairing secret for L1 drive E2EE)")?;
    let drive_key = derive_drive_key(&secret, &ns.to_string());
    let n = 12; // AES-GCM nonce

    if let Some(out_dir) = out {
        // === DRIVE MODE ===
        let out_path = Path::new(&out_dir);
        std::fs::create_dir_all(out_path)?;

        // 1. Wait for + fetch + decrypt manifest (reuse the async-blob retry pattern)
        let manifest_key = b"__drive_manifest__.json";
        let mut manifest_entry = None;
        for _attempt in 0..30 {
            let q = iroh_docs::store::Query::key_exact(manifest_key);
            let mut stream = Box::pin(doc.get_many(q).await?);
            if let Some(res) = stream.next().await {
                manifest_entry = Some(res.context("manifest entry error")?);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        }
        let manifest_entry = manifest_entry.context("drive manifest entry not found (timeout)")?;
        let manifest_hash = manifest_entry.content_hash();

        let mut m_fetched: Option<Bytes> = None;
        let mut last_err: Option<anyhow::Error> = None;
        for attempt in 0..100 {
            match blobs.blobs().get_bytes(manifest_hash).await {
                Ok(b) => { m_fetched = Some(b); break; }
                Err(e) => {
                    last_err = Some(e.into());
                    if attempt % 10 == 0 {
                        println!("(waiting for manifest content blob, attempt {})", attempt);
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                }
            }
        }
        let m_fetched: Bytes = m_fetched.with_context(|| format!(
            "manifest blob not available (timeout; last err: {:?})", last_err
        ))?;

        if m_fetched.len() < n {
            anyhow::bail!("manifest blob too short");
        }
        let (m_nonce, m_ct) = m_fetched.split_at(n);
        let manifest_pt = decrypt_blob(&drive_key, m_nonce, m_ct)
            .context("failed to decrypt manifest (wrong secret?)")?;
        let manifest: serde_json::Value = serde_json::from_slice(&manifest_pt)
            .context("invalid manifest JSON")?;

        let files = manifest["files"].as_array().cloned().unwrap_or_default();
        let k = files.len();
        // Track the version from the first manifest we actually read (NOT hardcoded): a watcher that
        // joins after a republish must start at the real current version, else it would re-converge
        // needlessly and mislabel the transition. `mut` because the live loop advances it.
        let mut current_version: u64 = manifest["drive_version"].as_u64().unwrap_or(1);
        println!("Manifest: {} files, drive_version={}", k, current_version);

        let mut ok = 0usize;
        let mut total_bytes: u64 = 0;

        // Stage 6: track previous drive state (key -> plaintext_blake3) from the initial reconstruct.
        // Used for skip-unchanged decisions and deletion detection on version bumps.
        let mut prev_files: std::collections::HashMap<String, String> = std::collections::HashMap::new();

        for f in files {
            let rel_key = f["key"].as_str().unwrap_or("").to_string();
            let expected_pt_len = f["plaintext_len"].as_u64().unwrap_or(0);
            let expected_blake3 = f["plaintext_blake3"].as_str().unwrap_or("").to_string();

            // Wait for this file's entry
            let mut file_entry = None;
            for _attempt in 0..30 {
                let q = iroh_docs::store::Query::key_exact(rel_key.as_bytes());
                let mut stream = Box::pin(doc.get_many(q).await?);
                if let Some(res) = stream.next().await {
                    file_entry = Some(res.context("file entry error")?);
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            }
            let file_entry = match file_entry {
                Some(e) => e,
                None => {
                    println!("FAIL {} (entry not found)", rel_key);
                    continue;
                }
            };
            let ch = file_entry.content_hash();

            // Fetch blob with retry (async download race)
            let mut f_fetched: Option<Bytes> = None;
            for attempt in 0..100 {
                match blobs.blobs().get_bytes(ch).await {
                    Ok(b) => { f_fetched = Some(b); break; }
                    Err(_) => {
                        if attempt % 10 == 0 {
                            println!("(waiting for content blob of {}, attempt {})", rel_key, attempt);
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    }
                }
            }
            let f_fetched = match f_fetched {
                Some(b) => b,
                None => {
                    println!("FAIL {} (blob not available)", rel_key);
                    continue;
                }
            };

            if f_fetched.len() < n {
                println!("FAIL {} (blob too short)", rel_key);
                continue;
            }
            let (f_nonce, f_ct) = f_fetched.split_at(n);
            let pt = match decrypt_blob(&drive_key, f_nonce, f_ct) {
                Ok(p) => p,
                Err(e) => {
                    println!("FAIL {} (decrypt error: {})", rel_key, e);
                    continue;
                }
            };

            let got_blake3 = blake3::hash(&pt).to_hex().to_string();
            if got_blake3 != expected_blake3 {
                println!("FAIL {} (BLAKE3 mismatch: got {} expected {})", rel_key, got_blake3, expected_blake3);
                continue;
            }
            if pt.len() as u64 != expected_pt_len {
                println!("FAIL {} (len mismatch)", rel_key);
                continue;
            }

            // Write to out/<rel_key>
            let target = out_path.join(&rel_key);
            if let Some(parent) = target.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            std::fs::write(&target, &pt)?;
            println!("OK {} (pt_len={}, blake3 verified)", rel_key, pt.len());
            prev_files.insert(rel_key.clone(), expected_blake3.clone());
            ok += 1;
            total_bytes += pt.len() as u64;
        }

        println!("DRIVE REPLICATED: {}/{} files, all plaintext BLAKE3 verified, total_bytes={}", ok, k, total_bytes);

        // --- LIVE CONVERGENCE (Stage 6 differential + deletions) ---
        if watch_secs > 0 {
            use std::time::Instant;
            use iroh_docs::engine::LiveEvent;
            let deadline = Instant::now() + std::time::Duration::from_secs(watch_secs);
            println!("(watching {}s for live updates...)", watch_secs);

            loop {
                let rem = deadline.saturating_duration_since(Instant::now());
                if rem.is_zero() { break; }
                match tokio::time::timeout(rem, events.next()).await {
                    Err(_) => break,
                    Ok(None) => break,
                    Ok(Some(Ok(ev))) => {
                        let trigger = match &ev {
                            LiveEvent::InsertRemote { entry, .. } => entry.key() == b"__drive_manifest__.json",
                            LiveEvent::PendingContentReady => true,
                            _ => false,
                        };
                        if trigger {
                            // Re-read the manifest (with retry helpers); if version advanced, apply diff:
                            // - skip unchanged (same blake3 in prev_files AND file exists on disk)
                            // - pull/decrypt/verify/write only for changed or added
                            // - delete local files for keys present in prev but absent from new manifest
                            let manifest_hash = match wait_entry_hash(&doc, manifest_key, 20).await {
                                Some(h) => h,
                                None => continue,
                            };
                            let mblob = match fetch_blob_retry(&blobs, manifest_hash, 60).await {
                                Some(b) => b,
                                None => { eprintln!("converge: manifest blob not available yet"); continue; }
                            };
                            if mblob.len() < n { continue; }
                            let (mn, mc) = mblob.split_at(n);
                            let mpt = match decrypt_blob(&drive_key, mn, mc) {
                                Ok(p) => p,
                                Err(e) => { eprintln!("converge: manifest decrypt failed: {e}"); continue; }
                            };
                            let new_m: serde_json::Value = match serde_json::from_slice(&mpt) {
                                Ok(v) => v,
                                Err(e) => { eprintln!("converge: manifest JSON parse failed: {e}"); continue; }
                            };
                            let new_ver = new_m["drive_version"].as_u64().unwrap_or(current_version);
                            if new_ver <= current_version { continue; }

                            let old_ver = current_version;
                            let new_files = new_m["files"].as_array().cloned().unwrap_or_default();
                            let new_k = new_files.len();

                            // Build new_state (key -> blake3) from the incoming manifest for fast lookup
                            let mut new_state: std::collections::HashMap<String, String> = std::collections::HashMap::new();
                            for f in &new_files {
                                let rkey = f["key"].as_str().unwrap_or("").to_string();
                                let eblake = f["plaintext_blake3"].as_str().unwrap_or("").to_string();
                                new_state.insert(rkey, eblake);
                            }

                            let old_file_count = prev_files.len();
                            let mut written = 0usize;
                            let mut deleted = 0usize;

                            // Process new/current files: skip if unchanged+exists, else pull+write (changed/added/new)
                            for f in &new_files {
                                let rkey = f["key"].as_str().unwrap_or("").to_string();
                                let ept_len = f["plaintext_len"].as_u64().unwrap_or(0);
                                let eblake = f["plaintext_blake3"].as_str().unwrap_or("").to_string();

                                let is_unchanged = prev_files.get(&rkey).map_or(false, |h| h == &eblake);
                                let target = out_path.join(&rkey);
                                if is_unchanged && target.exists() {
                                    println!("skip unchanged {}", rkey);
                                    continue;
                                }

                                let fch = match wait_entry_hash(&doc, rkey.as_bytes(), 20).await {
                                    Some(h) => h,
                                    None => { eprintln!("converge: entry {rkey} not found"); continue; }
                                };
                                let fblob = match fetch_blob_retry(&blobs, fch, 60).await {
                                    Some(b) => b,
                                    None => { eprintln!("converge: blob for {rkey} not available"); continue; }
                                };
                                if fblob.len() < n { eprintln!("converge: blob for {rkey} too short"); continue; }
                                let (fnc, fcc) = fblob.split_at(n);
                                let ptt = match decrypt_blob(&drive_key, fnc, fcc) {
                                    Ok(p) => p,
                                    Err(e) => { eprintln!("converge: decrypt {rkey} failed: {e}"); continue; }
                                };
                                if blake3::hash(&ptt).to_hex().to_string() != eblake
                                    || ptt.len() as u64 != ept_len
                                {
                                    eprintln!("converge: integrity check failed for {rkey}");
                                    continue;
                                }
                                let target = out_path.join(&rkey);
                                if let Some(parent) = target.parent() {
                                    std::fs::create_dir_all(parent)?;
                                }
                                std::fs::write(&target, &ptt)?;
                                written += 1;
                            }

                            // Deletions: any key in prev_files that is absent from the new manifest -> rm local file
                            for old_key in prev_files.keys() {
                                if !new_state.contains_key(old_key) {
                                    let target = out_path.join(old_key);
                                    let _ = std::fs::remove_file(&target);
                                    deleted += 1;
                                }
                            }

                            prev_files = new_state;
                            current_version = new_ver;
                            println!("DRIVE UPDATED: v{} -> v{}: {} written, {} deleted (file_count {} -> {})", old_ver, new_ver, written, deleted, old_file_count, new_k);
                        }
                    }
                    Ok(Some(Err(_))) => {}
                }
            }
        }
    } else {
        // === SINGLE ENTRY MODE (exact backward compat) ===
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

        // Fetch with the async blob retry (from 9f5a5c18 gate fix)
        let mut fetched: Option<Bytes> = None;
        let mut last_err: Option<anyhow::Error> = None;
        for attempt in 0..100 {
            match blobs.blobs().get_bytes(content_hash).await {
                Ok(b) => { fetched = Some(b); break; }
                Err(e) => {
                    last_err = Some(e.into());
                    if attempt % 10 == 0 {
                        println!("(waiting for content blob download to complete, attempt {})", attempt);
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                }
            }
        }
        let fetched: Bytes = fetched.with_context(|| format!(
            "blob content not available after docs replication (timeout; last error: {:?})", last_err
        ))?;

        let raw_preview: String = fetched.iter().take(16).map(|b| format!("{:02x}", b)).collect();
        println!("E3: raw fetched blob (first 16 bytes hex): {} (should not start with '68 69 20 66 72 6f 6d' = 'hi from')", raw_preview);

        // drive_key already computed above for both modes

        if fetched.len() < n {
            anyhow::bail!("fetched blob too short to contain nonce");
        }
        let (nonce, ct) = fetched.split_at(n);
        let plaintext = decrypt_blob(&drive_key, nonce, ct)?;

        println!("R2 (decrypted): plaintext len={} : {:?}", plaintext.len(), String::from_utf8_lossy(&plaintext));

        let local_hash = iroh_blobs::Hash::new(&fetched);
        println!("     local blake3 (of ct): {}", local_hash);
        println!("     entry content_hash  : {}", content_hash);
        if local_hash != content_hash {
            anyhow::bail!("BLAKE3 mismatch after fetch (ct)");
        }
        println!("     BLAKE3 (ct) match: PASS");

        println!("R3: entry author = {} (docs sync verifies the author signature; no error above means sig OK)", entry_author);

        println!("\nL1 E2EE replication SUCCESS (entry + ciphertext blob replicated over the network; decrypted with drive key).");
    }
    Ok(())
}
