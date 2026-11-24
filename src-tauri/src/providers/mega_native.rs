// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! MEGA Native API Provider: full JSON-RPC implementation.
//! Connects directly to MEGA servers without MEGAcmd dependency.

use crate::providers::HttpResultExt;
use async_trait::async_trait;
use base64::Engine as _;
use secrecy::ExposeSecret;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::time::{sleep, Duration};
use zeroize::Zeroize;

use super::{
    mega_crypto::{
        aes_ctr_apply_inplace, aes_ctr_decrypt, aes_ecb_decrypt_block, aes_ecb_encrypt_block,
        aes_ecb_encrypt_multi, chunk_mac, compute_chunk_boundaries, decrypt_node_attrs,
        decrypt_node_key_xor, decrypt_rsa_privkey, encrypt_node_attrs, kdf_v1, kdf_v2,
        mega_base64_decode, mega_base64_encode, meta_mac, meta_mac_legacy, pack_node_key,
        rsa_decrypt_csid, unpack_node_key_legacy, unpack_node_key_with_mac, username_hash_v1,
    },
    MegaConfig, MultipartHandle, ProviderError, ProviderType, RemoteEntry, ShareLinkOptions,
    ShareLinkResult, StorageInfo, StorageProvider, UploadedPart,
};

const MEGA_API_BASE_URL: &str = "https://g.api.mega.co.nz/cs";
const MEGA_API_VERSION: &str = "2";
const AERORSYNC_MAX_RETRIES: u32 = 2;
const AERORSYNC_RETRY_DELAY_MS: u64 = 2000;

/// Multipart threshold: files >= 32 MiB are routed through the shaped-graph
/// multipart trait so the runner can fan out parallel PUTs against the
/// per-session upload URL. Below the threshold the legacy single-stream
/// `upload()` path already pipelines MEGA's 128 KiB...1 MiB ramp efficiently.
const MEGA_MULTIPART_THRESHOLD: u64 = 32 * 1024 * 1024;

/// Multipart part size: 1 MiB matches MEGA's steady-state chunk boundary after
/// the initial 128/256/.../1024 KiB ramp. Aligning the runner's slice size to
/// the per-chunk MAC boundary lets `complete_multipart_upload` re-derive the
/// chunk MACs from the local source file in a single sequential pass without
/// keeping per-part plaintext in memory across the upload window.
const MEGA_MULTIPART_PART_SIZE: u64 = 1024 * 1024;

/// gfs* enforces the canonical 128 KiB → 1 MiB ramp + steady-state chunk
/// layout (`-7 EACCESS` on uniform-size PUTs; empirically confirmed against
/// `MEGA Dev` on 2026-05-26). The shaped-graph trait honours this by
/// buffering runner-sized parts inside `MegaMultipartState` and only
/// flushing to gfs* when a canonical chunk boundary lands. That state
/// machine is intrinsically serial, so the trait advertises
/// `multipart_max_parallel = 1` and the runner serialises `UploadPart`
/// nodes via the dependency chain in `shaped_file`. Real parallelism
/// against MEGA would need either N upload URLs per file or a redesign of
/// the buffer protocol; both are out of scope for v4.0.0.
const MEGA_MULTIPART_MAX_PARALLEL: u8 = 1;

