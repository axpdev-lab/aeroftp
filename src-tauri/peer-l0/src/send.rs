//! "Send file to user" one-shot (AirDrop) primitive for AeroShare.
//!
//! This is the cheapest, most-proven layer of the send-file track: an ephemeral
//! direct send of a single file to a recipient identified BY THEIR AFID, with no
//! persistent drive. It reuses the L0 transport that passed the GO gate (~289
//! transfers, 100%): the encrypted-blob framing + the end-to-end receipt ACK +
//! the BLAKE3 plaintext verify (`protocol::send_encrypted_blob` /
//! `recv_encrypted_blob_capped`).
//!
//! ## Identity-addressed, no pairing secret
//! The L0 spike derived a session key from an out-of-band PAIRING SECRET. Here we
//! need none: the AFID already carries the recipient's X25519 public key, so we
//! mint a random per-transfer content key and [`crate::seal`] it to the recipient
//! (anonymous-sender box). Only the recipient can open it.
//!
//! ## Sender authentication is FREE
//! The sender binds its iroh endpoint with its OWN identity ed seed (so its
//! `NodeId == sender AFID.ed`, the dial-by-AFID seam proven in
//! `endpoint::tests::node_id_equals_identity_ed_key`). iroh authenticates the
//! connection by that node key (QUIC/TLS), so the recipient learns the sender's
//! AFID ed CRYPTOGRAPHICALLY from `conn.remote_node_id()` - no separate offer
//! signature needed. The recipient cross-checks the claimed `sender_afid` in the
//! offer against the authenticated `remote_node_id` and rejects any mismatch.
//!
//! ## Decision is decoupled from the blob ACK
//! `send_encrypted_blob` waits only 30s for its receipt ACK - too short for a
//! human Accept/Decline. So the offer is a REQUEST/RESPONSE on one bi-stream: the
//! sender writes the offer, then waits up to [`DECISION_WAIT_SECS`] for an accept
//! byte, and only THEN ships the blob (whose fast transfer comfortably fits the
//! 30s ACK window). The user's think-time never races the blob ACK.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use crate::endpoint::{PeerBlobOffer, PeerEndpoint, PeerEndpointConfig};
use crate::identity::{Identity, IdentityPublic};
use crate::protocol::{recv_encrypted_blob_capped, send_encrypted_blob};
use anyhow::{bail, Context, Result};
use iroh::endpoint::Connection;
use iroh::NodeId;
use iroh_blobs::Hash;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{info, warn};

/// ALPN for the one-shot send protocol. Distinct from `PEER_L0_ALPN` so the
/// receive loop only ever accepts real send offers.
pub const PEER_SEND_ALPN: &[u8] = b"/aeroftp/peer/send/v1";

/// ALPN for a lightweight presence ping. A prober dials this and closes
/// immediately; a successful connect means the recipient's receive loop is up
/// (i.e. the friend is "online / receiving"). The receive loop answers it by
/// closing cleanly WITHOUT expecting an offer, so a presence check never shows
/// up as a failed transfer.
pub const PEER_PING_ALPN: &[u8] = b"/aeroftp/peer/ping/v1";

/// How long a presence probe waits for the connection before declaring the
/// friend offline. Short: a reachable receiver answers in well under this.
const PRESENCE_PROBE_SECS: u64 = 8;

/// Cap on the JSON offer (name + sealed key + AFID; a few hundred bytes in
/// practice). Generous so a long file name never trips it.
const MAX_SEND_OFFER_BYTES: usize = 16 * 1024;
/// Cap on the plaintext file size for the one-shot path. Larger than the L0
/// gate's 64 MiB (this path is a real file send, not a connectivity probe), but
/// still in-memory both sides for v1 - streaming large files is a later refinement.
pub const MAX_SEND_BYTES: u64 = 256 * 1024 * 1024;
/// How long the SENDER waits for the recipient's accept/decline after the offer.
/// Covers human think-time at the Accept/Decline prompt (the blob ACK timeout is
/// a separate, post-accept concern).
const DECISION_WAIT_SECS: u64 = 120;

