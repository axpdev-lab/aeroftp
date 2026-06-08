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

use anyhow::Result;
use clap::{Parser, Subcommand};
use iroh::NodeId;
use iroh_blobs::Hash;
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tracing::info;

use aeroftp_peer_l0::{
    decode_secret, derive_session_key, encode_secret, generate_pairing_secret,
    recv_encrypted_blob, recv_offer, send_encrypted_blob, send_offer, ConnectivitySample,
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

    match cli.mode {
        Mode::Listen { port, count } => {
            let samples = run_listen_multi(port, cli.note.clone(), effective_secret, count).await;

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
            let sample = run_dial(&node, size, cli.note.clone(), effective_secret).await;

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

    let path = "unknown".to_string();

    let remote_node_str = conn.remote_node_id().map(|id| id.to_string()).unwrap_or_default();
    let remote_fp = if remote_node_str.len() > 12 { &remote_node_str[..12] } else { &remote_node_str };

    let offer = match recv_offer(&conn).await {
        Ok(o) => o,
        Err(e) => {
            let _ = conn.close(1u32.into(), b"bad-offer");
            return ConnectivitySample::failure(e, note.clone());
        }
    };

    let remote_node_str2 = conn.remote_node_id().map(|id| id.to_string()).unwrap_or_else(|_| "unknown".to_string());
    let remote_fp = if remote_node_str2.len() > 12 { &remote_node_str2[..12] } else { &remote_node_str2 };

    println!("\n--- Incoming peer offer ---");
    println!("From NodeID: {} (fingerprint: {})", remote_node_str, remote_fp);
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
            "sender_fingerprint": remote_fp,
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
        "remote_node={remote_node_str} local_node={local_node_str}; connection=quic; path_info_limited_in_this_iroh_version"
    ));
    sample
}



async fn run_listen_multi(
    port: u16,
    note: Option<String>,
    secret_opt: Option<String>,
    count: u32,
) -> Vec<ConnectivitySample> {
    let ep = match aeroftp_peer_l0::endpoint::PeerEndpoint::new(
        aeroftp_peer_l0::endpoint::PeerEndpointConfig {
            bind_addr: if port == 0 {
                None
            } else {
                Some(([0, 0, 0, 0], port).into())
            },
            secret_key_path: None,
        },
    )
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

async fn run_dial(node_str: &str, blob_size: usize, note: Option<String>, secret_opt: Option<String>) -> ConnectivitySample {
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

    let ep = match aeroftp_peer_l0::endpoint::PeerEndpoint::new(
        aeroftp_peer_l0::endpoint::PeerEndpointConfig::default(),
    )
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

    // Best-effort path info for measurement.
    let path = "unknown".to_string();

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
        "Encrypted blob sent ({} bytes plaintext, {} bytes ciphertext, hash {}).",
        data.len(),
        ciphertext.len(),
        hash
    );
    println!("Waiting for close...");

    tokio::time::sleep(Duration::from_millis(200)).await;
    let _ = conn.close(0u32.into(), b"l0-sent");

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
        "local_node={local_node_str} remote_node={remote_node_str} connect_time_ms={connect_duration}; connection=quic; path_info_limited_in_this_iroh_version"
    ));

    sample
}
