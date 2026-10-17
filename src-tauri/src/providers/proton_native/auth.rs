//! Browser sign-in through a Proton session fork.
//!
//! Ported from the Proton Drive SDK at commit [`super::SDK_REFERENCE_COMMIT`]
//! (MIT, Copyright (c) 2025-2026 Proton AG, see `LICENSE-PROTON-DRIVE-SDK.md`):
//! `incubating/account/js/src/authWeb.ts` and `Auth.authViaWeb` in
//! `incubating/account/js/src/auth.ts`, the flow Proton's own CLI uses.
//!
//! 1. `GET /auth/v4/sessions/forks` returns a `Selector` and a `UserCode`.
//! 2. AeroFTP generates a random 32-byte key and opens
//!    `https://account.proton.me/desktop/login?app=drive&pv=3#payload=...`,
//!    where the payload carries the user code, that key and the client ID.
//!    The key travels in the URL fragment, which browsers never send to a
//!    server.
//! 3. The user signs in on Proton's own page (password, 2FA, CAPTCHA are all
//!    handled there). AeroFTP polls `GET /auth/v4/sessions/forks/{selector}`;
//!    HTTP 422 means "not yet".
//! 4. The answer carries the new session tokens and a payload encrypted with
//!    the key from step 2 (AES-256-GCM, AAD `fork`) that holds the user key
//!    password.
//!
//! AeroFTP therefore never sees the account password. The client ID is
//! `external-drive`, the one the SDK CLI uses for every build that is not the
//! official `cli-drive` release (`cli/src/config.ts`).

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use std::time::{Duration, Instant};

use aes_gcm::aead::{Aead, Payload};
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use rand::RngCore;
use secrecy::SecretString;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

use super::api::ProtonApi;
use super::error::ProtonError;
use super::session::{ProtonSession, SessionTokens};

/// Client ID the SDK assigns to third-party builds (`cli/src/config.ts`).
pub const THIRD_PARTY_CLIENT_ID: &str = "external-drive";

/// Account host that serves the sign-in page.
pub const DEFAULT_ACCOUNT_HOST: &str = "account.proton.me";

/// `authWeb.ts` timings.
pub const FORK_INITIAL_DELAY: Duration = Duration::from_secs(5);
pub const FORK_POLL_INTERVAL: Duration = Duration::from_secs(5);
pub const FORK_MAX_POLL_TIME: Duration = Duration::from_secs(10 * 60);

const FORK_AAD: &[u8] = b"fork";
const GCM_NONCE_LENGTH: usize = 12;
const GCM_TAG_LENGTH: usize = 16;

/// `accountApi.ts::SessionForkInitResponse`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ForkInit {
    selector: String,
    user_code: String,
}

/// `accountApi.ts::SessionForkStatusResponse`.
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ForkStatus {
    payload: String,
    #[serde(rename = "UID")]
    uid: String,
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
}

/// `authWeb.ts::ForkPayloadJson`.
#[derive(Deserialize)]
struct ForkPayload {
    #[serde(rename = "keyPassword")]
    key_password: Option<String>,
}

/// A browser sign-in in progress: show [`Self::sign_in_url`] to the user (or
/// open it), then call [`wait_for_browser_sign_in`].
pub struct PendingSignIn {
    selector: String,
    sign_in_url: String,
    encryption_key: Zeroizing<[u8; 32]>,
    started: Instant,
}

impl std::fmt::Debug for PendingSignIn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The URL carries the decryption key in its fragment: keep it out of
        // logs.
        f.debug_struct("PendingSignIn").finish_non_exhaustive()
    }
}

impl PendingSignIn {
    /// The page the user signs in on. It contains the one-time decryption key
    /// in its fragment: open it, never log it.
    pub fn sign_in_url(&self) -> &str {
        &self.sign_in_url
    }
}

