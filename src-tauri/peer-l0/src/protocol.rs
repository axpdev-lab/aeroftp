//! Dead-simple length-prefixed protocol for the L0 spike.
//! One offer (JSON) followed by raw bytes. Receiver verifies BLAKE3.
//!
//! This is intentionally tiny. When we converge we can adopt iroh-blobs
//! proper (which already gives us chunking, progress, verification, and
//! the DAG-friendly shape).

use crate::endpoint::PeerBlobOffer;
use anyhow::{bail, Context, Result};
use iroh::endpoint::Connection;
use iroh_blobs::Hash;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const MAX_OFFER_BYTES: usize = 8 * 1024; // small
const MAX_BLOB_FOR_GATE: u64 = 64 * 1024 * 1024; // 64 MiB safety cap for gate tests

pub async fn send_offer(conn: &Connection, offer: &PeerBlobOffer) -> Result<()> {
    let (mut send, _recv) = conn.open_bi().await.context("open_bi for offer")?;
    let json = serde_json::to_vec(offer)?;
    if json.len() > MAX_OFFER_BYTES {
        bail!("offer too large");
    }
    send.write_u32(json.len() as u32).await?;
    send.write_all(&json).await?;
    send.finish()?;
    Ok(())
}

pub async fn recv_offer(conn: &Connection) -> Result<PeerBlobOffer> {
    let (_send, mut recv) = conn.accept_bi().await.context("accept_bi for offer")?;
    let len = recv.read_u32().await? as usize;
    if len > MAX_OFFER_BYTES {
        bail!("offer too large");
    }
    let mut buf = vec![0u8; len];
    recv.read_exact(&mut buf).await?;
    let offer: PeerBlobOffer = serde_json::from_slice(&buf)?;
    Ok(offer)
}

pub async fn send_blob(conn: &Connection, data: &[u8]) -> Result<()> {
    let (mut send, _recv) = conn.open_bi().await.context("open_bi for blob")?;
    // Very naive: length (u64) + raw bytes.
    // Real version will stream + interleave progress.
    send.write_u64(data.len() as u64).await?;
    send.write_all(data).await?;
    send.finish()?;
    Ok(())
}

pub async fn recv_blob(conn: &Connection, offer: &PeerBlobOffer) -> Result<Vec<u8>> {
    if offer.size > MAX_BLOB_FOR_GATE {
        bail!("blob larger than L0 gate safety cap");
    }
    let (_send, mut recv) = conn.accept_bi().await.context("accept_bi for blob data")?;

    let declared = recv.read_u64().await?;
    if declared != offer.size {
        bail!("size mismatch in blob header");
    }

    let mut data = vec![0u8; declared as usize];
    recv.read_exact(&mut data).await?;

    // Verify content address (BLAKE3 via iroh-blobs Hash which is a 32-byte blake3)
    let computed = Hash::new(&data);
    if computed != offer.hash {
        bail!("BLAKE3 mismatch: got {}, expected {}", computed, offer.hash);
    }

    Ok(data)
}

/// Send an encrypted blob: first the 12-byte nonce, then the ciphertext.
/// The offer (sent earlier) still advertises the *plaintext* hash and size.
pub async fn send_encrypted_blob(conn: &Connection, nonce: &[u8], ciphertext: &[u8]) -> Result<()> {
    let (mut send, _recv) = conn.open_bi().await.context("open_bi for encrypted blob")?;
    if nonce.len() != 12 {
        bail!("nonce must be 12 bytes");
    }
    // We send nonce + ciphertext length + data for robustness.
    send.write_all(nonce).await?;
    send.write_u64(ciphertext.len() as u64).await?;
    send.write_all(ciphertext).await?;
    send.finish()?;
    Ok(())
}

/// Receive and decrypt a blob. Returns the plaintext.
/// The caller must have approved the offer (which carries the expected plaintext hash).
pub async fn recv_encrypted_blob(
    conn: &Connection,
    offer: &PeerBlobOffer,
    key: &[u8; 32],
) -> Result<Vec<u8>> {
    if offer.size > MAX_BLOB_FOR_GATE {
        bail!("blob larger than L0 gate safety cap");
    }
    let (_send, mut recv) = conn.accept_bi().await.context("accept_bi for encrypted blob data")?;

    let mut nonce = [0u8; 12];
    recv.read_exact(&mut nonce).await?;

    let ct_len = recv.read_u64().await? as usize;
    // Basic sanity: ciphertext is usually a bit larger than plaintext due to tag.
    if ct_len > (offer.size as usize) + 32 {
        bail!("suspicious ciphertext length");
    }

    let mut ciphertext = vec![0u8; ct_len];
    recv.read_exact(&mut ciphertext).await?;

    // Decrypt first (this also authenticates).
    let plaintext = crate::decrypt_blob(key, &nonce, &ciphertext)?;

    // Now verify the plaintext matches the hash the sender offered.
    let computed = Hash::new(&plaintext);
    if computed != offer.hash {
        bail!("BLAKE3 mismatch after decryption: got {}, expected {}", computed, offer.hash);
    }

    Ok(plaintext)
}
