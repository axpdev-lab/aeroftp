//! TOTP (Time-based One-Time Password) support for AeroFTP vault 2FA.
//! Uses RFC 6238 with SHA-1, 6 digits, 30-second period.
//!
//! Security hardening (v2.2.4 audit remediation):
//! - Single Mutex for atomic state transitions (RB-004)
//! - Verified gate prevents enable without verification (RB-003, SEC-003)
//! - Rate limiting with exponential backoff (RB-017, SEC-001)
//! - Explicit OsRng for CSPRNG clarity (SEC-010)
//! - Error propagation instead of .unwrap() on Mutex (RB-001)
//! - M17: TOTP secrets wrapped in SecretString for automatic zeroization on drop

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use secrecy::{ExposeSecret, SecretString};
use std::sync::Mutex;
use std::time::Instant;
use tauri::State;
use totp_rs::{Algorithm, Secret, TOTP};

/// Maximum failed TOTP attempts before lockout.
const MAX_FAILED_ATTEMPTS: u32 = 5;
/// Base lockout duration in seconds after exceeding MAX_FAILED_ATTEMPTS.
const BASE_LOCKOUT_SECS: u64 = 30;

/// Internal TOTP state: all fields protected by a single Mutex
/// to guarantee atomic state transitions.
struct TotpInner {
    /// M17: Pending secret during setup (base32 encoded), wrapped in SecretString
    /// for automatic zeroization when replaced or dropped.
    pending_secret: Option<SecretString>,
    /// Whether the pending secret has been verified via setup_verify
    setup_verified: bool,
    /// Whether TOTP is enabled for the current vault
    enabled: bool,
    /// M17: The active secret (base32 encoded), wrapped in SecretString
    /// for automatic zeroization when replaced or dropped.
    active_secret: Option<SecretString>,
    /// Failed verification attempt counter (for rate limiting)
    failed_attempts: u32,
    /// Lockout expiry time (None if not locked out)
    lockout_until: Option<Instant>,
}

/// Thread-safe TOTP state managed by Tauri.
pub struct TotpState {
    inner: Mutex<TotpInner>,
}

impl Default for TotpState {
    fn default() -> Self {
        Self {
            inner: Mutex::new(TotpInner {
                pending_secret: None,
                setup_verified: false,
                enabled: false,
                active_secret: None,
                failed_attempts: 0,
                lockout_until: None,
            }),
        }
    }
}

/// Acquire the inner lock with poison recovery.
fn lock_state(state: &TotpState) -> Result<std::sync::MutexGuard<'_, TotpInner>, String> {
    state
        .inner
        .lock()
        .map_err(|_| "TOTP internal state error".to_string())
}

/// Check rate limiting. Returns Err if locked out.
fn check_rate_limit(inner: &TotpInner) -> Result<(), String> {
    if let Some(until) = inner.lockout_until {
        if Instant::now() < until {
            let remaining = until.duration_since(Instant::now()).as_secs();
            return Err(format!(
                "Too many failed attempts. Try again in {} seconds.",
                remaining + 1
            ));
        }
    }
    Ok(())
}

/// Record a failed attempt. After MAX_FAILED_ATTEMPTS, impose exponential lockout.
fn record_failure(inner: &mut TotpInner) {
    inner.failed_attempts += 1;
    if inner.failed_attempts >= MAX_FAILED_ATTEMPTS {
        // Exponential backoff: 30s, 60s, 120s, 240s... capped at 15 min
        let multiplier = inner.failed_attempts.saturating_sub(MAX_FAILED_ATTEMPTS);
        let secs =
            BASE_LOCKOUT_SECS.saturating_mul(1u64.checked_shl(multiplier).unwrap_or(u64::MAX));
        let secs = secs.min(900); // Cap at 15 minutes
        inner.lockout_until = Some(Instant::now() + std::time::Duration::from_secs(secs));
    }
}

/// Reset rate limiting after successful verification.
fn reset_rate_limit(inner: &mut TotpInner) {
    inner.failed_attempts = 0;
    inner.lockout_until = None;
}

/// Build a TOTP instance from a base32-encoded secret.
///
/// Issuer / account_name choice:
/// - Authenticator apps render the entry as "<issuer>: <account_name>". Using
///   issuer "AeroFTP" + account_name "AeroFTP Vault" produced the awkward
///   "AeroFTP : AeroFTP Vault" duplication the user reported. We now use a
///   short, human-readable account name so Authy / Google Authenticator /
///   1Password show a clean "AeroFTP : Desktop 2FA".
fn build_totp(secret_base32: &str) -> Result<TOTP, String> {
    let secret = Secret::Encoded(secret_base32.to_string())
        .to_bytes()
        .map_err(|e| format!("Invalid TOTP secret: {}", e))?;
    TOTP::new(
        Algorithm::SHA1,
        6,
        1,
        30,
        secret,
        Some("AeroFTP".to_string()),
        "Desktop 2FA".to_string(),
    )
    .map_err(|e| format!("TOTP creation failed: {}", e))
}