/// `authWeb.ts::generateSignInUrl`.
fn sign_in_url(account_host: &str, client_id: &str, user_code: &str, key: &[u8; 32]) -> String {
    let base = if account_host.starts_with("http://") || account_host.starts_with("https://") {
        account_host.to_string()
    } else {
        format!("https://{account_host}")
    };
    let payload = format!("0:{user_code}:{}:{client_id}", BASE64.encode(key));
    format!(
        "{}/desktop/login?app=drive&pv=3#payload={}",
        base.trim_end_matches('/'),
        urlencoding::encode(&payload)
    )
}

/// Step 1 and 2: asks Proton for a fork and builds the sign-in URL.
pub async fn begin_browser_sign_in(
    api: &ProtonApi,
    account_host: &str,
) -> Result<PendingSignIn, ProtonError> {
    let fork: ForkInit = api.get_unauthenticated("/auth/v4/sessions/forks").await?;
    let mut key = Zeroizing::new([0u8; 32]);
    rand::rngs::OsRng.fill_bytes(&mut key[..]);
    let url = sign_in_url(account_host, THIRD_PARTY_CLIENT_ID, &fork.user_code, &key);
    Ok(PendingSignIn {
        selector: fork.selector,
        sign_in_url: url,
        encryption_key: key,
        started: Instant::now(),
    })
}

/// Steps 3 and 4: waits until the user completed the sign-in in the browser,
/// installs the new tokens in `api` and returns the session.
pub async fn wait_for_browser_sign_in(
    api: &ProtonApi,
    pending: PendingSignIn,
    cancel: &CancellationToken,
) -> Result<ProtonSession, ProtonError> {
    wait_with_timings(
        api,
        pending,
        cancel,
        FORK_INITIAL_DELAY,
        FORK_POLL_INTERVAL,
        FORK_MAX_POLL_TIME,
    )
    .await
}

async fn wait_with_timings(
    api: &ProtonApi,
    pending: PendingSignIn,
    cancel: &CancellationToken,
    initial_delay: Duration,
    poll_interval: Duration,
    max_poll_time: Duration,
) -> Result<ProtonSession, ProtonError> {
    sleep_or_cancel(initial_delay, cancel).await?;
    let path = format!(
        "/auth/v4/sessions/forks/{}",
        urlencoding::encode(&pending.selector)
    );
    loop {
        if pending.started.elapsed() > max_poll_time {
            return Err(ProtonError::SignInTimedOut);
        }
        let status: ForkStatus = match api.get_unauthenticated(&path).await {
            Ok(status) => status,
            // The API answers 422 while the user has not finished signing in.
            Err(ProtonError::Api { status: 422, .. }) => {
                sleep_or_cancel(poll_interval, cancel).await?;
                continue;
            }
            Err(other) => return Err(other),
        };

        let key_password = decrypt_key_password(&pending.encryption_key, &status.payload)?;
        let session = ProtonSession {
            tokens: SessionTokens {
                uid: status.uid,
                access_token: SecretString::from(status.access_token),
                refresh_token: status.refresh_token.map(SecretString::from),
            },
            key_password,
        };
        api.set_tokens(Some(session.tokens.clone())).await;
        return Ok(session);
    }
}

/// Revokes the session on Proton (`DELETE /auth/v4`, "Revoke a token" in the
/// SDK's published `api-auth-types.ts`) and drops it from `api`. The local
/// copy is dropped even when the revoke fails, so a dead session never lingers
/// in AeroFTP.
pub async fn sign_out(api: &ProtonApi) -> Result<(), ProtonError> {
    let result = api
        .delete::<serde_json::Value>("/auth/v4")
        .await
        .map(|_| ());
    api.set_tokens(None).await;
    match result {
        Err(ProtonError::NotSignedIn)
        | Err(ProtonError::SessionExpired)
        | Err(ProtonError::SessionReplaced) => Ok(()),
        other => other,
    }
}

async fn sleep_or_cancel(
    duration: Duration,
    cancel: &CancellationToken,
) -> Result<(), ProtonError> {
    tokio::select! {
        () = cancel.cancelled() => Err(ProtonError::SignInCancelled),
        () = tokio::time::sleep(duration) => Ok(()),
    }
}