// ─── Wire types ───────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct MegaPreloginResponse {
    version: u8,
    salt: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MegaPreloginWire {
    #[serde(rename = "v")]
    version: u8,
    #[serde(rename = "s")]
    salt: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MegaLoginResponseWire {
    #[serde(rename = "k")]
    encrypted_master_key: String,
    #[serde(rename = "tsid")]
    temporary_session_id: Option<String>,
    #[serde(rename = "csid")]
    encrypted_session_id: Option<String>,
    #[serde(rename = "privk")]
    encrypted_rsa_private_key: Option<String>,
    #[serde(rename = "u")]
    user_handle: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MegaUserInfoWire {
    #[serde(rename = "u")]
    user_handle: String,
    email: Option<String>,
    #[serde(rename = "sn")]
    sequence_number: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct MegaPersistedSession {
    session_id: String,
    master_key_b64: String,
    account_version: u8,
    user_handle: Option<String>,
    sequence_number: Option<String>,
    stored_at_unix_ms: u64,
}

#[derive(Debug, Deserialize)]
struct FetchNodesResponseWire {
    f: Vec<RawMegaNodeWire>,
}

#[derive(Debug, Deserialize)]
struct RawMegaNodeWire {
    /// Node handle
    h: String,
    /// Parent handle
    p: String,
    /// Node type: 0=file, 1=folder, 2=root, 3=inbox, 4=trash
    t: u8,
    /// Encrypted key (format: "owner_handle:base64_key" or multiple separated by /)
    #[serde(default)]
    k: Option<String>,
    /// Encrypted attributes
    #[serde(default, rename = "a")]
    attrs: Option<String>,
    /// File size (0 for folders)
    #[serde(default)]
    s: u64,
    /// Timestamp (unix)
    #[serde(default)]
    ts: u64,
    /// User handle (owner)
    #[serde(default)]
    _u: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GetDownloadUrlResponseWire {
    /// Download URL
    g: String,
    /// File size
    s: u64,
}

#[derive(Debug, Deserialize)]
struct RequestUploadUrlResponseWire {
    /// Upload URL
    p: String,
}

#[derive(Debug, Deserialize)]
struct PutNodesResponseWire {
    #[allow(dead_code)]
    f: Vec<RawMegaNodeWire>,
}

/// Side-band metadata embedded in `MultipartHandle.upload_id` for the
/// shaped-graph multipart upload trait wiring (S4-T01).
/// `begin_multipart_upload` resolves the destination parent, generates the
/// per-session file_key and nonce, and requests the upload URL from MEGA,
/// then encodes everything here so subsequent `upload_part` and
/// `complete_multipart_upload` invocations don't need to re-resolve.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct MegaMultipartMeta {
    upload_url: String,
    file_key: [u8; 16],
    nonce: [u8; 8],
    parent_handle: String,
    file_name: String,
    total: u64,
    part_size: u64,
    total_chunks: u32,
}

impl MegaMultipartMeta {
    fn encode(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    fn decode(raw: &str) -> Result<Self, ProviderError> {
        serde_json::from_str(raw)
            .map_err(|e| ProviderError::Other(format!("MEGA multipart handle decode failed: {e}")))
    }
}

/// Provider-internal state for an in-flight shaped-graph multipart upload.
///
/// MEGA's gfs* storage nodes are strict about the chunk layout that lands on
/// each PUT: they expect the canonical 128 KiB → 1 MiB ramp scheme that every
/// official client (MEGA SDK C++, megalib, mega-cmd, megajs) uses. Posting
/// uniformly-sized parts elicits a `-7 EACCESS` reply even when the offsets
/// stay inside the declared file size (confirmed empirically against
/// `MEGA Dev` profile, 2026-05-26). The trait wiring therefore decouples the
/// runner's slice size from the wire chunk size: `upload_part` accepts data
/// at the runner's preferred `multipart_part_size` and accumulates it into
/// the next MEGA-canonical chunk via this state machine, flushing to gfs*
/// only when the buffer reaches the expected ramp boundary.
///
/// `chunk_macs` is populated as each chunk completes (MAC computed on the
/// plaintext *before* the in-place AES-CTR encryption that feeds the PUT)
/// so `complete_multipart_upload` doesn't need to touch the local source
/// file a second time. `upload_handle` captures the cumulative completion
/// token gfs* returns on whichever PUT lands the final byte of the file.
///
/// The runner serialises `upload_part` invocations for MEGA
/// (`clone_multipart_worker` returns `None` for it), so this state lives on
/// a plain `&mut self` borrow without further synchronisation.
struct MegaMultipartState {
    /// MEGA-canonical chunk boundaries (offset, size) covering `[0, total)`.
    /// Same scheme as `compute_chunk_boundaries(total)`: 128 KiB, 256 KiB,
    /// ..., up to 1 MiB, then 1 MiB steady-state.
    ramp_chunks: Vec<(u64, usize)>,
    /// Index of the next MEGA chunk to flush to gfs*.
    chunk_idx: usize,
    /// Plaintext bytes pending the next chunk boundary.
    chunk_buffer: Vec<u8>,
    /// Per-chunk CBC-MACs accumulated in canonical chunk order. Consumed by
    /// `complete_multipart_upload` to derive the file's `meta_mac`.
    chunk_macs: Vec<[u8; 16]>,
    /// Cumulative completion token returned by gfs* on the PUT that lands
    /// the last byte of the file. `complete_multipart_upload` rejects the
    /// finalize call when this is still `None`.
    upload_handle: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GetQuotaResponseWire {
    /// Total storage in bytes
    #[serde(default)]
    mstrg: u64,
    /// Used storage in bytes
    #[serde(default)]
    cstrg: u64,
}

// ─── Internal node representation ─────────────────────────────────────────

#[derive(Debug, Clone)]
struct MegaNode {
    handle: String,
    parent: String,
    node_type: u8,
    name: String,
    size: u64,
    timestamp: u64,
    /// Decrypted key (16 bytes for folders, 32 bytes for files)
    key: Vec<u8>,
}

impl MegaNode {
    fn is_file(&self) -> bool {
        self.node_type == 0
    }
    fn is_folder(&self) -> bool {
        self.node_type == 1
    }
}

// ─── API Client ───────────────────────────────────────────────────────────

/// A challenge from MEGA's `X-Hashcash` header,
/// `<version>:<easiness>:<timestamp>:<token>` (MEGA SDK `src/posix/net.cpp`).
#[derive(Clone)]
struct MegaHashcashChallenge {
    easiness: u8,
    token: [u8; HASHCASH_TOKEN_BYTES],
}

const HASHCASH_TOKEN_BYTES: usize = 48;
/// The token fills 12 MiB after the 4-byte nonce (MEGA SDK `kRepeat`).
const HASHCASH_TOKEN_REPEAT: usize = 262_144;
/// How long MEGA keeps a challenge valid (MEGA SDK `kTtl`, "based on server's ttl").
const HASHCASH_SERVER_TTL: Duration = Duration::from_secs(300);
/// How long one challenge may be worked on: a third of its lifetime, as the
/// MEGA SDK does on desktop, so the proof reaches MEGA while it is still valid.
const HASHCASH_SOLVE_BUDGET: Duration = Duration::from_secs(HASHCASH_SERVER_TTL.as_secs() / 3);
/// The SDK's finite extended budget after two forced solve timeouts.
const HASHCASH_EXTENDED_BUDGET: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// Challenges answered for one command. Forced timeouts, not successful proofs,
/// select `HASHCASH_EXTENDED_BUDGET` after the SDK's `kMaxRetries` count.
const HASHCASH_MAX_CHALLENGES: usize = 3;
/// Solver threads, the MEGA SDK's desktop cap (`MAX_WORKERS_FOR_GENCASH`).
const HASHCASH_MAX_WORKERS: usize = 8;
const MEGA_ERROR_BODY_LIMIT: usize = 64 * 1024;
const MEGA_ERROR_BODY_READ_TIMEOUT: Duration = Duration::from_secs(30);

fn parse_hashcash_challenge(header: &str) -> Option<MegaHashcashChallenge> {
    let mut parts = header.trim().split(':');
    let version = parts.next()?;
    let easiness = parts.next()?.parse::<u8>().ok()?;
    let _timestamp = parts.next()?;
    let token = parts.next()?;
    if version != "1" || parts.next().is_some() {
        return None;
    }
    let token = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(token)
        .ok()?
        .try_into()
        .ok()?;
    Some(MegaHashcashChallenge { easiness, token })
}

#[derive(Debug, PartialEq, Eq)]
enum HashcashOutcome {
    /// The nonce, base64url without padding, as the `X-Hashcash` answer carries it.
    Solved(String),
    /// The caller dropped the solve; nobody reads this outcome.
    Cancelled,
    /// No nonce met the threshold within the budget.
    OutOfBudget,
    /// A worker could not start, so the nonce partition was incomplete.
    WorkerStartFailed,
}

/// MEGA SDK `thresholdFromEasiness`.
fn hashcash_threshold(easiness: u8) -> u32 {
    ((((easiness & 63) as u32) << 1) + 1) << (((easiness >> 6) as u32) * 7 + 3)
}

/// The 12 MiB message with a zero nonce: `nonce(4) || token x 262144`.
fn hashcash_message(token: &[u8; HASHCASH_TOKEN_BYTES]) -> Vec<u8> {
    let mut message = Vec::with_capacity(4 + HASHCASH_TOKEN_REPEAT * HASHCASH_TOKEN_BYTES);
    message.extend_from_slice(&[0; 4]);
    for _ in 0..HASHCASH_TOKEN_REPEAT {
        message.extend_from_slice(token);
    }
    message
}

/// Whether `nonce` answers the challenge. `message` is shared read-only by
/// every worker; the nonce stands in for its first four bytes.
fn hashcash_nonce_meets(message: &[u8], nonce: [u8; 4], threshold: u32) -> bool {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(nonce);
    hasher.update(&message[4..]);
    let digest = hasher.finalize();
    u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]) <= threshold
}

fn hashcash_workers() -> usize {
    std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(HASHCASH_MAX_WORKERS)
}

#[cfg(test)]
thread_local! {
    static HASHCASH_FAIL_SPAWN: std::cell::Cell<Option<u32>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn hashcash_spawn_should_fail(start: u32) -> bool {
    HASHCASH_FAIL_SPAWN.with(|slot| slot.get() == Some(start))
}

/// Search the nonce space in big-endian order with `workers` threads, worker
/// `w` trying `w, w + workers, ...` (the MEGA SDK `gencashWorker` stride).
/// Worker 0 runs on the calling thread. A failed launch stops every worker and
/// returns a distinct error instead of reporting an incomplete search as no
/// proof. Each worker checks found, start failure, cancellation and deadline
/// before every hash, and all started workers are joined before this returns.
fn solve_hashcash_with(
    challenge: &MegaHashcashChallenge,
    cancelled: Option<&AtomicBool>,
    budget: Duration,
    workers: usize,
) -> HashcashOutcome {
    let message = hashcash_message(&challenge.token);
    let threshold = hashcash_threshold(challenge.easiness);
    // `None`: no deadline, for a budget no `Instant` can hold (`Duration::MAX`);
    // `Instant + Duration::MAX` would panic.
    let deadline = std::time::Instant::now().checked_add(budget);
    let stride = workers.clamp(1, HASHCASH_MAX_WORKERS) as u32;
    let found = AtomicBool::new(false);
    let start_failed = AtomicBool::new(false);
    let winner = std::sync::Mutex::new(None::<u32>);

    let search = |start: u32| {
        let mut nonce = start;
        loop {
            if found.load(Ordering::Relaxed)
                || start_failed.load(Ordering::Relaxed)
                || cancelled.is_some_and(|flag| flag.load(Ordering::Relaxed))
                || deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline)
            {
                return;
            }
            if hashcash_nonce_meets(&message, nonce.to_be_bytes(), threshold) {
                let mut best = winner
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let previous = *best;
                *best = Some(previous.map_or(nonce, |other| other.min(nonce)));
                found.store(true, Ordering::Relaxed);
                return;
            }
            match nonce.checked_add(stride) {
                Some(next) => nonce = next,
                None => return,
            }
        }
    };

    std::thread::scope(|scope| {
        for start in 1..stride {
            let search = &search;
            #[cfg(test)]
            if hashcash_spawn_should_fail(start) {
                start_failed.store(true, Ordering::Relaxed);
                break;
            }
            if std::thread::Builder::new()
                .name("mega-hashcash".into())
                .spawn_scoped(scope, move || search(start))
                .is_err()
            {
                start_failed.store(true, Ordering::Relaxed);
                break;
            }
        }
        search(0);
    });

    if start_failed.load(Ordering::Relaxed) {
        return HashcashOutcome::WorkerStartFailed;
    }

    let winner = winner
        .into_inner()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match winner {
        Some(nonce) => HashcashOutcome::Solved(
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(nonce.to_be_bytes()),
        ),
        None if cancelled.is_some_and(|flag| flag.load(Ordering::Relaxed)) => {
            HashcashOutcome::Cancelled
        }
        None => HashcashOutcome::OutOfBudget,
    }
}

struct CancelMegaHashcashOnDrop(Arc<AtomicBool>);

impl Drop for CancelMegaHashcashOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// Solve on a blocking worker within `budget` (`Duration::MAX`: no deadline).
/// `Ok(None)`: no proof within the budget.
async fn solve_hashcash_async(
    challenge: MegaHashcashChallenge,
    budget: Duration,
) -> Result<Option<String>, ProviderError> {
    let cancelled = Arc::new(AtomicBool::new(false));
    let worker_cancelled = cancelled.clone();
    let workers = hashcash_workers();
    let mut worker = tokio::task::spawn_blocking(move || {
        solve_hashcash_with(&challenge, Some(&worker_cancelled), budget, workers)
    });
    wait_for_hashcash_worker(cancelled, &mut worker).await
}

async fn wait_for_hashcash_worker(
    cancelled: Arc<AtomicBool>,
    worker: &mut tokio::task::JoinHandle<HashcashOutcome>,
) -> Result<Option<String>, ProviderError> {
    let _cancel_on_drop = CancelMegaHashcashOnDrop(cancelled);
    match worker.await {
        Ok(HashcashOutcome::Solved(answer)) => Ok(Some(answer)),
        Ok(HashcashOutcome::OutOfBudget) => Ok(None),
        Ok(HashcashOutcome::Cancelled) => Err(ProviderError::Cancelled),
        Ok(HashcashOutcome::WorkerStartFailed) => Err(ProviderError::Other(
            "MEGA proof-of-work solver could not start every worker".to_string(),
        )),
        Err(err) => Err(ProviderError::Other(format!(
            "MEGA proof-of-work solver failed: {err}"
        ))),
    }
}

/// A negative MEGA error code that is the whole body, bare (`-15`) or in an
/// array (`[-15]`).
fn negative_mega_code(body: &str) -> Option<i64> {
    let value: Value = serde_json::from_str(body.trim()).ok()?;
    let code = value
        .as_i64()
        .or_else(|| value.as_array()?.first()?.as_i64())?;
    (code < 0).then_some(code)
}

async fn read_mega_error_body(
    mut response: reqwest::Response,
    timeout: Duration,
) -> Result<(String, bool), ProviderError> {
    tokio::time::timeout(timeout, async move {
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(map_reqwest_error)? {
            let remaining = MEGA_ERROR_BODY_LIMIT.saturating_sub(bytes.len());
            if chunk.len() > remaining {
                bytes.extend_from_slice(&chunk[..remaining]);
                return Ok((String::from_utf8_lossy(&bytes).into_owned(), false));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok((String::from_utf8_lossy(&bytes).into_owned(), true))
    })
    .await
    .map_err(|_| ProviderError::Timeout)?
}

fn mega_402_error(challenges_answered: usize) -> ProviderError {
    ProviderError::ServerError(if challenges_answered == 0 {
        "MEGA API returned HTTP 402 without a proof-of-work challenge AeroFTP can answer"
            .to_string()
    } else {
        format!(
            "MEGA API returned HTTP 402 after {challenges_answered} answered proof-of-work challenge(s)"
        )
    })
}

struct MegaApiClient {
    client: reqwest::Client,
    next_request_id: AtomicU64,
    session_id: Option<String>,
    /// Points the API at a local double in tests, so no test reaches MEGA.
    #[cfg(test)]
    base_url_override: Option<String>,
    /// Per-challenge normal budgets in tests; extended budget stays fixed.
    #[cfg(test)]
    hashcash_budgets: Option<Vec<Duration>>,
    #[cfg(test)]
    error_body_read_timeout: Option<Duration>,
}

impl MegaApiClient {
    /// Build the API client, its HTTP limits taken from [`crate::providers::http_limits`].
    /// Nothing is sent until the first request.
    fn new(session_id: Option<String>) -> Self {
        let client = reqwest::Client::builder()
            .user_agent(crate::providers::AEROFTP_USER_AGENT)
            .connect_timeout(crate::providers::http_limits::limit(Duration::from_secs(
                30,
            )))
            .read_timeout(crate::providers::http_limits::limit(
                crate::providers::http_limits::READ_TIMEOUT,
            ))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self {
            client,
            next_request_id: AtomicU64::new(1),
            session_id,
            #[cfg(test)]
            base_url_override: None,
            #[cfg(test)]
            hashcash_budgets: None,
            #[cfg(test)]
            error_body_read_timeout: None,
        }
    }

    fn base_url(&self) -> &str {
        #[cfg(test)]
        if let Some(url) = &self.base_url_override {
            return url;
        }
        MEGA_API_BASE_URL
    }

    fn error_body_read_timeout(&self) -> Duration {
        #[cfg(test)]
        if let Some(timeout) = self.error_body_read_timeout {
            return timeout;
        }
        MEGA_ERROR_BODY_READ_TIMEOUT
    }

    /// Only two forced timeouts select the SDK's finite extended budget.
    fn hashcash_budget(&self, _challenge_index: usize, extended: bool) -> Duration {
        if extended {
            return HASHCASH_EXTENDED_BUDGET;
        }
        #[cfg(test)]
        if let Some(budgets) = &self.hashcash_budgets {
            return budgets
                .get(_challenge_index)
                .copied()
                .unwrap_or(HASHCASH_SOLVE_BUDGET);
        }
        HASHCASH_SOLVE_BUDGET
    }

    fn set_session_id(&mut self, session_id: Option<String>) {
        if let Some(ref mut old) = self.session_id {
            old.zeroize();
        }
        self.session_id = session_id;
    }

    async fn command<T>(&self, command: Value) -> Result<T, ProviderError>
    where
        T: DeserializeOwned,
    {
        let request_id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        let mut url = reqwest::Url::parse(self.base_url()).map_err(|err| {
            ProviderError::InvalidConfig(format!("Invalid MEGA API base URL: {err}"))
        })?;

        {
            let mut query = url.query_pairs_mut();
            query.append_pair("id", &request_id.to_string());
            query.append_pair("v", MEGA_API_VERSION);
            if let Some(session_id) = self.session_id.as_deref() {
                query.append_pair("sid", session_id);
            }
        }

        let cmd_name = command
            .get("a")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string();
        tracing::debug!(
            "[MEGA Native] API: cmd={}, has_sid={}",
            cmd_name,
            self.session_id.is_some()
        );

        let payload = vec![command];
        let mut proof: Option<String> = None;
        let mut challenges_answered = 0usize;
        let mut forced_timeouts = 0usize;
        let response = loop {
            let mut request = self.client.post(url.clone()).json(&payload);
            if let Some(proof) = proof.take() {
                request = request.header("X-Hashcash", proof);
            }
            let response = request.send().await.map_err(map_reqwest_error)?;
            if response.status().as_u16() != 402 {
                break response;
            }

            let challenge = response
                .headers()
                .get("X-Hashcash")
                .and_then(|value| value.to_str().ok())
                .and_then(parse_hashcash_challenge);
            let (body, complete) =
                read_mega_error_body(response, self.error_body_read_timeout()).await?;
            let mega_code = if complete {
                negative_mega_code(&body)
            } else {
                None
            };
            tracing::warn!(
                "[MEGA Native] non-success HTTP 402 cmd={} body_len={} complete={} mega_code={:?}",
                cmd_name,
                body.len(),
                complete,
                mega_code
            );
            if let Some(code) = mega_code {
                // MEGA's -13 is EINCOMPLETE, not a session rejection. On a 402
                // it must not clear a saved session; -15 remains ESID.
                if code == -13 {
                    return Err(mega_402_error(challenges_answered));
                }
                return Err(map_mega_error_code(code));
            }
            // Like the SDK, PoW is only for an empty-body challenge. A body is
            // classified first and never triggers a proof-bearing retry.
            if !body.is_empty() || challenges_answered == HASHCASH_MAX_CHALLENGES {
                return Err(mega_402_error(challenges_answered));
            }
            let Some(challenge) = challenge else {
                return Err(mega_402_error(challenges_answered));
            };

            challenges_answered += 1;
            let easiness = challenge.easiness;
            tracing::debug!(
                "[MEGA Native] Hashcash challenge {}/{} received: easiness={}",
                challenges_answered,
                HASHCASH_MAX_CHALLENGES,
                easiness
            );
            let started = std::time::Instant::now();
            let budget = self.hashcash_budget(challenges_answered - 1, forced_timeouts >= 2);
            match solve_hashcash_async(challenge.clone(), budget).await? {
                Some(answer) => {
                    tracing::debug!(
                        "[MEGA Native] Hashcash challenge solved: easiness={}, elapsed_ms={}",
                        easiness,
                        started.elapsed().as_millis()
                    );
                    proof = Some(format!(
                        "1:{}:{answer}",
                        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(challenge.token)
                    ));
                }
                None => {
                    forced_timeouts += 1;
                    if challenges_answered == HASHCASH_MAX_CHALLENGES {
                        return Err(ProviderError::Timeout);
                    }
                    tracing::debug!(
                        "[MEGA Native] Hashcash challenge not solved: easiness={}, elapsed_ms={}",
                        easiness,
                        started.elapsed().as_millis()
                    );
                }
            }
        };

        if !response.status().is_success() {
            let status = response.status();
            let (body, complete) =
                read_mega_error_body(response, self.error_body_read_timeout()).await?;
            let mega_code = if complete {
                negative_mega_code(&body)
            } else {
                None
            };
            tracing::warn!(
                "[MEGA Native] non-success HTTP {} cmd={} body_len={} complete={} mega_code={:?}",
                status,
                cmd_name,
                body.len(),
                complete,
                mega_code
            );
            if let Some(code) = mega_code {
                // Of the auth-mapped codes, only API_ESID (-15) is a session
                // verdict. Other non-2xx failures keep the legacy NetworkError.
                if !matches!(code, -13 | -14 | -22 | -26) {
                    return Err(map_mega_error_code(code));
                }
            }
            return Err(ProviderError::NetworkError(format!(
                "MEGA API returned HTTP {status}"
            )));
        }

        // Read body as text first instead of going through reqwest's .json()
        // helper. The helper requires Content-Type: application/json, but MEGA
        // sometimes returns global error codes (E_MFAREQUIRED, E_BLOCKED,
        // E_RATE) with text/plain or no Content-Type at all, which makes the
        // helper bail with "error decoding response body" before we ever see
        // the actual payload (issue #128 dev-test report).
        let raw = response.text().await.map_http_err(|err| {
            ProviderError::NetworkError(format!("MEGA API read failed: {err}"))
        })?;
        let trimmed = raw.trim();

        // MEGA replies as `[<value>]` for normal commands, but for global
        // failures it can also reply with a bare negative integer (e.g. `-26`)
        // as the entire body. Try the simple integer path first.
        if let Ok(code) = trimmed.parse::<i64>() {
            if code < 0 {
                return Err(map_mega_error_code(code));
            }
        }

        let payload: Value = serde_json::from_str(trimmed).map_err(|err| {
            // Surface the first 200 bytes of the body so dev-mode can see what
            // MEGA actually returned without dumping secrets to the log.
            let preview: String = trimmed.chars().take(200).collect();
            ProviderError::ParseError(format!(
                "Invalid JSON response from MEGA API: {err} (body: {preview})"
            ))
        })?;

        if let Some(code) = payload.as_i64() {
            if code < 0 {
                return Err(map_mega_error_code(code));
            }
        }

        let first = payload
            .as_array()
            .and_then(|arr| arr.first().cloned())
            .ok_or_else(|| {
                ProviderError::ParseError("MEGA API returned an empty response array".to_string())
            })?;

        decode_command_response(first)
    }
}

fn decode_command_response<T>(value: Value) -> Result<T, ProviderError>
where
    T: DeserializeOwned,
{
    // MEGA returns negative integers for errors, 0 for success on mutation commands
    if let Some(code) = value.as_i64() {
        if code < 0 {
            return Err(map_mega_error_code(code));
        }
        // 0 or positive = success. Try to deserialize; if T can't be built from
        // an integer (e.g. T = Value), just return the raw integer as Value.
    }
    serde_json::from_value(value)
        .map_err(|err| ProviderError::ParseError(format!("Invalid MEGA response payload: {err}")))
}

/// Mask the value of any `sid=` query parameter to `***`. reqwest::Error's
/// Display embeds the request URL (which carries the live MEGA session token as
/// `?sid=...`) for connect/TLS/reset failures, so the token must be redacted
/// before the error is surfaced.
fn redact_sid(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = rest.find("sid=") {
        out.push_str(&rest[..pos + 4]);
        let after = &rest[pos + 4..];
        let end = after.find(['&', ' ', '"', ')', ',']).unwrap_or(after.len());
        out.push_str("***");
        rest = &after[end..];
    }
    out.push_str(rest);
    out
}

fn map_reqwest_error(err: reqwest::Error) -> ProviderError {
    if err.is_timeout() {
        ProviderError::Timeout
    } else {
        let detail = redact_sid(&err.to_string());
        ProviderError::NetworkError(format!("MEGA API request failed: {detail}"))
    }
}

/// The messages `MegaNativeProvider::is_transient_error` matches, exactly: a
/// server error that quotes a response body must not turn transient because
/// of what the body says.
const MEGA_EAGAIN_MESSAGE: &str = "MEGA requested a retry (EAGAIN)";
const MEGA_RATE_LIMIT_MESSAGE: &str = "MEGA rate limit exceeded";

fn map_mega_error_code(code: i64) -> ProviderError {
    match code {
        -1 => ProviderError::ServerError("MEGA internal error".to_string()),
        -2 => ProviderError::InvalidConfig("MEGA rejected the request arguments".to_string()),
        -3 => ProviderError::ServerError(MEGA_EAGAIN_MESSAGE.to_string()),
        -4 => ProviderError::ServerError(MEGA_RATE_LIMIT_MESSAGE.to_string()),
        -7 | -11 => ProviderError::PermissionDenied("MEGA access denied".to_string()),
        -8 => ProviderError::AlreadyExists("MEGA resource already exists".to_string()),
        -9 => ProviderError::NotFound("MEGA resource not found".to_string()),
        -13 => ProviderError::AuthenticationFailed("MEGA session expired".to_string()),
        -14 => ProviderError::AuthenticationFailed("MEGA account not confirmed".to_string()),
        -15 => ProviderError::AuthenticationFailed("MEGA session ID invalid".to_string()),
        -16 => ProviderError::ServerError("MEGA over quota".to_string()),
        // MEGA authentication-related error codes:
        //   -26 EMFAREQUIRED: account has 2FA enabled and the request was
        //                       missing the `mfa` field
        //   -27 EMASTERONLY : operation needs master account access
        //   -28 EBUSINESSPASTDUE
        //   -29 EPAYWALL
        //   -22 EAPPKEY     : invalid or missing application key
        // The legacy shared mapping is unchanged here; the user-facing 2FA
        // messages remain specific so the UI can prompt for a fresh TOTP.
        -22 => ProviderError::AuthenticationFailed(
            "Wrong or already-used 2FA code. Generate a fresh code from your authenticator app.".to_string(),
        ),
        -26 => ProviderError::AuthenticationFailed(
            "This MEGA account has two-factor authentication enabled. Open Edit on the saved profile, fill the 2FA Code field with a fresh 6-digit code from your authenticator app, then click Connect from the form.".to_string(),
        ),
        other => ProviderError::ServerError(format!("MEGA API error {other}")),
    }
}

// ─── Provider ─────────────────────────────────────────────────────────────

pub struct MegaNativeProvider {
    config: MegaConfig,
    api_client: MegaApiClient,
    connected: bool,
    current_path: String,
    account_version: Option<u8>,
    prelogin_salt: Option<String>,
    session_id: Option<String>,
    master_key: Option<[u8; 16]>,
    user_handle: Option<String>,
    sequence_number: Option<String>,
    /// RSA private key components (p, q, d, u) for csid decryption and share key ops
    rsa_components: Option<(
        num_bigint_dig::BigUint,
        num_bigint_dig::BigUint,
        num_bigint_dig::BigUint,
        num_bigint_dig::BigUint,
    )>,
    /// Decrypted node tree (handle → node)
    nodes: HashMap<String, MegaNode>,
    /// Children index (parent_handle → child handles)
    children: HashMap<String, Vec<String>>,
    root_handle: Option<String>,
    trash_handle: Option<String>,
    nodes_loaded: bool,
    /// In-flight shaped-graph multipart upload state. Holds the canonical
    /// MEGA chunk boundary list, a partial-chunk buffer, the accumulating
    /// per-chunk MAC chain and the cumulative completion token. Lives only
    /// between `begin_multipart_upload` and `complete_multipart_upload` /
    /// `abort_multipart_upload`. Reset on `clear_runtime_session` so a
    /// disconnect drops any half-built session.
    multipart_state: Option<MegaMultipartState>,
}

/// Ensure all sensitive material is zeroized when the provider is dropped,
/// even if disconnect() was never called (e.g. on panic or early return).
impl Drop for MegaNativeProvider {
    fn drop(&mut self) {
        self.clear_runtime_session();
    }
}

/// The commands `plan_relocation` built for one rename or move, and their
/// order.
struct Relocation {
    move_command: Option<Value>,
    rename_command: Option<Value>,
    rename_first: bool,
}

impl MegaNativeProvider {
    /// Resolve the effective `mfa` value forwarded to MEGA's `us` (login)
    /// command. Returns the derived code when the user persisted a base32
    /// TOTP secret, falls back to the single-use code, returns `None` when
    /// the account has no 2FA configured. Called once per login attempt;
    /// the derived value is bound to the current 30s slot.
    fn effective_mfa_code(&self) -> Result<Option<String>, ProviderError> {
        if let Some(secret) = self.config.totp_secret.as_ref() {
            return crate::providers::totp_helper::generate_totp_code(secret).map(Some);
        }
        Ok(self.config.two_factor_code.clone())
    }

    pub fn new(config: MegaConfig) -> Self {
        Self {
            config,
            api_client: MegaApiClient::new(None),
            connected: false,
            current_path: "/".to_string(),
            account_version: None,
            prelogin_salt: None,
            session_id: None,
            master_key: None,
            user_handle: None,
            sequence_number: None,
            rsa_components: None,
            nodes: HashMap::new(),
            children: HashMap::new(),
            root_handle: None,
            trash_handle: None,
            nodes_loaded: false,
            multipart_state: None,
        }
    }

    fn session_vault_key(&self) -> String {
        format!(
            "mega_native_session_{}",
            self.config.email.trim().to_lowercase()
        )
    }

    fn now_unix_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    fn clear_runtime_session(&mut self) {
        self.connected = false;
        self.current_path = "/".to_string();
        self.api_client.set_session_id(None);

        // Zeroize session ID before dropping
        if let Some(ref mut sid) = self.session_id {
            sid.zeroize();
        }
        self.session_id = None;

        self.account_version = None;
        self.prelogin_salt = None;
        self.user_handle = None;
        self.sequence_number = None;

        // Zeroize RSA private key components (BigUint bytes)
        if let Some((ref mut p, ref mut q, ref mut d, ref mut u)) = self.rsa_components {
            p.zeroize();
            q.zeroize();
            d.zeroize();
            u.zeroize();
        }
        self.rsa_components = None;

        // Zeroize all decrypted node keys before clearing
        for node in self.nodes.values_mut() {
            node.key.zeroize();
        }
        self.nodes.clear();
        self.children.clear();
        self.root_handle = None;
        self.trash_handle = None;
        self.nodes_loaded = false;
        // Drop any in-flight multipart session: an upload URL bound to a
        // disconnected session is no longer reachable, and the gfs* node
        // GCs orphaned URLs after their TTL expires.
        self.multipart_state = None;

        if let Some(mut mk) = self.master_key.take() {
            mk.zeroize();
        }
    }

    fn is_transient_error(err: &ProviderError) -> bool {
        matches!(err, ProviderError::ServerError(msg)
            if msg == MEGA_EAGAIN_MESSAGE || msg == MEGA_RATE_LIMIT_MESSAGE)
    }

    async fn command_with_retry<T>(&self, command: Value) -> Result<T, ProviderError>
    where
        T: DeserializeOwned,
    {
        let mut attempt = 0;
        loop {
            match self.api_client.command(command.clone()).await {
                Ok(r) => return Ok(r),
                Err(err) if attempt < AERORSYNC_MAX_RETRIES && Self::is_transient_error(&err) => {
                    attempt += 1;
                    tracing::warn!(
                        "[MEGA Native] transient error, retry {}/{}: {}",
                        attempt,
                        AERORSYNC_MAX_RETRIES,
                        err
                    );
                    sleep(Duration::from_millis(AERORSYNC_RETRY_DELAY_MS)).await;
                }
                Err(err) => return Err(err),
            }
        }
    }

    // ─── Auth helpers ─────────────────────────────────────────────────

    fn verify_tsid(tsid: &str, master_key: &[u8; 16]) -> Result<(), ProviderError> {
        use subtle::ConstantTimeEq;

        let decoded = mega_base64_decode(tsid)?;
        if decoded.len() != 32 {
            return Err(ProviderError::ParseError(format!(
                "Invalid MEGA tsid length: expected 32, got {}",
                decoded.len()
            )));
        }
        let first_half = <[u8; 16]>::try_from(&decoded[..16])
            .map_err(|_| ProviderError::ParseError("tsid slice error".into()))?;
        let expected = aes_ecb_encrypt_block(&first_half, master_key)?;
        // Constant-time comparison to prevent timing side-channels
        if expected.ct_eq(&decoded[16..32]).into() {
            Ok(())
        } else {
            Err(ProviderError::AuthenticationFailed(
                "MEGA tsid verification failed".into(),
            ))
        }
    }

    async fn get_user_info(&self) -> Result<MegaUserInfoWire, ProviderError> {
        self.command_with_retry(json!({ "a": "ug", "v": 1 })).await
    }

    fn ensure_expected_email(&self, returned: Option<&str>) -> Result<(), ProviderError> {
        if let Some(returned) = returned {
            let expected = self.config.email.trim().to_lowercase();
            let actual = returned.trim().to_lowercase();
            if actual != expected {
                return Err(ProviderError::AuthenticationFailed(format!(
                    "MEGA session belongs to {actual}, expected {expected}"
                )));
            }
        }
        Ok(())
    }

    /// Persist the session, off the async worker (see
    /// [`crate::credential_store::off_async_worker`]).
    fn persist_session(&self) -> Result<(), ProviderError> {
        crate::credential_store::off_async_worker(|| self.persist_session_now())
    }

    fn persist_session_now(&self) -> Result<(), ProviderError> {
        if !self.config.save_session {
            return Ok(());
        }
        let (Some(sid), Some(mk), Some(ver)) = (
            self.session_id.as_ref(),
            self.master_key.as_ref(),
            self.account_version,
        ) else {
            return Ok(());
        };
        let Some(store) = crate::credential_store::CredentialStore::from_cache() else {
            return Ok(());
        };

        let persisted = MegaPersistedSession {
            session_id: sid.clone(),
            master_key_b64: mega_base64_encode(mk),
            account_version: ver,
            user_handle: self.user_handle.clone(),
            sequence_number: self.sequence_number.clone(),
            stored_at_unix_ms: Self::now_unix_ms(),
        };
        let mut serialized = serde_json::to_string(&persisted)
            .map_err(|e| ProviderError::ParseError(format!("Failed to serialize session: {e}")))?;
        let result = store
            .store(&self.session_vault_key(), &serialized)
            .map_err(|e| ProviderError::Other(format!("Failed to persist session: {e}")));
        serialized.zeroize();
        result
    }

    fn clear_persisted_session(&self) -> Result<(), ProviderError> {
        let Some(store) = crate::credential_store::CredentialStore::from_cache() else {
            return Ok(());
        };
        match store.delete(&self.session_vault_key()) {
            Ok(()) | Err(crate::credential_store::CredentialError::NotFound(_)) => Ok(()),
            Err(e) => Err(ProviderError::Other(format!(
                "Failed to clear session: {e}"
            ))),
        }
    }

    async fn try_resume_session(&mut self) -> Result<bool, ProviderError> {
        if !self.config.save_session {
            return Ok(false);
        }
        let Some(store) = crate::credential_store::CredentialStore::from_cache() else {
            return Ok(false);
        };

        let mut serialized = match store.get(&self.session_vault_key()) {
            Ok(s) => s,
            Err(crate::credential_store::CredentialError::NotFound(_)) => return Ok(false),
            Err(e) => return Err(ProviderError::Other(format!("Failed to load session: {e}"))),
        };

        let persisted: MegaPersistedSession = match serde_json::from_str(&serialized) {
            Ok(p) => {
                serialized.zeroize();
                p
            }
            Err(_) => {
                serialized.zeroize();
                self.clear_persisted_session()?;
                return Ok(false);
            }
        };

        let master_key = match mega_base64_decode(&persisted.master_key_b64).and_then(|bytes| {
            if bytes.len() != 16 {
                return Err(ProviderError::ParseError("bad key length".into()));
            }
            let mut arr = [0u8; 16];
            arr.copy_from_slice(&bytes);
            Ok(arr)
        }) {
            Ok(mk) => mk,
            Err(_) => {
                self.clear_persisted_session()?;
                return Ok(false);
            }
        };

        self.api_client
            .set_session_id(Some(persisted.session_id.clone()));

        match self.get_user_info().await {
            Ok(info) => {
                self.ensure_expected_email(info.email.as_deref())?;
                self.connected = true;
                self.session_id = Some(persisted.session_id);
                self.account_version = Some(persisted.account_version);
                self.master_key = Some(master_key);
                self.user_handle = Some(info.user_handle);
                self.sequence_number = info.sequence_number;
                self.current_path = "/".to_string();
                Ok(true)
            }
            Err(ProviderError::AuthenticationFailed(_)) => {
                self.clear_runtime_session();
                self.clear_persisted_session()?;
                Ok(false)
            }
            Err(e) => {
                self.clear_runtime_session();
                Err(e)
            }
        }
    }

    // ─── Login flows ──────────────────────────────────────────────────

    async fn login_v2(&mut self, prelogin: MegaPreloginResponse) -> Result<(), ProviderError> {
        let salt_b64 = prelogin.salt.ok_or_else(|| {
            ProviderError::ParseError("MEGA v2 pre-login did not include a salt".into())
        })?;
        let salt = mega_base64_decode(&salt_b64)?;
        let password = self.config.password.expose_secret();
        let (mut password_key, user_hash) = kdf_v2(password.as_bytes(), &salt)?;
        let user_hash_b64 = mega_base64_encode(&user_hash);

        let login_result: Result<(), ProviderError> = async {
            // Build the `us` (user-session) request, optionally with the TOTP
            // 2FA code under the `mfa` field. Server returns -26 (E_MFAREQUIRED)
            // when the account has 2FA on and the field is absent, and -9
            // (E_NOENT) / -16 (E_BLOCKED) on a wrong code; both surface to the
            // user as the same generic auth failure as a wrong password.
            let mut us_request = json!({
                "a": "us",
                "user": self.config.email.trim().to_lowercase(),
                "uh": user_hash_b64,
            });
            if let Some(mfa) = self.effective_mfa_code()? {
                if let Some(obj) = us_request.as_object_mut() {
                    obj.insert("mfa".into(), json!(mfa));
                }
            }
            let response: MegaLoginResponseWire = self.command_with_retry(us_request).await?;

            let enc_mk = mega_base64_decode(&response.encrypted_master_key)?;
            let enc_mk_arr = <[u8; 16]>::try_from(enc_mk.as_slice())
                .map_err(|_| ProviderError::ParseError("master key not 16 bytes".into()))?;
            let master_key = aes_ecb_decrypt_block(&enc_mk_arr, &password_key)?;

            // Determine session ID via tsid or RSA/csid path
            let session_id = if let Some(tsid) = response.temporary_session_id {
                Self::verify_tsid(&tsid, &master_key)?;
                tsid
            } else if let Some(ref csid) = response.encrypted_session_id {
                // RSA/csid path: decrypt private key, then decrypt session ID
                let privk_b64 = response.encrypted_rsa_private_key.as_ref().ok_or_else(|| {
                    ProviderError::ParseError("MEGA login: csid present but no privk".into())
                })?;
                let privk_encrypted = mega_base64_decode(privk_b64)?;
                let (p, q, d, _u) = decrypt_rsa_privkey(&privk_encrypted, &master_key)?;
                let sid = rsa_decrypt_csid(csid, &p, &q, &d)?;

                // Store RSA components for later use (share keys, etc.)
                self.rsa_components = Some((p, q, d, _u));
                sid
            } else {
                return Err(ProviderError::ParseError(
                    "MEGA login: no tsid or csid".into(),
                ));
            };

            self.api_client.set_session_id(Some(session_id.clone()));
            let user_info = self.get_user_info().await?;
            self.ensure_expected_email(user_info.email.as_deref())?;

            self.connected = true;
            self.current_path = "/".to_string();
            self.account_version = Some(prelogin.version);
            self.prelogin_salt = Some(salt_b64);
            self.session_id = Some(session_id);
            self.master_key = Some(master_key);
            self.user_handle = Some(response.user_handle.unwrap_or(user_info.user_handle));
            self.sequence_number = user_info.sequence_number;

            self.persist_session()?;
            Ok(())
        }
        .await;

        password_key.zeroize();
        if login_result.is_err() {
            self.clear_runtime_session();
        }
        login_result
    }

    async fn login_v1(&mut self) -> Result<(), ProviderError> {
        let password = self.config.password.expose_secret();
        let mut password_key = kdf_v1(password.as_bytes())?;
        let email_hash = username_hash_v1(&self.config.email, &password_key)?;
        let uh_b64 = mega_base64_encode(&email_hash);

        let login_result: Result<(), ProviderError> = async {
            // Same TOTP 2FA path as login_v2 above.
            let mut us_request = json!({
                "a": "us",
                "user": self.config.email.trim().to_lowercase(),
                "uh": uh_b64,
            });
            if let Some(mfa) = self.effective_mfa_code()? {
                if let Some(obj) = us_request.as_object_mut() {
                    obj.insert("mfa".into(), json!(mfa));
                }
            }
            let response: MegaLoginResponseWire = self.command_with_retry(us_request).await?;

            let enc_mk = mega_base64_decode(&response.encrypted_master_key)?;
            let enc_mk_arr = <[u8; 16]>::try_from(enc_mk.as_slice())
                .map_err(|_| ProviderError::ParseError("master key not 16 bytes".into()))?;
            let master_key = aes_ecb_decrypt_block(&enc_mk_arr, &password_key)?;

            let session_id = if let Some(tsid) = response.temporary_session_id {
                Self::verify_tsid(&tsid, &master_key)?;
                tsid
            } else if let Some(ref csid) = response.encrypted_session_id {
                let privk_b64 = response.encrypted_rsa_private_key.as_ref().ok_or_else(|| {
                    ProviderError::ParseError("MEGA v1 login: csid present but no privk".into())
                })?;
                let privk_encrypted = mega_base64_decode(privk_b64)?;
                let (p, q, d, _u) = decrypt_rsa_privkey(&privk_encrypted, &master_key)?;
                let sid = rsa_decrypt_csid(csid, &p, &q, &d)?;
                self.rsa_components = Some((p, q, d, _u));
                sid
            } else {
                return Err(ProviderError::ParseError(
                    "MEGA v1 login: no tsid or csid".into(),
                ));
            };

            self.api_client.set_session_id(Some(session_id.clone()));
            let user_info = self.get_user_info().await?;
            self.ensure_expected_email(user_info.email.as_deref())?;

            self.connected = true;
            self.current_path = "/".to_string();
            self.account_version = Some(1);
            self.session_id = Some(session_id);
            self.master_key = Some(master_key);
            self.user_handle = Some(response.user_handle.unwrap_or(user_info.user_handle));
            self.sequence_number = user_info.sequence_number;

            self.persist_session()?;
            Ok(())
        }
        .await;

        password_key.zeroize();
        if login_result.is_err() {
            self.clear_runtime_session();
        }
        login_result
    }

    // ─── Node tree ────────────────────────────────────────────────────

    /// Fetch and decrypt the entire MEGA node tree.
    async fn fetch_nodes(&mut self) -> Result<(), ProviderError> {
        let master_key = self.master_key.ok_or(ProviderError::NotConnected)?;

        let response: FetchNodesResponseWire = self
            .command_with_retry(json!({ "a": "f", "c": 1, "r": 1 }))
            .await?;

        self.nodes.clear();
        self.children.clear();
        self.root_handle = None;
        self.trash_handle = None;

        let my_handle = self.user_handle.clone().unwrap_or_default();

        for raw in &response.f {
            // Special node types
            match raw.t {
                2 => {
                    self.root_handle = Some(raw.h.clone());
                }
                4 => {
                    self.trash_handle = Some(raw.h.clone());
                }
                _ => {}
            }

            // Decrypt node key
            let key = if let Some(ref k_field) = raw.k {
                self.decrypt_node_key_field(k_field, &master_key, &my_handle)
                    .unwrap_or_default()
            } else {
                Vec::new()
            };

            // Decrypt name from attributes
            let name = if !key.is_empty() {
                if let Some(ref attrs_b64) = raw.attrs {
                    match mega_base64_decode(attrs_b64) {
                        Ok(encrypted_attrs) => decrypt_node_attrs(&encrypted_attrs, &key)
                            .ok()
                            .and_then(|json_str| {
                                serde_json::from_str::<Value>(&json_str).ok().and_then(|v| {
                                    v.get("n").and_then(|n| n.as_str()).map(String::from)
                                })
                            })
                            .unwrap_or_default(),
                        Err(_) => String::new(),
                    }
                } else {
                    String::new()
                }
            } else {
                String::new()
            };

            // Name special nodes
            let display_name = match raw.t {
                2 => "Cloud Drive".to_string(),
                3 => "Inbox".to_string(),
                4 => "Rubbish Bin".to_string(),
                _ => name,
            };

            let node = MegaNode {
                handle: raw.h.clone(),
                parent: raw.p.clone(),
                node_type: raw.t,
                name: display_name,
                size: raw.s,
                timestamp: raw.ts,
                key,
            };

            // Build children index
            self.children
                .entry(raw.p.clone())
                .or_default()
                .push(raw.h.clone());
            self.nodes.insert(raw.h.clone(), node);
        }

        self.nodes_loaded = true;
        tracing::info!("[MEGA Native] Node tree loaded: {} nodes", self.nodes.len());
        Ok(())
    }

    /// Decrypt a node's key field ("owner:b64key" or "owner:b64key/owner2:b64key2").
    fn decrypt_node_key_field(
        &self,
        k_field: &str,
        master_key: &[u8; 16],
        my_handle: &str,
    ) -> Result<Vec<u8>, ProviderError> {
        // Try to find our own key entry first
        for entry in k_field.split('/') {
            let parts: Vec<&str> = entry.splitn(2, ':').collect();
            if parts.len() != 2 {
                continue;
            }
            let (owner, key_b64) = (parts[0], parts[1]);
            if owner == my_handle {
                let encrypted_key = mega_base64_decode(key_b64)?;
                return decrypt_node_key_xor(&encrypted_key, master_key);
            }
        }
        // Fallback: try the first entry (might work for shared nodes)
        if let Some(entry) = k_field.split('/').next() {
            let parts: Vec<&str> = entry.splitn(2, ':').collect();
            if parts.len() == 2 {
                let encrypted_key = mega_base64_decode(parts[1])?;
                return decrypt_node_key_xor(&encrypted_key, master_key);
            }
        }
        Err(ProviderError::ParseError(
            "Could not decrypt node key".into(),
        ))
    }

    async fn ensure_nodes_loaded(&mut self) -> Result<(), ProviderError> {
        if !self.nodes_loaded {
            self.fetch_nodes().await?;
        }
        Ok(())
    }

    // ─── Path resolution ──────────────────────────────────────────────

    /// Resolve an absolute path to a node handle.
    fn resolve_path(&self, path: &str) -> Result<String, ProviderError> {
        let root = self
            .root_handle
            .as_ref()
            .ok_or(ProviderError::NotConnected)?;

        let clean_path = path.trim_matches('/');
        if clean_path.is_empty() {
            return Ok(root.clone());
        }

        let mut current_handle = root.clone();
        for component in clean_path.split('/') {
            if component.is_empty() {
                continue;
            }
            let child_handles = self
                .children
                .get(&current_handle)
                .cloned()
                .unwrap_or_default();
            let found = child_handles.iter().find(|h| {
                self.nodes
                    .get(*h)
                    .map(|n| n.name == component)
                    .unwrap_or(false)
            });
            match found {
                Some(h) => current_handle = h.clone(),
                None => return Err(ProviderError::NotFound(path.to_string())),
            }
        }
        Ok(current_handle)
    }

    /// The child of the folder `parent_handle` named `name`, if any. MEGA
    /// keeps siblings with the same name side by side, so nothing on the
    /// server stops a second one: this in-memory look is the only guard.
    fn child_named(&self, parent_handle: &str, name: &str) -> Option<&MegaNode> {
        self.children
            .get(parent_handle)?
            .iter()
            .filter_map(|handle| self.nodes.get(handle))
            .find(|node| node.name == name)
    }

    /// The commands that take `from` to `to` (see `relocate_node`), or
    /// `None` when it is already there. The trait promises no overwrite, and
    /// MEGA would keep two siblings with one name, so a taken destination is
    /// refused and neither step may pass through a taken name; `set_aside` is
    /// a node a replace renames out of the way first, which does not count.
    /// Every check runs before anything changes.
    fn plan_relocation(
        &self,
        from: &str,
        to: &str,
        set_aside: Option<&str>,
    ) -> Result<Option<Relocation>, ProviderError> {
        let from_handle = self.resolve_path(from)?;
        let from_node = self
            .nodes
            .get(&from_handle)
            .cloned()
            .ok_or_else(|| ProviderError::NotFound(from.to_string()))?;

        let (to_parent_handle, to_name) = self.resolve_parent_and_name(to)?;
        let moves = from_node.parent != to_parent_handle;
        let renames = from_node.name != to_name;
        if !moves && !renames {
            return Ok(None);
        }
        let taken = |parent: &str, name: &str| {
            self.children.get(parent).is_some_and(|handles| {
                handles.iter().any(|handle| {
                    *handle != from_handle
                        && Some(handle.as_str()) != set_aside
                        && self.nodes.get(handle).is_some_and(|node| node.name == name)
                })
            })
        };
        if taken(&to_parent_handle, &to_name) {
            return Err(ProviderError::AlreadyExists(to.to_string()));
        }
        // The new name is encrypted with the node key: without one the name
        // cannot change, and moving first would leave a half-done rename.
        if renames && from_node.key.is_empty() {
            return Err(ProviderError::Other(format!(
                "Cannot rename {from}: the node has no key to encrypt its new name with"
            )));
        }
        let rename_command = if renames {
            let encrypted_attrs = encrypt_node_attrs(&to_name, &from_node.key)?;
            Some(json!({
                "a": "a",
                "n": from_handle,
                "attr": mega_base64_encode(&encrypted_attrs),
            }))
        } else {
            None
        };
        let move_command = moves.then(|| {
            json!({
                "a": "m",
                "n": from_handle,
                "t": to_parent_handle,
            })
        });
        // The move keeps the old name: when the destination already holds it
        // the rename goes first, in the source folder, so no step puts two
        // siblings under one name.
        let rename_first = moves && renames && taken(&to_parent_handle, &from_node.name);
        if rename_first && taken(&from_node.parent, &to_name) {
            return Err(ProviderError::Other(format!(
                "Cannot move {from} to {to} in two steps without two items sharing a name: \
                 the destination folder holds {} and the source folder holds {to_name}",
                from_node.name
            )));
        }
        Ok(Some(Relocation {
            move_command,
            rename_command,
            rename_first,
        }))
    }

    /// Send the move (`a: "m"`) and rename (`a: "a"`) commands `rename`
    /// built, in the order it chose. A failure after the first command went
    /// through says so: the node is then half-way, and the caller must not
    /// read the error as "nothing changed".
    async fn relocate_node(
        &self,
        from: &str,
        to: &str,
        plan: Relocation,
    ) -> Result<(), ProviderError> {
        let Relocation {
            move_command,
            rename_command,
            rename_first,
        } = plan;
        let steps = if rename_first {
            [(rename_command, "renamed"), (move_command, "moved")]
        } else {
            [(move_command, "moved"), (rename_command, "renamed")]
        };
        let mut done: Option<&str> = None;
        for (command, what) in steps {
            let Some(command) = command else { continue };
            if let Err(e) = self.command_with_retry::<Value>(command).await {
                return Err(match done {
                    None => e,
                    Some(previous) => ProviderError::Other(format!(
                        "{previous} {from}, but the second step toward {to} failed, so it \
                         is only half-way there: {e}"
                    )),
                });
            }
            done = Some(what);
        }
        Ok(())
    }

    /// Resolve parent path and extract the final name component.
    fn resolve_parent_and_name(&self, path: &str) -> Result<(String, String), ProviderError> {
        let clean = path.trim_matches('/');
        if clean.is_empty() {
            return Err(ProviderError::InvalidPath("Cannot operate on root".into()));
        }
        let last_slash = clean.rfind('/');
        let (parent_path, name) = match last_slash {
            Some(pos) => (&clean[..pos], &clean[pos + 1..]),
            None => ("", clean),
        };
        let parent_handle = self.resolve_path(&format!("/{parent_path}"))?;
        Ok((parent_handle, name.to_string()))
    }

    fn node_to_remote_entry(&self, node: &MegaNode) -> RemoteEntry {
        let modified =
            chrono::DateTime::from_timestamp(node.timestamp as i64, 0).map(|dt| dt.to_rfc3339());

        RemoteEntry {
            name: node.name.clone(),
            path: self.build_node_path(&node.handle),
            is_dir: node.is_folder() || node.node_type >= 2,
            size: node.size,
            modified,
            permissions: None,
            owner: None,
            group: None,
            is_symlink: false,
            link_target: None,
            mime_type: None,
            metadata: Default::default(),
        }
    }

    /// Build the full path for a node by walking up parents.
    fn build_node_path(&self, handle: &str) -> String {
        let mut parts = Vec::new();
        let mut current = handle.to_string();
        let root = self.root_handle.clone().unwrap_or_default();

        loop {
            if current == root || current.is_empty() {
                break;
            }
            if let Some(node) = self.nodes.get(&current) {
                if node.node_type >= 2 {
                    break;
                } // root/inbox/trash
                parts.push(node.name.clone());
                current = node.parent.clone();
            } else {
                break;
            }
        }

        parts.reverse();
        format!("/{}", parts.join("/"))
    }

    fn invalidate_nodes(&mut self) {
        self.nodes_loaded = false;
    }

    fn verify_chunk_macs(
        chunk_macs: &[[u8; 16]],
        file_key: &[u8; 16],
        expected_meta_mac: &[u8; 8],
        legacy: bool,
    ) -> Result<bool, ProviderError> {
        let actual = if legacy {
            meta_mac_legacy(chunk_macs, file_key)?
        } else {
            meta_mac(chunk_macs, file_key)?
        };
        Ok(&actual == expected_meta_mac)
    }

    #[allow(clippy::too_many_arguments)]
    async fn download_to_path_with_key(
        &self,
        download_url: &str,
        total_size: u64,
        actual_size: u64,
        file_key: &[u8; 16],
        nonce: &[u8; 8],
        expected_meta_mac: &[u8; 8],
        legacy_mac: bool,
        local_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<bool, ProviderError> {
        let response = self
            .api_client
            .client
            .get(download_url)
            .send()
            .await
            .map_err(map_reqwest_error)?;

        if !response.status().is_success() {
            return Err(ProviderError::TransferFailed(format!(
                "Download HTTP {}",
                response.status()
            )));
        }

        let chunks = compute_chunk_boundaries(total_size);
        let mut stream = Box::pin(crate::transfer_dag::throttle::throttle_stream(
            response.bytes_stream(),
            crate::transfer_dag::governor::TransferDirection::Download,
        ));
        use futures_util::StreamExt;

        let mut atomic = super::atomic_write::AtomicFile::new(local_path)
            .await
            .map_err(ProviderError::IoError)?;
        let mut downloaded: u64 = 0;
        let mut chunk_idx = 0usize;
        let mut chunk_buf: Vec<u8> = Vec::new();
        let mut chunk_macs = Vec::with_capacity(chunks.len());

        while let Some(chunk_result) = stream.next().await {
            let http_chunk = chunk_result.map_http_err(|e| {
                ProviderError::TransferFailed(format!("Download stream error: {e}"))
            })?;
            chunk_buf.extend_from_slice(&http_chunk);

            while chunk_idx < chunks.len() {
                let (mega_offset, mega_size) = chunks[chunk_idx];
                if chunk_buf.len() < mega_size {
                    break;
                }

                let mut mega_chunk: Vec<u8> = chunk_buf.drain(..mega_size).collect();
                aes_ctr_apply_inplace(&mut mega_chunk, file_key, nonce, mega_offset);

                let write_end = if mega_offset + mega_size as u64 > actual_size {
                    actual_size.saturating_sub(mega_offset) as usize
                } else {
                    mega_size
                };
                let plain = &mega_chunk[..write_end];
                chunk_macs.push(chunk_mac(plain, file_key, nonce)?);

                atomic.write_all(plain).await.map_err(|e| {
                    ProviderError::TransferFailed(format!("Write chunk failed: {e}"))
                })?;

                downloaded += write_end as u64;
                if let Some(ref cb) = on_progress {
                    cb(std::cmp::min(downloaded, actual_size), actual_size);
                }

                chunk_idx += 1;
            }
        }

        if !chunk_buf.is_empty() && chunk_idx < chunks.len() {
            let (mega_offset, _) = chunks[chunk_idx];
            let mut remaining = chunk_buf;
            aes_ctr_apply_inplace(&mut remaining, file_key, nonce, mega_offset);
            let write_end = if mega_offset + remaining.len() as u64 > actual_size {
                actual_size.saturating_sub(mega_offset) as usize
            } else {
                remaining.len()
            };
            let plain = &remaining[..write_end];
            chunk_macs.push(chunk_mac(plain, file_key, nonce)?);
            atomic.write_all(plain).await.map_err(|e| {
                ProviderError::TransferFailed(format!("Write final chunk failed: {e}"))
            })?;
        }

        if !Self::verify_chunk_macs(&chunk_macs, file_key, expected_meta_mac, legacy_mac)? {
            return Ok(false);
        }

        atomic.commit().await.map_err(|e| {
            ProviderError::TransferFailed(format!("Failed to finalize download: {e}"))
        })?;

        Ok(true)
    }
}

// ─── StorageProvider implementation ───────────────────────────────────────

#[async_trait]
impl StorageProvider for MegaNativeProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
    fn provider_type(&self) -> ProviderType {
        ProviderType::Mega
    }
    fn display_name(&self) -> String {
        self.config.email.clone()
    }

    async fn connect(&mut self) -> Result<(), ProviderError> {
        // When the user supplies a fresh TOTP, force a real login flow.
        // Without this guard, try_resume_session() would short-circuit on a
        // persisted session_id from a previous (pre-2FA) connect, and the
        // server-side `mfa` validation in login_v1 / login_v2 would never run
        // (issue #128: reported during v3.6.8 dev testing). The TOTP is
        // single-use and only kept in memory for one connect attempt, so its
        // presence is a clear "I want a fresh authentication" signal.
        if self.config.two_factor_code.is_some() {
            let _ = self.clear_persisted_session();
            self.clear_runtime_session();
        } else if self.try_resume_session().await? {
            return Ok(());
        }

        if self.config.password.expose_secret().is_empty() {
            return Err(ProviderError::AuthenticationFailed(
                "MEGA password is required when no saved session is available".into(),
            ));
        }

        let prelogin: MegaPreloginWire = self
            .command_with_retry(json!({
                "a": "us0",
                "user": self.config.email.trim().to_lowercase(),
            }))
            .await?;

        let prelogin = MegaPreloginResponse {
            version: prelogin.version,
            salt: prelogin.salt,
        };

        match prelogin.version {
            2 => self.login_v2(prelogin).await,
            1 => self.login_v1().await,
            v => Err(ProviderError::ParseError(format!(
                "Unsupported MEGA account version {v}"
            ))),
        }
    }

    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        let should_clear =
            self.config.logout_on_disconnect.unwrap_or(false) || !self.config.save_session;
        if should_clear {
            self.clear_persisted_session()?;
        }
        self.clear_runtime_session();
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.connected
    }

    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        self.ensure_nodes_loaded().await?;
        let target_path = if path.is_empty() || path == "." {
            &self.current_path
        } else {
            path
        };
        let handle = self.resolve_path(target_path)?;

        let child_handles = self.children.get(&handle).cloned().unwrap_or_default();
        let mut entries = Vec::with_capacity(child_handles.len());

        for ch in &child_handles {
            if let Some(node) = self.nodes.get(ch) {
                // Skip special nodes (inbox=3, trash=4) when listing root
                if node.node_type >= 3 {
                    continue;
                }
                // Skip nodes with empty names (decryption failed)
                if node.name.is_empty() {
                    continue;
                }
                entries.push(self.node_to_remote_entry(node));
            }
        }

        entries.sort_by(|a, b| {
            b.is_dir
                .cmp(&a.is_dir)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });

        Ok(entries)
    }

    async fn pwd(&mut self) -> Result<String, ProviderError> {
        Ok(self.current_path.clone())
    }

    async fn cd(&mut self, path: &str) -> Result<(), ProviderError> {
        self.ensure_nodes_loaded().await?;
        let target = if path.starts_with('/') {
            path.to_string()
        } else {
            format!("{}/{}", self.current_path.trim_end_matches('/'), path)
        };
        // Normalize
        let normalized = normalize_path(&target);
        // Verify it exists and is a directory
        let handle = self.resolve_path(&normalized)?;
        if let Some(node) = self.nodes.get(&handle) {
            if node.is_file() {
                return Err(ProviderError::InvalidPath(format!(
                    "{normalized} is not a directory"
                )));
            }
        }
        self.current_path = normalized;
        Ok(())
    }

    async fn cd_up(&mut self) -> Result<(), ProviderError> {
        self.ensure_nodes_loaded().await?;
        if self.current_path == "/" {
            return Ok(());
        }
        let parent = match self.current_path.rfind('/') {
            Some(0) | None => "/".to_string(),
            Some(pos) => self.current_path[..pos].to_string(),
        };
        self.current_path = parent;
        Ok(())
    }

    async fn download(
        &mut self,
        remote_path: &str,
        local_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        self.ensure_nodes_loaded().await?;

        let handle = self.resolve_path(remote_path)?;
        let node = self
            .nodes
            .get(&handle)
            .cloned()
            .ok_or_else(|| ProviderError::NotFound(remote_path.to_string()))?;

        if !node.is_file() {
            return Err(ProviderError::InvalidPath(format!(
                "{remote_path} is not a file"
            )));
        }

        if node.key.len() != 32 {
            return Err(ProviderError::ParseError(format!(
                "File node key invalid length: {} (need 32)",
                node.key.len()
            )));
        }

        let packed_key: [u8; 32] = node.key[..32]
            .try_into()
            .map_err(|_| ProviderError::ParseError("key conversion failed".into()))?;
        let (file_key, nonce, expected_meta_mac) = unpack_node_key_with_mac(&packed_key)?;
        let (legacy_file_key, legacy_nonce, legacy_expected_meta_mac) =
            unpack_node_key_legacy(&packed_key)?;

        // Get download URL
        let dl_resp: GetDownloadUrlResponseWire = self
            .command_with_retry(json!({ "a": "g", "g": 1, "n": handle }))
            .await?;

        let total_size = dl_resp.s;
        let actual_size = node.size;

        if self
            .download_to_path_with_key(
                &dl_resp.g,
                total_size,
                actual_size,
                &file_key,
                &nonce,
                &expected_meta_mac,
                false,
                local_path,
                on_progress,
            )
            .await?
        {
            return Ok(());
        }

        tracing::warn!(
            "[MEGA Native] canonical MAC verification failed for '{}'; trying legacy AeroFTP key layout",
            remote_path
        );

        if self
            .download_to_path_with_key(
                &dl_resp.g,
                total_size,
                actual_size,
                &legacy_file_key,
                &legacy_nonce,
                &legacy_expected_meta_mac,
                true,
                local_path,
                None,
            )
            .await?
        {
            return Ok(());
        }

        Err(ProviderError::TransferFailed(
            "MEGA decryption failed: file MAC did not match canonical or legacy AeroFTP key layout"
                .to_string(),
        ))
    }

    async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>, ProviderError> {
        self.download_to_bytes_with_progress(remote_path, None)
            .await
    }

    async fn upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        use tokio::io::AsyncReadExt;

        let file_meta = tokio::fs::metadata(local_path)
            .await
            .map_err(ProviderError::IoError)?;
        let file_size = file_meta.len();

        self.ensure_nodes_loaded().await?;
        let master_key = self.master_key.ok_or(ProviderError::NotConnected)?;

        let (parent_handle, file_name) = self.resolve_parent_and_name(remote_path)?;

        // Generate file key and nonce
        let file_key: [u8; 16] = rand::random();
        let nonce: [u8; 8] = rand::random();

        let chunks = compute_chunk_boundaries(file_size);

        // Request upload URL
        let upload_resp: RequestUploadUrlResponseWire = self
            .command_with_retry(json!({ "a": "u", "s": file_size, "ssl": 2 }))
            .await?;

        // Streaming upload: read each MEGA chunk from disk, compute MAC on plaintext,
        // encrypt in-place, upload. Peak memory = 1 chunk (max 1 MB).
        let mut file = tokio::fs::File::open(local_path)
            .await
            .map_err(ProviderError::IoError)?;

        let mut upload_handle = String::new();
        let mut uploaded = 0u64;
        let mut chunk_macs = Vec::with_capacity(chunks.len());

        for &(offset, size) in &chunks {
            // Read one MEGA chunk from disk
            let mut chunk_buf = vec![0u8; size];
            file.read_exact(&mut chunk_buf)
                .await
                .map_err(|e| ProviderError::TransferFailed(format!("Failed to read chunk: {e}")))?;

            // Compute MAC on plaintext BEFORE encryption
            chunk_macs.push(chunk_mac(&chunk_buf, &file_key, &nonce)?);

            // Encrypt in-place (zero extra allocation)
            aes_ctr_apply_inplace(&mut chunk_buf, &file_key, &nonce, offset);

            // Upload encrypted chunk
            let url = format!("{}/{}", upload_resp.p, offset);
            let resp = self
                .api_client
                .client
                .post(&url)
                .body(chunk_buf)
                .send()
                .await
                .map_err(map_reqwest_error)?;

            let status = resp.status();
            let body = resp.text().await.map_http_err(|e| {
                ProviderError::TransferFailed(format!("Upload chunk failed: {e}"))
            })?;

            if !status.is_success() {
                return Err(ProviderError::TransferFailed(format!(
                    "Upload chunk failed ({}): {}",
                    status, body
                )));
            }

            uploaded += size as u64;
            if let Some(ref cb) = on_progress {
                cb(uploaded, file_size);
            }

            // Last chunk response is the upload handle
            if !body.is_empty() && !body.starts_with('-') {
                upload_handle = body;
            } else if body.starts_with('-') {
                return Err(ProviderError::TransferFailed(format!(
                    "MEGA upload error: {body}"
                )));
            }
        }

        if upload_handle.is_empty() {
            return Err(ProviderError::TransferFailed(
                "No upload handle received".into(),
            ));
        }

        let file_meta_mac = meta_mac(&chunk_macs, &file_key)?;

        // Pack node key and encrypt
        let packed_key = pack_node_key(&file_key, &nonce, &file_meta_mac)?;
        let encrypted_key = aes_ecb_encrypt_multi(&packed_key, &master_key)?;
        let key_b64 = mega_base64_encode(&encrypted_key);

        // Encrypt attributes
        let encrypted_attrs = encrypt_node_attrs(&file_name, &packed_key)?;
        let attrs_b64 = mega_base64_encode(&encrypted_attrs);

        // Create file node
        let _resp: PutNodesResponseWire = self
            .command_with_retry(json!({
                "a": "p",
                "t": parent_handle,
                "n": [{
                    "h": upload_handle,
                    "t": 0,
                    "a": attrs_b64,
                    "k": key_b64,
                }],
            }))
            .await?;

        self.invalidate_nodes();
        Ok(())
    }

    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError> {
        self.ensure_nodes_loaded().await?;
        let master_key = self.master_key.ok_or(ProviderError::NotConnected)?;

        let (parent_handle, folder_name) = self.resolve_parent_and_name(path)?;
        if self.child_named(&parent_handle, &folder_name).is_some() {
            return Err(ProviderError::AlreadyExists(path.to_string()));
        }

        // Generate random folder key
        let folder_key: [u8; 16] = rand::random();

        // Encrypt attributes
        let encrypted_attrs = encrypt_node_attrs(&folder_name, &folder_key[..])?;
        let attrs_b64 = mega_base64_encode(&encrypted_attrs);

        // Encrypt folder key with master key
        let encrypted_key = aes_ecb_encrypt_multi(&folder_key, &master_key)?;
        let key_b64 = mega_base64_encode(&encrypted_key);

        let _resp: PutNodesResponseWire = self
            .command_with_retry(json!({
                "a": "p",
                "t": parent_handle,
                "n": [{
                    "h": "xxxxxxxx",
                    "t": 1,
                    "a": attrs_b64,
                    "k": key_b64,
                }],
            }))
            .await?;

        self.invalidate_nodes();
        Ok(())
    }

    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        // Soft delete: move to trash (same behavior as MEGAcmd and MEGA web client)
        self.move_to_trash(path).await
    }

    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        self.ensure_nodes_loaded().await?;
        let handle = self.resolve_path(path)?;

        // Check if empty
        let children = self.children.get(&handle).cloned().unwrap_or_default();
        if !children.is_empty() {
            return Err(ProviderError::DirectoryNotEmpty(format!(
                "{path} is not empty"
            )));
        }

        // Soft delete: move to trash
        self.move_to_trash(path).await
    }

    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        // Soft delete: move to trash (MEGA moves the whole subtree)
        self.move_to_trash(path).await
    }

    async fn delete_permanent(&mut self, path: &str) -> Result<bool, ProviderError> {
        // MEGA native `delete()` calls move_to_trash, which sends `{a:"m"}`
        // to move ONLY the file node (not its parents) under the trash
        // root. The node keeps its basename, so the inherent
        // `permanent_delete_from_trash` (which scans trash children by
        // name == filename) needs the basename, not the original full
        // path. Strip to basename before dispatch. Treat NotFound as
        // Ok(false) so a missing item (already cleared elsewhere) does
        // not fail the caller.
        let basename = path
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or(path);
        if basename.is_empty() {
            return Ok(false);
        }
        match self.permanent_delete_from_trash(basename).await {
            Ok(()) => Ok(true),
            Err(ProviderError::NotFound(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        self.ensure_nodes_loaded().await?;
        let Some(plan) = self.plan_relocation(from, to, None)? else {
            return Ok(());
        };
        let outcome = self.relocate_node(from, to, plan).await;
        self.invalidate_nodes();
        outcome
    }

    /// MEGA has no move that overwrites, and keeps two siblings with one
    /// name, so a replace renames the item at `to` aside, moves `from` in,
    /// and only then sends the one set aside to the rubbish bin; if the move
    /// fails the item set aside gets its name back. Every command is built
    /// from the node tree before the first goes out. `to` is empty between
    /// the first two, which is why `supports_atomic_replace` says no.
    async fn replace(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        self.ensure_nodes_loaded().await?;
        let from_handle = self.resolve_path(from)?;
        let (to_parent_handle, to_name) = self.resolve_parent_and_name(to)?;
        let Some(occupant) = self
            .child_named(&to_parent_handle, &to_name)
            .filter(|node| node.handle != from_handle)
            .cloned()
        else {
            return self.rename(from, to).await;
        };
        let source_is_dir = self
            .nodes
            .get(&from_handle)
            .is_some_and(|node| node.node_type != 0);
        super::refuse_replace_across_types(to, source_is_dir, occupant.node_type != 0)?;
        if occupant.key.is_empty() {
            return Err(ProviderError::Other(format!(
                "Cannot replace {to}: the item there has no key to rename it aside with"
            )));
        }
        let trash = self
            .trash_handle
            .clone()
            .ok_or_else(|| ProviderError::NotFound("Trash handle not found".into()))?;
        let Some(plan) = self.plan_relocation(from, to, Some(&occupant.handle))? else {
            return Ok(());
        };
        let aside = super::set_aside_name(&to_name);
        let aside_path = match to.trim_end_matches('/').rsplit_once('/') {
            Some((parent, _)) => format!("{parent}/{aside}"),
            None => aside.clone(),
        };
        let rename_to = |name: &str| -> Result<Value, ProviderError> {
            Ok(json!({
                "a": "a",
                "n": occupant.handle,
                "attr": mega_base64_encode(&encrypt_node_attrs(name, &occupant.key)?),
            }))
        };
        let (set_aside, restore) = (rename_to(&aside)?, rename_to(&to_name)?);

        let outcome = async {
            self.command_with_retry::<Value>(set_aside).await?;
            if let Err(e) = self.relocate_node(from, to, plan).await {
                let restored = self.command_with_retry::<Value>(restore).await.map(|_| ());
                return Err(super::set_aside_move_failed(to, &aside_path, e, restored));
            }
            let binned = self
                .command_with_retry::<Value>(json!({
                    "a": "m",
                    "n": occupant.handle,
                    "t": trash,
                }))
                .await;
            if let Err(e) = binned {
                super::report_set_aside_leftover(to, &aside_path, &e);
            }
            Ok(())
        }
        .await;
        self.invalidate_nodes();
        outcome
    }

    /// No: MEGA can neither overwrite on a move nor give a node new content,
    /// so a replace sets the old node aside and the name is empty for a
    /// moment. Callers that need atomicity (CLI `edit`, MCP `remote_edit`,
    /// the crypt marker paths) refuse before they write anything.
    async fn supports_atomic_replace(&mut self) -> Result<bool, ProviderError> {
        Ok(false)
    }

    /// Yes: the replace above renames the item at the destination aside,
    /// moves the new one in, and only then deletes the old one, which is
    /// what an edit's non-atomic opt-in needs.
    fn replace_sets_aside(&self) -> bool {
        true
    }

    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        self.ensure_nodes_loaded().await?;
        let handle = self.resolve_path(path)?;
        let node = self
            .nodes
            .get(&handle)
            .ok_or_else(|| ProviderError::NotFound(path.to_string()))?;
        Ok(self.node_to_remote_entry(node))
    }

    async fn size(&mut self, path: &str) -> Result<u64, ProviderError> {
        let entry = self.stat(path).await?;
        Ok(entry.size)
    }

    async fn exists(&mut self, path: &str) -> Result<bool, ProviderError> {
        self.ensure_nodes_loaded().await?;
        match self.resolve_path(path) {
            Ok(_) => Ok(true),
            Err(ProviderError::NotFound(_)) => Ok(false),
            Err(error) => Err(error),
        }
    }

    async fn keep_alive(&mut self) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let info = self.get_user_info().await?;
        self.sequence_number = info.sequence_number;
        Ok(())
    }

    async fn server_info(&mut self) -> Result<String, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        Ok(format!(
            "MEGA Native API (auth v{})",
            self.account_version.unwrap_or(0)
        ))
    }

    async fn storage_info(&mut self) -> Result<StorageInfo, ProviderError> {
        let quota: GetQuotaResponseWire = self
            .command_with_retry(json!({ "a": "uq", "xfer": 1, "strg": 1 }))
            .await?;

        Ok(StorageInfo {
            used: quota.cstrg,
            total: quota.mstrg,
            free: quota.mstrg.saturating_sub(quota.cstrg),
            versioning_bytes: None,
        })
    }

    fn supports_share_links(&self) -> bool {
        true
    }

    async fn create_share_link(
        &mut self,
        path: &str,
        options: ShareLinkOptions,
    ) -> Result<ShareLinkResult, ProviderError> {
        self.ensure_nodes_loaded().await?;
        let handle = self.resolve_path(path)?;
        let node = self
            .nodes
            .get(&handle)
            .ok_or_else(|| ProviderError::NotFound(path.to_string()))?;

        if node.key.is_empty() {
            return Err(ProviderError::ParseError(
                "Cannot share: node has no key".into(),
            ));
        }

        // MEGA share link = export the node via `l` command, then build URL using
        // the public handle returned by the API (NOT the internal node handle).
        // The `l` command response is the 8-character base64 public handle.
        let public_handle: String = self
            .command_with_retry(json!({
                "a": "l",
                "n": handle,
                "i": self.sequence_number.clone().unwrap_or_default(),
            }))
            .await
            .map_err(|e| {
                crate::providers::keep_timeout(e, |e| {
                    ProviderError::ServerError(format!("MEGA `l` (export) command failed: {e}"))
                })
            })?;

        if public_handle.is_empty() {
            return Err(ProviderError::ServerError(
                "MEGA `l` command returned empty public handle".into(),
            ));
        }

        let export_key = mega_base64_encode(&node.key);
        let link_type = if node.is_file() { "file" } else { "folder" };
        let _ = &options; // acknowledge usage
        Ok(ShareLinkResult {
            url: format!("https://mega.nz/{link_type}/{public_handle}#{export_key}"),
            password: None,
            expires_at: None,
        })
    }

    fn transfer_optimization_hints(&self) -> super::TransferOptimizationHints {
        // Shaped-graph multipart trait (S4-T01).
        //
        // MEGA's upload protocol is "one upload URL, N independent POSTs at
        // strict per-chunk boundaries". Empirical testing (2026-05-26)
        // against `MEGA Dev` shows gfs* rejects uniform 1 MiB PUTs with
        // `-7 EACCESS` — every official client (MEGA SDK C++, megalib,
        // mega-cmd, megajs) sticks to the canonical 128 KiB → 1 MiB ramp
        // and steady-state scheme returned by `compute_chunk_boundaries()`.
        // We honour the trait by accepting the runner's chosen
        // `multipart_part_size` and accumulating bytes into the next
        // canonical chunk inside `upload_part`, flushing to gfs* only when
        // the buffer hits the expected boundary. Parallel fan-out is left
        // at 1 because the buffer state machine is intrinsically serial;
        // moving to N parallel upload URLs is a future iteration.
        super::TransferOptimizationHints {
            supports_multipart: true,
            multipart_threshold: MEGA_MULTIPART_THRESHOLD,
            multipart_part_size: MEGA_MULTIPART_PART_SIZE,
            multipart_max_parallel: MEGA_MULTIPART_MAX_PARALLEL,
            supports_resume_download: true,
            supports_resume_upload: true,
            ..Default::default()
        }
    }

    // Shaped-graph multipart trait wiring (S4-T01).
    //
    // MEGA's upload protocol maps onto the trait as:
    //   1. `begin_multipart_upload` → POST `{"a":"u","s":size,"ssl":2}` for
    //      the per-session upload URL, generate the random file_key (16 B)
    //      and nonce (8 B), resolve `(parent_handle, file_name)` from the
    //      destination path, snapshot the canonical chunk boundaries via
    //      `compute_chunk_boundaries(total)`, and prime the
    //      `MegaMultipartState` buffer machine.
    //   2. `upload_part` → append the part's plaintext bytes to the buffer.
    //      Every time the buffer hits the next ramp boundary the helper
    //      computes the chunk MAC on the plaintext, encrypts the chunk in
    //      place with AES-CTR keyed to that chunk's absolute byte offset,
    //      POSTs `{upload_url}/{offset}` and captures gfs*'s reply (empty
    //      = intermediate, non-empty = cumulative completion token).
    //   3. `complete_multipart_upload` → consume the accumulated chunk MACs
    //      to derive the 8-byte meta MAC, pack `(file_key, nonce, meta_mac)`
    //      into MEGA's 32-byte node key, wrap with the master key, and POST
    //      `{"a":"p", ...}` to create the file node under the parent folder.
    //      Mirrors the legacy `upload()` finalizer byte-for-byte.
    //   4. `abort_multipart_upload` → no wire call (MEGA has no abort verb;
    //      orphan upload URLs lapse via gfs* TTL). Drops in-flight state so
    //      the next `begin_multipart_upload` starts clean.
    //
    // The runner serialises `upload_part` invocations for MEGA
    // (`clone_multipart_worker` returns `None`), so the `multipart_state`
    // field lives on a plain `&mut self` borrow without further locking.
    async fn begin_multipart_upload(
        &mut self,
        remote_path: &str,
        total_size: u64,
        _content_type: Option<&str>,
        _local_source_path: Option<&str>,
    ) -> Result<MultipartHandle, ProviderError> {
        if total_size == 0 {
            return Err(ProviderError::TransferFailed(
                "MEGA does not accept empty files via multipart upload".to_string(),
            ));
        }

        self.ensure_nodes_loaded().await?;
        // master_key is needed at commit time, fail fast if the session
        // dropped before we even opened the upload URL.
        if self.master_key.is_none() {
            return Err(ProviderError::NotConnected);
        }
        let (parent_handle, file_name) = self.resolve_parent_and_name(remote_path)?;

        // Random per-session content key and CTR nonce.
        let file_key: [u8; 16] = rand::random();
        let nonce: [u8; 8] = rand::random();

        // Request the upload URL from MEGA. `s` is the full file size so the
        // storage node can pre-allocate and signal the cumulative completion
        // token on the PUT that lands the final byte.
        let upload_resp: RequestUploadUrlResponseWire = self
            .command_with_retry(json!({ "a": "u", "s": total_size, "ssl": 2 }))
            .await?;

        let part_size = MEGA_MULTIPART_PART_SIZE;
        let total_chunks = u32::try_from(total_size.div_ceil(part_size)).map_err(|_| {
            ProviderError::TransferFailed(format!(
                "MEGA multipart: total_chunks overflow for {total_size} B / {part_size} B"
            ))
        })?;

        let ramp_chunks = compute_chunk_boundaries(total_size);
        let mac_capacity = ramp_chunks.len();

        let meta = MegaMultipartMeta {
            upload_url: upload_resp.p,
            file_key,
            nonce,
            parent_handle,
            file_name,
            total: total_size,
            part_size,
            total_chunks,
        };

        self.multipart_state = Some(MegaMultipartState {
            ramp_chunks,
            chunk_idx: 0,
            chunk_buffer: Vec::new(),
            chunk_macs: Vec::with_capacity(mac_capacity),
            upload_handle: None,
        });

        Ok(MultipartHandle {
            upload_id: meta.encode(),
            remote_path: remote_path.to_string(),
        })
    }

    async fn upload_part(
        &mut self,
        handle: &MultipartHandle,
        part_number: u32,
        data: Vec<u8>,
    ) -> Result<UploadedPart, ProviderError> {
        if part_number == 0 {
            return Err(ProviderError::Other(
                "MEGA upload_part requires 1-based part_number".to_string(),
            ));
        }
        if data.is_empty() {
            return Err(ProviderError::Other(
                "MEGA upload_part received empty data".to_string(),
            ));
        }
        let meta = MegaMultipartMeta::decode(&handle.upload_id)?;
        let zero_index = u64::from(part_number - 1);
        if zero_index >= u64::from(meta.total_chunks) {
            return Err(ProviderError::Other(format!(
                "MEGA part {part_number} exceeds declared total_chunks {}",
                meta.total_chunks
            )));
        }
        let part_offset = zero_index * meta.part_size;
        let part_end = part_offset
            .checked_add(data.len() as u64)
            .ok_or_else(|| ProviderError::Other("MEGA part offset overflow".to_string()))?;
        if part_end > meta.total {
            return Err(ProviderError::Other(format!(
                "MEGA part {part_number} exceeds declared total: offset {part_offset} + len {} > total {}",
                data.len(),
                meta.total
            )));
        }

        // file_key and nonce are `Copy`; pulling them onto the stack avoids
        // re-borrowing `meta` after we hand a mutable borrow to the state
        // machine below.
        let file_key = meta.file_key;
        let nonce = meta.nonce;
        let upload_url = meta.upload_url.clone();

        let mut cursor = 0usize;
        while cursor < data.len() {
            // Look up the size of the next canonical MEGA chunk we are
            // filling. The state borrow is released before we kick off the
            // async POST so the borrow checker stays happy across awaits.
            let (chunk_offset, target_size, need) = {
                let state = self.multipart_state.as_ref().ok_or_else(|| {
                    ProviderError::Other(
                        "MEGA upload_part called without an active multipart session".to_string(),
                    )
                })?;
                if state.chunk_idx >= state.ramp_chunks.len() {
                    return Err(ProviderError::Other(
                        "MEGA upload_part: bytes received beyond declared file size".to_string(),
                    ));
                }
                let (off, sz) = state.ramp_chunks[state.chunk_idx];
                let need = sz - state.chunk_buffer.len();
                (off, sz, need)
            };

            let take = (data.len() - cursor).min(need);
            self.multipart_state
                .as_mut()
                .expect("state checked above")
                .chunk_buffer
                .extend_from_slice(&data[cursor..cursor + take]);
            cursor += take;

            let buffer_full = self
                .multipart_state
                .as_ref()
                .expect("state checked above")
                .chunk_buffer
                .len()
                == target_size;
            if !buffer_full {
                continue;
            }

            // Detach the completed plaintext buffer from the state so the
            // POST body owns it (Vec<u8> for reqwest), then advance the
            // chunk pointer. The MAC is computed on plaintext before the
            // in-place AES-CTR pass.
            let plaintext = {
                let state = self.multipart_state.as_mut().expect("state checked above");
                std::mem::take(&mut state.chunk_buffer)
            };
            let mac = chunk_mac(&plaintext, &file_key, &nonce)?;
            {
                let state = self.multipart_state.as_mut().expect("state checked above");
                state.chunk_macs.push(mac);
                state.chunk_idx += 1;
            }

            let mut ciphertext = plaintext;
            aes_ctr_apply_inplace(&mut ciphertext, &file_key, &nonce, chunk_offset);

            let url = format!("{upload_url}/{chunk_offset}");
            let resp = self
                .api_client
                .client
                .post(&url)
                .body(ciphertext)
                .send()
                .await
                .map_err(map_reqwest_error)?;

            let status = resp.status();
            let body = resp.text().await.map_http_err(|e| {
                ProviderError::TransferFailed(format!("MEGA upload chunk failed: {e}"))
            })?;

            if !status.is_success() {
                return Err(ProviderError::TransferFailed(format!(
                    "MEGA upload chunk failed ({status}): {body}"
                )));
            }
            if body.starts_with('-') {
                return Err(ProviderError::TransferFailed(format!(
                    "MEGA upload chunk rejected: {body}"
                )));
            }
            if !body.is_empty() {
                // gfs* returns the cumulative completion token on the PUT
                // that lands the last byte of the file. Capture it for the
                // commit step.
                let state = self.multipart_state.as_mut().expect("state checked above");
                state.upload_handle = Some(body);
            }
        }

        Ok(UploadedPart {
            part_number,
            // MEGA does not surface a per-part ETag; the cumulative upload
            // handle is captured separately and consumed in
            // complete_multipart_upload.
            etag: String::new(),
        })
    }

    async fn complete_multipart_upload(
        &mut self,
        handle: MultipartHandle,
        parts: Vec<UploadedPart>,
    ) -> Result<(), ProviderError> {
        let meta = MegaMultipartMeta::decode(&handle.upload_id)?;
        if parts.len() != meta.total_chunks as usize {
            return Err(ProviderError::TransferFailed(format!(
                "MEGA complete: expected {} runner parts, got {}",
                meta.total_chunks,
                parts.len()
            )));
        }
        let state = self.multipart_state.take().ok_or_else(|| {
            ProviderError::TransferFailed(
                "MEGA complete: no active multipart session in provider state".to_string(),
            )
        })?;
        if !state.chunk_buffer.is_empty() {
            return Err(ProviderError::TransferFailed(format!(
                "MEGA complete: partial canonical chunk pending ({} B in buffer); runner stopped feeding data before the last MEGA chunk closed",
                state.chunk_buffer.len()
            )));
        }
        if state.chunk_idx != state.ramp_chunks.len() {
            return Err(ProviderError::TransferFailed(format!(
                "MEGA complete: flushed {} of {} canonical chunks",
                state.chunk_idx,
                state.ramp_chunks.len()
            )));
        }
        let upload_handle = state.upload_handle.ok_or_else(|| {
            ProviderError::TransferFailed(
                "MEGA complete: gfs* never returned the cumulative upload handle; storage node likely rejected the final PUT".to_string(),
            )
        })?;
        let master_key = self.master_key.ok_or(ProviderError::NotConnected)?;

        let file_meta_mac = meta_mac(&state.chunk_macs, &meta.file_key)?;
        let packed_key = pack_node_key(&meta.file_key, &meta.nonce, &file_meta_mac)?;
        let encrypted_key = aes_ecb_encrypt_multi(&packed_key, &master_key)?;
        let key_b64 = mega_base64_encode(&encrypted_key);
        let encrypted_attrs = encrypt_node_attrs(&meta.file_name, &packed_key)?;
        let attrs_b64 = mega_base64_encode(&encrypted_attrs);

        let _resp: PutNodesResponseWire = self
            .command_with_retry(json!({
                "a": "p",
                "t": meta.parent_handle,
                "n": [{
                    "h": upload_handle,
                    "t": 0,
                    "a": attrs_b64,
                    "k": key_b64,
                }],
            }))
            .await?;

        self.invalidate_nodes();
        Ok(())
    }

    async fn abort_multipart_upload(
        &mut self,
        _handle: MultipartHandle,
    ) -> Result<(), ProviderError> {
        // MEGA has no explicit abort verb. Orphan upload URLs lapse via
        // gfs* TTL, so the only thing we need to do is drop any in-flight
        // state so the next `begin_multipart_upload` starts clean.
        self.multipart_state = None;
        Ok(())
    }
}

