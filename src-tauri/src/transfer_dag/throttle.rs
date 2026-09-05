//! Byte-level pacing against the process-global bandwidth governor.
//!
//! The user's speed limit (`--limit-rate`, `--bwlimit`, the GUI setting) used
//! to be a per-provider courtesy: `StorageProvider::set_speed_limit` was
//! implemented by SFTP and MEGA and answered `NotSupported` everywhere else,
//! and the CLI discarded that answer. On S3, WebDAV, FTP and every HTTP cloud
//! the flag was therefore a silent no-op. These helpers put the pacing where
//! the bytes actually move, once, for every provider that streams through
//! them: the shared HTTP download loops, the multipart part bodies, the
//! whole-file `ReaderStream` uploads and the FTP data channel.
//!
//! Charging happens *before* a chunk is written to the wire or to disk, on the
//! directional bucket plus the combined cap, so concurrent transfers share one
//! budget instead of each honouring its own. When no cap is configured every
//! helper is a pass-through: same body types, same chunk sizes, no extra
//! allocation on the hot path.

use super::governor::{self, GlobalTransferGovernor, TransferDirection};
use futures_util::{Stream, StreamExt};
use std::sync::Arc;

/// Largest slice of an owned body that is charged and sent as one chunk when a
/// cap is active. Small enough that a modest cap still paces smoothly, large
/// enough that an uncapped-looking transfer is not fragmented needlessly.
pub const OWNED_BODY_CHUNK_BYTES: usize = 256 * 1024;

/// Wait until `bytes` may travel in `direction`, then charge them.
pub async fn charge(direction: TransferDirection, bytes: u64) {
    governor::global().charge(direction, bytes).await;
}

/// True when neither the combined cap nor the cap for `direction` is set.
pub fn is_unlimited(direction: TransferDirection) -> bool {
    governor_is_unlimited(&governor::global(), direction)
}

fn governor_is_unlimited(g: &GlobalTransferGovernor, direction: TransferDirection) -> bool {
    g.bandwidth().is_unlimited() && g.directional_bandwidth(direction).is_unlimited()
}

/// Pace a stream of byte chunks against the process-global governor: each `Ok`
/// chunk is charged for its length before it is yielded. Errors pass through
/// untouched. The item type is preserved, so this wraps a `reqwest` response
/// stream, a `tokio_util::io::ReaderStream`, or a part-body window stream alike.
pub fn throttle_stream<S, T, E>(
    stream: S,
    direction: TransferDirection,
) -> impl Stream<Item = Result<T, E>> + Send
where
    S: Stream<Item = Result<T, E>> + Send,
    T: AsRef<[u8]> + Send,
    E: Send,
{
    throttle_stream_with(stream, governor::global(), direction)
}

/// [`throttle_stream`] against an explicit governor (tests own a private one).
pub fn throttle_stream_with<S, T, E>(
    stream: S,
    governor: Arc<GlobalTransferGovernor>,
    direction: TransferDirection,
) -> impl Stream<Item = Result<T, E>> + Send
where
    S: Stream<Item = Result<T, E>> + Send,
    T: AsRef<[u8]> + Send,
    E: Send,
{
    stream.then(move |item| {
        let governor = Arc::clone(&governor);
        async move {
            if let Ok(chunk) = &item {
                governor
                    .charge(direction, chunk.as_ref().len() as u64)
                    .await;
            }
            item
        }
    })
}

/// A `reqwest` body for an owned buffer. Unlimited: the buffer itself, exactly
/// as before (reqwest derives `Content-Length` from it). Capped: the buffer is
/// sent as paced slices. Callers that need `Content-Length` (S3 signed PUTs set
/// it explicitly from the buffer length) keep it, because a streamed body under
/// an explicit `Content-Length` header is sent unchunked.
pub fn owned_body(data: Vec<u8>, direction: TransferDirection) -> reqwest::Body {
    owned_body_with(data, governor::global(), direction)
}

