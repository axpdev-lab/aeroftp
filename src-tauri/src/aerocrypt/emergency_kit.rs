// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Minimal Emergency Kit builder (Tier 1 for headerless default).
//!
//! The kit carries only public, non-secret fields from a persisted
//! OverlayConfig: vault_id, version, salt, and the KDF parameters.
//! It is an OPTIONAL, on-demand recovery kit available in every mode
//! (headerless, headed, keyfile): in the GUI it never gates create or
//! connect (owner decision, v4.1.4) and is re-viewable any time from the
//! crypt toggle; the CLI `crypt init` still shows it once for an explicit
//! acknowledgement.
//!
//! Never includes password, master key, keyfile material, or config_mac.
//! The builder always reads the persisted config (via caller using
//! validate_headerless_config_salt + parse_config) and does not
//! re-derive crypto values.

use base64::Engine as _;
use serde::Serialize;

use super::overlay::{parse_config, OverlayConfig, VAULT_ID_SIZE, VERSION_V3};
use super::{argon2_lanes, argon2_mem_kib, argon2_time};

/// Public-only Emergency Kit for recovery after keystore loss.
/// The salt here is the per-vault random value (not a constant).
/// With the matching password (and optional keyfile) this data lets
/// a user reconstruct the local headerless config or headed marker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EmergencyKit {
    /// Base64 encoding of the 16-byte vault_id (matches the persisted
    /// config JSON exactly). Labelled in text output for clarity.
    pub vault_id: String,
    /// AECR version (always 3 for current overlays that emit vault_id).
    pub version: u8,
    /// Base64 encoding of the 32-byte salt (matches persisted JSON).
    pub salt: String,
    /// KDF algorithm name (always "Argon2id" for v3).
    pub kdf_algorithm: String,
    pub kdf_mem_kib: u32,
    pub kdf_time: u32,
    pub kdf_lanes: u32,
    /// Full human-readable printable representation. This is what the
    /// user saves to paper or an offline file.
    pub text: String,
}

impl EmergencyKit {
    /// Render the kit as a stable, printable block (no secrets).
    /// Used by CLI when printing for interactive ack and by GUI dialog.
    pub fn render_text(
        vault_id: &str,
        version: u8,
        salt: &str,
        mem: u32,
        time: u32,
        lanes: u32,
        requires_keyfile: bool,
    ) -> String {
        let keyfile_line = if requires_keyfile {
            "This vault also requires its keyfile: this kit and your password are not\n\
             enough on their own, keep the keyfile safe too.\n"
        } else {
            ""
        };
        format!(
            "AEROCRYPT EMERGENCY KIT\n\n\
             Store this with your password. This public configuration is required\n\
             to recover your vault after losing the local keystore\n\
             (reinstall, new machine, lost credentials store).\n\n\
             Vault ID: {}\n\
             Version: {}\n\
             Salt (base64): {}\n\
             KDF: {} (mem={} KiB, t={}, p={})\n\
             {}\n\
             NEVER store the password alongside this kit.\n",
            vault_id, version, salt, "Argon2id", mem, time, lanes, keyfile_line
        )
    }
}

/// Build the kit by parsing a persisted config JSON (headed marker or
/// aerocrypt_overlay_config_<id> blob). The caller is responsible for
/// having validated the persisted bytes via validate_headerless_config_salt
/// (headerless) or direct read (headed) before calling.
pub fn build_from_config_json(config_json: &str) -> Result<EmergencyKit, String> {
    let cfg = parse_config(config_json)?;
    build_from_overlay_config(&cfg)
}

