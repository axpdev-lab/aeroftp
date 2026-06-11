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

use aeroftp_peer_l0::drive::{
    run_docs_publish, run_docs_replicate, CapIssue, PublishKey, ReplicateKey, StatusReporter,
};
use aeroftp_peer_l0::endpoint::{DiscoveryMode, PeerEndpointConfig};
use aeroftp_peer_l0::{open_capability, seal_capability, Capability, Identity, IdentityPublic};
use zeroize::Zeroizing;

/// AeroShare Phase 1: the GUI-side registry of long-lived drive sync tasks
/// (Tauri managed state; wired into `provider_connect` for `protocol="peer"`).
pub mod runtime;

/// What a capability-path publish hands back to the CLI once the drive exists (so it can custody the
/// key in the vault and print the ticket while the publisher keeps serving). Re-exported from peer-l0.
pub use aeroftp_peer_l0::drive::DrivePublished;

// ----------------------------------------------------------------------------
// WI-4d identity + capability custody seam (app-side; returns plain bytes/strings
// so the aeroftp-cli verbs and the `peer_identity` vault facade never name a
// peer-l0 crypto type). Private key material is returned in `Zeroizing` buffers.
// ----------------------------------------------------------------------------

/// Reconstruct an `Identity` from its 64 stored secret bytes (ed32 || x32).
fn identity_from_secret(secret: &[u8]) -> anyhow::Result<Identity> {
    let arr: [u8; 64] = secret
        .try_into()
        .map_err(|_| anyhow::anyhow!("identity secret must be exactly 64 bytes"))?;
    Ok(Identity::from_secret_bytes(&arr))
}

/// Generate a fresh peer identity. Returns `(64 secret bytes, AeroFTP-ID)`; store
/// the secret in the partition vault via [`crate::peer_identity::store_identity`].
pub fn generate_identity() -> (Zeroizing<Vec<u8>>, String) {
    let id = Identity::generate();
    let public_id = id.public().to_aeroftp_id();
    (Zeroizing::new(id.to_secret_bytes().to_vec()), public_id)
}

/// The AeroFTP-ID for a stored 64-byte identity secret (round-trips the vault blob).
pub fn identity_public_id(secret: &[u8]) -> anyhow::Result<String> {
    Ok(identity_from_secret(secret)?.public().to_aeroftp_id())
}

/// Validate an AeroFTP-ID string (prefix + checksum); returns the normalized form.
pub fn validate_aeroftp_id(aeroftp_id: &str) -> anyhow::Result<String> {
    let pk = IdentityPublic::from_aeroftp_id(aeroftp_id)?;
    Ok(pk.to_aeroftp_id())
}

/// A fresh per-drive RANDOM 32-byte content key (the drive's E2EE blob key).
pub fn fresh_content_key() -> Zeroizing<Vec<u8>> {
    Zeroizing::new(aeroftp_peer_l0::drive::random_content_key().to_vec())
}

/// The contents recovered from a capability token after a successful import.
/// Plain data only (no peer-l0 type leaks); `content_key` is wiped on drop.
pub struct ImportedCapability {
    pub namespace_id: String,
    pub content_key: Zeroizing<Vec<u8>>,
    pub drive_name: String,
    pub version: u64,
}

/// Seal a per-drive read capability to `recipient_aeroftp_id`, signed by my
/// identity, and return the shareable `aeroftp-drive://` token (WI-4d grant).
#[allow(clippy::too_many_arguments)]
pub fn grant_capability(
    my_secret: &[u8],
    recipient_aeroftp_id: &str,
    namespace_id: &str,
    content_key: &[u8],
    drive_name: &str,
    version: u64,
    node_addrs: Vec<String>,
    issued_at: i64,
) -> anyhow::Result<String> {
    let me = identity_from_secret(my_secret)?;
    let recipient = IdentityPublic::from_aeroftp_id(recipient_aeroftp_id)
        .map_err(|e| anyhow::anyhow!("invalid recipient AeroFTP-ID: {e}"))?;
    let content_key: [u8; 32] = content_key
        .try_into()
        .map_err(|_| anyhow::anyhow!("content key must be exactly 32 bytes"))?;
    let cap = Capability {
        namespace_id: namespace_id.to_string(),
        content_key,
        node_addrs,
        drive_name: drive_name.to_string(),
        version,
        granted_to_ed: recipient.ed_bytes(),
        issued_at,
    };
    seal_capability(&me, &recipient, &cap)
}

