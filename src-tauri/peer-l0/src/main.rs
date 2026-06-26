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

use aeroftp_peer_l0::drive::{
    decode_content_key, load_capability_token, load_identity, random_content_key, run_docs_publish,
    run_docs_replicate, write_identity, CapIssue, PublishKey, ReplicateCap, ReplicateKey,
};
use aeroftp_peer_l0::{
    decode_secret, derive_session_key, encode_secret, generate_pairing_secret, recv_encrypted_blob,
    recv_offer, send_encrypted_blob, send_offer, ConnectivitySample, Identity, IdentityPublic,
};

#[derive(Parser, Debug)]
#[command(
    name = "peer-l0-dial",
    version,
    about = "AeroFTP Peer L0 spike dialer (isolated)"
)]
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

        /// Number of republishes AFTER v1 (only with --dir + --republish-after). Default 1 keeps
        /// Stage 5/6 behavior (v1 then one v2). Use e.g. 2 for v1->v2->v3.
        #[arg(long, default_value_t = 1)]
        republish_count: u64,

        /// Optional persistent store directory. If present, use FsStore + Docs::persistent
        /// (drive survives publisher restart). Only affects publish side for now.
        /// Format: --store /path/to/store  (will use subdirs blobs/ and docs/)
        #[arg(long)]
        store: Option<String>,

        /// WI-4c capability path: publisher identity file (64 secret bytes). Mutually exclusive
        /// with --secret. When present, the drive key is a per-drive RANDOM content key and each
        /// --grant gets a sealed capability token.
        #[arg(long)]
        identity: Option<String>,

        /// WI-4c: AeroFTP-ID to issue a sealed capability to (repeatable; >=1 on the capability path).
        #[arg(long)]
        grant: Vec<String>,

        /// WI-4c: optional explicit 32-byte content key (base64url); default = fresh random.
        #[arg(long)]
        content_key: Option<String>,

        /// WI-4c: optional dir to write each sealed token to as <recipient-short>.token.
        #[arg(long)]
        cap_out: Option<String>,
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

        /// Optional persistent store directory (FsStore + Docs::persistent). If present the replicate
        /// side can resume a previously synced drive from local disk (even with publisher offline) and
        /// keep live-watching for subsequent versions (converge on manifest bumps). Requires --out.
        /// Mirrors the publish --store added in Stage 8.
        #[arg(long)]
        store: Option<String>,

        /// WI-4c capability path: my identity file (64 secret bytes). Mutually exclusive with --secret.
        #[arg(long)]
        identity: Option<String>,

        /// WI-4c: the aeroftp-drive:// capability token, or a path to a file containing it.
        #[arg(long)]
        capability: Option<String>,

        /// WI-4c: expected issuer AeroFTP-ID (the publisher); REQUIRED with --capability.
        #[arg(long)]
        issuer: Option<String>,
    },

    /// WI-4c: generate a fresh peer Identity, write its 64 secret bytes (0600) to --out, print the AeroFTP-ID.
    IdentityNew {
        /// Output file for the 64 secret bytes (will be chmod 0600 on unix).
        #[arg(long)]
        out: String,
    },

    /// WI-4c: print the AeroFTP-ID of a saved identity file.
    IdentityShow {
        /// Path to a 64-byte identity secret file.
        #[arg(long)]
        identity: String,
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
    let effective_secret = cli
        .secret
        .or_else(|| std::env::var("AEROFTP_PEER_SECRET").ok());

    // Shared endpoint config: carries the optional custom-relay override (bind_addr is
    // set per-mode for the listener). Built once and threaded into listen/dial.
    // Clone so later docs- arms can also read it (L0 paths consume one copy).
    let custom_relay = cli.custom_relay_urls.clone();
    let cfg = aeroftp_peer_l0::endpoint::PeerEndpointConfig {
        bind_addr: None,
        secret_key_path: None,
        custom_relay_urls: custom_relay,
        ..Default::default()
    };

    match cli.mode {
        Mode::Listen { port, count } => {
            let samples =
                run_listen_multi(port, cli.note.clone(), effective_secret, count, cfg).await;

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
                    sample.path,
                    sample.total_duration_ms,
                    sample.connect_duration_ms,
                    sample.transfer_duration_ms,
                    sample.network_note
                );
            } else {
                println!(
                    "SAMPLE_FAIL error={:?} note={:?}",
                    sample.error, sample.network_note
                );
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
        Mode::DocsPublish {
            key,
            dir,
            republish_after,
            republish_count,
            store,
            identity,
            grant,
            content_key,
            cap_out,
        } => {
            let docs_cfg = aeroftp_peer_l0::endpoint::PeerEndpointConfig {
                bind_addr: None,
                secret_key_path: None,
                custom_relay_urls: cli.custom_relay_urls.clone(),
                ..Default::default()
            };
            // WI-4c: --identity selects the capability path (per-drive random content key); otherwise
            // the legacy --secret dev path. The two are mutually exclusive.
            let publish_key = if let Some(id_path) = identity {
                if effective_secret.is_some() {
                    anyhow::bail!("--identity and --secret are mutually exclusive (capability path OR dev path)");
                }
                if grant.is_empty() {
                    anyhow::bail!("the capability path requires at least one --grant <AeroFTP-ID>");
                }
                let issuer = load_identity(&id_path)?;
                let mut grants = Vec::with_capacity(grant.len());
                for afid in &grant {
                    let pk = IdentityPublic::from_aeroftp_id(afid)
                        .with_context(|| format!("invalid --grant AeroFTP-ID: {afid}"))?;
                    grants.push((afid.clone(), pk));
                }
                let ck = match content_key.as_deref() {
                    Some(s) => decode_content_key(s)?,
                    None => random_content_key(),
                };
                let drive_name = dir
                    .as_deref()
                    .and_then(|d| Path::new(d).file_name().and_then(|s| s.to_str()))
                    .unwrap_or("drive")
                    .to_string();
                PublishKey::Capability {
                    content_key: ck,
                    issue: Box::new(CapIssue {
                        issuer,
                        grants,
                        cap_out,
                        drive_name,
                    }),
                }
            } else {
                let secret = effective_secret
                    .as_deref()
                    .map(decode_secret)
                    .transpose()?
                    .context(
                        "docs-publish requires --secret (dev path) or --identity (capability path)",
                    )?;
                PublishKey::DevSecret(secret)
            };
            run_docs_publish(
                key,
                dir,
                republish_after,
                republish_count,
                store,
                docs_cfg,
                publish_key,
                None, // WI-4d ready-hook: the spike CLI does not need the namespace handed back
            )
            .await?;
            return Ok(());
        }
        Mode::DocsReplicate {
            ticket,
            out,
            watch_secs,
            store,
            identity,
            capability,
            issuer,
        } => {
            let docs_cfg = aeroftp_peer_l0::endpoint::PeerEndpointConfig {
                bind_addr: None,
                secret_key_path: None,
                custom_relay_urls: cli.custom_relay_urls.clone(),
                ..Default::default()
            };
            // WI-4c: --capability selects the capability path; otherwise the legacy --secret dev path.
            let key_src = if let Some(token_arg) = capability {
                if effective_secret.is_some() {
                    anyhow::bail!("--capability and --secret are mutually exclusive");
                }
                let id_path = identity
                    .context("--capability requires --identity (your own identity file)")?;
                let issuer_afid = issuer
                    .context("--capability requires --issuer <expected publisher AeroFTP-ID>")?;
                let me = load_identity(&id_path)?;
                let expected_issuer = IdentityPublic::from_aeroftp_id(&issuer_afid)
                    .context("invalid --issuer AeroFTP-ID")?;
                let token = load_capability_token(&token_arg)?;
                ReplicateKey::Capability(Box::new(ReplicateCap {
                    me,
                    expected_issuer,
                    token,
                }))
            } else {
                let secret = effective_secret
                    .as_deref()
                    .map(decode_secret)
                    .transpose()?
                    .context("docs-replicate requires --secret (dev path) or --capability (capability path)")?;
                ReplicateKey::DevSecret(secret)
            };
            run_docs_replicate(ticket, out, watch_secs, store, docs_cfg, key_src, None).await?;
            return Ok(());
        }
        Mode::IdentityNew { out } => {
            let id = Identity::generate();
            write_identity(&out, &id)?;
            println!("{}", id.public().to_aeroftp_id());
            return Ok(());
        }
        Mode::IdentityShow { identity } => {
            let id = load_identity(&identity)?;
            println!("{}", id.public().to_aeroftp_id());
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
                    if p.extension().is_some_and(|e| e == "json") {
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
    anyhow::bail!(
        "{} is not a valid ConnectivitySample or array of them",
        path.display()
    )
}

fn print_campaign_summary(samples: &[ConnectivitySample]) {
    let total = samples.len();
    let successes: Vec<_> = samples.iter().filter(|s| s.success).collect();
    let fails = total - successes.len();
    let success_rate = if total > 0 {
        (successes.len() as f64 / total as f64) * 100.0
    } else {
        0.0
    };

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
    let keywords = [
        "CGNAT",
        "mobile",
        "home",
        "office",
        "hotel",
        "VPN",
        "Starlink",
        "4G",
        "5G",
        "double NAT",
        "residential",
    ];
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
        println!(
            "{},{},{},{},{}",
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

    let remote_node_str = conn
        .remote_node_id()
        .map(|id| id.to_string())
        .unwrap_or_default();
    let _remote_fp = if remote_node_str.len() > 12 {
        &remote_node_str[..12]
    } else {
        &remote_node_str
    };

    let offer = match recv_offer(&conn).await {
        Ok(o) => o,
        Err(e) => {
            conn.close(1u32.into(), b"bad-offer");
            return ConnectivitySample::failure(e, note.clone());
        }
    };

    let remote_node_str2 = conn
        .remote_node_id()
        .map(|id| id.to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    let _remote_fp = if remote_node_str2.len() > 12 {
        &remote_node_str2[..12]
    } else {
        &remote_node_str2
    };

    println!("\n--- Incoming peer offer ---");
    println!(
        "From NodeID: {} (fingerprint: {})",
        remote_node_str, remote_node_str2
    );
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
        conn.close(0u32.into(), b"rejected-by-user");
        println!("Rejected by user.");
        return ConnectivitySample::failure("user rejected", note.clone());
    }

    let local_node_str = node.to_string();
    let key = derive_session_key(secret_bytes, &local_node_str, &remote_node_str);

    let xfer_start = Instant::now();
    let received = match recv_encrypted_blob(&conn, &offer, &key).await {
        Ok(plain) => plain,
        Err(e) => {
            conn.close(1u32.into(), b"recv-failed");
            return ConnectivitySample::failure(e, note.clone());
        }
    };
    let xfer_duration = xfer_start.elapsed().as_millis() as u64;

    // Snapshot the path type (direct/mixed/relay) while the connection is still
    // open, for the L0 gate's hole-punch-vs-relay accounting.
    if let Ok(rid) = conn.remote_node_id() {
        path = conn_type_label(ep, rid).await;
    }

    println!(
        "Received and decrypted {} bytes. BLAKE3 verified after decryption.",
        received.len()
    );

    // C: Controlled inbox + basic guards (for the spike; real version will be under per-user private storage).
    let inbox_dir = std::path::Path::new("l0-peer-inbox");
    let _ = std::fs::create_dir_all(inbox_dir);

    // Simple guard (offer.size is the claimed plaintext size).
    if offer.size > 256 * 1024 * 1024 {
        conn.close(1u32.into(), b"too-large");
        return ConnectivitySample::failure("file exceeds spike safety cap (256MB)", note.clone());
    }

    let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
    let safe_name = offer
        .name_hint
        .replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|'], "_");
    let final_name = format!("{}_{}", ts, safe_name);
    let file_path = inbox_dir.join(&final_name);

    if std::fs::write(&file_path, &received).is_ok() {
        let meta = serde_json::json!({
            "sender_node": remote_node_str,
            "sender_fingerprint": remote_node_str2,
            "received_at": ts,
            "original_name_hint": offer.name_hint,
            "note": offer.note,
            "plaintext_hash": offer.hash.to_string(),
            "size": received.len(),
        });
        let _ = std::fs::write(
            file_path.with_extension("meta.json"),
            serde_json::to_string_pretty(&meta).unwrap_or_default(),
        );
        println!("Saved to controlled inbox: {}", file_path.display());
    } else {
        println!("Warning: failed to persist to inbox (still counted as success for measurement).");
    }

    println!("L0 receive complete (E2EE + inbox).");

    conn.close(0u32.into(), b"l0-ok");

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
        // Exits when the watcher disconnects (`Err`); then use whatever we have.
        while let Ok(val) = w.updated().await {
            let s = format!("{:?}", val);
            if !s.starts_with("None") {
                return classify_conn_type_debug(&s);
            }
            // still None after this update; wait for the next one
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
    let ep = match aeroftp_peer_l0::endpoint::PeerEndpoint::new(cfg).await {
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
    println!(
        "Short fingerprint: {}",
        &node.to_string()[..12.min(node.to_string().len())]
    );
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
                sample.path,
                sample.total_duration_ms,
                sample.connect_duration_ms,
                sample.transfer_duration_ms,
                sample.network_note
            );
        } else {
            println!(
                "SAMPLE_FAIL error={:?} note={:?}",
                sample.error, sample.network_note
            );
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

async fn run_dial(
    node_str: &str,
    blob_size: usize,
    note: Option<String>,
    secret_opt: Option<String>,
    cfg: aeroftp_peer_l0::endpoint::PeerEndpointConfig,
) -> ConnectivitySample {
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

    let ep = match aeroftp_peer_l0::endpoint::PeerEndpoint::new(cfg).await {
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

    let offer =
        aeroftp_peer_l0::endpoint::PeerBlobOffer::new(hash, data.len() as u64, "l0-spike-test.bin")
            .with_note(note.clone().unwrap_or_default());

    // Send the (still plaintext) offer so the receiver can see size/name/note before deciding.
    if let Err(e) = send_offer(&conn, &offer).await {
        conn.close(1u32.into(), b"offer-failed");
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
            conn.close(1u32.into(), b"encrypt-failed");
            return ConnectivitySample::failure(e, note);
        }
    };

    // Measure actual transfer time.
    let xfer_start = Instant::now();
    if let Err(e) = send_encrypted_blob(&conn, &nonce, &ciphertext).await {
        conn.close(1u32.into(), b"send-failed");
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
    conn.close(0u32.into(), b"l0-ok");

    let total_duration = total_start.elapsed().as_millis() as u64;

    let mut sample =
        ConnectivitySample::success(total_duration, connect_duration, xfer_duration, path, note);

    // Enhanced diagnostics for real data collection (same limitation note as listen side).
    sample.diagnostics = Some(format!(
        "local_node={local_node_str} remote_node={remote_node_str} connect_time_ms={connect_duration}; connection=quic; iroh_path={}",
        sample.path
    ));

    sample
}