const DECISION_ACCEPT: u8 = 1;
const DECISION_DECLINE: u8 = 0;

/// The offer a sender presents before any file bytes flow. `sealed_content_key`
/// is the per-transfer AES-256 key sealed to the recipient's X25519 key; only the
/// recipient can open it. `hash` is the BLAKE3 of the PLAINTEXT (verified after
/// decrypt). `sender_afid` is cross-checked against the iroh-authenticated
/// `remote_node_id` on the receiving side.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendOffer {
    pub name: String,
    pub size: u64,
    pub hash: Hash,
    pub sealed_content_key: Vec<u8>,
    pub sender_afid: String,
}

/// What the app needs to show an Accept/Decline prompt (plain data; no crypto
/// types leak). Handed to the receive-loop's `decide` callback.
#[derive(Debug, Clone)]
pub struct IncomingOffer {
    pub sender_afid: String,
    pub name: String,
    pub size: u64,
}

/// Outcome of one incoming transfer, reported to the app's `notify` callback so
/// it can emit a UI event. Owned data (no borrows) for an easy `Fn` boundary.
#[derive(Debug, Clone)]
pub enum ReceiveEvent {
    Completed {
        sender_afid: String,
        name: String,
        path: PathBuf,
    },
    Declined {
        sender_afid: String,
        name: String,
    },
    Failed {
        sender_afid: String,
        name: String,
        error: String,
    },
}

fn ed_seed_of(secret: &[u8]) -> Result<[u8; 32]> {
    if secret.len() != 64 {
        bail!("identity secret must be 64 bytes (ed32 || x32)");
    }
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&secret[..32]);
    Ok(seed)
}

/// Send `file_path` to `recipient_afid` over a fresh identity-seeded endpoint.
/// `my_secret` is the sender's 64-byte identity secret; `cfg` carries the relay /
/// discovery selection (its `identity_secret_key` is overwritten with the sender
/// identity here so the recipient authenticates us). Returns once the recipient
/// has ACKed a verified receipt, or errors (incl. an explicit recipient decline).
pub async fn run_send_file(
    recipient_afid: &str,
    my_secret: &[u8],
    file_path: &str,
    mut cfg: PeerEndpointConfig,
) -> Result<()> {
    let me = Identity::from_secret_bytes(&{
        let mut b = [0u8; 64];
        if my_secret.len() != 64 {
            bail!("identity secret must be 64 bytes");
        }
        b.copy_from_slice(my_secret);
        b
    });
    let recipient = IdentityPublic::from_aeroftp_id(recipient_afid)
        .map_err(|e| anyhow::anyhow!("invalid recipient AeroFTP-ID: {e}"))?;
    let recipient_node = NodeId::from_bytes(&recipient.ed_bytes())
        .context("recipient AFID does not yield a valid node id")?;

    let data = std::fs::read(file_path).with_context(|| format!("cannot read {file_path}"))?;
    if data.len() as u64 > MAX_SEND_BYTES {
        bail!(
            "file is {} bytes; the one-shot send cap is {} bytes",
            data.len(),
            MAX_SEND_BYTES
        );
    }
    let name = Path::new(file_path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string());
    let hash = Hash::new(&data);

    // Per-transfer random content key, sealed to the recipient (only they open it).
    let content_key = crate::drive::random_content_key();
    let sealed_content_key = crate::seal(&recipient, &content_key);

    let offer = SendOffer {
        name: name.clone(),
        size: data.len() as u64,
        hash,
        sealed_content_key,
        sender_afid: me.public().to_aeroftp_id(),
    };

    // Bind OUR endpoint with our identity so remote_node_id == our AFID ed.
    cfg.identity_secret_key = Some(ed_seed_of(my_secret)?);
    let ep = PeerEndpoint::new(cfg).await?;
    let conn = ep.connect(recipient_node, PEER_SEND_ALPN).await?;

    // Offer + decision on one bi-stream (request/response).
    let (mut s, mut r) = conn.open_bi().await.context("open_bi for send offer")?;
    let json = serde_json::to_vec(&offer)?;
    if json.len() > MAX_SEND_OFFER_BYTES {
        bail!("send offer too large");
    }
    s.write_u32(json.len() as u32).await?;
    s.write_all(&json).await?;
    s.finish().context("finish offer stream")?;

    let decision = tokio::time::timeout(
        std::time::Duration::from_secs(DECISION_WAIT_SECS),
        r.read_u8(),
    )
    .await
    .context("recipient did not respond to the offer in time")?
    .context("connection lost before the recipient responded")?;
    if decision != DECISION_ACCEPT {
        conn.close(0u32.into(), b"declined");
        bail!("the recipient declined the transfer");
    }

    // Accepted: encrypt with the per-transfer key and ship via the proven path
    // (the receiver ACKs only after decrypt + BLAKE3 verify).
    let (nonce, ciphertext) = crate::encrypt_blob(&content_key, &data)?;
    send_encrypted_blob(&conn, &nonce, &ciphertext).await?;
    conn.close(0u32.into(), b"sent");
    info!(%name, bytes = data.len(), "one-shot send complete (recipient ACKed)");
    Ok(())
}