/// Public URL of the AeroFTP logo, served from the docs site (GitHub Pages).
/// Embedded into the otpauth URI as `image=` so authenticator apps that
/// honor the Google extension (FreeOTP+, Yubico Authenticator, Bitwarden,
/// 1Password, recent Google Authenticator) can show the logo.
///
/// NOTE on Authy: Twilio's app does NOT read this field. It looks up issuer
/// names against an internal Twilio database, so adding our logo there would
/// require a submission to Twilio support. Until then, Authy will show a
/// generic icon: this is a vendor limitation, not something the URI can
/// override.
const AEROFTP_LOGO_URL: &str = "https://docs.aeroftp.app/web-app-manifest-512x512.png";

/// Append `image=` query parameter to a totp-rs generated URI.
fn append_image_param(uri: &str) -> String {
    let encoded = url_encode_image(AEROFTP_LOGO_URL);
    let separator = if uri.contains('?') { '&' } else { '?' };
    format!("{}{}image={}", uri, separator, encoded)
}

/// Minimal RFC 3986 percent-encoding for the `image` URL value. We only need
/// to encode the characters that conflict with otpauth URI grammar (`:`, `/`,
/// `?`, `&`, `=`, `#`, plus space). Everything else is left as-is which is
/// safe for the docs.aeroftp.app URL.
fn url_encode_image(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + 8);
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => {
                out.push_str(&format!("%{:02X}", byte));
            }
        }
    }
    out
}

/// Generate a random 20-byte secret encoded as base32.
/// Uses OsRng explicitly for cryptographic security.
fn generate_secret_base32() -> String {
    use rand::rngs::OsRng;
    use rand::RngCore;
    let mut bytes = [0u8; 20];
    OsRng.fill_bytes(&mut bytes);
    let encoded = data_encoding::BASE32_NOPAD.encode(&bytes);
    // Zeroize the raw bytes
    bytes.fill(0);
    encoded
}

/// Start 2FA setup: generate a new TOTP secret and return the otpauth URI.
/// Returns: { secret: string, uri: string }
#[tauri::command]
pub fn totp_setup_start(state: State<'_, TotpState>) -> Result<serde_json::Value, String> {
    let secret_base32 = generate_secret_base32();
    let totp = build_totp(&secret_base32)?;
    let uri = append_image_param(&totp.get_url());

    let mut inner = lock_state(&state)?;
    // Return the secret to the frontend for QR code display, then wrap in SecretString
    let result = serde_json::json!({
        "secret": secret_base32,
        "uri": uri,
    });
    inner.pending_secret = Some(SecretString::from(secret_base32));
    inner.setup_verified = false;

    Ok(result)
}

/// Verify a TOTP code during setup. If valid, marks the pending secret as verified.
/// The caller must then call totp_enable to activate.
#[tauri::command]
pub fn totp_setup_verify(state: State<'_, TotpState>, code: String) -> Result<bool, String> {
    let mut inner = lock_state(&state)?;
    check_rate_limit(&inner)?;

    let secret = inner
        .pending_secret
        .as_ref()
        .ok_or("No pending TOTP setup")?;
    let totp = build_totp(secret.expose_secret())?;
    let valid = totp
        .check_current(&code)
        .map_err(|e| format!("TOTP check error: {}", e))?;

    if valid {
        inner.setup_verified = true;
        reset_rate_limit(&mut inner);
    } else {
        record_failure(&mut inner);
    }
    Ok(valid)
}

/// Verify a TOTP code during unlock (using the active secret).
#[tauri::command]
pub fn totp_verify(state: State<'_, TotpState>, code: String) -> Result<bool, String> {
    let mut inner = lock_state(&state)?;
    check_rate_limit(&inner)?;

    let secret = inner
        .active_secret
        .as_ref()
        .ok_or("No active TOTP secret")?;
    let totp = build_totp(secret.expose_secret())?;
    let valid = totp
        .check_current(&code)
        .map_err(|e| format!("TOTP check error: {}", e))?;

    if valid {
        reset_rate_limit(&mut inner);
    } else {
        record_failure(&mut inner);
    }
    Ok(valid)
}