/// MEGA-specific methods (trash management, share links).
impl MegaNativeProvider {
    /// Move a file or directory to the MEGA rubbish bin (soft delete via `m` command).
    pub async fn move_to_trash(&mut self, path: &str) -> Result<(), ProviderError> {
        self.ensure_nodes_loaded().await?;
        let handle = self.resolve_path(path)?;
        let trash = self
            .trash_handle
            .clone()
            .ok_or_else(|| ProviderError::NotFound("Trash handle not found".into()))?;

        let _: Value = self
            .command_with_retry(json!({
                "a": "m",
                "n": handle,
                "t": trash,
            }))
            .await?;

        self.invalidate_nodes();
        Ok(())
    }

    /// List items in the MEGA rubbish bin.
    pub async fn list_trash(&mut self) -> Result<Vec<RemoteEntry>, ProviderError> {
        self.ensure_nodes_loaded().await?;
        let trash = self
            .trash_handle
            .clone()
            .ok_or_else(|| ProviderError::NotFound("Trash handle not found".into()))?;

        let child_handles = self.children.get(&trash).cloned().unwrap_or_default();
        let mut entries = Vec::new();
        for ch in &child_handles {
            if let Some(node) = self.nodes.get(ch) {
                if !node.name.is_empty() {
                    entries.push(self.node_to_remote_entry(node));
                }
            }
        }
        Ok(entries)
    }

