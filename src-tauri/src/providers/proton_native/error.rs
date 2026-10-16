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

    /// Too many consecutive 429 answers: requests stop for a cool-down, as the
    /// SDK does, instead of hammering the service.
    #[error("Proton rate limit reached, retry after {retry_after_secs} s")]
    RateLimited { retry_after_secs: u64 },

    /// The session can no longer be refreshed: the user must sign in again.
    #[error("the Proton session expired, sign in again")]
    SessionExpired,

    /// A call that needs a session ran without one.
    #[error("not signed in to Proton")]
    NotSignedIn,

    /// The browser sign-in was not completed in time.
    #[error("Proton sign-in timed out")]
    SignInTimedOut,

    /// The browser sign-in was cancelled by the user.
    #[error("Proton sign-in cancelled")]
    SignInCancelled,

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
