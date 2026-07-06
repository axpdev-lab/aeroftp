// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! App-aware status probe for the local bridge helper apps (Filen Desktop,
//! MEGAcmd) that back the "Local WebDAV" / "Local S3" / "WebDAV (MEGAcmd)"
//! Quick Connect modes (issue #215 follow-up, owner UX request).
//!
//! Each of those modes needs a helper app installed and running on the
//! loopback. The frontend turns this probe into a 🔴/🟠/🟢 status dot and an
//! app-aware message:
//!   - not installed         -> 🔴, Save disabled
//!   - installed, not active  -> 🟠
//!   - installed and active   -> 🟢
//!
//! "active" is a cheap TCP connect to the loopback bridge port. "installed" is
//! best-effort: reliable for MEGAcmd (we already resolve and spawn its binary),
//! and a known-paths heuristic for Filen Desktop. When installation cannot be
//! told (the common Filen case on Linux) the probe reports `install_known:
//! false` so a legitimate user is never hard-blocked on a false negative.

use serde::Serialize;
use std::time::Duration;
use tokio::net::TcpStream;

/// Loopback connect budget. The bridges are local, so a healthy port answers in
/// single-digit ms; this only bounds the "app is down" path.
const PROBE_TIMEOUT_MS: u64 = 400;

#[derive(Debug, Clone, Serialize)]
pub struct BridgeStatus {
    /// Helper app detected on this machine (best-effort; see `install_known`).
    pub installed: bool,
    /// Bridge port reachable on the loopback right now.
    pub active: bool,
    /// The loopback port that was probed.
    pub port: u16,
    /// True when `installed` is a confident reading. For apps we cannot reliably
    /// detect (Filen Desktop when not running) this is `false`, and the UI must
    /// NOT show the hard 🔴 / disable Save: it falls back to the 🟠 "not active"
    /// state instead.
    pub install_known: bool,
}

/// Traffic-light state, mirroring the frontend `deriveBridgeUiState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeUiState {
    /// Bridge reachable (app installed and active).
    Green,
    /// Installed but not active, or installation unknown.
    Amber,
    /// Confidently not installed.
    Red,
}

impl BridgeStatus {
    /// Map a probe result to the traffic-light state. Same rules as the GUI:
    /// active → green; confidently-not-installed → red; everything else → amber.
    pub fn ui_state(&self) -> BridgeUiState {
        if self.active {
            BridgeUiState::Green
        } else if self.install_known && !self.installed {
            BridgeUiState::Red
        } else {
            BridgeUiState::Amber
        }
    }
}

/// Map a bridge `kind` to its default loopback port. Pure, for testability.
pub fn bridge_kind_default_port(kind: &str) -> Option<u16> {
    match kind {
        "filen-webdav" => Some(1900),
        "filen-s3" => Some(1800),
        "megacmd-webdav" => Some(4443),
        _ => None,
    }
}

/// Map a GUI registry `providerId` to its bridge `kind`, if it is a local-bridge
/// preset. Lets the CLI (and anything else holding a profile's providerId) reuse
/// the same probe. Returns `None` for non-bridge providers.
pub fn bridge_kind_for_provider_id(provider_id: &str) -> Option<&'static str> {
    match provider_id {
        "filen-desktop-webdav" => Some("filen-webdav"),
        "filen-desktop-s3" => Some("filen-s3"),
        "megacmd" | "megacmd-webdav" => Some("megacmd-webdav"),
        _ => None,
    }
}

/// Probe whether a loopback port accepts a TCP connection (async, for the Tauri
/// command running inside the app's tokio runtime).
async fn port_open(port: u16) -> bool {
    let addr = format!("127.0.0.1:{}", port);
    matches!(
        tokio::time::timeout(
            Duration::from_millis(PROBE_TIMEOUT_MS),
            TcpStream::connect(&addr),
        )
        .await,
        Ok(Ok(_))
    )
}