    /// Restore an item from rubbish bin to a destination path.
    pub async fn restore_from_trash(
        &mut self,
        filename: &str,
        dest: &str,
    ) -> Result<(), ProviderError> {
        self.ensure_nodes_loaded().await?;
        let trash = self
            .trash_handle
            .clone()
            .ok_or_else(|| ProviderError::NotFound("Trash handle not found".into()))?;

        // Find the node in trash by name
        let child_handles = self.children.get(&trash).cloned().unwrap_or_default();
        let node_handle = child_handles
            .iter()
            .find(|h| {
                self.nodes
                    .get(*h)
                    .map(|n| n.name == filename)
                    .unwrap_or(false)
            })
            .cloned()
            .ok_or_else(|| ProviderError::NotFound(format!("{filename} not found in trash")))?;

        let dest_handle = self.resolve_path(dest)?;

        let _: Value = self
            .command_with_retry(json!({
                "a": "m",
                "n": node_handle,
                "t": dest_handle,
            }))
            .await?;

        self.invalidate_nodes();
        Ok(())
    }

    /// Permanently delete an item from the rubbish bin.
    pub async fn permanent_delete_from_trash(
        &mut self,
        filename: &str,
    ) -> Result<(), ProviderError> {
        self.ensure_nodes_loaded().await?;
        let trash = self
            .trash_handle
            .clone()
            .ok_or_else(|| ProviderError::NotFound("Trash handle not found".into()))?;

        let child_handles = self.children.get(&trash).cloned().unwrap_or_default();
        let node_handle = child_handles
            .iter()
            .find(|h| {
                self.nodes
                    .get(*h)
                    .map(|n| n.name == filename)
                    .unwrap_or(false)
            })
            .cloned()
            .ok_or_else(|| ProviderError::NotFound(format!("{filename} not found in trash")))?;

        // MEGA delete command returns 0 on success. In batch operations,
        // subsequent deletes may hit rate limits or return unexpected responses.
        // Treat parse errors as success since the node may already be deleted.
        match self
            .command_with_retry::<Value>(json!({
                "a": "d",
                "n": node_handle,
            }))
            .await
        {
            Ok(_) => {}
            Err(ProviderError::ParseError(_)) => {
                tracing::warn!(
                    "[MEGA Native] Delete response parse error for '{}': node likely deleted",
                    filename
                );
            }
            Err(e) => return Err(e),
        }

        self.invalidate_nodes();
        Ok(())
    }
}

