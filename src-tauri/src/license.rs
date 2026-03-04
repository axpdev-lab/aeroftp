// AeroFTP License System — Ed25519 token verification
// Validates digitally signed license tokens for Pro feature unlocking.
// Private key stays on Supabase; only the public key is embedded here.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::credential_store::CredentialStore;

// Ed25519 public key for license verification.
// DEVELOPMENT KEY — will be regenerated with a new keypair before production launch.
const PUBLIC_KEY_BYTES: [u8; 32] = [0xae, 0x69, 0x76, 0x32, 0x1d, 0xd8, 0x6d, 0xac, 0x74, 0x1b, 0x99, 0x1d, 0xac, 0xfa, 0x12, 0x87, 0x73, 0x1c, 0x36, 0x3e, 0x28, 0x5c, 0x13, 0xa2, 0x4e, 0x17, 0x9d, 0x30, 0xd2, 0xea, 0x95, 0x24];

const LICENSE_CREDENTIAL_KEY: &str = "license_token";
const LICENSE_LAST_VERIFIED_KEY: &str = "license_last_verified";
const GRACE_PERIOD_DAYS: u64 = 30;

static LICENSE_CACHE: Mutex<Option<LicensePayload>> = Mutex::new(None);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LicensePayload {
    /// Unique license subject ID (e.g. "aero_xxxxxxxxxxxx")
    pub sub: String,
    /// Issuer — always "aeroftp-license"
    pub iss: String,
    /// Issued-at timestamp (Unix seconds)
    pub iat: u64,
    /// Expiry timestamp (0 = perpetual)
    pub exp: u64,
    /// License tier ("pro")
    pub tier: String,
    /// Maximum device activations
    pub max_devices: u32,
    /// Google Play order ID
    pub order_id: String,
    /// SHA-256 hash prefix of purchase token (for audit)
    #[serde(default)]
    pub purchase_token_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LicenseStatus {
    pub is_pro: bool,
    pub tier: String,
    pub license_id: Option<String>,
    pub activated_at: Option<u64>,
    pub grace_period_remaining_days: Option<u64>,
}

/// Verify an Ed25519 signed license token.
/// Format: base64url(JSON_payload).base64url(Ed25519_signature)
fn verify_token(token: &str) -> Result<LicensePayload, String> {
    let parts: Vec<&str> = token.splitn(2, '.').collect();
    if parts.len() != 2 {
        return Err("Invalid token format: expected payload.signature".into());
    }

    let payload_bytes = URL_SAFE_NO_PAD
        .decode(parts[0])
        .map_err(|e| format!("Failed to decode payload: {}", e))?;

    let sig_bytes = URL_SAFE_NO_PAD
        .decode(parts[1])
        .map_err(|e| format!("Failed to decode signature: {}", e))?;

    // Verify Ed25519 signature
    let public_key = VerifyingKey::from_bytes(&PUBLIC_KEY_BYTES)
        .map_err(|e| format!("Invalid public key: {}", e))?;

    let signature = Signature::from_slice(&sig_bytes)
        .map_err(|e| format!("Invalid signature bytes: {}", e))?;

    public_key
        .verify(&payload_bytes, &signature)
        .map_err(|_| "License signature verification failed".to_string())?;

    // Parse payload
    let payload: LicensePayload = serde_json::from_slice(&payload_bytes)
        .map_err(|e| format!("Failed to parse license payload: {}", e))?;

    // Check issuer
    if payload.iss != "aeroftp-license" {
        return Err("Invalid license issuer".into());
    }

    // Validate tier
    if payload.tier != "pro" {
        return Err(format!("Unknown license tier: {}", payload.tier));
    }

    // Validate iat (must not be in the future, with 5-minute clock skew tolerance)
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    if payload.iat > now + 300 {
        return Err("License issued-at timestamp is in the future".into());
    }

    // Check expiry (0 = perpetual/never expires)
    if payload.exp > 0 && now > payload.exp {
        return Err("License has expired".into());
    }

    Ok(payload)
}

/// Generate a device fingerprint from system info.
/// SHA-256 of "hostname:username:OS:arch" — non-invasive, no hardware UUIDs.
fn generate_fingerprint() -> String {
    let hostname = hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown".into());

    let username = whoami::username();
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;

    let input = format!("{}:{}:{}:{}", hostname, username, os, arch);
    let hash = Sha256::digest(input.as_bytes());
    hex::encode(hash)
}

/// Convert a raw token to human-readable AERO-XXXX-XXXX-XXXX-XXXX format
fn token_to_human_readable(token: &str) -> String {
    let hash = Sha256::digest(token.as_bytes());
    let encoded = data_encoding::BASE32_NOPAD.encode(&hash[..10]);
    // Take 16 chars, split into 4 groups of 4
    let chars: String = encoded.chars().take(16).collect();
    format!(
        "AERO-{}-{}-{}-{}",
        &chars[0..4],
        &chars[4..8],
        &chars[8..12],
        &chars[12..16]
    )
}

// ─── Tauri Commands ───

/// Activate a license token — verify signature, persist in vault.db
#[tauri::command]
pub async fn license_activate(token: String) -> Result<LicenseStatus, String> {
    let payload = verify_token(&token)?;

    // Store token and verification timestamp in vault.db
    let store = CredentialStore::from_cache()
        .ok_or_else(|| "Credential store not ready".to_string())?;
    store
        .store(LICENSE_CREDENTIAL_KEY, &token)
        .map_err(|e| format!("Failed to store license: {}", e))?;

    // Record when the token was last successfully verified (for grace period)
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let _ = store.store(LICENSE_LAST_VERIFIED_KEY, &now.to_string());

    // Cache in memory
    let status = LicenseStatus {
        is_pro: true,
        tier: payload.tier.clone(),
        license_id: Some(payload.sub.clone()),
        activated_at: Some(payload.iat),
        grace_period_remaining_days: None,
    };

    if let Ok(mut cache) = LICENSE_CACHE.lock() {
        *cache = Some(payload);
    }

    Ok(status)
}

/// Check current license status — loads from vault.db if not cached
#[tauri::command]
pub async fn license_check() -> Result<LicenseStatus, String> {
    // Check in-memory cache first
    if let Ok(cache) = LICENSE_CACHE.lock() {
        if let Some(ref payload) = *cache {
            return Ok(LicenseStatus {
                is_pro: true,
                tier: payload.tier.clone(),
                license_id: Some(payload.sub.clone()),
                activated_at: Some(payload.iat),
                grace_period_remaining_days: None,
            });
        }
    }

    // Try loading from vault.db
    let store = match CredentialStore::from_cache() {
        Some(s) => s,
        None => {
            return Ok(LicenseStatus {
                is_pro: false,
                tier: "free".into(),
                license_id: None,
                activated_at: None,
                grace_period_remaining_days: None,
            });
        }
    };

    match store.get(LICENSE_CREDENTIAL_KEY) {
        Ok(token) => {
            match verify_token(&token) {
                Ok(payload) => {
                    let status = LicenseStatus {
                        is_pro: true,
                        tier: payload.tier.clone(),
                        license_id: Some(payload.sub.clone()),
                        activated_at: Some(payload.iat),
                        grace_period_remaining_days: None,
                    };
                    // Update last_verified timestamp
                    let now = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    let _ = store.store(LICENSE_LAST_VERIFIED_KEY, &now.to_string());
                    // Cache for future calls
                    if let Ok(mut cache) = LICENSE_CACHE.lock() {
                        *cache = Some(payload);
                    }
                    Ok(status)
                }
                Err(_) => {
                    // Token signature verification failed (e.g. key rotation).
                    // Grace period: only grant if we have a stored last_verified timestamp
                    // (proving this token was once valid), NOT from the unsigned payload.
                    if let Ok(last_verified_str) = store.get(LICENSE_LAST_VERIFIED_KEY) {
                        if let Ok(last_verified) = last_verified_str.parse::<u64>() {
                            let now = SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs();
                            let grace_end = last_verified + (GRACE_PERIOD_DAYS * 86400);
                            if now < grace_end {
                                let remaining = (grace_end - now) / 86400;
                                return Ok(LicenseStatus {
                                    is_pro: true,
                                    tier: "pro".into(),
                                    license_id: None,
                                    activated_at: Some(last_verified),
                                    grace_period_remaining_days: Some(remaining),
                                });
                            }
                        }
                    }
                    // Grace period expired or no prior verification — revoke
                    let _ = store.delete(LICENSE_CREDENTIAL_KEY);
                    let _ = store.delete(LICENSE_LAST_VERIFIED_KEY);
                    Ok(LicenseStatus {
                        is_pro: false,
                        tier: "free".into(),
                        license_id: None,
                        activated_at: None,
                        grace_period_remaining_days: None,
                    })
                }
            }
        }
        Err(_) => Ok(LicenseStatus {
            is_pro: false,
            tier: "free".into(),
            license_id: None,
            activated_at: None,
            grace_period_remaining_days: None,
        }),
    }
}

/// Deactivate license — remove token from vault.db and clear cache
#[tauri::command]
pub async fn license_deactivate() -> Result<(), String> {
    // Clear in-memory cache
    if let Ok(mut cache) = LICENSE_CACHE.lock() {
        *cache = None;
    }

    // Remove from vault.db
    if let Some(store) = CredentialStore::from_cache() {
        let _ = store.delete(LICENSE_CREDENTIAL_KEY);
        let _ = store.delete(LICENSE_LAST_VERIFIED_KEY);
    }

    Ok(())
}

/// Get device fingerprint for activation
#[tauri::command]
pub async fn license_get_device_fingerprint() -> Result<String, String> {
    Ok(generate_fingerprint())
}

/// Get human-readable license key from stored token
#[tauri::command]
pub async fn license_get_key() -> Result<Option<String>, String> {
    let store = match CredentialStore::from_cache() {
        Some(s) => s,
        None => return Ok(None),
    };

    match store.get(LICENSE_CREDENTIAL_KEY) {
        Ok(token) => Ok(Some(token_to_human_readable(&token))),
        Err(_) => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fingerprint_deterministic() {
        let fp1 = generate_fingerprint();
        let fp2 = generate_fingerprint();
        assert_eq!(fp1, fp2);
        assert_eq!(fp1.len(), 64); // SHA-256 hex = 64 chars
    }

    #[test]
    fn test_token_to_human_readable() {
        let key = token_to_human_readable("test_token_value");
        assert!(key.starts_with("AERO-"));
        assert_eq!(key.len(), 24); // AERO- + 4x4 + 3 dashes
        // Verify format: AERO-XXXX-XXXX-XXXX-XXXX
        let parts: Vec<&str> = key.split('-').collect();
        assert_eq!(parts.len(), 5);
        assert_eq!(parts[0], "AERO");
        for part in &parts[1..] {
            assert_eq!(part.len(), 4);
        }
    }

    #[test]
    fn test_verify_token_invalid_format() {
        let result = verify_token("no_dot_separator");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid token format"));
    }

    #[test]
    fn test_verify_token_invalid_signature() {
        // Valid base64url but wrong signature
        let payload = URL_SAFE_NO_PAD.encode(b"{\"sub\":\"test\",\"iss\":\"aeroftp-license\",\"iat\":0,\"exp\":0,\"tier\":\"pro\",\"max_devices\":5,\"order_id\":\"test\"}");
        let fake_sig = URL_SAFE_NO_PAD.encode([0u8; 64]);
        let token = format!("{}.{}", payload, fake_sig);
        let result = verify_token(&token);
        assert!(result.is_err());
    }

    #[test]
    fn test_verify_real_token() {
        // DEVELOPMENT ONLY: This token is signed with the test keypair.
        // The PUBLIC_KEY_BYTES and this token will BOTH be regenerated before production launch.
        // The test key pair has order_id "TEST-ORDER-001" — never valid in production.
        let token = "eyJzdWIiOiJhZXJvX3Rlc3QxMjM0NTY3OCIsImlzcyI6ImFlcm9mdHAtbGljZW5zZSIsImlhdCI6MTc3MjU1NDAxNCwiZXhwIjowLCJ0aWVyIjoicHJvIiwibWF4X2RldmljZXMiOjUsIm9yZGVyX2lkIjoiVEVTVC1PUkRFUi0wMDEiLCJwdXJjaGFzZV90b2tlbl9oYXNoIjoiMDAwMDAwMDAwMDAwMDAwMCJ9.ImQHFdFyNhXP72pbVzJ7_2EIWarHnNV2FF6a4cGhW1JzIh9sU4iWb5AEbfrq_SNRbCLcWDae6MgRGUVrM_4tBw";
        let result = verify_token(token);
        assert!(result.is_ok(), "Token verification failed: {:?}", result.err());
        let payload = result.unwrap();
        assert_eq!(payload.sub, "aero_test12345678");
        assert_eq!(payload.iss, "aeroftp-license");
        assert_eq!(payload.tier, "pro");
        assert_eq!(payload.max_devices, 5);
        assert_eq!(payload.exp, 0); // perpetual
        assert_eq!(payload.order_id, "TEST-ORDER-001");
    }
}
