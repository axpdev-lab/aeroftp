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

use aeroftp_peer_l0::{recv_blob, recv_offer, send_blob, send_offer, ConnectivitySample};

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
}

#[derive(Subcommand, Debug)]
enum Mode {
    /// Listen for one incoming peer connection and receive a blob (after explicit approval).
    Listen {
        /// Optional explicit port (0 = random). Printed together with the NodeID.
        #[arg(long, default_value_t = 0)]
        port: u16,
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
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "info,aeroftp_peer_l0=debug".to_string()),
        )
        .init();

    let cli = Cli::parse();

    let sample = match cli.mode {
        Mode::Listen { port } => run_listen(port, cli.note.clone()).await,
        Mode::Dial { node, size } => run_dial(&node, size, cli.note.clone()).await,
    };

    if let Some(path) = cli.report {
        let json = serde_json::to_string_pretty(&sample)?;
        std::fs::write(&path, json)?;
        info!(path = %path.display(), "report written");
    }

    if !sample.success {
        // Non-zero exit helps scripts collecting many samples.
        std::process::exit(2);
    }

    Ok(())
}

async fn run_listen(port: u16, note: Option<String>) -> ConnectivitySample {
    let start = Instant::now();

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
            return ConnectivitySample::failure(e, note);
        }
    };

    let node = ep.node_id();
    println!("=== AERO FTP PEER L0 LISTEN ===");
    println!("NodeID: {}", node);
    println!("(share this exact string with the sender)");
    println!("Waiting for one connection... (Ctrl-C to abort)");

    let conn = match ep.accept().await {
        Ok(c) => c,
        Err(e) => return ConnectivitySample::failure(e, note),
    };

    // For L0 we only accept a single connection per listen run.
    let used_relay = false; // TODO: later inspect conn info for relay usage

    // Receive the offer first (small JSON or length-prefixed struct).
    let offer = match recv_offer(&conn).await {
        Ok(o) => o,
        Err(e) => {
            let _ = conn.close(1u32.into(), b"bad-offer");
            return ConnectivitySample::failure(e, note);
        }
    };

    println!("\n--- Incoming peer offer ---");
    println!("Hash: {}", offer.hash);
    println!("Size: {} bytes", offer.size);
    println!("Name hint: {}", offer.name_hint);
    if let Some(n) = &offer.note {
        println!("Note: {}", n);
    }
    println!("---------------------------");

    // Explicit human approval — this is mandatory even in the spike.
    // In the real product this will be a GUI dialog with file icon, size, sender fingerprint, etc.
    print!("Accept this transfer? [y/N]: ");
    io::stdout().flush().ok();
    let mut answer = String::new();
    if io::stdin().read_line(&mut answer).is_err() || !answer.trim().eq_ignore_ascii_case("y") {
        let _ = conn.close(0u32.into(), b"rejected-by-user");
        println!("Rejected by user.");
        return ConnectivitySample::failure("user rejected", note);
    }

    // Receive the actual blob bytes (for L0 we just throw them away after verifying the hash,
    // or write to a temp "peer-inbox" dir in a later slice).
    let received = match recv_blob(&conn, &offer).await {
        Ok(data) => data,
        Err(e) => {
            let _ = conn.close(1u32.into(), b"recv-failed");
            return ConnectivitySample::failure(e, note);
        }
    };

    println!("Received {} bytes. BLAKE3 verified (by the protocol layer).", received.len());
    println!("L0 receive complete. In a real client we would now store this under the active user's peer-inbox.");

    let _ = conn.close(0u32.into(), b"l0-ok");

    let duration = start.elapsed().as_millis() as u64;
    ConnectivitySample::success(duration, used_relay, note)
}

async fn run_dial(node_str: &str, blob_size: usize, note: Option<String>) -> ConnectivitySample {
    let start = Instant::now();

    let remote: NodeId = match node_str.parse() {
        Ok(n) => n,
        Err(e) => return ConnectivitySample::failure(format!("bad NodeId: {e}"), note),
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

    let conn = match ep.connect(remote, aeroftp_peer_l0::PEER_L0_ALPN).await {
        Ok(c) => c,
        Err(e) => return ConnectivitySample::failure(e, note),
    };

    // Build a tiny offer + random blob for the gate test.
    let data: Vec<u8> = (0..blob_size).map(|i| (i % 251) as u8).collect();
    let hash = Hash::new(&data); // iroh-blobs Hash is a typed BLAKE3 hash

    let offer = aeroftp_peer_l0::endpoint::PeerBlobOffer::new(hash, data.len() as u64, "l0-spike-test.bin")
        .with_note(note.clone().unwrap_or_default());

    if let Err(e) = send_offer(&conn, &offer).await {
        let _ = conn.close(1u32.into(), b"offer-failed");
        return ConnectivitySample::failure(e, note);
    }

    if let Err(e) = send_blob(&conn, &data).await {
        let _ = conn.close(1u32.into(), b"send-failed");
        return ConnectivitySample::failure(e, note);
    }

    println!("Blob sent ({} bytes, hash {}). Waiting for close...", data.len(), hash);

    // Graceful close from the other side or timeout.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let _ = conn.close(0u32.into(), b"l0-sent");

    let duration = start.elapsed().as_millis() as u64;

    // We don't know from the client side whether a relay was used without extra API.
    // For the gate we can enhance later with Connection::remote_address_info or similar.
    let used_relay = false;

    ConnectivitySample::success(duration, used_relay, note)
}
