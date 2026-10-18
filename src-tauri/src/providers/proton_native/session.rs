//! Proton session state.
//!
//! Follows `incubating/account/js/src/sessionCredentials.ts` of the Proton
//! Drive SDK (commit [`super::SDK_REFERENCE_COMMIT`]; MIT, Copyright (c)
//! 2025-2026 Proton AG, see `LICENSE-PROTON-DRIVE-SDK.md`): a session is the
//! API `UID`, an access token, an optional refresh token and the user key
//! password handed over by the browser fork. None of it is the account
//! password, which AeroFTP never sees.
//!
//! Every secret is a [`SecretString`]: it is zeroized on drop and never shows
//! up in `Debug` output or logs. [`StoredSession`] is the only shape that
//! exposes the values, and only for writing to AeroFTP's encrypted vault.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};

/// The tokens that authenticate API calls.
#[derive(Clone)]
pub struct SessionTokens {
    pub uid: String,
    pub access_token: SecretString,
    pub refresh_token: Option<SecretString>,
}

impl std::fmt::Debug for SessionTokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionTokens")
            .field("uid", &self.uid)
            .field("access_token", &"[REDACTED]")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
}

/// A signed-in session: tokens plus the password that unlocks the user keys.
#[derive(Clone)]
pub struct ProtonSession {
    pub tokens: SessionTokens,
    /// The user key password from the fork payload (`keyPassword`). It
    /// decrypts the user's private keys; it is not the login password.
    pub key_password: SecretString,
}

impl std::fmt::Debug for ProtonSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProtonSession")
            .field("tokens", &self.tokens)
            .field("key_password", &"[REDACTED]")
            .finish()
    }
}

/// Serializable form of a [`ProtonSession`], for the encrypted vault only.
/// Never log it and never write it anywhere else.
#[derive(Serialize, Deserialize)]
pub struct StoredSession {
    pub uid: String,
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    pub key_password: String,
}

impl From<&ProtonSession> for StoredSession {
    fn from(session: &ProtonSession) -> Self {
        Self {
            uid: session.tokens.uid.clone(),
            access_token: session.tokens.access_token.expose_secret().to_owned(),
            refresh_token: session
                .tokens
                .refresh_token
                .as_ref()
                .map(|token| token.expose_secret().to_owned()),
            key_password: session.key_password.expose_secret().to_owned(),
        }
    }
}

impl From<StoredSession> for ProtonSession {
    fn from(stored: StoredSession) -> Self {
        Self {
            tokens: SessionTokens {
                uid: stored.uid,
                access_token: SecretString::from(stored.access_token),
                refresh_token: stored.refresh_token.map(SecretString::from),
            },
            key_password: SecretString::from(stored.key_password),
        }
    }
}

impl std::fmt::Debug for StoredSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoredSession")
            .field("uid", &self.uid)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ProtonSession {
        ProtonSession {
            tokens: SessionTokens {
                uid: "uid-1".into(),
                access_token: SecretString::from("access-secret".to_string()),
                refresh_token: Some(SecretString::from("refresh-secret".to_string())),
            },
            key_password: SecretString::from("key-secret".to_string()),
        }
    }

    #[test]
    fn debug_output_never_contains_a_secret() {
        let session = sample();
        let stored = StoredSession::from(&session);
        for rendered in [format!("{session:?}"), format!("{stored:?}")] {
            for secret in ["access-secret", "refresh-secret", "key-secret"] {
                assert!(
                    !rendered.contains(secret),
                    "Debug output leaks `{secret}`: {rendered}"
                );
            }
        }
    }

    #[test]
    fn stored_session_round_trips_every_field() {
        let stored = StoredSession::from(&sample());
        let json = serde_json::to_string(&stored).expect("serialize");
        let back =
            ProtonSession::from(serde_json::from_str::<StoredSession>(&json).expect("deserialize"));
        assert_eq!(back.tokens.uid, "uid-1");
        assert_eq!(back.tokens.access_token.expose_secret(), "access-secret");
        assert_eq!(
            back.tokens
                .refresh_token
                .as_ref()
                .map(|t| t.expose_secret().to_owned()),
            Some("refresh-secret".to_owned())
        );
        assert_eq!(back.key_password.expose_secret(), "key-secret");
    }
}