impl MegaNativeProvider {
    /// Download with optional progress callback.
    async fn download_to_bytes_with_progress(
        &mut self,
        remote_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<Vec<u8>, ProviderError> {
        self.ensure_nodes_loaded().await?;

        let handle = self.resolve_path(remote_path)?;
        let node = self
            .nodes
            .get(&handle)
            .cloned()
            .ok_or_else(|| ProviderError::NotFound(remote_path.to_string()))?;

        if !node.is_file() {
            return Err(ProviderError::InvalidPath(format!(
                "{remote_path} is not a file"
            )));
        }

        if node.key.len() != 32 {
            return Err(ProviderError::ParseError(format!(
                "File node key invalid length: {} (need 32)",
                node.key.len()
            )));
        }

        let packed_key: [u8; 32] = node.key[..32]
            .try_into()
            .map_err(|_| ProviderError::ParseError("key conversion failed".into()))?;
        let (file_key, nonce, expected_meta_mac) = unpack_node_key_with_mac(&packed_key)?;
        let (legacy_file_key, legacy_nonce, legacy_expected_meta_mac) =
            unpack_node_key_legacy(&packed_key)?;

        // Get download URL
        let dl_resp: GetDownloadUrlResponseWire = self
            .command_with_retry(json!({ "a": "g", "g": 1, "n": handle }))
            .await?;

        let total_size = dl_resp.s;

        // Download encrypted data with progress
        let response = self
            .api_client
            .client
            .get(&dl_resp.g)
            .send()
            .await
            .map_err(map_reqwest_error)?;

        if !response.status().is_success() {
            return Err(ProviderError::TransferFailed(format!(
                "Download HTTP {}",
                response.status()
            )));
        }

        let mut encrypted = Vec::with_capacity(total_size as usize);
        let mut stream = Box::pin(crate::transfer_dag::throttle::throttle_stream(
            response.bytes_stream(),
            crate::transfer_dag::governor::TransferDirection::Download,
        ));
        use futures_util::StreamExt;

        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result.map_http_err(|e| {
                ProviderError::TransferFailed(format!("Download stream error: {e}"))
            })?;
            encrypted.extend_from_slice(&chunk);
            if let Some(ref cb) = on_progress {
                cb(encrypted.len() as u64, total_size);
            }
        }

        let actual_size = node.size as usize;
        let chunks = compute_chunk_boundaries(node.size);

        let decrypted = aes_ctr_decrypt(&encrypted, &file_key, &nonce, 0)?;
        let plain = if decrypted.len() >= actual_size {
            decrypted[..actual_size].to_vec()
        } else {
            decrypted
        };
        let chunk_macs: Result<Vec<_>, _> = chunks
            .iter()
            .map(|(offset, size)| {
                let start = *offset as usize;
                let end = std::cmp::min(start + *size, plain.len());
                chunk_mac(&plain[start..end], &file_key, &nonce)
            })
            .collect();
        if Self::verify_chunk_macs(&chunk_macs?, &file_key, &expected_meta_mac, false)? {
            return Ok(plain);
        }

        tracing::warn!(
            "[MEGA Native] canonical MAC verification failed for '{}'; trying legacy AeroFTP key layout",
            remote_path
        );

        let legacy_decrypted = aes_ctr_decrypt(&encrypted, &legacy_file_key, &legacy_nonce, 0)?;
        let legacy_plain = if legacy_decrypted.len() >= actual_size {
            legacy_decrypted[..actual_size].to_vec()
        } else {
            legacy_decrypted
        };
        let legacy_chunk_macs: Result<Vec<_>, _> = chunks
            .iter()
            .map(|(offset, size)| {
                let start = *offset as usize;
                let end = std::cmp::min(start + *size, legacy_plain.len());
                chunk_mac(&legacy_plain[start..end], &legacy_file_key, &legacy_nonce)
            })
            .collect();
        if Self::verify_chunk_macs(
            &legacy_chunk_macs?,
            &legacy_file_key,
            &legacy_expected_meta_mac,
            true,
        )? {
            return Ok(legacy_plain);
        }

        Err(ProviderError::TransferFailed(
            "MEGA decryption failed: file MAC did not match canonical or legacy AeroFTP key layout"
                .to_string(),
        ))
    }
}