/// Blocking loopback port probe (for synchronous callers like the CLI, which
/// runs inside `#[tokio::main]` where a nested `block_on` would panic).
fn port_open_blocking(port: u16) -> bool {
    use std::net::{SocketAddr, TcpStream as StdTcpStream};
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    StdTcpStream::connect_timeout(&addr, Duration::from_millis(PROBE_TIMEOUT_MS)).is_ok()
}

/// Decide `(installed, install_known)` from the bridge kind and live reachability.
/// Shared by the async and blocking probes so the rules never drift.
fn decide_install(kind: &str, active: bool) -> (bool, bool) {
    if active {
        // A reachable bridge proves the app is installed and known.
        (true, true)
    } else if kind == "megacmd-webdav" {
        (megacmd_installed(), true)
    } else {
        match filen_desktop_installed() {
            Some(i) => (i, true),
            None => (true, false), // unknown: never hard-block Save
        }
    }
}

/// Look up a bare command name on PATH (and Windows `.exe`/`.bat` variants).
fn which_on_path(cmd: &str) -> bool {
    let path = match std::env::var_os("PATH") {
        Some(p) => p,
        None => return false,
    };
    let exe_names: Vec<String> = {
        #[cfg(windows)]
        {
            vec![
                format!("{}.exe", cmd),
                format!("{}.bat", cmd),
                cmd.to_string(),
            ]
        }
        #[cfg(not(windows))]
        {
            vec![cmd.to_string()]
        }
    };
    for dir in std::env::split_paths(&path) {
        for name in &exe_names {
            if dir.join(name).exists() {
                return true;
            }
        }
    }
    false
}

/// Reliable MEGAcmd detection: the binary resolves to an existing install path
/// or is found on PATH. Reuses the same resolver we spawn `mega-*` with.
fn megacmd_installed() -> bool {
    let resolved = crate::providers::mega_df::resolve_mega_cmd("mega-webdav");
    let p = std::path::Path::new(&resolved);
    if p.is_absolute() {
        return p.exists();
    }
    which_on_path(&resolved)
}

/// Best-effort Filen Desktop detection across known install locations. Returns
/// `None` when we cannot tell (common on Linux), so the caller reports it as
/// "unknown" rather than a hard 🔴.
fn filen_desktop_installed() -> Option<bool> {
    #[allow(unused_mut)]
    let mut candidates: Vec<String> = Vec::new();
    #[cfg(target_os = "linux")]
    {
        if which_on_path("filen") {
            return Some(true);
        }
        candidates.push("/opt/Filen/filen".into());
        candidates.push("/opt/filen/filen".into());
        if let Ok(home) = std::env::var("HOME") {
            candidates.push(format!("{}/.local/bin/filen", home));
            candidates.push(format!("{}/Applications/Filen.AppImage", home));
            candidates.push(format!("{}/.local/share/flatpak/app/io.filen.Filen", home));
        }
        candidates.push("/var/lib/flatpak/app/io.filen.Filen".into());
    }
    #[cfg(target_os = "macos")]
    {
        candidates.push("/Applications/Filen.app".into());
    }
    #[cfg(windows)]
    {
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            candidates.push(format!(r"{}\Programs\filen\Filen.exe", local));
        }
        if let Ok(pf) = std::env::var("ProgramFiles") {
            candidates.push(format!(r"{}\Filen\Filen.exe", pf));
        }
    }
    for c in &candidates {
        if std::path::Path::new(c).exists() {
            return Some(true);
        }
    }
    None
}

/// Probe the install/active status of a local bridge helper app. Reusable core
/// shared by the Tauri command and the CLI `profiles --health` view.
///
/// `kind` is one of `filen-webdav`, `filen-s3`, `megacmd-webdav`. `port`
/// overrides the default loopback port (e.g. a custom MEGAcmd WebDAV port).
pub async fn probe_bridge(kind: &str, port: Option<u16>) -> Result<BridgeStatus, String> {
    let default_port =
        bridge_kind_default_port(kind).ok_or_else(|| format!("unknown bridge kind: {}", kind))?;
    let port = port.unwrap_or(default_port);
    let active = port_open(port).await;
    let (installed, install_known) = decide_install(kind, active);
    Ok(BridgeStatus {
        installed,
        active,
        port,
        install_known,
    })
}

