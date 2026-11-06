// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! The remote tools the CLI agent offers reach a saved server.
//!
//! `aeroftp-cli agent` runs every tool call through `CliToolCtx`, and that
//! context gave the remote tools no backend: `remote_list` and its siblings
//! answered "not available in CLI mode" for every saved server. It now hands
//! them the MCP server's backend over a process-wide pool. This test lists a
//! folder served by `aeroftp-cli serve ftp` through that context, from a
//! profile imported into a private master-password vault.
//!
//! Unix only: the vault directory follows `HOME` and `XDG_CONFIG_HOME` there,
//! so this process and the importing CLI share a vault nobody else uses. On
//! Windows the config folder comes from the Known Folder API and cannot be
//! moved by the environment (see `support/isolated_cli.rs`).
#![cfg(unix)]

#[path = "support/isolated_cli.rs"]
mod isolated_cli;

use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use ftp_client_gui_lib::ai_core::cli_impl::{CliCredentialProvider, CliEventSink, CliToolCtx};
use ftp_client_gui_lib::ai_core::tools::dispatch_tool;
use ftp_client_gui_lib::credential_store::CredentialStore;

const USER: &str = "u";
const PASSWORD: &str = "cli agent remote tools server";
const MASTER: &str = "cli agent remote tools vault";
const LISTED: &str = "listed-by-the-agent.txt";

fn cli() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_aeroftp-cli"))
}

/// rclone's `obscure` (AES-256-CTR under its published key, then raw URL-safe
/// base64 of IV and ciphertext): the form an rclone.conf `pass` takes.
fn obscure(plain: &str) -> String {
    use aes::cipher::{KeyIvInit, StreamCipher};
    use base64::Engine;
    const KEY: [u8; 32] = [
        0x9c, 0x93, 0x5b, 0x48, 0x73, 0x0a, 0x55, 0x4d, 0x6b, 0xfd, 0x7c, 0x63, 0xc8, 0x86, 0xa9,
        0x2b, 0xd3, 0x90, 0x19, 0x8e, 0xb8, 0x12, 0x8a, 0xfb, 0xf4, 0xde, 0x16, 0x2b, 0x8b, 0x95,
        0xf6, 0x38,
    ];
    let iv = [7u8; 16];
    let mut bytes = plain.as_bytes().to_vec();
    ctr::Ctr128BE::<aes::Aes256>::new((&KEY).into(), (&iv).into()).apply_keystream(&mut bytes);
    let mut out = iv.to_vec();
    out.extend_from_slice(&bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(out)
}

/// A loopback port nobody listens on right now.
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind a free port")
        .local_addr()
        .unwrap()
        .port()
}

/// `serve ftp` of `folder` with its own private vault, killed when dropped.
struct Server {
    child: Child,
    port: u16,
    _config: tempfile::TempDir,
}

impl Server {
    fn start(folder: &Path) -> Self {
        let config = isolated_cli::root();
        let port = free_port();
        let child = isolated_cli::command(cli(), config.path())
            .args(["serve", "ftp"])
            .arg(folder)
            .args(["--addr", &format!("127.0.0.1:{port}")])
            .env("AEROFTP_SERVE_USER", USER)
            .env("AEROFTP_SERVE_PASSWORD", PASSWORD)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn aeroftp-cli serve ftp");
        let deadline = Instant::now() + Duration::from_secs(60);
        while TcpStream::connect(("127.0.0.1", port)).is_err() {
            assert!(Instant::now() < deadline, "serve ftp did not answer");
            std::thread::sleep(Duration::from_millis(100));
        }
        Self {
            child,
            port,
            _config: config,
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_cli_agent_context_lists_a_saved_server() {
    // The vault of this process and of the importing CLI: a private home, set
    // before anything reads the config folder. The only test of this binary,
    // so no other test sees these variables.
    let home = tempfile::tempdir().expect("private home");
    for name in ["HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_CACHE_HOME"] {
        std::env::set_var(name, home.path());
    }
    std::env::set_var("AEROFTP_MASTER_PASSWORD", MASTER);
    std::env::remove_var("RUST_LOG");

    let served = tempfile::tempdir().expect("served folder");
    std::fs::write(served.path().join(LISTED), b"served").unwrap();
    let server = Server::start(served.path());

    let conf = home.path().join("rclone.conf");
    std::fs::write(
        &conf,
        format!(
            "[lab]\ntype = ftp\nhost = 127.0.0.1\nport = {}\nuser = {USER}\npass = {}\n",
            server.port,
            obscure(PASSWORD)
        ),
    )
    .unwrap();
    // The original binary, without a portable marker: its vault follows the
    // home set above, the same one this process opens.
    let imported = std::process::Command::new(cli())
        .current_dir(home.path())
        .args(["import", "rclone"])
        .arg(&conf)
        .arg("--apply")
        .stdin(Stdio::null())
        .output()
        .expect("run aeroftp-cli import rclone");
    assert!(
        imported.status.success(),
        "import: {}\n{}",
        String::from_utf8_lossy(&imported.stdout),
        String::from_utf8_lossy(&imported.stderr)
    );

    CredentialStore::unlock_with_master(MASTER, None).expect("open the private vault");
    let ctx = CliToolCtx::new(CliEventSink { json_mode: true }, CliCredentialProvider);
    let listing = dispatch_tool(
        &ctx,
        "remote_list",
        &serde_json::json!({"server": "lab", "path": "/"}),
    )
    .await
    .expect("the CLI agent context lists the saved server");
    assert!(listing.to_string().contains(LISTED), "{listing}");
    drop(server);
}
