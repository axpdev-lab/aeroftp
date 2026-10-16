//! Proton Drive, native Rust client (Tier 0, development only).
//!
//! This is the in-process successor to [`super::proton`], which drives
//! Proton's official `proton-drive` CLI. It is compiled only with the
//! `proton-native` feature, which is off by default and never shipped: the
//! CLI wrapper stays the production path until this module passes the gates
//! recorded in the roadmap appendix (`APPENDIX-PROTON-DRIVE/TODO.md`).
//!
//! Where the code comes from: the logic is ported from Proton's own Proton
//! Drive SDK (<https://github.com/ProtonDriveApps/sdk>, MIT, Copyright (c)
//! 2025-2026 Proton AG, see `LICENSE-PROTON-DRIVE-SDK.md` next to this file)
//! and every port names the SDK file and commit it follows. OpenPGP and key
//! handling use Proton's official `proton-crypto-account` crate with its pure
//! Rust backend. Nothing here is reverse engineered from network traffic or
//! from a third-party client.
//!
//! Rules this module must keep, from the SDK README "Usage Guidelines":
//! - only official endpoints (`drive-api.proton.me`, `account.proton.me`);
//! - an honest `x-pm-appversion` ([`app_version`]), and never
//!   `x-pm-drive-sdk-version`, which only the real SDK may send;
//! - retries, backoff and rate limiting no more aggressive than the SDK's
//!   ([`api`]);
//! - event-based sync, no polling and no frequent recursive traversals;
//! - no Proton branding, and a third-party disclosure wherever account
//!   details are asked for ([`THIRD_PARTY_DISCLOSURE`]).
//!
//! Sign-in is the SDK's browser session fork ([`auth`]): AeroFTP never sees
//! the account password.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

pub mod account;
pub mod api;
pub mod app_version;
pub mod auth;
pub mod error;
pub mod session;

pub use error::ProtonError;

/// Text the SDK README requires wherever a third-party app asks for Proton
/// account details. The UI shows it, translated, next to the sign-in button.
pub const THIRD_PARTY_DISCLOSURE: &str =
    "This is a third-party application not officially supported by Proton.";

/// The SDK commit the ports in this module follow. Bump it only together with
/// a re-read of the ported files at the new commit.
pub const SDK_REFERENCE_COMMIT: &str = "8ffae00";

#[cfg(test)]
mod live_tests {
    use super::*;
    use tokio_util::sync::CancellationToken;

    /// Live, on a real Proton account: browser sign-in through the session
    /// fork, unlock of the user and primary address keys, then revoke.
    ///
    /// Needs a person at the browser, on any device. Run:
    /// `cargo test --lib --features proton-native proton_native::live_tests -- --ignored --nocapture`
    /// and open the printed `PROTON_SIGN_IN_URL`. Optional
    /// `AEROFTP_PROTON_LIVE_HOLD_SECS=<n>` keeps the session open n seconds.
    #[tokio::test]
    #[ignore = "live: needs a person to sign in to Proton in the browser"]
    async fn live_browser_sign_in_unlocks_the_primary_address() {
        let api = api::ProtonApi::new(app_version::app_version());
        let pending = auth::begin_browser_sign_in(&api, auth::DEFAULT_ACCOUNT_HOST)
            .await
            .expect("session fork");
        // The fork is not tied to this machine: the page can be opened in a
        // browser on any device. The fragment holds a one-time key for this
        // fork only, useless without the selector that never leaves this
        // process, and the fork expires after ten minutes.
        eprintln!("{THIRD_PARTY_DISCLOSURE}");
        eprintln!("PROTON_SIGN_IN_URL {}", pending.sign_in_url());
        let cancel = CancellationToken::new();
        let session = auth::wait_for_browser_sign_in(&api, pending, &cancel)
            .await
            .expect("browser sign-in");
        let account = account::load_account(&api, &session.key_password)
            .await
            .expect("key unlock");
        let primary = account
            .primary_address()
            .expect("primary address keys unlocked");
        assert!(!primary.keys.is_empty());
        eprintln!(
            "unlocked {} user key(s) and {} address(es); primary address has {} key(s)",
            account.user_keys().len(),
            account.addresses().len(),
            primary.keys.len()
        );
        // `AEROFTP_PROTON_LIVE_HOLD_SECS` keeps the session open before the
        // revoke, so the tester can read how Proton lists it.
        let hold = std::env::var("AEROFTP_PROTON_LIVE_HOLD_SECS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0);
        if hold > 0 {
            eprintln!("PROTON_SESSION_OPEN for {hold} s");
            tokio::time::sleep(std::time::Duration::from_secs(hold)).await;
        }
        auth::sign_out(&api).await.expect("revoke the test session");
        eprintln!("PROTON_SESSION_REVOKED");
    }
}