/// Run the standing receive loop on an identity-seeded endpoint. `my_secret` is
/// the receiver's 64-byte identity secret (binds the endpoint so senders can dial
/// us by AFID, and opens sealed content keys). For each incoming offer, `decide`
/// is awaited with the plain [`IncomingOffer`]; it returns `Some(dest_dir)` to
/// accept (the file is written into that directory) or `None` to decline.
/// `notify` is called with the outcome so the app can emit a UI event.
///
/// v1 handles one incoming transfer at a time (sequential): while the user is
/// deciding, a second sender waits in the accept backlog. Concurrency is a later
/// refinement. Cancellation is the caller's job: drop this future (e.g. lose a
/// `select!` race against a CancellationToken) and the endpoint tears down.
pub async fn run_receiver<D, DF, N>(
    my_secret: &[u8],
    mut cfg: PeerEndpointConfig,
    decide: D,
    notify: N,
) -> Result<()>
where
    D: Fn(IncomingOffer) -> DF,
    DF: std::future::Future<Output = Option<PathBuf>>,
    N: Fn(ReceiveEvent),
{
    let mut secret = [0u8; 64];
    if my_secret.len() != 64 {
        bail!("identity secret must be 64 bytes");
    }
    secret.copy_from_slice(my_secret);

    cfg.identity_secret_key = Some(ed_seed_of(my_secret)?);
    let ep = PeerEndpoint::new(cfg).await?;
    info!(node_id = %ep.node_id(), "AeroShare receive loop listening");

    loop {
        let conn = ep
            .accept()
            .await
            .context("receive endpoint accept failed")?;
        // A presence ping: answer by closing cleanly, never an offer. Branch
        // BEFORE handle_incoming so a probe is not mistaken for a failed transfer.
        if conn.alpn().as_deref() == Some(PEER_PING_ALPN) {
            conn.close(0u32.into(), b"pong");
            continue;
        }
        // Per-connection errors must not kill the loop (one bad sender shouldn't
        // stop the receiver); only an endpoint-level accept failure (above) does.
        if let Err(e) = handle_incoming(&conn, &secret, &decide, &notify).await {
            warn!("AeroShare incoming transfer failed: {e}");
        }
    }
}