/// Check if TOTP is enabled.
#[tauri::command]
pub fn totp_status(state: State<'_, TotpState>) -> Result<bool, String> {
    let inner = lock_state(&state)?;
    Ok(inner.enabled)
}

/// Enable TOTP after successful verification. Requires that totp_setup_verify
/// returned true before calling this (verified gate: RB-003, SEC-003).
/// A2-05: Atomically stores the TOTP secret in the credential vault before enabling.
/// If the vault store fails, TOTP is NOT enabled (fail-closed).
/// Returns the secret as a plain String for backward compatibility.
#[tauri::command]
pub fn totp_enable(state: State<'_, TotpState>) -> Result<String, String> {
    let mut inner = lock_state(&state)?;

    if !inner.setup_verified {
        return Err("Must verify TOTP code before enabling".into());
    }

    let secret = inner
        .pending_secret
        .as_ref()
        .ok_or("No pending secret to enable")?;
    let secret_plain = secret.expose_secret().to_string();

    // A2-05: Store TOTP secret in vault BEFORE enabling: atomic operation.
    // If vault write fails, TOTP is not enabled (fail-closed).
    if let Some(store) = crate::credential_store::CredentialStore::from_cache() {
        store
            .store_internal("totp_secret", &secret_plain)
            .map_err(|e| format!("Failed to store TOTP secret in vault: {}", e))?;
    } else {
        return Err("Credential vault not available: cannot enable TOTP".into());
    }

    // Only enable after successful vault store
    let secret = inner.pending_secret.take().unwrap();
    inner.active_secret = Some(secret);
    inner.enabled = true;
    inner.setup_verified = false;

    Ok(secret_plain)
}

/// Disable TOTP (requires valid code first).
#[tauri::command]
pub fn totp_disable(state: State<'_, TotpState>, code: String) -> Result<bool, String> {
    let mut inner = lock_state(&state)?;
    check_rate_limit(&inner)?;

    let secret = inner.active_secret.as_ref().ok_or("TOTP not enabled")?;
    let totp = build_totp(secret.expose_secret())?;
    let valid = totp
        .check_current(&code)
        .map_err(|e| format!("TOTP check error: {}", e))?;

    if valid {
        inner.active_secret = None;
        inner.pending_secret = None;
        inner.enabled = false;
        inner.setup_verified = false;
        reset_rate_limit(&mut inner);
        Ok(true)
    } else {
        record_failure(&mut inner);
        Ok(false)
    }
}

/// Load TOTP state from a stored secret (called after vault unlock).
#[tauri::command]
pub fn totp_load_secret(state: State<'_, TotpState>, secret: String) -> Result<(), String> {
    load_secret_internal(&state, &secret)
}

/// Internal: Load a TOTP secret into state without requiring Tauri State wrapper.
/// Used by unlock_credential_store for 2FA enforcement.
pub fn load_secret_internal(state: &TotpState, secret: &str) -> Result<(), String> {
    // Validate the secret is valid base32
    build_totp(secret)?;
    let mut inner = lock_state(state)?;
    inner.active_secret = Some(SecretString::from(secret.to_string()));
    inner.enabled = true;
    Ok(())
}

