//! Errors of the native Proton Drive client.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use thiserror::Error;

/// Proton API code for a successful response.
pub const CODE_OK: i64 = 1000;
/// Proton API code asking the user to prove they are human (CAPTCHA, email or
/// SMS). The SDK cannot answer it; neither can we, so it surfaces to the user.
pub const CODE_HUMAN_VERIFICATION_REQUIRED: i64 = 9001;
/// `incubating/account/js/src/accountApi.ts`: the address does not exist.
pub const CODE_ADDRESS_MISSING: i64 = 33_102;
/// `incubating/account/js/src/accountApi.ts`: the domain is external.
pub const CODE_DOMAIN_EXTERNAL: i64 = 33_103;
/// `errorCodes.ts`: a node with the same name already exists.
pub const CODE_ALREADY_EXISTS: i64 = 2500;
/// `errorCodes.ts` `NOT_EXISTS`.
pub const CODE_NOT_EXISTS: i64 = 2501;
/// `errorCodes.ts` `NOT_ENOUGH_PERMISSIONS` and `..._TO_GRANT_PERMISSIONS`.
pub const CODES_NOT_ENOUGH_PERMISSIONS: [i64; 2] = [2011, 2026];
/// `errorCodes.ts` `MAX_FILE_SIZE_FOR_FREE_USER`.
pub const CODE_MAX_FILE_SIZE_FOR_FREE_USER: i64 = 200_003;
/// `errorCodes.ts` `INSUFFICIENT_QUOTA`, `INSUFFICIENT_SPACE`,
/// `INSUFFICIENT_VOLUME_QUOTA`, `INSUFFICIENT_DEVICE_QUOTA`.
pub const CODES_NO_SPACE: [i64; 4] = [200_001, 200_002, 200_100, 200_101];

#[derive(Debug, Error)]
pub enum ProtonError {
    /// The request never got an HTTP answer (DNS, TLS, connection, timeout).
    #[error("network error talking to Proton: {0}")]
    Network(String),

    /// Proton answered with an error. `code` is Proton's own error code from
    /// the JSON body (0 when the body had none), `message` its `Error` text.
    #[error("Proton API error {code} (HTTP {status}): {message}")]
    Api {
        status: u16,
        code: i64,
        message: String,
    },

    /// Proton wants a human verification step the user must complete in the
    /// official web page. Never solved automatically.
    #[error("Proton requires a human verification step ({})", methods.join(", "))]
    HumanVerificationRequired { methods: Vec<String> },

    /// A request or a body read ran out of time (after the SDK's retries).
    #[error("Proton did not answer in time")]
    Timeout,

    /// Reading or writing the local file failed (not a Proton error).
    #[error("local file error: {0}")]
    LocalIo(std::io::Error),

    /// Too many consecutive 429 answers: requests stop for a cool-down, as the
    /// SDK does, instead of hammering the service.
    #[error("Proton rate limit reached, retry after {retry_after_secs} s")]
    RateLimited { retry_after_secs: u64 },

    /// The session can no longer be refreshed: the user must sign in again.
    #[error("the Proton session expired, sign in again")]
    SessionExpired,

    /// A new sign-in saved another session for this profile and ended the
    /// one in use: a reconnect continues with the new one.
    #[error("the Proton session was replaced by a new sign-in")]
    SessionReplaced,

    /// A call that needs a session ran without one.
    #[error("not signed in to Proton")]
    NotSignedIn,

    /// The browser sign-in was not completed in time.
    #[error("Proton sign-in timed out")]
    SignInTimedOut,

    /// The browser sign-in was cancelled by the user.
    #[error("Proton sign-in cancelled")]
    SignInCancelled,

    /// A block storage URL answered 404: its token expired and a fresh one
    /// must be requested for that block (`fileDownloader.ts`).
    #[error("Proton storage token expired")]
    StorageTokenExpired,

    /// A downloaded block or manifest failed its integrity check.
    #[error("Proton integrity check failed: {0}")]
    Integrity(String),

    /// A node with the same name exists in the folder (API code 2500).
    /// `draft_client_uid` is set when the conflict is an unfinished upload.
    #[error("an item with this name already exists")]
    AlreadyExists {
        link_id: Option<String>,
        draft_client_uid: Option<String>,
        is_draft: bool,
    },

    /// A name Proton refuses: empty, longer than 255 UTF-16 units, `.`, `..`
    /// or containing `/` (`nodes/validations.ts`).
    #[error("invalid Proton Drive name: {0}")]
    InvalidName(String),

    /// A new revision of this file cannot be written: its content key came
    /// from a v6 (AEAD) key packet, a format `proton-crypto` cannot write
    /// yet, and Proton refuses a revision whose format differs from the
    /// file's.
    #[error(
        "this file uses Proton's newer content format; AeroFTP cannot write a revision of it yet"
    )]
    RevisionUnsupported,

    /// The node, path or volume does not exist (API code 2501).
    #[error("not found on Proton Drive: {0}")]
    NotFound(String),

    /// A response did not have the shape the SDK expects.
    #[error("unexpected Proton response: {0}")]
    Decode(String),

    /// Decryption, signature verification or key unlock failed.
    #[error("Proton cryptography error: {0}")]
    Crypto(String),
}

impl ProtonError {
    /// True when the error means the address is unknown to Proton, which the
    /// SDK treats as "no keys" rather than a failure.
    pub fn is_address_not_found(&self) -> bool {
        matches!(
            self,
            ProtonError::Api { code, .. }
                if *code == CODE_ADDRESS_MISSING || *code == CODE_DOMAIN_EXTERNAL
        )
    }
}