/// Extract the drive namespace id from a publisher's DocTicket, so the CLI can look up the
/// vault-custodied content key for `peer replicate` (no import/sync needed).
pub fn namespace_from_ticket(ticket: &str) -> anyhow::Result<String> {
    aeroftp_peer_l0::drive::namespace_from_ticket(ticket)
}

/// Open a capability token addressed to me, verifying it came from
/// `issuer_aeroftp_id` (defends against a forged issuer) before unsealing.
/// Fails closed on a wrong issuer, a token sealed to someone else, or tampering.
pub fn import_capability(
    my_secret: &[u8],
    issuer_aeroftp_id: &str,
    token: &str,
) -> anyhow::Result<ImportedCapability> {
    let me = identity_from_secret(my_secret)?;
    let issuer = IdentityPublic::from_aeroftp_id(issuer_aeroftp_id)
        .map_err(|e| anyhow::anyhow!("invalid issuer AeroFTP-ID: {e}"))?;
    let cap = open_capability(&me, &issuer, token)?;
    Ok(ImportedCapability {
        namespace_id: cap.namespace_id,
        content_key: Zeroizing::new(cap.content_key.to_vec()),
        drive_name: cap.drive_name,
        version: cap.version,
    })
}

/// Build the endpoint config used by both directions. `custom_relay_urls` lets a
/// caller point at a self-hosted relay; `None` uses the research default.
fn endpoint_config(custom_relay_urls: Option<Vec<String>>) -> PeerEndpointConfig {
    PeerEndpointConfig {
        bind_addr: None,
        secret_key_path: None,
        custom_relay_urls,
        discovery: discovery_from_env(),
    }
}

/// WI-5a independence seam: select the discovery backend from `AEROFTP_PEER_DISCOVERY`
/// (`dht` | `n0` | `both`). Default `both` = n0 DNS + Mainline DHT concurrently (additive).
/// `dht` = the zero-n0 path exercised by GATE IND-1. An unknown value falls back to the
/// default rather than failing, so a stray env var never breaks a transfer.
fn discovery_from_env() -> DiscoveryMode {
    match std::env::var("AEROFTP_PEER_DISCOVERY")
        .ok()
        .as_deref()
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("dht") => DiscoveryMode::Dht,
        Some("n0") => DiscoveryMode::N0,
        _ => DiscoveryMode::Both,
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
        None, // dev path does not custody a namespace back to a caller
    )
    .await
}

/// Publish a local directory as a CAPABILITY-path drive (WI-4d): the blobs are E2EE'd with a per-drive
/// RANDOM `content_key` the caller minted (and will custody in the vault keyed by the returned
/// namespace). No tokens are sealed here — granting is a separate `peer grant` verb. The drive's
/// `(namespace_id, ticket)` is sent on `ready` as soon as it exists, then this serves until cancelled.
#[allow(clippy::too_many_arguments)]
pub async fn publish_drive_cap(
    dir: String,
    content_key: &[u8],
    identity_secret: &[u8],
    drive_name: String,
    store: Option<String>,
    republish_after: u64,
    republish_count: u64,
    custom_relay_urls: Option<Vec<String>>,
    ready: tokio::sync::oneshot::Sender<DrivePublished>,
) -> anyhow::Result<()> {
    let content_key: [u8; 32] = content_key
        .try_into()
        .map_err(|_| anyhow::anyhow!("content key must be exactly 32 bytes"))?;
    let issuer = identity_from_secret(identity_secret)?;
    run_docs_publish(
        "hello.txt".to_string(),
        Some(dir),
        republish_after,
        republish_count,
        store,
        endpoint_config(custom_relay_urls),
        PublishKey::Capability {
            content_key,
            issue: Box::new(CapIssue {
                issuer,
                grants: Vec::new(),
                cap_out: None,
                drive_name,
            }),
        },
        Some(ready),
    )
    .await
}