/// Internal: Verify a TOTP code against the active secret without requiring Tauri State wrapper.
/// Used by unlock_credential_store for 2FA enforcement.
/// Returns Ok(true) if valid, Ok(false) if invalid, Err on rate limit or missing secret.
pub fn verify_internal(state: &TotpState, code: &str) -> Result<bool, String> {
    let mut inner = lock_state(state)?;
    check_rate_limit(&inner)?;

    let secret = inner
        .active_secret
        .as_ref()
        .ok_or("No active TOTP secret")?;
    let totp = build_totp(secret.expose_secret())?;
    let valid = totp
        .check_current(code)
        .map_err(|e| format!("TOTP check error: {}", e))?;

    if valid {
        reset_rate_limit(&mut inner);
    } else {
        record_failure(&mut inner);
    }
    Ok(valid)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_inner() -> TotpInner {
        TotpInner {
            pending_secret: None,
            setup_verified: false,
            enabled: false,
            active_secret: None,
            failed_attempts: 0,
            lockout_until: None,
        }
    }

    #[test]
    fn generate_secret_base32_is_deterministic_length_and_alphabet() {
        let s = generate_secret_base32();
        // 20 bytes → 32 base32 characters (no padding since BASE32_NOPAD)
        assert_eq!(s.len(), 32);
        assert!(s
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()));

        // two consecutive calls produce different secrets (CSPRNG)
        let s2 = generate_secret_base32();
        assert_ne!(s, s2, "generate_secret_base32 must not produce duplicates");
    }

    #[test]
    fn build_totp_accepts_valid_base32_and_rejects_garbage() {
        let secret = generate_secret_base32();
        assert!(build_totp(&secret).is_ok());
        assert!(build_totp("not-valid-base32!!!").is_err());
        assert!(build_totp("").is_err());
    }

    #[test]
    fn check_rate_limit_passes_when_not_locked_out() {
        let inner = fresh_inner();
        assert!(check_rate_limit(&inner).is_ok());
    }

    #[test]
    fn check_rate_limit_rejects_while_locked_out() {
        let mut inner = fresh_inner();
        inner.lockout_until = Some(Instant::now() + std::time::Duration::from_secs(60));
        let err = check_rate_limit(&inner).unwrap_err();
        assert!(err.contains("Too many failed attempts"));
    }

    #[test]
    fn check_rate_limit_passes_after_lockout_expires() {
        let mut inner = fresh_inner();
        // Lockout set in the past: should no longer block
        inner.lockout_until = Some(Instant::now() - std::time::Duration::from_secs(1));
        assert!(check_rate_limit(&inner).is_ok());
    }

    #[test]
    fn record_failure_increments_counter_but_no_lockout_below_threshold() {
        let mut inner = fresh_inner();
        for i in 1..MAX_FAILED_ATTEMPTS {
            record_failure(&mut inner);
            assert_eq!(inner.failed_attempts, i);
            assert!(inner.lockout_until.is_none());
        }
    }

    #[test]
    fn record_failure_sets_lockout_at_threshold_with_exponential_backoff() {
        let mut inner = fresh_inner();
        for _ in 0..MAX_FAILED_ATTEMPTS {
            record_failure(&mut inner);
        }
        assert_eq!(inner.failed_attempts, MAX_FAILED_ATTEMPTS);
        let first_lockout = inner.lockout_until.expect("lockout should be set");

        // Additional failures extend the lockout exponentially
        record_failure(&mut inner);
        let second_lockout = inner.lockout_until.expect("lockout should still be set");
        assert!(second_lockout > first_lockout);
    }

    #[test]
    fn record_failure_caps_lockout_at_15_minutes() {
        let mut inner = fresh_inner();
        // Push far beyond threshold: lockout should saturate at 900s
        inner.failed_attempts = MAX_FAILED_ATTEMPTS + 50;
        record_failure(&mut inner);
        let lockout = inner.lockout_until.expect("lockout should be set");
        let max = Instant::now() + std::time::Duration::from_secs(901);
        assert!(lockout <= max, "lockout should be capped at 15 minutes");
    }

    #[test]
    fn reset_rate_limit_clears_counter_and_lockout() {
        let mut inner = fresh_inner();
        inner.failed_attempts = 3;
        inner.lockout_until = Some(Instant::now() + std::time::Duration::from_secs(60));
        reset_rate_limit(&mut inner);
        assert_eq!(inner.failed_attempts, 0);
        assert!(inner.lockout_until.is_none());
    }

    #[test]
    fn append_image_param_adds_amp_when_query_present() {
        let base = "otpauth://totp/AeroFTP:Desktop%202FA?secret=ABC&issuer=AeroFTP";
        let result = append_image_param(base);
        assert!(result.starts_with(base));
        assert!(result.contains("&image=https%3A%2F%2Fdocs.aeroftp.app"));
    }

    #[test]
    fn url_encode_image_handles_reserved_chars() {
        let encoded = url_encode_image("https://x.test/a b?c=d");
        assert_eq!(encoded, "https%3A%2F%2Fx.test%2Fa%20b%3Fc%3Dd");
    }

    #[test]
    fn build_totp_uses_clean_account_name() {
        let secret = generate_secret_base32();
        let totp = build_totp(&secret).unwrap();
        let url = totp.get_url();
        assert!(url.contains("AeroFTP:Desktop%202FA") || url.contains("AeroFTP:Desktop 2FA"));
        assert!(!url.contains("AeroFTP%20Vault"));
    }

    #[test]
    fn totp_state_default_starts_empty_and_disabled() {
        let state = TotpState::default();
        let inner = state.inner.lock().unwrap();
        assert!(!inner.enabled);
        assert!(!inner.setup_verified);
        assert!(inner.active_secret.is_none());
        assert!(inner.pending_secret.is_none());
        assert_eq!(inner.failed_attempts, 0);
    }
}
