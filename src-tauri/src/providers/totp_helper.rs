// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
//
// Shared TOTP code generation for providers that gate authentication
// behind RFC 6238 two-factor codes (Filen, MEGA). The user persists the
// base32 secret once (the same value an authenticator app would scan
// from the QR), and the backend derives the 6-digit code at every
// connect, exactly as the user would type it.
//
// All secrets are accepted as base32 (RFC 4648), case-insensitive,
// with optional whitespace and '=' padding stripped. This matches what
// providers display to the user when 2FA is set up.

use secrecy::{ExposeSecret, SecretString};
use totp_rs::{Algorithm, Secret, TOTP};

use super::types::ProviderError;

/// Derive the current 6-digit TOTP code from a base32-encoded secret.
///
/// Uses the RFC 6238 defaults shared by Filen, MEGA, and every standard
/// authenticator app: SHA-1, 6 digits, 30-second period, t0 = Unix epoch.
pub fn generate_totp_code(secret: &SecretString) -> Result<String, ProviderError> {
    let totp = build_totp(secret)?;
    totp.generate_current()
        .map_err(|err| ProviderError::InvalidConfig(format!("TOTP generation failed: {err}")))
}

/// Like [`generate_totp_code`] but also returns the seconds remaining
/// before the current code rolls over. Used by the connection-form
/// diagnostic that shows the live code + 30s countdown next to the saved
/// secret field, so the user can confirm AeroFTP derives the same code as
/// their authenticator app (issue #128).
pub fn generate_totp_code_with_ttl(
    secret: &SecretString,
) -> Result<(String, u64), ProviderError> {
    let totp = build_totp(secret)?;
    let code = totp
        .generate_current()
        .map_err(|err| ProviderError::InvalidConfig(format!("TOTP generation failed: {err}")))?;
    let ttl = totp
        .ttl()
        .map_err(|err| ProviderError::InvalidConfig(format!("TOTP ttl failed: {err}")))?;
    Ok((code, ttl))
}

/// Build the RFC 6238 TOTP from a base32 secret using the defaults shared
/// by Filen, MEGA, and every standard authenticator app: SHA-1, 6 digits,
/// 30-second period, t0 = Unix epoch.
fn build_totp(secret: &SecretString) -> Result<TOTP, ProviderError> {
    // Normalize: strip whitespace and force uppercase so users can paste
    // the secret with spaces (the usual display format from providers)
    // without an "invalid character" error.
    let normalized: String = secret
        .expose_secret()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_uppercase();

    if normalized.is_empty() {
        return Err(ProviderError::InvalidConfig(
            "TOTP secret is empty".to_string(),
        ));
    }

    let bytes = Secret::Encoded(normalized).to_bytes().map_err(|err| {
        ProviderError::InvalidConfig(format!("Invalid TOTP secret (base32 expected): {err:?}"))
    })?;

    // We deliberately use `new_unchecked` here instead of `new`. totp-rs's
    // checked constructor enforces an RFC 4226 minimum of 128 bits on the
    // shared secret, but Filen and MEGA both provision 80-bit secrets
    // (16 base32 chars) by default. Refusing them client-side would make
    // this feature unusable for the very accounts the user wants to attach.
    // We trust the provider's own 2FA enrollment to be authoritative.
    // Issuer / account_name are required by the signature even though we
    // never emit an otpauth URI here (only generate_current is called).
    Ok(TOTP::new_unchecked(
        Algorithm::SHA1,
        6,
        1,
        30,
        bytes,
        Some("AeroFTP".to_string()),
        "provider-2fa".to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_secret() {
        let err = generate_totp_code(&SecretString::from(String::new())).unwrap_err();
        assert!(matches!(err, ProviderError::InvalidConfig(_)));
    }

    #[test]
    fn rejects_non_base32_secret() {
        let err = generate_totp_code(&SecretString::from("not-base32!".to_string())).unwrap_err();
        assert!(matches!(err, ProviderError::InvalidConfig(_)));
    }

    #[test]
    fn accepts_whitespace_and_lowercase() {
        // Same secret in three forms: padded base32, lowercased, with spaces.
        // All must produce identical 6-digit codes for the current 30s slot.
        let secret = "JBSWY3DPEHPK3PXP";
        let lower = secret.to_ascii_lowercase();
        let spaced = "JBSW Y3DP EHPK 3PXP";
        let a = generate_totp_code(&SecretString::from(secret.to_string())).unwrap();
        let b = generate_totp_code(&SecretString::from(lower)).unwrap();
        let c = generate_totp_code(&SecretString::from(spaced.to_string())).unwrap();
        assert_eq!(a.len(), 6);
        assert_eq!(a, b);
        assert_eq!(a, c);
        assert!(a.chars().all(|ch| ch.is_ascii_digit()));
    }

    #[test]
    fn accepts_short_secret_used_by_filen_and_mega() {
        // Filen and MEGA enrollment QRs encode 80-bit secrets (16 base32
        // chars). totp-rs's checked constructor would reject these because
        // RFC 4226 recommends 128 bits, but we use `new_unchecked` so the
        // user can paste the secret verbatim from their provider. This test
        // pins that behavior so a future refactor cannot silently regress
        // it back to the strict path.
        let code = generate_totp_code(&SecretString::from("JBSWY3DPEHPK3PXP".to_string())).unwrap();
        assert_eq!(code.len(), 6);
    }
}
