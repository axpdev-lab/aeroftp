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

use totp_rs::{Algorithm, TOTP, Secret};
use tauri::State;
use secrecy::{ExposeSecret, SecretString};
use std::sync::Mutex;
use std::time::Instant;

/// Maximum failed TOTP attempts before lockout.
const MAX_FAILED_ATTEMPTS: u32 = 5;
/// Base lockout duration in seconds after exceeding MAX_FAILED_ATTEMPTS.
const BASE_LOCKOUT_SECS: u64 = 30;

/// Internal TOTP state — all fields protected by a single Mutex
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
    state.inner.lock()
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
        let secs = BASE_LOCKOUT_SECS.saturating_mul(1u64.checked_shl(multiplier).unwrap_or(u64::MAX));
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
        "AeroFTP Vault".to_string(),
    )
    .map_err(|e| format!("TOTP creation failed: {}", e))
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
pub fn totp_setup_start(
    state: State<'_, TotpState>,
) -> Result<serde_json::Value, String> {
    let secret_base32 = generate_secret_base32();
    let totp = build_totp(&secret_base32)?;
    let uri = totp.get_url();

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
pub fn totp_setup_verify(
    state: State<'_, TotpState>,
    code: String,
) -> Result<bool, String> {
    let mut inner = lock_state(&state)?;
    check_rate_limit(&inner)?;

    let secret = inner.pending_secret.as_ref()
        .ok_or("No pending TOTP setup")?;
    let totp = build_totp(secret.expose_secret())?;
    let valid = totp.check_current(&code)
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
pub fn totp_verify(
    state: State<'_, TotpState>,
    code: String,
) -> Result<bool, String> {
    let mut inner = lock_state(&state)?;
    check_rate_limit(&inner)?;

    let secret = inner.active_secret.as_ref()
        .ok_or("No active TOTP secret")?;
    let totp = build_totp(secret.expose_secret())?;
    let valid = totp.check_current(&code)
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
pub fn totp_status(
    state: State<'_, TotpState>,
) -> Result<bool, String> {
    let inner = lock_state(&state)?;
    Ok(inner.enabled)
}

/// Enable TOTP after successful verification. Requires that totp_setup_verify
/// returned true before calling this (verified gate: RB-003, SEC-003).
/// Returns the secret as a plain String for vault storage.
#[tauri::command]
pub fn totp_enable(
    state: State<'_, TotpState>,
) -> Result<String, String> {
    let mut inner = lock_state(&state)?;

    if !inner.setup_verified {
        return Err("Must verify TOTP code before enabling".into());
    }

    let secret = inner.pending_secret.take()
        .ok_or("No pending secret to enable")?;
    // Extract the plain string for vault persistence, then store as SecretString
    let secret_plain = secret.expose_secret().to_string();
    inner.active_secret = Some(secret);
    inner.enabled = true;
    inner.setup_verified = false;

    Ok(secret_plain)
}

/// Disable TOTP (requires valid code first).
#[tauri::command]
pub fn totp_disable(
    state: State<'_, TotpState>,
    code: String,
) -> Result<bool, String> {
    let mut inner = lock_state(&state)?;
    check_rate_limit(&inner)?;

    let secret = inner.active_secret.as_ref()
        .ok_or("TOTP not enabled")?;
    let totp = build_totp(secret.expose_secret())?;
    let valid = totp.check_current(&code)
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
pub fn totp_load_secret(
    state: State<'_, TotpState>,
    secret: String,
) -> Result<(), String> {
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

    let secret = inner.active_secret.as_ref()
        .ok_or("No active TOTP secret")?;
    let totp = build_totp(secret.expose_secret())?;
    let valid = totp.check_current(code)
        .map_err(|e| format!("TOTP check error: {}", e))?;

    if valid {
        reset_rate_limit(&mut inner);
    } else {
        record_failure(&mut inner);
    }
    Ok(valid)
}