/// Blocking variant of [`probe_bridge`] for synchronous callers (the CLI).
pub fn probe_bridge_blocking(kind: &str, port: Option<u16>) -> Result<BridgeStatus, String> {
    let default_port =
        bridge_kind_default_port(kind).ok_or_else(|| format!("unknown bridge kind: {}", kind))?;
    let port = port.unwrap_or(default_port);
    let active = port_open_blocking(port);
    let (installed, install_known) = decide_install(kind, active);
    Ok(BridgeStatus {
        installed,
        active,
        port,
        install_known,
    })
}

/// Tauri command wrapper around [`probe_bridge`].
#[tauri::command]
pub async fn bridge_status(kind: String, port: Option<u16>) -> Result<BridgeStatus, String> {
    probe_bridge(&kind, port).await
}

/// Whether `(host, port)` is a supported local mount-app bridge endpoint whose
/// HTTP/HTTPS protocol is chosen inside the host app (independently of the
/// scheme saved in the AeroFTP profile), so AeroFTP auto-resolves it: the Filen
/// Desktop S3 (1800) and WebDAV (1900) servers, and the MEGAcmd WebDAV bridge
/// (4443). Matched on the reserved loopback hostname or the loopback IP.
/// Deliberately NOT any local host: a LAN NAS on one of these ports is not a
/// bridge and must keep its saved scheme untouched. #389.
pub fn is_local_bridge_authority(host: &str, port: u16) -> bool {
    let loopback = host == "127.0.0.1" || host == "::1" || host.eq_ignore_ascii_case("localhost");
    match port {
        1800 => loopback || host.eq_ignore_ascii_case("local.s3.filen.io"),
        1900 => loopback || host.eq_ignore_ascii_case("local.webdav.filen.io"),
        4443 => loopback, // MEGAcmd local WebDAV bridge
        _ => false,
    }
}

/// Probe a loopback bridge port to learn whether it speaks HTTPS or plain HTTP.
///
/// The local mount apps let the user pick the protocol (HTTP/HTTPS) inside the
/// app, independently of the scheme saved in the AeroFTP profile, so a mismatch
/// breaks the connect (#389). We try HTTPS then HTTP and report whichever
/// answers an HTTP response; a scheme mismatch fails at the transport / TLS
/// layer and yields `Err`, so any HTTP status (even 400/401/404) proves the
/// live scheme. Returns `None` when neither answered (bridge down / not yet
/// started), so the caller keeps the saved URL and the normal flow proceeds.
pub async fn detect_bridge_scheme(host: &str, port: u16) -> Option<&'static str> {
    for scheme in ["https", "http"] {
        let Ok(client) = reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .connect_timeout(std::time::Duration::from_secs(3))
            .timeout(std::time::Duration::from_secs(5))
            .build()
        else {
            continue;
        };
        let url = format!("{scheme}://{host}:{port}/");
        if client.get(&url).send().await.is_ok() {
            return Some(scheme);
        }
    }
    None
}