/// [`owned_body`] against an explicit governor.
pub fn owned_body_with(
    data: Vec<u8>,
    governor: Arc<GlobalTransferGovernor>,
    direction: TransferDirection,
) -> reqwest::Body {
    if governor_is_unlimited(&governor, direction) {
        return reqwest::Body::from(data);
    }
    reqwest::Body::wrap_stream(throttle_stream_with(
        owned_chunks(data),
        governor,
        direction,
    ))
}

/// A `reqwest` body for a refcounted payload. Unlimited: `Body::from(bytes)`,
/// no copy. Capped: paced `Bytes::slice` windows, still no copy. This is the
/// body S3 uses for signed part uploads, so a retry rebuilds it from the same
/// `Bytes` for the price of a refcount.
pub fn owned_body_bytes(data: bytes::Bytes, direction: TransferDirection) -> reqwest::Body {
    owned_body_bytes_with(data, governor::global(), direction)
}

/// [`owned_body_bytes`] against an explicit governor.
pub fn owned_body_bytes_with(
    data: bytes::Bytes,
    governor: Arc<GlobalTransferGovernor>,
    direction: TransferDirection,
) -> reqwest::Body {
    if governor_is_unlimited(&governor, direction) {
        return reqwest::Body::from(data);
    }
    reqwest::Body::wrap_stream(throttle_stream_with(
        bytes_windows(data),
        governor,
        direction,
    ))
}

/// The paced windows of a refcounted payload: slices, not copies.
fn bytes_windows(
    data: bytes::Bytes,
) -> impl Stream<Item = Result<bytes::Bytes, std::io::Error>> + Send {
    let len = data.len();
    futures_util::stream::iter(
        (0..len)
            .step_by(OWNED_BODY_CHUNK_BYTES.max(1))
            .map(move |start| {
                let end = (start + OWNED_BODY_CHUNK_BYTES).min(len);
                Ok(data.slice(start..end))
            })
            .collect::<Vec<_>>(),
    )
}