/// `authWeb.ts::parseUserKeyPassword`: `base64(nonce || ciphertext || tag)`,
/// AES-256-GCM with AAD `fork`, plaintext JSON `{"keyPassword": "..."}`.
fn decrypt_key_password(key: &[u8; 32], encoded: &str) -> Result<SecretString, ProtonError> {
    let blob = BASE64
        .decode(encoded.trim())
        .map_err(|_| ProtonError::Crypto("fork payload is not base64".into()))?;
    if blob.len() < GCM_NONCE_LENGTH + GCM_TAG_LENGTH {
        return Err(ProtonError::Crypto(
            "invalid fork payload blob length".into(),
        ));
    }
    let (nonce, sealed) = blob.split_at(GCM_NONCE_LENGTH);
    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|_| ProtonError::Crypto("invalid fork key length".into()))?;
    // aes-gcm expects ciphertext followed by the tag, which is the layout of
    // the remaining bytes.
    let plaintext = Zeroizing::new(
        cipher
            .decrypt(
                Nonce::from_slice(nonce),
                Payload {
                    msg: sealed,
                    aad: FORK_AAD,
                },
            )
            .map_err(|_| ProtonError::Crypto("failed to decrypt the fork payload".into()))?,
    );
    let payload: ForkPayload = serde_json::from_slice(&plaintext)
        .map_err(|_| ProtonError::Crypto("failed to deserialize the fork payload".into()))?;
    payload
        .key_password
        .map(SecretString::from)
        .ok_or_else(|| ProtonError::Crypto("fork payload without keyPassword".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;

    fn seal(key: &[u8; 32], nonce: &[u8; 12], plaintext: &[u8]) -> String {
        let cipher = Aes256Gcm::new_from_slice(key).unwrap();
        let sealed = cipher
            .encrypt(
                Nonce::from_slice(nonce),
                Payload {
                    msg: plaintext,
                    aad: FORK_AAD,
                },
            )
            .unwrap();
        let mut blob = nonce.to_vec();
        blob.extend_from_slice(&sealed);
        BASE64.encode(blob)
    }

    #[test]
    fn sign_in_url_has_the_sdk_shape() {
        let key = [7u8; 32];
        let url = sign_in_url("account.proton.me", THIRD_PARTY_CLIENT_ID, "ABCD1234", &key);
        let (base, fragment) = url.split_once('#').expect("fragment");
        assert_eq!(
            base,
            "https://account.proton.me/desktop/login?app=drive&pv=3"
        );
        let payload = fragment.strip_prefix("payload=").expect("payload=");
        let decoded = urlencoding::decode(payload).expect("percent-decoding");
        assert_eq!(
            decoded,
            format!("0:ABCD1234:{}:external-drive", BASE64.encode(key))
        );
        assert!(
            !payload.contains(':') && !payload.contains('+') && !payload.contains('/'),
            "the payload must be percent-encoded like encodeURIComponent: {payload}"
        );
    }

    #[test]
    fn fork_payload_decrypts_to_the_key_password() {
        let key = [3u8; 32];
        let blob = seal(
            &key,
            &[9u8; 12],
            br#"{"type":"default","keyPassword":"k3y-p4ss"}"#,
        );
        let password = decrypt_key_password(&key, &blob).expect("decrypts");
        assert_eq!(password.expose_secret(), "k3y-p4ss");
    }

    #[test]
    fn a_payload_sealed_without_the_fork_aad_is_rejected() {
        let key = [3u8; 32];
        let cipher = Aes256Gcm::new_from_slice(&key).unwrap();
        let sealed = cipher
            .encrypt(
                Nonce::from_slice(&[1u8; 12]),
                &br#"{"keyPassword":"x"}"#[..],
            )
            .unwrap();
        let mut blob = vec![1u8; 12];
        blob.extend_from_slice(&sealed);
        assert!(decrypt_key_password(&key, &BASE64.encode(blob)).is_err());
    }

    #[test]
    fn a_payload_for_another_key_is_rejected() {
        let blob = seal(&[3u8; 32], &[9u8; 12], br#"{"keyPassword":"x"}"#);
        assert!(decrypt_key_password(&[4u8; 32], &blob).is_err());
    }

    #[test]
    fn a_short_blob_is_rejected_before_decryption() {
        assert!(decrypt_key_password(&[0u8; 32], &BASE64.encode([0u8; 20])).is_err());
    }

    #[test]
    fn debug_never_prints_the_url_with_its_key() {
        let pending = PendingSignIn {
            selector: "sel".into(),
            sign_in_url: "https://account.proton.me/desktop/login#payload=SECRET".into(),
            encryption_key: Zeroizing::new([0u8; 32]),
            started: Instant::now(),
        };
        assert!(!format!("{pending:?}").contains("SECRET"));
    }

    /// Full fork against a local double: two 422 answers, then the session.
    #[tokio::test]
    async fn fork_polls_through_422_then_returns_the_session() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Mutex};

        let key_slot: Arc<Mutex<Option<[u8; 32]>>> = Arc::default();
        let polls = Arc::new(AtomicUsize::new(0));
        let slot_for_status = Arc::clone(&key_slot);
        let polls_for_status = Arc::clone(&polls);
        let app = axum::Router::new()
            .route(
                "/auth/v4/sessions/forks",
                axum::routing::get(|| async {
                    r#"{"Code":1000,"Selector":"sel-1","UserCode":"CODE42"}"#
                }),
            )
            .route(
                "/auth/v4/sessions/forks/{selector}",
                axum::routing::get(move || {
                    let n = polls_for_status.fetch_add(1, Ordering::SeqCst);
                    let key = *slot_for_status.lock().unwrap();
                    async move {
                        if n < 2 {
                            return (
                                axum::http::StatusCode::UNPROCESSABLE_ENTITY,
                                r#"{"Code":9201,"Error":"Fork not ready"}"#.to_string(),
                            );
                        }
                        let payload = seal(&key.unwrap(), &[5u8; 12], br#"{"keyPassword":"from-fork"}"#);
                        (
                            axum::http::StatusCode::OK,
                            format!(
                                r#"{{"Code":1000,"Payload":"{payload}","UID":"uid-9","AccessToken":"acc-9","RefreshToken":"ref-9"}}"#
                            ),
                        )
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.ok() });

        let api = ProtonApi::with_base_url(
            format!("http://{addr}"),
            "external-drive-aeroftp@1.2.3-alpha".into(),
        );
        let pending = begin_browser_sign_in(&api, "account.example.test")
            .await
            .expect("fork");
        assert!(pending
            .sign_in_url()
            .starts_with("https://account.example.test/desktop/login"));
        *key_slot.lock().unwrap() = Some(*pending.encryption_key);

        let cancel = CancellationToken::new();
        let session = wait_with_timings(
            &api,
            pending,
            &cancel,
            Duration::from_millis(1),
            Duration::from_millis(1),
            Duration::from_secs(5),
        )
        .await
        .expect("session");
        assert_eq!(polls.load(Ordering::SeqCst), 3);
        assert_eq!(session.tokens.uid, "uid-9");
        assert_eq!(session.key_password.expose_secret(), "from-fork");
        assert_eq!(
            api.tokens()
                .await
                .map(|t| t.access_token.expose_secret().to_owned()),
            Some("acc-9".to_owned()),
            "the API client must now carry the new session"
        );
    }

    #[tokio::test]
    async fn cancelling_stops_the_wait() {
        let api = ProtonApi::with_base_url(
            "http://127.0.0.1:9".into(),
            "external-drive-aeroftp@1.2.3-alpha".into(),
        );
        let pending = PendingSignIn {
            selector: "sel".into(),
            sign_in_url: String::new(),
            encryption_key: Zeroizing::new([0u8; 32]),
            started: Instant::now(),
        };
        let cancel = CancellationToken::new();
        cancel.cancel();
        let error = wait_for_browser_sign_in(&api, pending, &cancel)
            .await
            .unwrap_err();
        assert!(matches!(error, ProtonError::SignInCancelled), "{error:?}");
    }
}