/// Replicate a CAPABILITY-path drive (WI-4d) using a `content_key` the caller already custodies in the
/// vault (recovered at `peer import` time). The namespace + addrs come from the `ticket`.
pub async fn replicate_drive_cap(
    ticket: String,
    out: String,
    content_key: &[u8],
    watch_secs: u64,
    store: Option<String>,
    custom_relay_urls: Option<Vec<String>>,
    on_status: Option<StatusReporter>,
) -> anyhow::Result<()> {
    let content_key: [u8; 32] = content_key
        .try_into()
        .map_err(|_| anyhow::anyhow!("content key must be exactly 32 bytes"))?;
    run_docs_replicate(
        ticket,
        Some(out),
        watch_secs,
        store,
        endpoint_config(custom_relay_urls),
        ReplicateKey::Content(content_key),
        on_status,
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
        None,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_generate_and_public_id_round_trip() {
        let (secret, public_id) = generate_identity();
        assert_eq!(secret.len(), 64, "identity secret is 64 bytes");
        assert!(public_id.starts_with("AFID1"));
        // The stored secret yields the same AeroFTP-ID on reload.
        assert_eq!(identity_public_id(&secret).expect("public id"), public_id);
        // And the ID validates.
        assert_eq!(validate_aeroftp_id(&public_id).expect("valid"), public_id);
    }

    #[test]
    fn grant_then_import_recovers_the_same_content_key() {
        let (issuer_secret, issuer_id) = generate_identity();
        let (recipient_secret, recipient_id) = generate_identity();
        let content_key = fresh_content_key();
        let ns = "8ce68153cc3b80d778b594b7e3787e3511745ca28b384ebdb4fab5ec41be0832";

        let token = grant_capability(
            &issuer_secret,
            &recipient_id,
            ns,
            &content_key,
            "photos",
            1,
            vec!["node-a".into()],
            1_749_500_000,
        )
        .expect("grant");
        assert!(token.starts_with("aeroftp-drive://"));

        let imported = import_capability(&recipient_secret, &issuer_id, &token).expect("import");
        assert_eq!(imported.namespace_id, ns);
        assert_eq!(imported.content_key.as_slice(), content_key.as_slice());
        assert_eq!(imported.drive_name, "photos");
        assert_eq!(imported.version, 1);
    }

    #[test]
    fn import_fails_closed_on_wrong_issuer() {
        let (issuer_secret, _issuer_id) = generate_identity();
        let (recipient_secret, recipient_id) = generate_identity();
        let (_mallory_secret, mallory_id) = generate_identity();
        let content_key = fresh_content_key();
        let ns = "1111111111111111111111111111111111111111111111111111111111111111";

        let token = grant_capability(
            &issuer_secret,
            &recipient_id,
            ns,
            &content_key,
            "drive",
            1,
            vec![],
            1_749_500_000,
        )
        .expect("grant");

        // Expecting Mallory as the issuer (it was really the issuer) must reject.
        assert!(
            import_capability(&recipient_secret, &mallory_id, &token).is_err(),
            "wrong expected issuer must fail closed"
        );
    }

    #[test]
    fn import_fails_closed_for_wrong_recipient() {
        let (issuer_secret, issuer_id) = generate_identity();
        let (_recipient_secret, recipient_id) = generate_identity();
        let (other_secret, _other_id) = generate_identity();
        let content_key = fresh_content_key();
        let ns = "2222222222222222222222222222222222222222222222222222222222222222";

        let token = grant_capability(
            &issuer_secret,
            &recipient_id,
            ns,
            &content_key,
            "drive",
            1,
            vec![],
            1_749_500_000,
        )
        .expect("grant");

        // A different identity (not the named recipient) cannot unseal it.
        assert!(
            import_capability(&other_secret, &issuer_id, &token).is_err(),
            "a non-recipient must not open the capability"
        );
    }

    #[test]
    fn grant_rejects_a_bad_content_key_length() {
        let (issuer_secret, _id) = generate_identity();
        let (_r, recipient_id) = generate_identity();
        let err = grant_capability(
            &issuer_secret,
            &recipient_id,
            "ns",
            &[0u8; 16], // wrong length
            "drive",
            1,
            vec![],
            0,
        );
        assert!(err.is_err(), "16-byte content key must be rejected");
    }
}