/// The paced slices of an owned buffer, in order, covering it exactly once.
fn owned_chunks(data: Vec<u8>) -> impl Stream<Item = Result<Vec<u8>, std::io::Error>> + Send {
    let chunks: Vec<Result<Vec<u8>, std::io::Error>> = data
        .chunks(OWNED_BODY_CHUNK_BYTES)
        .map(|c| Ok(c.to_vec()))
        .collect();
    futures_util::stream::iter(chunks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transfer_dag::governor::GovernorConfig;

    fn capped(upload_bps: u64, download_bps: u64) -> Arc<GlobalTransferGovernor> {
        let g = GlobalTransferGovernor::new(GovernorConfig {
            buffer_bytes: 0,
            bandwidth_bps: 0,
            endpoint_slots: 4,
            disk_device_slots: 4,
        });
        g.set_transfer_limits(upload_bps, download_bps);
        g
    }

    #[tokio::test]
    async fn directional_charge_lands_on_its_own_bucket_only() {
        let g = capped(8 * 1024 * 1024, 4 * 1024 * 1024);
        g.charge(TransferDirection::Upload, 300 * 1024).await;
        assert_eq!(
            g.directional_bandwidth(TransferDirection::Upload)
                .granted_bytes(),
            300 * 1024
        );
        assert_eq!(
            g.directional_bandwidth(TransferDirection::Download)
                .granted_bytes(),
            0
        );
        // The combined cap is unset here: it grants nothing and charges nothing.
        assert_eq!(g.bandwidth().granted_bytes(), 0);
    }

    #[tokio::test]
    async fn set_rate_rearms_and_lifting_the_cap_frees_a_waiter() {
        let g = capped(0, 0);
        let bucket = g.directional_bandwidth(TransferDirection::Download);
        assert!(bucket.is_unlimited());
        bucket.set_rate_bps(2 * 1024 * 1024);
        assert_eq!(bucket.rate_bps(), 2 * 1024 * 1024);
        assert_eq!(bucket.burst_bytes(), 2 * 1024 * 1024);
        // Drain the burst, then park a waiter that needs a full second of
        // refill; lifting the cap must release it long before that.
        bucket.acquire(2 * 1024 * 1024).await;
        let waiter = {
            let bucket = Arc::clone(&bucket);
            tokio::spawn(async move { bucket.acquire(2 * 1024 * 1024).await })
        };
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(
            !waiter.is_finished(),
            "waiter must still be parked under the cap"
        );
        bucket.set_rate_bps(0);
        tokio::time::timeout(std::time::Duration::from_millis(500), waiter)
            .await
            .expect("lifting the cap releases the waiter")
            .expect("waiter task joins");
    }

    #[tokio::test]
    async fn throttled_stream_charges_every_ok_chunk_and_passes_errors() {
        let g = capped(64 * 1024 * 1024, 64 * 1024 * 1024);
        let items: Vec<Result<Vec<u8>, &'static str>> =
            vec![Ok(vec![0u8; 1000]), Err("boom"), Ok(vec![0u8; 24])];
        let out: Vec<_> = throttle_stream_with(
            futures_util::stream::iter(items),
            Arc::clone(&g),
            TransferDirection::Upload,
        )
        .collect()
        .await;
        assert_eq!(out.len(), 3);
        assert!(out[1].is_err());
        assert_eq!(
            g.directional_bandwidth(TransferDirection::Upload)
                .granted_bytes(),
            1024
        );
        assert_eq!(
            g.directional_bandwidth(TransferDirection::Download)
                .granted_bytes(),
            0
        );
    }

    #[tokio::test]
    async fn unlimited_stream_charges_nothing() {
        let g = capped(0, 0);
        let items: Vec<Result<Vec<u8>, std::io::Error>> = vec![Ok(vec![1u8; 4096])];
        let out: Vec<_> = throttle_stream_with(
            futures_util::stream::iter(items),
            Arc::clone(&g),
            TransferDirection::Download,
        )
        .collect()
        .await;
        assert_eq!(out.len(), 1);
        assert_eq!(
            g.directional_bandwidth(TransferDirection::Download)
                .granted_bytes(),
            0
        );
    }

    #[tokio::test]
    async fn bytes_windows_are_slices_that_cover_the_payload_once() {
        let g = capped(64 * 1024 * 1024, 0);
        let data: Vec<u8> = (0..(OWNED_BODY_CHUNK_BYTES * 3 + 5))
            .map(|i| (i % 199) as u8)
            .collect();
        let payload = bytes::Bytes::from(data.clone());
        let paced: Vec<bytes::Bytes> = throttle_stream_with(
            bytes_windows(payload.clone()),
            Arc::clone(&g),
            TransferDirection::Upload,
        )
        .map(|r| r.expect("slices never fail"))
        .collect()
        .await;
        assert_eq!(paced.len(), 4);
        assert_eq!(paced.concat(), data);
        // Slices share the payload's allocation: the first window starts at
        // the same address as the original buffer.
        assert_eq!(paced[0].as_ptr(), payload.as_ptr());
        assert_eq!(
            g.directional_bandwidth(TransferDirection::Upload)
                .granted_bytes(),
            data.len() as u64
        );
    }

    #[tokio::test]
    async fn owned_chunks_cover_the_buffer_once_and_are_charged_in_full() {
        let g = capped(64 * 1024 * 1024, 0);
        let data: Vec<u8> = (0..(OWNED_BODY_CHUNK_BYTES * 2 + 17))
            .map(|i| (i % 251) as u8)
            .collect();
        let paced: Vec<Vec<u8>> = throttle_stream_with(
            owned_chunks(data.clone()),
            Arc::clone(&g),
            TransferDirection::Upload,
        )
        .map(|r| r.expect("owned chunks never fail"))
        .collect()
        .await;
        assert_eq!(paced.len(), 3);
        assert_eq!(paced.concat(), data);
        assert_eq!(
            g.directional_bandwidth(TransferDirection::Upload)
                .granted_bytes(),
            data.len() as u64
        );
    }
}
