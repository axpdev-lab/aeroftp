//! Tauri commands of the native Proton Drive client: browser sign-in,
//! sign-out and the signed-in state of a profile.
//!
//! Sign-in is the SDK's session fork ([`super::auth`]): AeroFTP opens Proton's
//! own page in the system browser and waits for the fork; the account
//! password never reaches AeroFTP. The session goes straight to the vault
//! ([`super::vault`]), never to the web view: under the profile's key when
//! the profile is saved, else under the unbound key that the profile's first
//! connect after saving takes over.
//!
//! The commands exist only in builds with the `proton-native` feature; the
//! frontend treats "command not found" from [`proton_native_status`] as "this
//! build has no native client".

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use serde::{Deserialize, Serialize};
use tauri::State;
use tokio_util::sync::CancellationToken;

use super::api::ProtonApi;
use super::error::ProtonError;
use super::{account, app_version, auth, vault};
use crate::provider_commands::{ConnectTokenGuard, ConnectionCancelRegistry, CONNECT_CANCELLED};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProtonSignInParams {
    /// The saved profile the session belongs to; absent before the first
    /// save (Quick Connect).
    #[serde(default, alias = "profile_id")]
    pub profile_id: Option<String>,
    #[serde(default, alias = "connect_token")]
    pub connect_token: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProtonSignInResult {
    /// The account's primary address, for the form's account field.
    pub email: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProtonNativeStatus {
    /// Always true: the command exists only in builds with the native client.
    pub available: bool,
    /// A session is saved for the profile (or, without one, an unbound one).
    pub signed_in: bool,
}

fn profile(profile_id: &Option<String>) -> Option<&str> {
    profile_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
}

/// Error string of a sign-in whose page expired (ten minutes): the frontend
/// shows its translated text for it.
pub const SIGN_IN_EXPIRED: &str = "PROTON_SIGN_IN_EXPIRED";

/// What the sign-in reports when it fails: the shared cancel marker, the
/// expiry marker, or the error itself.
fn sign_in_error(error: ProtonError) -> String {
    match error {
        ProtonError::SignInCancelled => CONNECT_CANCELLED.to_string(),
        ProtonError::SignInTimedOut => SIGN_IN_EXPIRED.to_string(),
        other => format!("Proton sign-in failed: {other}"),
    }
}

/// The whole sign-in: fork, browser (through `open_page`), wait, key unlock
/// and save. Returns the primary address. Shared by the GUI command and its
/// tests; `open_page` receives the sign-in page URL, which carries a one-time
/// key in its fragment and must never be logged.
pub async fn sign_in(
    profile_id: Option<&str>,
    cancel: &CancellationToken,
    open_page: impl FnOnce(&str) -> Result<(), String>,
) -> Result<Option<String>, String> {
    let api = ProtonApi::new(app_version::app_version());
    let pending = auth::begin_browser_sign_in(&api, auth::DEFAULT_ACCOUNT_HOST)
        .await
        .map_err(sign_in_error)?;
    open_page(pending.sign_in_url())?;
    let session = auth::wait_for_browser_sign_in(&api, pending, cancel)
        .await
        .map_err(sign_in_error)?;
    // Unlock the keys now, so a session that cannot read the Drive is
    // reported here and never saved.
    let account = account::load_account(&api, &session.key_password)
        .await
        .map_err(sign_in_error)?;
    let email = account.primary_address().ok().map(|a| a.email.clone());
    let tokens = api.tokens().await.unwrap_or(session.tokens.clone());
    let session = super::session::ProtonSession {
        tokens,
        key_password: session.key_password,
    };
    // A session this one replaces is ended on Proton first (unless another
    // saved profile still uses it), so a re-sign-in leaves none behind.
    vault::revoke_saved_session(profile_id).await;
    vault::store_session(profile_id, &session).map_err(|error| error.to_string())?;
    Ok(email)
}

/// Opens Proton's sign-in page in the system browser and waits until the
/// user signed in there (or cancelled through `cancel_connection` with
/// `connect_token`, or the page expired after ten minutes).
#[tauri::command]
pub async fn proton_native_sign_in(
    cancel_registry: State<'_, ConnectionCancelRegistry>,
    params: ProtonSignInParams,
) -> Result<ProtonSignInResult, String> {
    let token = match params.connect_token.as_deref() {
        Some(key) => cancel_registry.register(key),
        None => CancellationToken::new(),
    };
    let _guard = params
        .connect_token
        .as_deref()
        .map(|key| ConnectTokenGuard::new(&cancel_registry, key.to_string()));
    let email = sign_in(profile(&params.profile_id), &token, |url| {
        open::that(url).map_err(|error| {
            format!("Could not open the system browser for the Proton sign-in: {error}")
        })
    })
    .await?;
    log::info!("Proton native: signed in");
    Ok(ProtonSignInResult { email })
}

/// Ends the profile's session on Proton and removes it from the vault.
#[tauri::command]
pub async fn proton_native_sign_out(profile_id: Option<String>) -> Result<(), String> {
    let profile_id = profile(&profile_id);
    vault::revoke_saved_session(profile_id).await;
    vault::delete_session(profile_id).map_err(|error| error.to_string())
}

/// Whether the profile has a saved session. Also the frontend's probe for a
/// build with the native client, so it answers even when the vault cannot be
/// read (locked): then nothing counts as signed in.
#[tauri::command]
pub async fn proton_native_status(
    profile_id: Option<String>,
) -> Result<ProtonNativeStatus, String> {
    let profile_id = profile(&profile_id);
    let signed_in = match vault::load_session(profile_id) {
        Ok(Some(_)) => true,
        Ok(None) if profile_id.is_some() => matches!(vault::load_session(None), Ok(Some(_))),
        Ok(None) => false,
        Err(error) => {
            log::warn!("Proton native: session state unknown: {error}");
            false
        }
    };
    Ok(ProtonNativeStatus {
        available: true,
        signed_in,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blank_profile_id_is_no_profile() {
        assert_eq!(profile(&Some(" srv_1 ".into())), Some("srv_1"));
        assert_eq!(profile(&Some("  ".into())), None);
        assert_eq!(profile(&None), None);
    }

    #[test]
    fn a_cancelled_sign_in_reads_as_a_cancelled_connection() {
        assert_eq!(
            sign_in_error(ProtonError::SignInCancelled),
            CONNECT_CANCELLED
        );
        assert_eq!(sign_in_error(ProtonError::SignInTimedOut), SIGN_IN_EXPIRED);
        assert!(sign_in_error(ProtonError::Network("reset".into())).contains("reset"));
    }

    #[test]
    fn the_params_take_both_spellings() {
        let camel: ProtonSignInParams =
            serde_json::from_str(r#"{"profileId":"a","connectToken":"t"}"#).unwrap();
        let snake: ProtonSignInParams =
            serde_json::from_str(r#"{"profile_id":"a","connect_token":"t"}"#).unwrap();
        assert_eq!(camel.profile_id, snake.profile_id);
        assert_eq!(camel.connect_token, snake.connect_token);
        let empty: ProtonSignInParams = serde_json::from_str("{}").unwrap();
        assert!(empty.profile_id.is_none());
    }
}