/// Reconcile a local mount-app bridge URL/endpoint against the live bridge:
/// probe the actual scheme (the user's HTTP/HTTPS choice in the host app) and
/// pin the loopback IP so `local.*.filen.io` DNS NODATA on Windows cannot break
/// the connect. Returns the corrected `scheme://127.0.0.1:port<path>` (the path
/// and query are preserved, e.g. MEGAcmd serves under a path), or the input
/// unchanged when it is not a known bridge or the bridge does not answer (so the
/// normal connect / auto-arm flow still runs). #389.
pub async fn reconcile_local_bridge_url(url: &str) -> String {
    let Some((_, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    // Split authority from the path/query tail (preserved verbatim).
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(authority_end);
    let Some((host, port_str)) = authority.rsplit_once(':') else {
        return url.to_string();
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let Ok(port) = port_str.parse::<u16>() else {
        return url.to_string();
    };
    if !is_local_bridge_authority(host, port) {
        return url.to_string();
    }
    match detect_bridge_scheme("127.0.0.1", port).await {
        Some(scheme) => format!("{scheme}://127.0.0.1:{port}{tail}"),
        None => url.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_maps_to_known_loopback_ports() {
        assert_eq!(bridge_kind_default_port("filen-webdav"), Some(1900));
        assert_eq!(bridge_kind_default_port("filen-s3"), Some(1800));
        assert_eq!(bridge_kind_default_port("megacmd-webdav"), Some(4443));
        assert_eq!(bridge_kind_default_port("nope"), None);
    }

    #[test]
    fn local_bridge_authority_gates_to_the_known_bridges() {
        assert!(is_local_bridge_authority("local.webdav.filen.io", 1900));
        assert!(is_local_bridge_authority("local.s3.filen.io", 1800));
        assert!(is_local_bridge_authority("127.0.0.1", 1900));
        assert!(is_local_bridge_authority("localhost", 1800));
        assert!(is_local_bridge_authority("127.0.0.1", 4443)); // MEGAcmd WebDAV
                                                               // Wrong port on the right host, a filen host on the wrong bridge port,
                                                               // and a LAN NAS on the bridge ports must NOT be treated as a bridge
                                                               // (their saved scheme is intentional).
        assert!(!is_local_bridge_authority("local.webdav.filen.io", 443));
        assert!(!is_local_bridge_authority("local.webdav.filen.io", 4443));
        assert!(!is_local_bridge_authority("192.168.1.10", 1800));
        assert!(!is_local_bridge_authority("example.com", 1900));
    }

    #[tokio::test]
    async fn reconcile_passes_through_non_bridge_urls_without_probing() {
        // Non-bridge URLs return unchanged and never touch the network.
        for u in [
            "https://cloud.example.com/remote.php/dav",
            "https://192.168.1.10:1800",
            "https://s3.us-east-1.amazonaws.com",
            "not-a-url",
        ] {
            assert_eq!(reconcile_local_bridge_url(u).await, u);
        }
    }

    #[test]
    fn provider_id_maps_to_bridge_kind() {
        assert_eq!(
            bridge_kind_for_provider_id("filen-desktop-webdav"),
            Some("filen-webdav")
        );
        assert_eq!(
            bridge_kind_for_provider_id("filen-desktop-s3"),
            Some("filen-s3")
        );
        assert_eq!(
            bridge_kind_for_provider_id("megacmd-webdav"),
            Some("megacmd-webdav")
        );
        assert_eq!(
            bridge_kind_for_provider_id("megacmd"),
            Some("megacmd-webdav")
        );
        assert_eq!(bridge_kind_for_provider_id("s3"), None);
        assert_eq!(bridge_kind_for_provider_id("filen"), None);
    }

    #[test]
    fn ui_state_matches_traffic_light_rules() {
        let mk = |active, installed, install_known| BridgeStatus {
            installed,
            active,
            port: 1900,
            install_known,
        };
        assert_eq!(mk(true, false, false).ui_state(), BridgeUiState::Green);
        assert_eq!(mk(false, false, true).ui_state(), BridgeUiState::Red);
        assert_eq!(mk(false, true, true).ui_state(), BridgeUiState::Amber);
        assert_eq!(mk(false, true, false).ui_state(), BridgeUiState::Amber);
    }

    #[tokio::test]
    async fn unknown_kind_is_an_error() {
        assert!(bridge_status("bogus".into(), None).await.is_err());
    }

    #[tokio::test]
    async fn closed_port_reports_inactive() {
        // Port 1 is reserved/unused on every supported OS, so this never races a
        // real bridge.
        let s = bridge_status("megacmd-webdav".into(), Some(1))
            .await
            .unwrap();
        assert!(!s.active);
        assert_eq!(s.port, 1);
    }
}