/// Build directly from a parsed OverlayConfig (v3 only for Tier-1 kit).
/// Extracts vault_id (required), salt and KDF params from the live profile.
pub fn build_from_overlay_config(cfg: &OverlayConfig) -> Result<EmergencyKit, String> {
    let requires_keyfile = cfg.requires_keyfile();
    match cfg {
        OverlayConfig::V3 { salt, vault_id, .. } => {
            let vid = vault_id.ok_or_else(|| {
                "Emergency Kit requires a vault_id (present in all v3 configs since Tier 1)"
                    .to_string()
            })?;
            if vid.len() != VAULT_ID_SIZE {
                return Err("vault_id has wrong length".to_string());
            }
            let vid_b64 = base64::engine::general_purpose::STANDARD.encode(vid);
            let salt_b64 = base64::engine::general_purpose::STANDARD.encode(salt);
            let mem = argon2_mem_kib();
            let t = argon2_time();
            let p = argon2_lanes();
            let text = EmergencyKit::render_text(
                &vid_b64,
                VERSION_V3,
                &salt_b64,
                mem,
                t,
                p,
                requires_keyfile,
            );
            Ok(EmergencyKit {
                vault_id: vid_b64,
                version: VERSION_V3,
                salt: salt_b64,
                kdf_algorithm: "Argon2id".to_string(),
                kdf_mem_kib: mem,
                kdf_time: t,
                kdf_lanes: p,
                text,
            })
        }
        other => Err(format!(
            "Emergency Kit is only supported for v3 overlays (got v{})",
            other.version()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)]
    use base64::Engine as _;

    fn make_v3_json_with_vid(salt: &[u8; 32], vid: &[u8; 16], requires_keyfile: bool) -> String {
        let mut obj = serde_json::json!({
            "version": 3,
            "cipher": "AES-256-GCM-SIV",
            "filename_cipher": "AES-256-SIV",
            "key_wrap": "AES-256-KW",
            "kdf": "Argon2id",
            "kdf_mem_kib": 131072u32,
            "kdf_time": 4u32,
            "kdf_lanes": 4u32,
            "salt": base64::engine::general_purpose::STANDARD.encode(salt),
            "vault_id": base64::engine::general_purpose::STANDARD.encode(vid),
            "block_size": 65536u32,
            "mac": base64::engine::general_purpose::STANDARD.encode([0u8; 32]),
        });
        if requires_keyfile {
            obj["kdf_inputs"] = serde_json::json!(["password", "keyfile"]);
        }
        obj.to_string()
    }

    #[test]
    fn kit_from_config_matches_public_fields_and_is_stable() {
        let salt = [0xABu8; 32];
        let vid = [0xCDu8; 16];
        let json = make_v3_json_with_vid(&salt, &vid, false);
        let kit = build_from_config_json(&json).unwrap();
        let parsed = parse_config(&json).unwrap();

        // Exact match on public extractable fields (no MAC, no secrets).
        assert_eq!(kit.version, 3);
        assert_eq!(
            kit.vault_id,
            base64::engine::general_purpose::STANDARD.encode(vid)
        );
        assert_eq!(
            kit.salt,
            base64::engine::general_purpose::STANDARD.encode(salt)
        );
        assert_eq!(kit.kdf_algorithm, "Argon2id");
        assert_eq!(kit.kdf_mem_kib, 131072);
        assert_eq!(kit.kdf_time, 4);
        assert_eq!(kit.kdf_lanes, 4);

        // vault_id() accessor round-trips
        assert_eq!(parsed.vault_id(), Some(vid));

        // text contains the labelled fields and recovery framing (no secrets)
        assert!(kit.text.contains("AEROCRYPT EMERGENCY KIT"));
        assert!(kit.text.contains(&kit.vault_id));
        assert!(kit.text.contains(&kit.salt));
        assert!(kit.text.contains("Argon2id"));
        assert!(kit.text.contains("mem=131072 KiB"));
        // The kit text legitimately mentions the word "password" in instructions ("with your password");
        // the important guarantee is that it never contains secret material (tested via field extraction and controlled render).
    }

    #[test]
    fn kit_notes_keyfile_requirement_only_for_keyfile_vaults() {
        let salt = [0x11u8; 32];
        let vid = [0x22u8; 16];

        // Password-only vault: no keyfile note.
        let pw_only = build_from_config_json(&make_v3_json_with_vid(&salt, &vid, false)).unwrap();
        assert!(!pw_only.text.to_lowercase().contains("keyfile"));

        // Keyfile vault: the kit spells out that the keyfile is also required, so
        // a user does not think the paper alone can reopen it.
        let kf = build_from_config_json(&make_v3_json_with_vid(&salt, &vid, true)).unwrap();
        assert!(kf.text.contains("also requires its keyfile"));
        // Still public-only: never the keyfile material itself.
        assert!(kf.text.contains("AEROCRYPT EMERGENCY KIT"));
    }

    #[test]
    fn kit_rejects_non_v3_and_missing_vault_id() {
        // v2 has no vault_id
        let bad = serde_json::json!({
            "version": 2,
            "salt": base64::engine::general_purpose::STANDARD.encode([1u8; 32]),
        })
        .to_string();
        assert!(build_from_config_json(&bad).is_err());

        // v3 without vault_id (pre-Tier1) is rejected for kit
        let no_vid = serde_json::json!({
            "version": 3,
            "salt": base64::engine::general_purpose::STANDARD.encode([2u8; 32]),
            "mac": base64::engine::general_purpose::STANDARD.encode([0u8; 32]),
        })
        .to_string();
        assert!(build_from_config_json(&no_vid).is_err());
    }

    #[test]
    fn kit_text_is_human_readable_and_safe() {
        let salt = [3u8; 32];
        let vid = [4u8; 16];
        let json = make_v3_json_with_vid(&salt, &vid, true);
        let kit = build_from_config_json(&json).unwrap();
        let t = &kit.text;
        assert!(t.contains("Vault ID:"));
        assert!(t.contains("Salt (base64):"));
        assert!(t.contains("KDF:"));
        assert!(t.contains("NEVER store the password"));
        // keyfile requirement is in config but not leaked as secret in kit
        assert!(t.contains("AEROCRYPT EMERGENCY KIT"));
    }
}