// ─── Path utilities ───────────────────────────────────────────────────────

fn normalize_path(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for component in path.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    if parts.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", parts.join("/"))
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn probe_contract_mega_preserves_missing_root_state() {
        use crate::providers::StorageProvider;
        let (mut p, _) = provider_with_tree(vec![0; 32], false).await;
        assert!(!p.exists("/missing").await.unwrap());
        p.root_handle = None;
        assert!(matches!(
            p.exists("/target").await,
            Err(ProviderError::NotConnected)
        ));
    }

    use super::{decode_command_response, map_mega_error_code, normalize_path};
    use crate::providers::ProviderError;
    use serde::Deserialize;
    use serde_json::json;

    #[derive(Debug, Deserialize)]
    struct SampleResponse {
        ok: i32,
    }

    #[test]
    fn decode_command_response_maps_negative_codes() {
        let err = decode_command_response::<SampleResponse>(json!(-9)).unwrap_err();
        assert!(matches!(err, ProviderError::NotFound(_)));
    }

    #[test]
    fn decode_command_response_deserializes_objects() {
        let response = decode_command_response::<SampleResponse>(json!({ "ok": 1 })).unwrap();
        assert_eq!(response.ok, 1);
    }

    #[test]
    fn mega_error_code_mapping_keeps_auth_specific_errors() {
        let err = map_mega_error_code(-14);
        assert!(matches!(err, ProviderError::AuthenticationFailed(_)));
    }

    const SDK_TOKEN: &str = "wFqIT_wY3tYKcrm5zqwaUoWym3ZCz32cCsrJOgYBgihtpaWUhGyWJ--EY-zfwI-i";

    /// The MEGA SDK `validateHashcash`, written out again so that no test
    /// checks the solver against its own helpers.
    fn mega_accepts(token: &[u8; 48], easiness: u8, nonce_b64: &str) -> bool {
        use base64::Engine as _;
        use sha2::{Digest, Sha256};

        let mut message = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(nonce_b64)
            .unwrap();
        assert_eq!(message.len(), 4, "a nonce is four bytes: {nonce_b64}");
        for _ in 0..262_144 {
            message.extend_from_slice(token);
        }
        let digest = Sha256::digest(&message);
        let threshold = ((((easiness & 63) as u32) << 1) + 1) << (((easiness >> 6) as u32) * 7 + 3);
        u32::from_be_bytes(digest[..4].try_into().unwrap()) <= threshold
    }

    /// A challenge at the highest easiness (about one nonce in two
    /// answers it), so a test that needs several proofs stays cheap.
    fn easy_challenge(fill: u8) -> (String, [u8; 48]) {
        use base64::Engine as _;
        let token = [fill; 48];
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(token);
        (format!("1:255:1731410499:{encoded}"), token)
    }

    /// The nonce of an `X-Hashcash` answer, after checking that it answers `token`.
    fn proof_nonce<'a>(proof: &'a str, token: &[u8; 48]) -> &'a str {
        use base64::Engine as _;
        let prefix = format!(
            "1:{}:",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(token)
        );
        proof
            .strip_prefix(prefix.as_str())
            .unwrap_or_else(|| panic!("{proof} does not answer the challenge's token"))
    }

    type SeenProofs = std::sync::Arc<std::sync::Mutex<Vec<Option<String>>>>;

    fn capture_logs() -> (
        tracing::subscriber::DefaultGuard,
        std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    ) {
        #[derive(Clone)]
        struct Sink(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
        impl std::io::Write for Sink {
            fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(data);
                Ok(data.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let buffer = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = Sink(buffer.clone());
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(move || sink.clone())
            .finish();
        (tracing::subscriber::set_default(subscriber), buffer)
    }

    /// A MEGA API double. Request `n` gets `replies[n]`, the last reply
    /// repeating: a status, an optional `X-Hashcash` challenge and a body. It
    /// records the `X-Hashcash` header of every request it receives.
    async fn hashcash_double(
        replies: Vec<(u16, Option<String>, &'static str)>,
    ) -> (
        super::MegaApiClient,
        SeenProofs,
        tokio::task::JoinHandle<()>,
    ) {
        use axum::response::IntoResponse;

        let seen: SeenProofs = Default::default();
        let recorded = seen.clone();
        let replies = std::sync::Arc::new(replies);
        let app = axum::Router::new().fallback(axum::routing::post(
            move |headers: axum::http::HeaderMap| {
                let recorded = recorded.clone();
                let replies = replies.clone();
                async move {
                    let index = {
                        let mut seen = recorded.lock().unwrap();
                        seen.push(
                            headers
                                .get("x-hashcash")
                                .and_then(|v| v.to_str().ok())
                                .map(String::from),
                        );
                        seen.len() - 1
                    };
                    let (status, challenge, body) = &replies[index.min(replies.len() - 1)];
                    let mut response =
                        (axum::http::StatusCode::from_u16(*status).unwrap(), *body).into_response();
                    if let Some(challenge) = challenge {
                        response
                            .headers_mut()
                            .insert("x-hashcash", challenge.parse().unwrap());
                    }
                    response
                }
            },
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        let mut client = super::MegaApiClient::new(None);
        client.base_url_override = Some(format!("http://{addr}/cs"));
        (client, seen, server)
    }

    #[test]
    fn hashcash_threshold_follows_the_mega_sdk_formula() {
        assert_eq!(super::hashcash_threshold(0), 8);
        assert_eq!(super::hashcash_threshold(100), 74_752);
        assert_eq!(super::hashcash_threshold(192), 1 << 24);
        assert_eq!(super::hashcash_threshold(255), 0x7F00_0000);
    }

    #[test]
    fn hashcash_single_worker_finds_the_first_big_endian_proof() {
        let challenge =
            super::parse_hashcash_challenge(&format!("1:192:1731410499:{SDK_TOKEN}")).unwrap();
        assert_eq!(challenge.easiness, 192);
        // 0x00000127 is the first big-endian nonce that answers this token
        // (computed outside this code). The MEGA SDK test uses the same token
        // but only validates its proof, since its workers race.
        let outcome = super::solve_hashcash_with(&challenge, None, std::time::Duration::MAX, 1);
        assert_eq!(outcome, super::HashcashOutcome::Solved("AAABJw".into()));
        assert!(mega_accepts(&challenge.token, 192, "AAABJw"));
        assert!(super::parse_hashcash_challenge("2:192:1731410499:invalid").is_none());
    }

    #[test]
    fn hashcash_parallel_workers_return_proofs_mega_accepts() {
        let challenge =
            super::parse_hashcash_challenge(&format!("1:200:1731410499:{SDK_TOKEN}")).unwrap();
        for workers in [2, 3, 8] {
            match super::solve_hashcash_with(&challenge, None, std::time::Duration::MAX, workers) {
                super::HashcashOutcome::Solved(nonce) => assert!(
                    mega_accepts(&challenge.token, 200, &nonce),
                    "{workers} workers returned {nonce}, which MEGA would refuse"
                ),
                other => panic!("{workers} workers: {other:?}"),
            }
        }
    }

    #[test]
    fn hashcash_worker_start_failure_is_not_reported_as_no_proof() {
        let (header, _) = easy_challenge(0);
        let challenge = super::parse_hashcash_challenge(&header).unwrap();
        super::HASHCASH_FAIL_SPAWN.with(|slot| slot.set(Some(1)));
        let outcome = super::solve_hashcash_with(&challenge, None, std::time::Duration::MAX, 2);
        super::HASHCASH_FAIL_SPAWN.with(|slot| slot.set(None));
        assert_eq!(outcome, super::HashcashOutcome::WorkerStartFailed);
    }

    #[test]
    fn hashcash_solver_stops_at_its_budget_while_the_challenge_is_still_valid() {
        use base64::Engine as _;

        assert!(super::HASHCASH_SOLVE_BUDGET * 2 <= super::HASHCASH_SERVER_TTL);
        let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([0u8; 48]);
        let challenge =
            super::parse_hashcash_challenge(&format!("1:0:1731410499:{token}")).unwrap();
        let started = std::time::Instant::now();
        let outcome =
            super::solve_hashcash_with(&challenge, None, std::time::Duration::from_millis(300), 2);
        assert_eq!(outcome, super::HashcashOutcome::OutOfBudget);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "a 300 ms budget ran for {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn hashcash_parser_rejects_invalid_base64_and_wrong_token_lengths() {
        use base64::Engine as _;

        for token in [
            "!".repeat(64),
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([0u8; 47]),
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([0u8; 49]),
        ] {
            assert!(
                super::parse_hashcash_challenge(&format!("1:192:1731410499:{token}")).is_none(),
                "accepted malformed token {token}"
            );
        }
    }

    #[test]
    fn hashcash_solver_honours_cancellation_before_the_first_nonce() {
        use std::sync::atomic::AtomicBool;

        let challenge =
            super::parse_hashcash_challenge(&format!("1:192:1731410499:{SDK_TOKEN}")).unwrap();
        let cancelled = AtomicBool::new(true);
        let outcome = super::solve_hashcash_with(
            &challenge,
            Some(&cancelled),
            super::HASHCASH_SOLVE_BUDGET,
            4,
        );
        assert_eq!(outcome, super::HashcashOutcome::Cancelled);
    }

    #[tokio::test]
    async fn hashcash_cancellation_and_worker_panic_have_specific_error_kinds() {
        use std::sync::{atomic::AtomicBool, Arc};

        let cancelled = Arc::new(AtomicBool::new(false));
        let mut worker = tokio::task::spawn_blocking(|| super::HashcashOutcome::Cancelled);
        let err = super::wait_for_hashcash_worker(cancelled.clone(), &mut worker)
            .await
            .unwrap_err();
        assert!(matches!(err, ProviderError::Cancelled));

        let mut worker = tokio::task::spawn_blocking(|| -> super::HashcashOutcome {
            panic!("intentional Hashcash worker panic")
        });
        let err = super::wait_for_hashcash_worker(cancelled, &mut worker)
            .await
            .unwrap_err();
        assert!(matches!(err, ProviderError::Other(_)), "got {err:?}");
        assert!(!err.is_recoverable());
    }

    #[tokio::test]
    async fn command_solves_a_mega_hashcash_challenge_and_retries_the_same_command() {
        use axum::{
            extract::State,
            http::{HeaderMap, StatusCode},
            response::IntoResponse,
        };
        use base64::Engine as _;
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };

        let challenge = format!("1:192:1731410499:{SDK_TOKEN}");
        let attempts = Arc::new(AtomicUsize::new(0));
        let proof_headers = Arc::new(std::sync::Mutex::new(Vec::new()));
        type TestState = (Arc<AtomicUsize>, Arc<std::sync::Mutex<Vec<String>>>, String);
        let app = axum::Router::new()
            .fallback(axum::routing::post(
                |State((attempts, proof_headers, challenge)): State<TestState>,
                 headers: HeaderMap,
                 axum::Json(body): axum::Json<serde_json::Value>| async move {
                    assert_eq!(body, json!([{ "a": "us" }]));
                    if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                        return (
                            StatusCode::PAYMENT_REQUIRED,
                            [("x-hashcash", challenge)],
                            "",
                        )
                            .into_response();
                    }
                    proof_headers.lock().unwrap().push(
                        headers
                            .get("x-hashcash")
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or_default()
                            .to_string(),
                    );
                    axum::Json(json!([{ "ok": 1 }])).into_response()
                },
            ))
            .with_state((attempts.clone(), proof_headers.clone(), challenge));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await });

        let mut client = super::MegaApiClient::new(None);
        client.base_url_override = Some(format!("http://{addr}/cs"));
        client.hashcash_budgets = Some(vec![std::time::Duration::MAX]);
        let response: SampleResponse = client.command(json!({ "a": "us" })).await.unwrap();
        server.abort();

        assert_eq!(response.ok, 1);
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        let proofs = proof_headers.lock().unwrap().clone();
        assert_eq!(proofs.len(), 1);
        let token: [u8; 48] = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(SDK_TOKEN)
            .unwrap()
            .try_into()
            .unwrap();
        // Workers race, so any accepted nonce is right, as in the MEGA SDK test.
        assert!(mega_accepts(&token, 192, proof_nonce(&proofs[0], &token)));
    }

    #[tokio::test]
    async fn command_answers_each_fresh_challenge_until_mega_accepts() {
        let (first, first_token) = easy_challenge(0);
        let (second, second_token) = easy_challenge(1);
        let (mut client, seen, server) = hashcash_double(vec![
            (402, Some(first), ""),
            (402, Some(second), ""),
            (200, None, r#"[{"ok":1}]"#),
        ])
        .await;
        client.hashcash_budgets = Some(vec![std::time::Duration::MAX]);

        let response: SampleResponse = client.command(json!({ "a": "ug" })).await.unwrap();
        server.abort();

        assert_eq!(response.ok, 1);
        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 3);
        assert_eq!(seen[0], None);
        let first_proof = seen[1].as_deref().expect("first challenge unanswered");
        let second_proof = seen[2].as_deref().expect("second challenge unanswered");
        assert!(mega_accepts(
            &first_token,
            255,
            proof_nonce(first_proof, &first_token)
        ));
        assert!(mega_accepts(
            &second_token,
            255,
            proof_nonce(second_proof, &second_token)
        ));
    }

    #[tokio::test]
    async fn two_successful_proofs_do_not_extend_the_third_challenge_budget() {
        use base64::Engine as _;

        let (first, _) = easy_challenge(0);
        let (second, _) = easy_challenge(1);
        let hard_token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([0u8; 48]);
        let hard = format!("1:0:1731410499:{hard_token}");
        let (mut client, seen, server) = hashcash_double(vec![
            (402, Some(first), ""),
            (402, Some(second), ""),
            (402, Some(hard), ""),
        ])
        .await;
        client.hashcash_budgets = Some(vec![
            std::time::Duration::MAX,
            std::time::Duration::MAX,
            std::time::Duration::ZERO,
        ]);

        let err = client
            .command::<serde_json::Value>(json!({ "a": "ug" }))
            .await
            .unwrap_err();
        server.abort();
        assert!(matches!(err, ProviderError::Timeout), "got {err:?}");
        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 3);
        assert!(seen[0].is_none());
        assert!(seen[1].is_some());
        assert!(seen[2].is_some());
    }

    #[tokio::test]
    async fn explicit_mega_error_with_a_challenge_is_classified_without_proof() {
        let (challenge, _) = easy_challenge(0);
        let (client, seen, server) = hashcash_double(vec![(402, Some(challenge), "-15")]).await;
        let err = client
            .command::<serde_json::Value>(json!({ "a": "ug" }))
            .await
            .unwrap_err();
        server.abort();
        assert!(matches!(err, ProviderError::AuthenticationFailed(_)));
        assert_eq!(seen.lock().unwrap().as_slice(), &[None]);
    }

    #[tokio::test]
    async fn complete_bounded_402_body_controls_mega_code_classification() {
        let padded = Box::leak(format!("[-15{}]", " ".repeat(250)).into_boxed_str());
        let oversized = Box::leak(
            format!("[-15{}]", " ".repeat(super::MEGA_ERROR_BODY_LIMIT)).into_boxed_str(),
        );
        let oversized_valid_prefix = Box::leak(
            format!("[-15]{}", " ".repeat(super::MEGA_ERROR_BODY_LIMIT)).into_boxed_str(),
        );
        for (body, authentication_failure) in [
            (&*padded, true),
            ("[-1", false),
            (&*oversized, false),
            (&*oversized_valid_prefix, false),
        ] {
            let (client, seen, server) = hashcash_double(vec![(402, None, body)]).await;
            let err = client
                .command::<serde_json::Value>(json!({ "a": "ug" }))
                .await
                .unwrap_err();
            server.abort();
            assert_eq!(
                matches!(err, ProviderError::AuthenticationFailed(_)),
                authentication_failure,
                "body length {}: {err:?}",
                body.len()
            );
            assert_eq!(seen.lock().unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn returned_402_server_error_does_not_quote_the_response_body() {
        let secret = "synthetic-402-secret-that-must-not-escape";
        let (client, _, server) = hashcash_double(vec![(402, None, secret)]).await;
        let err = client
            .command::<serde_json::Value>(json!({ "a": "ug" }))
            .await
            .unwrap_err();
        server.abort();
        assert!(matches!(err, ProviderError::ServerError(_)));
        assert!(!err.to_string().contains(secret), "returned error: {err}");
    }

    #[tokio::test]
    async fn error_body_read_has_a_total_deadline_even_when_chunks_keep_arriving() {
        let app = axum::Router::new().fallback(axum::routing::post(|| async {
            let stream = futures_util::stream::unfold((), |_| async {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                Some((Ok::<&'static str, std::io::Error>("x"), ()))
            });
            axum::response::Response::builder()
                .status(axum::http::StatusCode::PAYMENT_REQUIRED)
                .body(axum::body::Body::from_stream(stream))
                .unwrap()
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await });
        let mut client = super::MegaApiClient::new(None);
        client.base_url_override = Some(format!("http://{addr}/cs"));
        client.error_body_read_timeout = Some(std::time::Duration::from_millis(30));

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            client.command::<serde_json::Value>(json!({ "a": "ug" })),
        )
        .await;
        server.abort();
        let err = result
            .expect("error body read ignored its total deadline")
            .unwrap_err();
        assert!(matches!(err, ProviderError::Timeout), "got {err:?}");
    }

    #[test]
    fn error_logs_and_returns_keep_402_and_500_body_text_secret() {
        if !crate::provider_commands::isolated_account_fixture::in_isolated_child(
            "providers::mega_native::tests::error_logs_and_returns_keep_402_and_500_body_text_secret",
        ) {
            return;
        }
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let secret = "synthetic-http-body-secret-for-logs";
                for status in [402, 500] {
                    let (_guard, buffer) = capture_logs();
                    let (client, _, server) = hashcash_double(vec![(status, None, secret)]).await;
                    let err = client
                        .command::<serde_json::Value>(json!({ "a": "ug" }))
                        .await
                        .unwrap_err();
                    server.abort();
                    let logged = String::from_utf8_lossy(&buffer.lock().unwrap()).into_owned();
                    assert!(!logged.contains(secret), "HTTP {status} log: {logged}");
                    assert!(
                        !err.to_string().contains(secret),
                        "HTTP {status} error: {err}"
                    );
                    assert!(logged.contains(&format!("HTTP {status}")), "{logged}");
                    assert!(logged.contains("cmd=ug"), "{logged}");
                    assert!(err.to_string().contains(&status.to_string()), "{err}");
                }
            });
    }

    #[tokio::test]
    async fn command_stops_answering_challenges_after_the_limit() {
        let (challenge, _) = easy_challenge(0);
        let (mut client, seen, server) = hashcash_double(vec![(402, Some(challenge), "")]).await;
        client.hashcash_budgets = Some(vec![std::time::Duration::MAX]);

        let err = client
            .command::<serde_json::Value>(json!({ "a": "ug" }))
            .await
            .unwrap_err();
        server.abort();

        assert!(matches!(err, ProviderError::ServerError(_)), "got {err:?}");
        assert_eq!(seen.lock().unwrap().len(), 4);
    }

    #[tokio::test]
    async fn command_asks_again_after_two_timeouts_and_uses_the_extended_budget() {
        let (first, _) = easy_challenge(0);
        let (second, _) = easy_challenge(1);
        let (last, last_token) = easy_challenge(2);
        let (mut client, seen, server) = hashcash_double(vec![
            (402, Some(first), ""),
            (402, Some(second), ""),
            (402, Some(last), ""),
            (200, None, r#"[{"ok":1}]"#),
        ])
        .await;
        // No time at all before two forced timeouts. Only the extended budget
        // lets the third challenge solve; successful proofs never select it.
        client.hashcash_budgets = Some(vec![
            std::time::Duration::ZERO,
            std::time::Duration::ZERO,
            std::time::Duration::ZERO,
        ]);
        assert_eq!(
            super::HASHCASH_EXTENDED_BUDGET,
            std::time::Duration::from_secs(7 * 24 * 60 * 60)
        );

        let response: SampleResponse = client.command(json!({ "a": "ug" })).await.unwrap();
        server.abort();

        assert_eq!(response.ok, 1);
        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 4);
        assert_eq!(
            &seen[..3],
            &[None, None, None],
            "an unsolved challenge was answered, or a stale proof was resent"
        );
        let proof = seen[3].as_deref().expect("last challenge unanswered");
        assert!(mega_accepts(
            &last_token,
            255,
            proof_nonce(proof, &last_token)
        ));
    }

    #[tokio::test]
    async fn command_402_without_a_usable_challenge_is_not_a_session_error() {
        for challenge in [
            None,
            Some(format!("1:192:1731410499:{}", "!".repeat(64))),
            Some(format!("2:192:1731410499:{SDK_TOKEN}")),
        ] {
            let (client, seen, server) =
                hashcash_double(vec![(402, challenge.clone(), "payment required")]).await;
            let err = client
                .command::<serde_json::Value>(json!({ "a": "ug" }))
                .await
                .unwrap_err();
            server.abort();
            assert!(
                matches!(err, ProviderError::ServerError(_)),
                "{challenge:?}: got {err:?}"
            );
            assert_eq!(seen.lock().unwrap().len(), 1, "{challenge:?} was retried");
        }
    }

    #[tokio::test]
    async fn command_402_incomplete_request_code_is_not_a_session_verdict() {
        let (client, seen, server) = hashcash_double(vec![(402, None, "-13")]).await;
        let err = client
            .command::<serde_json::Value>(json!({ "a": "ug" }))
            .await
            .unwrap_err();
        server.abort();
        assert!(matches!(err, ProviderError::ServerError(_)), "got {err:?}");
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn command_402_with_a_mega_session_code_stays_an_authentication_failure() {
        let (client, _, server) = hashcash_double(vec![(402, None, "-15")]).await;
        let err = client
            .command::<serde_json::Value>(json!({ "a": "ug" }))
            .await
            .unwrap_err();
        server.abort();
        assert!(
            matches!(err, ProviderError::AuthenticationFailed(_)),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn a_402_body_does_not_decide_whether_the_command_is_retried() {
        // (body, requests): only a MEGA rate-limit code is transient; prose
        // that merely mentions one is not.
        for (body, requests) in [("rate limit reached, retry (EAGAIN)", 1), ("-4", 3)] {
            let (client, seen, server) = hashcash_double(vec![(402, None, body)]).await;
            let mut provider = super::MegaNativeProvider::new(super::MegaConfig {
                email: "transient@example.invalid".into(),
                password: secrecy::SecretString::new(String::new().into()),
                two_factor_code: None,
                totp_secret: None,
                save_session: false,
                logout_on_disconnect: None,
                connection_mode: crate::providers::types::MegaConnectionMode::Native,
            });
            provider.api_client = client;
            let err = provider.get_user_info().await.unwrap_err();
            server.abort();
            assert!(
                matches!(err, ProviderError::ServerError(_)),
                "{body}: got {err:?}"
            );
            assert_eq!(seen.lock().unwrap().len(), requests, "{body}");
        }
    }

    async fn assert_dropping_hashcash_future_stops_worker(worker_already_started: bool) {
        use base64::Engine as _;
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        };
        use std::time::Duration;

        let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([0u8; 48]);
        let challenge =
            super::parse_hashcash_challenge(&format!("1:0:1731410499:{token}")).unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let started = Arc::new(AtomicBool::new(false));
        let mut worker = {
            let cancelled = cancelled.clone();
            let started = started.clone();
            tokio::task::spawn_blocking(move || {
                started.store(true, Ordering::Release);
                super::solve_hashcash_with(
                    &challenge,
                    Some(&cancelled),
                    super::HASHCASH_SOLVE_BUDGET,
                    1,
                )
            })
        };
        if worker_already_started {
            while !started.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        }
        {
            let future = super::wait_for_hashcash_worker(cancelled.clone(), &mut worker);
            tokio::pin!(future);
            let wait_for_start = async {
                while !started.load(Ordering::Acquire) {
                    tokio::task::yield_now().await;
                }
            };
            // `biased` polls the future first, so its drop guard exists even
            // when the worker had started before the first poll.
            tokio::select! {
                biased;
                result = &mut future => panic!("solver finished before cancellation: {result:?}"),
                () = wait_for_start => {}
            }
        }

        let cancelled_on_drop = cancelled.load(Ordering::Relaxed);
        cancelled.store(true, Ordering::Relaxed);
        let result = tokio::time::timeout(Duration::from_secs(2), &mut worker)
            .await
            .expect("blocking worker did not stop after its future was dropped")
            .expect("blocking worker panicked");
        assert!(
            cancelled_on_drop,
            "dropping the future did not set the flag"
        );
        assert_eq!(result, super::HashcashOutcome::Cancelled);
    }

    #[tokio::test]
    async fn dropping_hashcash_future_stops_and_joins_the_blocking_worker() {
        assert_dropping_hashcash_future_stops_worker(false).await;
    }

    #[tokio::test]
    async fn dropping_hashcash_future_after_worker_already_started_always_sets_the_guard() {
        for _ in 0..32 {
            assert_dropping_hashcash_future_stops_worker(true).await;
        }
    }

    #[test]
    fn unresolved_hashcash_402_preserves_saved_session_during_resume() {
        if !crate::provider_commands::isolated_account_fixture::in_isolated_child(
            "providers::mega_native::tests::unresolved_hashcash_402_preserves_saved_session_during_resume",
        ) {
            return;
        }
        use crate::credential_store::CredentialStore;
        use crate::user_partitions as partitions;

        let _env = crate::test_env::lock();
        let _partition = partitions::test_lock();
        let temporary = tempfile::TempDir::new().expect("temporary MEGA resume vault");
        std::env::set_var("XDG_CONFIG_HOME", temporary.path());
        std::env::remove_var("AEROFTP_USER_PASSPHRASE");
        CredentialStore::clear_cache();
        CredentialStore::bootstrap_master_password("mega-resume-test-pass")
            .expect("bootstrap MEGA resume vault");
        CredentialStore::unlock_with_master("mega-resume-test-pass", None)
            .expect("unlock MEGA resume vault");
        let store = CredentialStore::from_cache().expect("cached MEGA resume vault");
        partitions::init_or_migrate_cli(&store).expect("initialize MEGA resume partitions");

        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let app = axum::Router::new().fallback(axum::routing::post(|| async {
                    (
                        axum::http::StatusCode::PAYMENT_REQUIRED,
                        "proof of work required",
                    )
                }));
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let addr = listener.local_addr().unwrap();
                let server = tokio::spawn(async move { axum::serve(listener, app).await });
                let mut provider = super::MegaNativeProvider::new(super::MegaConfig {
                    email: "resume@example.invalid".into(),
                    password: secrecy::SecretString::new(String::new().into()),
                    two_factor_code: None,
                    totp_secret: None,
                    save_session: true,
                    logout_on_disconnect: None,
                    connection_mode: crate::providers::types::MegaConnectionMode::Native,
                });
                provider.api_client.base_url_override = Some(format!("http://{addr}/cs"));
                let persisted = super::MegaPersistedSession {
                    session_id: "saved-session".into(),
                    master_key_b64: super::mega_base64_encode(&[7u8; 16]),
                    account_version: 2,
                    user_handle: Some("saved-user".into()),
                    sequence_number: Some("saved-sequence".into()),
                    stored_at_unix_ms: 1,
                };
                let serialized = serde_json::to_string(&persisted).unwrap();
                let key = provider.session_vault_key();
                store.store(&key, &serialized).unwrap();

                let err = provider.try_resume_session().await.unwrap_err();
                server.abort();
                assert!(matches!(err, ProviderError::ServerError(_)), "got {err:?}");
                assert_eq!(store.get(&key).unwrap(), serialized);

                use crate::providers::StorageProvider;
                for code in ["-13", "-14"] {
                    let commands = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
                    let received = commands.clone();
                    let app = axum::Router::new().fallback(axum::routing::post(
                        move |axum::Json(body): axum::Json<serde_json::Value>| {
                            let received = received.clone();
                            async move {
                                let command = body[0]["a"].as_str().unwrap().to_string();
                                received.lock().unwrap().push(command.clone());
                                let response = if command == "ug" { code } else { "-15" };
                                (axum::http::StatusCode::INTERNAL_SERVER_ERROR, response)
                            }
                        },
                    ));
                    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                    let addr = listener.local_addr().unwrap();
                    let server = tokio::spawn(async move { axum::serve(listener, app).await });
                    let mut provider = super::MegaNativeProvider::new(super::MegaConfig {
                        email: format!("non402-{code}@example.invalid"),
                        password: secrecy::SecretString::new("synthetic-password".into()),
                        two_factor_code: None,
                        totp_secret: None,
                        save_session: true,
                        logout_on_disconnect: None,
                        connection_mode: crate::providers::types::MegaConnectionMode::Native,
                    });
                    provider.api_client.base_url_override = Some(format!("http://{addr}/cs"));
                    let key = provider.session_vault_key();
                    store.store(&key, &serialized).unwrap();

                    let err = provider.connect().await.unwrap_err();
                    server.abort();
                    assert!(
                        matches!(err, ProviderError::NetworkError(_)),
                        "{code}: {err:?}"
                    );
                    assert_eq!(store.get(&key).unwrap(), serialized, "{code}");
                    assert_eq!(
                        commands.lock().unwrap().as_slice(),
                        &["ug".to_string()],
                        "{code}"
                    );
                }

                use std::sync::atomic::{AtomicUsize, Ordering};
                let requests = std::sync::Arc::new(AtomicUsize::new(0));
                let received = requests.clone();
                let rejected = axum::Router::new().fallback(axum::routing::post(move || {
                    let received = received.clone();
                    async move {
                        received.fetch_add(1, Ordering::SeqCst);
                        (axum::http::StatusCode::PAYMENT_REQUIRED, "-15")
                    }
                }));
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let addr = listener.local_addr().unwrap();
                let rejected_server =
                    tokio::spawn(async move { axum::serve(listener, rejected).await });
                let mut provider = super::MegaNativeProvider::new(super::MegaConfig {
                    email: "rejected@example.invalid".into(),
                    password: secrecy::SecretString::new(String::new().into()),
                    two_factor_code: None,
                    totp_secret: None,
                    save_session: true,
                    logout_on_disconnect: None,
                    connection_mode: crate::providers::types::MegaConnectionMode::Native,
                });
                provider.api_client.base_url_override = Some(format!("http://{addr}/cs"));
                let rejected_key = provider.session_vault_key();
                store.store(&rejected_key, &serialized).unwrap();
                let err = provider.connect().await.unwrap_err();
                rejected_server.abort();
                assert!(matches!(err, ProviderError::AuthenticationFailed(_)));
                assert!(err.to_string().contains("password is required"));
                assert!(matches!(
                    store.get(&rejected_key),
                    Err(crate::credential_store::CredentialError::NotFound(_))
                ));
                assert_eq!(requests.load(Ordering::SeqCst), 1);

                let commands = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
                let received = commands.clone();
                let fresh_login = axum::Router::new().fallback(axum::routing::post(
                    move |axum::Json(body): axum::Json<serde_json::Value>| {
                        let received = received.clone();
                        async move {
                            let command = body[0]["a"].as_str().unwrap().to_string();
                            received.lock().unwrap().push(command.clone());
                            if command == "ug" {
                                (axum::http::StatusCode::PAYMENT_REQUIRED, "-15")
                            } else {
                                (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "-15")
                            }
                        }
                    },
                ));
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let addr = listener.local_addr().unwrap();
                let fresh_login_server =
                    tokio::spawn(async move { axum::serve(listener, fresh_login).await });
                let mut provider = super::MegaNativeProvider::new(super::MegaConfig {
                    email: "fresh-login@example.invalid".into(),
                    password: secrecy::SecretString::new("synthetic-password".into()),
                    two_factor_code: None,
                    totp_secret: None,
                    save_session: true,
                    logout_on_disconnect: None,
                    connection_mode: crate::providers::types::MegaConnectionMode::Native,
                });
                provider.api_client.base_url_override = Some(format!("http://{addr}/cs"));
                let fresh_login_key = provider.session_vault_key();
                store.store(&fresh_login_key, &serialized).unwrap();
                let err = provider.connect().await.unwrap_err();
                fresh_login_server.abort();
                assert!(matches!(err, ProviderError::AuthenticationFailed(_)));
                assert!(matches!(
                    store.get(&fresh_login_key),
                    Err(crate::credential_store::CredentialError::NotFound(_))
                ));
                assert_eq!(
                    commands.lock().unwrap().as_slice(),
                    &["ug".to_string(), "us0".to_string()]
                );
            });
        CredentialStore::clear_cache();
    }

    #[tokio::test]
    async fn connect_without_password_fails_auth_before_sending_a_login_command() {
        use crate::providers::StorageProvider;
        use std::sync::{atomic::AtomicUsize, atomic::Ordering, Arc};

        let requests = Arc::new(AtomicUsize::new(0));
        let received = Arc::clone(&requests);
        let app = axum::Router::new().fallback(axum::routing::post(move || {
            received.fetch_add(1, Ordering::SeqCst);
            async { axum::Json(json!([{ "v": 1 }])) }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await });

        let mut provider = super::MegaNativeProvider::new(super::MegaConfig {
            email: "test@example.invalid".into(),
            password: secrecy::SecretString::new(String::new().into()),
            two_factor_code: None,
            totp_secret: None,
            save_session: false,
            logout_on_disconnect: None,
            connection_mode: crate::providers::types::MegaConnectionMode::Native,
        });
        provider.api_client.base_url_override = Some(format!("http://{addr}/cs"));

        let result = provider.connect().await;
        server.abort();
        assert!(
            matches!(
                result,
                Err(ProviderError::AuthenticationFailed(_) | ProviderError::InvalidConfig(_))
            ),
            "empty credentials must fail as auth/config, got {result:?}"
        );
        assert_eq!(requests.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn verify_tsid_rejects_invalid_lengths() {
        let err = super::MegaNativeProvider::verify_tsid("AA", &[0u8; 16]).unwrap_err();
        assert!(matches!(err, ProviderError::ParseError(_)));
    }

    #[test]
    fn normalize_path_basic() {
        assert_eq!(normalize_path("/"), "/");
        assert_eq!(normalize_path("/a/b/c"), "/a/b/c");
        assert_eq!(normalize_path("/a/../b"), "/b");
        assert_eq!(normalize_path("/a/./b"), "/a/b");
        assert_eq!(normalize_path("/a/b/.."), "/a");
        assert_eq!(normalize_path("//a///b//"), "/a/b");
    }

    /// A provider whose tree holds `/a` (folder), `/a/f.txt` (file, key
    /// `file_key`) and `/b` (folder, holding `f.txt` when `b_holds_f`),
    /// pointed at a MEGA API double that answers every command with `0`.
    /// Returns it and the commands the double received, as `a n t` (the
    /// command, the node, and the target folder of a move).
    async fn provider_with_tree(
        file_key: Vec<u8>,
        b_holds_f: bool,
    ) -> (
        super::MegaNativeProvider,
        std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    ) {
        use std::sync::{Arc, Mutex};
        let commands: Arc<Mutex<Vec<String>>> = Arc::default();
        let seen = Arc::clone(&commands);
        let app = axum::Router::new().fallback(axum::routing::post(
            move |axum::Json(body): axum::Json<serde_json::Value>| {
                let command = &body[0];
                let words: Vec<&str> = ["a", "n", "t"]
                    .iter()
                    .filter_map(|key| command[*key].as_str())
                    .collect();
                seen.lock().unwrap().push(words.join(" "));
                async { axum::Json(json!([0])) }
            },
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.ok() });

        let mut provider = super::MegaNativeProvider::new(super::MegaConfig {
            email: "test@example.com".to_string(),
            password: secrecy::SecretString::new("unused".into()),
            two_factor_code: None,
            totp_secret: None,
            save_session: false,
            logout_on_disconnect: None,
            connection_mode: crate::providers::types::MegaConnectionMode::Native,
        });
        provider.api_client.base_url_override = Some(format!("http://{addr}/cs"));
        provider.root_handle = Some("ROOT".to_string());
        provider.master_key = Some([7u8; 16]);
        provider.nodes_loaded = true;
        let mut add = |handle: &str, parent: &str, node_type: u8, name: &str, key: Vec<u8>| {
            provider.nodes.insert(
                handle.to_string(),
                super::MegaNode {
                    handle: handle.to_string(),
                    parent: parent.to_string(),
                    node_type,
                    name: name.to_string(),
                    size: 0,
                    timestamp: 0,
                    key,
                },
            );
            provider
                .children
                .entry(parent.to_string())
                .or_default()
                .push(handle.to_string());
        };
        add("ROOT", "", 2, "", Vec::new());
        add("A", "ROOT", 1, "a", vec![1u8; 16]);
        add("F", "A", 0, "f.txt", file_key);
        add("B", "ROOT", 1, "b", vec![2u8; 16]);
        if b_holds_f {
            add("G", "B", 0, "f.txt", vec![3u8; 32]);
        }
        (provider, commands)
    }

    #[tokio::test]
    async fn mkdir_refuses_an_existing_folder() {
        use crate::providers::StorageProvider;
        let (mut provider, commands) = provider_with_tree(vec![4u8; 32], false).await;
        let outcome = provider.mkdir("/a").await;
        assert!(
            matches!(outcome, Err(ProviderError::AlreadyExists(_))),
            "{outcome:?}"
        );
        assert!(commands.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn rename_moves_then_renames() {
        use crate::providers::StorageProvider;
        let (mut provider, commands) = provider_with_tree(vec![4u8; 32], false).await;
        provider
            .rename("/a/f.txt", "/b/g.txt")
            .await
            .expect("rename");
        assert_eq!(*commands.lock().unwrap(), ["m F B", "a F"]);
    }

    /// A node without a key cannot get a new name. That used to be skipped
    /// in silence after the move, reporting a rename that never happened.
    #[tokio::test]
    async fn rename_of_a_node_without_a_key_fails_before_moving() {
        use crate::providers::StorageProvider;
        let (mut provider, commands) = provider_with_tree(Vec::new(), false).await;
        let outcome = provider.rename("/a/f.txt", "/b/g.txt").await;
        assert!(
            matches!(outcome, Err(ProviderError::Other(ref m)) if m.contains("no key")),
            "{outcome:?}"
        );
        assert!(commands.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn rename_refuses_an_existing_destination() {
        use crate::providers::StorageProvider;
        let (mut provider, commands) = provider_with_tree(vec![4u8; 32], true).await;
        let outcome = provider.rename("/a/f.txt", "/b/f.txt").await;
        assert!(
            matches!(outcome, Err(ProviderError::AlreadyExists(_))),
            "{outcome:?}"
        );
        assert!(commands.lock().unwrap().is_empty());
    }

    /// The move keeps the old name. With `/b/f.txt` present, moving first
    /// put a second `f.txt` in `/b` until the rename, and left it there if
    /// the rename failed: the rename now goes first, in `/a`.
    #[tokio::test]
    async fn a_move_whose_destination_holds_the_old_name_renames_first() {
        use crate::providers::StorageProvider;
        let (mut provider, commands) = provider_with_tree(vec![4u8; 32], true).await;
        provider
            .rename("/a/f.txt", "/b/g.txt")
            .await
            .expect("rename then move");
        assert_eq!(*commands.lock().unwrap(), ["a F", "m F B"]);
    }

    /// Old name taken at the destination and new name taken at the source:
    /// either order would put two siblings under one name.
    #[tokio::test]
    async fn a_move_that_cannot_avoid_a_shared_name_changes_nothing() {
        use crate::providers::StorageProvider;
        let (mut provider, commands) = provider_with_tree(vec![4u8; 32], true).await;
        provider.nodes.insert(
            "H".to_string(),
            super::MegaNode {
                handle: "H".to_string(),
                parent: "A".to_string(),
                node_type: 0,
                name: "g.txt".to_string(),
                size: 0,
                timestamp: 0,
                key: vec![5u8; 32],
            },
        );
        provider
            .children
            .entry("A".to_string())
            .or_default()
            .push("H".to_string());
        let outcome = provider.rename("/a/f.txt", "/b/g.txt").await;
        assert!(outcome.is_err(), "{outcome:?}");
        assert!(commands.lock().unwrap().is_empty());
    }

    /// `replace` is the verb for "put this over that" (CLI `edit`, MCP
    /// `remote_edit`, the AeroCrypt marker publish), and the trait default
    /// forwards it to `rename`, which refuses an occupied destination. The
    /// item there is renamed aside, the new one moved in, and only then the
    /// old one goes to the rubbish bin.
    #[tokio::test]
    async fn replace_sets_the_old_item_aside_moves_the_new_one_in_then_trashes_it() {
        use crate::providers::StorageProvider;
        let (mut provider, commands) = provider_with_tree(vec![4u8; 32], true).await;
        provider.trash_handle = Some("TRASH".to_string());
        provider
            .replace("/a/f.txt", "/b/f.txt")
            .await
            .expect("replace");
        assert_eq!(*commands.lock().unwrap(), ["a G", "m F B", "m G TRASH"]);
        assert!(provider.replace_sets_aside(), "what the edit opt-in asks");
    }

    /// A replace puts one file in place of another. Onto a folder it set the
    /// whole folder aside and binned it, contents and all, to leave a file
    /// under its name; a folder onto a file did the reverse. Both are refused
    /// before any command.
    #[tokio::test]
    async fn a_replace_across_file_and_folder_is_refused_before_any_command() {
        use crate::providers::StorageProvider;
        let (mut provider, commands) = provider_with_tree(vec![4u8; 32], true).await;
        provider.trash_handle = Some("TRASH".to_string());
        for (from, to) in [("/a/f.txt", "/b"), ("/a", "/b/f.txt")] {
            let outcome = provider.replace(from, to).await;
            assert!(
                matches!(outcome, Err(ProviderError::AlreadyExists(_))),
                "{from} -> {to}: {outcome:?}"
            );
            provider.nodes_loaded = true;
        }
        {
            let seen_commands = commands.lock().unwrap().clone();
            assert!(seen_commands.is_empty(), "{:?}", seen_commands);
        }
    }

    #[test]
    fn not_found_display_carries_the_prefix_once() {
        let config = super::MegaConfig {
            email: "test@example.com".to_string(),
            password: secrecy::SecretString::new("unused".into()),
            two_factor_code: None,
            totp_secret: None,
            save_session: false,
            logout_on_disconnect: None,
            connection_mode: crate::providers::types::MegaConnectionMode::Native,
        };
        let mut provider = super::MegaNativeProvider::new(config);
        provider.root_handle = Some("ROOT".to_string());
        let err = provider.resolve_path("/missing/dir").unwrap_err();
        assert!(matches!(err, ProviderError::NotFound(_)));
        let rendered = err.to_string();
        assert_eq!(
            rendered.matches("Path not found").count(),
            1,
            "NotFound display must carry the prefix once, got: {rendered}"
        );
    }
}