/// Probe whether each AFID's receive loop is reachable right now (one shared
/// identity-seeded endpoint, sequential bounded connects on [`PEER_PING_ALPN`]).
/// `true` = the friend is online and receiving; `false` = unreachable / offline /
/// not receiving. Best-effort: a malformed AFID maps to `false`, never an error.
/// Result order matches `afids`.
pub async fn probe_presence_many(
    my_secret: &[u8],
    afids: &[String],
    mut cfg: PeerEndpointConfig,
) -> Result<Vec<bool>> {
    cfg.identity_secret_key = Some(ed_seed_of(my_secret)?);
    let ep = PeerEndpoint::new(cfg).await?;
    let mut out = Vec::with_capacity(afids.len());
    for afid in afids {
        let online = match IdentityPublic::from_aeroftp_id(afid)
            .ok()
            .and_then(|pk| NodeId::from_bytes(&pk.ed_bytes()).ok())
        {
            Some(node) => {
                let attempt = ep.connect(node, PEER_PING_ALPN);
                match tokio::time::timeout(
                    std::time::Duration::from_secs(PRESENCE_PROBE_SECS),
                    attempt,
                )
                .await
                {
                    Ok(Ok(conn)) => {
                        conn.close(0u32.into(), b"ping");
                        true
                    }
                    _ => false,
                }
            }
            None => false,
        };
        out.push(online);
    }
    Ok(out)
}

