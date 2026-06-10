//! WI-3d seam: the in-app entry point to the user-to-user P2P encrypted-drive
//! engine, which lives in the `aeroftp-peer-l0` crate (the iroh isolation
//! boundary). As of WI-3d `aeroftp-peer-l0` is a NORMAL (shipped) dependency of
//! the app, so calling its `drive::*` entry points from this module is what
//! actually links the iroh P2P stack into the shipped binary.
//!
//! This is deliberately a THIN facade over the dev/pairing-secret path only,
//! enough to give iroh a real non-test call site and to give WI-4d a stable
//! surface to build the `aeroftp-cli` verbs on top of. The capability path
//! (sealed per-recipient tokens) and the vault-backed identity/contact custody
//! (`crate::peer_identity`) are wired by WI-4d; no Tauri commands or CLI verbs
//! are added here yet.

use aeroftp_peer_l0::drive::{run_docs_publish, run_docs_replicate, PublishKey, ReplicateKey};
use aeroftp_peer_l0::endpoint::PeerEndpointConfig;

/// Build the endpoint config used by both directions. `custom_relay_urls` lets a
/// caller point at a self-hosted relay; `None` uses the research default.
fn endpoint_config(custom_relay_urls: Option<Vec<String>>) -> PeerEndpointConfig {
    PeerEndpointConfig {
        bind_addr: None,
        secret_key_path: None,
        custom_relay_urls,
    }
}

/// Publish a local directory as a persistent, encrypted, versioned P2P drive on
/// the dev/pairing-secret key path (the WI-1/WI-2 gate path: drive_key =
/// HKDF(secret, namespace)). Prints the DocTicket and stays alive until the
/// engine returns. `secret_b64` is a base64url (no pad) pairing secret.
///
/// WI-4d CLI seam: the `aeroftp peer publish` verb will call this.
pub async fn publish_drive_dev(
    dir: String,
    secret_b64: &str,
    store: Option<String>,
    republish_after: u64,
    republish_count: u64,
    custom_relay_urls: Option<Vec<String>>,
) -> anyhow::Result<()> {
    let secret = aeroftp_peer_l0::decode_secret(secret_b64)?;
    run_docs_publish(
        "hello.txt".to_string(),
        Some(dir),
        republish_after,
        republish_count,
        store,
        endpoint_config(custom_relay_urls),
        PublishKey::DevSecret(secret),
    )
    .await
}

/// Replicate a P2P drive from a DocTicket into `out` on the dev/pairing-secret
/// key path. With `watch_secs > 0` it keeps converging on later versions.
///
/// WI-4d CLI seam: the `aeroftp peer replicate` verb will call this.
pub async fn replicate_drive_dev(
    ticket: String,
    out: String,
    secret_b64: &str,
    watch_secs: u64,
    store: Option<String>,
    custom_relay_urls: Option<Vec<String>>,
) -> anyhow::Result<()> {
    let secret = aeroftp_peer_l0::decode_secret(secret_b64)?;
    run_docs_replicate(
        ticket,
        Some(out),
        watch_secs,
        store,
        endpoint_config(custom_relay_urls),
        ReplicateKey::DevSecret(secret),
    )
    .await
}