async fn handle_incoming<D, DF, N>(
    conn: &Connection,
    secret: &[u8; 64],
    decide: &D,
    notify: &N,
) -> Result<()>
where
    D: Fn(IncomingOffer) -> DF,
    DF: std::future::Future<Output = Option<PathBuf>>,
    N: Fn(ReceiveEvent),
{
    let (mut s, mut r) = conn.accept_bi().await.context("accept_bi for send offer")?;
    let len = r.read_u32().await.context("read offer length")? as usize;
    if len > MAX_SEND_OFFER_BYTES {
        conn.close(1u32.into(), b"offer-too-large");
        bail!("incoming offer too large");
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await.context("read offer body")?;
    let offer: SendOffer = serde_json::from_slice(&buf).context("parse send offer")?;

    // Authenticate the sender: the claimed AFID's ed key MUST equal the
    // iroh-authenticated remote node id. Reject forgery / spoofed AFIDs.
    let claimed = IdentityPublic::from_aeroftp_id(&offer.sender_afid)
        .context("offer carries an invalid sender AeroFTP-ID")?;
    let remote = conn
        .remote_node_id()
        .context("could not read the authenticated remote node id")?;
    if remote.as_bytes() != &claimed.ed_bytes() {
        conn.close(1u32.into(), b"sender-mismatch");
        bail!("sender AFID does not match the authenticated connection identity");
    }

    let incoming = IncomingOffer {
        sender_afid: offer.sender_afid.clone(),
        name: offer.name.clone(),
        size: offer.size,
    };

    let dest_dir = match decide(incoming).await {
        Some(dir) => dir,
        None => {
            s.write_u8(DECISION_DECLINE).await.ok();
            s.finish().ok();
            conn.close(0u32.into(), b"declined");
            notify(ReceiveEvent::Declined {
                sender_afid: offer.sender_afid,
                name: offer.name,
            });
            return Ok(());
        }
    };

    s.write_u8(DECISION_ACCEPT)
        .await
        .context("write accept byte")?;
    s.finish().context("finish decision stream")?;

    // Open the sealed per-transfer content key with our identity.
    let me = Identity::from_secret_bytes(secret);
    let key_bytes = me
        .open(&offer.sealed_content_key)
        .context("could not open the sealed content key (not the intended recipient?)")?;
    let key: [u8; 32] = key_bytes
        .as_slice()
        .try_into()
        .context("sealed content key was not 32 bytes")?;

    let pbo = PeerBlobOffer {
        hash: offer.hash,
        size: offer.size,
        name_hint: offer.name.clone(),
        note: None,
    };

    let result = recv_encrypted_blob_capped(conn, &pbo, &key, MAX_SEND_BYTES).await;
    match result {
        Ok(bytes) => {
            match write_to_inbox(&dest_dir, &offer.name, &bytes) {
                Ok(path) => {
                    // Do NOT close the connection here: closing immediately after
                    // the ACK can truncate its delivery (a QUIC CONNECTION_CLOSE
                    // racing the just-finished ACK stream), which the sender sees
                    // as "connection lost before receipt". Instead wait for the
                    // SENDER to close (it does so right after reading our ACK), so
                    // the ACK is guaranteed delivered. Bounded so a vanished peer
                    // cannot hang the receive loop.
                    let _ =
                        tokio::time::timeout(std::time::Duration::from_secs(30), conn.closed())
                            .await;
                    notify(ReceiveEvent::Completed {
                        sender_afid: offer.sender_afid,
                        name: offer.name,
                        path,
                    });
                }
                Err(e) => {
                    notify(ReceiveEvent::Failed {
                        sender_afid: offer.sender_afid,
                        name: offer.name,
                        error: e.to_string(),
                    });
                    return Err(e);
                }
            }
            Ok(())
        }
        Err(e) => {
            conn.close(1u32.into(), b"recv-failed");
            notify(ReceiveEvent::Failed {
                sender_afid: offer.sender_afid,
                name: offer.name,
                error: e.to_string(),
            });
            Err(e)
        }
    }
}

/// Sanitize a sender-supplied file name to a single safe path component (no
/// separators, no traversal, no device-hostile chars).
fn sanitize_name(name: &str) -> String {
    let base = Path::new(name)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let safe: String = base
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let trimmed = safe.trim_matches(['.', ' ']).to_string();
    if trimmed.is_empty() {
        "file".to_string()
    } else {
        trimmed
    }
}

/// Write `bytes` into `dest_dir` under a sanitized, collision-free name. Returns
/// the final path. Never overwrites: an existing name gets ` (1)`, ` (2)`, ...
fn write_to_inbox(dest_dir: &Path, name: &str, bytes: &[u8]) -> Result<PathBuf> {
    std::fs::create_dir_all(dest_dir)
        .with_context(|| format!("cannot create inbox dir {}", dest_dir.display()))?;
    let safe = sanitize_name(name);
    let candidate = dest_dir.join(&safe);
    let path = if !candidate.exists() {
        candidate
    } else {
        let p = Path::new(&safe);
        let stem = p
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| safe.clone());
        let ext = p.extension().map(|e| e.to_string_lossy().to_string());
        let mut n = 1u32;
        loop {
            let alt = match &ext {
                Some(e) => format!("{stem} ({n}).{e}"),
                None => format!("{stem} ({n})"),
            };
            let alt_path = dest_dir.join(alt);
            if !alt_path.exists() {
                break alt_path;
            }
            n += 1;
        }
    };
    std::fs::write(&path, bytes).with_context(|| format!("cannot write {}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_traversal_and_separators() {
        assert_eq!(sanitize_name("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_name("a/b/c.txt"), "c.txt");
        assert_eq!(sanitize_name("evil:name*?.bin"), "evil_name__.bin");
        assert_eq!(sanitize_name(""), "file");
        assert_eq!(sanitize_name("   "), "file");
        assert_eq!(sanitize_name("..."), "file");
    }

    #[test]
    fn write_to_inbox_dedups_without_overwriting() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p1 = write_to_inbox(dir.path(), "foto.jpg", b"a").expect("first");
        let p2 = write_to_inbox(dir.path(), "foto.jpg", b"b").expect("second");
        assert_ne!(p1, p2, "second write must not overwrite the first");
        assert_eq!(std::fs::read(&p1).unwrap(), b"a");
        assert_eq!(std::fs::read(&p2).unwrap(), b"b");
        assert!(p2.file_name().unwrap().to_string_lossy().contains("(1)"));
    }

    #[test]
    fn sealed_content_key_round_trips_to_recipient() {
        // The crypto contract the protocol relies on: a per-transfer key sealed to
        // the recipient AFID is recovered byte-identical, and a non-recipient fails.
        let recipient = Identity::generate();
        let key = crate::drive::random_content_key();
        let sealed = crate::seal(&recipient.public(), &key);
        let opened = recipient.open(&sealed).expect("recipient opens");
        assert_eq!(opened.as_slice(), &key, "content key round-trips");

        let stranger = Identity::generate();
        assert!(
            stranger.open(&sealed).is_err(),
            "a non-recipient cannot open the sealed key"
        );
    }
}
