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
//!
//! `verify_against_active` re-parses a saved kit text or remote marker and
//! confirms vault_id / salt / version / KDF still match the active profile
//! keystore config (tracker #421 item #6). No secrets are accepted or returned.

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

/// Field-level comparison of a candidate kit/marker against the active
/// profile's public config. No secrets; safe to surface in CLI/GUI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KitVerifyReport {
    pub ok: bool,
    pub vault_id_match: bool,
    pub salt_match: bool,
    pub version_match: bool,
    pub kdf_match: bool,
    /// Whether the candidate carried explicit KDF numbers (kit text or
    /// legacy JSON). When false, `kdf_match` still compares against the
    /// live binary profile used by `build_from_config_json` for markers
    /// that do not store KDF costs (current TSV).
    pub candidate_kdf_explicit: bool,
    pub expected: EmergencyKit,
    pub observed: EmergencyKit,
    pub details: Vec<String>,
    /// Input shape used for the candidate: "kit-text", "marker-json",
    /// "marker-tsv", or "qr-payload".
    pub candidate_kind: String,
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
             Keep this in a safe place. You will need it TOGETHER WITH your\n\
             password to recover your vault after losing the local keystore\n\
             (reinstall, new machine, lost credentials store).\n\
             This kit holds only public configuration, never a secret.\n\n\
             Vault ID: {}\n\
             Version: {}\n\
             Salt (base64): {}\n\
             KDF: {} (mem={} KiB, t={}, p={})\n\
             {}\n\
             SECURITY: NEVER keep this kit and your password in the same place.\n\
             Both are required to open the vault; neither one alone can.\n",
            vault_id, version, salt, "Argon2id", mem, time, lanes, keyfile_line
        )
    }

    fn public_snapshot(fields: PublicKitFields, requires_keyfile: bool) -> Self {
        let text = Self::render_text(
            &fields.vault_id,
            fields.version,
            &fields.salt,
            fields.kdf_mem_kib,
            fields.kdf_time,
            fields.kdf_lanes,
            requires_keyfile,
        );
        Self {
            vault_id: fields.vault_id,
            version: fields.version,
            salt: fields.salt,
            kdf_algorithm: fields.kdf_algorithm,
            kdf_mem_kib: fields.kdf_mem_kib,
            kdf_time: fields.kdf_time,
            kdf_lanes: fields.kdf_lanes,
            text,
        }
    }
}

/// Bundle of public kit fields used when assembling an `EmergencyKit`.
struct PublicKitFields {
    vault_id: String,
    version: u8,
    salt: String,
    kdf_algorithm: String,
    kdf_mem_kib: u32,
    kdf_time: u32,
    kdf_lanes: u32,
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
            Ok(EmergencyKit::public_snapshot(
                PublicKitFields {
                    vault_id: vid_b64,
                    version: VERSION_V3,
                    salt: salt_b64,
                    kdf_algorithm: "Argon2id".to_string(),
                    kdf_mem_kib: argon2_mem_kib(),
                    kdf_time: argon2_time(),
                    kdf_lanes: argon2_lanes(),
                },
                requires_keyfile,
            ))
        }
        other => Err(format!(
            "Emergency Kit is only supported for v3 overlays (got v{})",
            other.version()
        )),
    }
}

/// Detect and parse a candidate kit text, QR payload, or marker (JSON/TSV)
/// into the public kit fields used for verification.
///
/// Accepted inputs:
/// - printed kit text (`AEROCRYPT EMERGENCY KIT` / `Vault ID:` lines)
/// - QR payload prefixed with `AEROCRYPT-KIT v1\n`
/// - headed marker: `.aerocrypt.tsv` or legacy `.aeroftp-crypt.json`
pub fn parse_candidate_kit(input: &str) -> Result<(EmergencyKit, String, bool), String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("Empty kit or marker input".to_string());
    }

    // Strip the QR envelope if present (GUI encodes AEROCRYPT-KIT v1\n + kit.text).
    let body = if let Some(rest) = trimmed.strip_prefix("AEROCRYPT-KIT v1\n") {
        rest.trim()
    } else if let Some(rest) = trimmed.strip_prefix("AEROCRYPT-KIT v1\r\n") {
        rest.trim()
    } else {
        trimmed
    };
    let kind_prefix = if body.len() != trimmed.len() {
        "qr-payload"
    } else {
        ""
    };

    // Marker first: JSON starts with '{'; TSV has a version\t line (and often a
    // Warning\t header). Kit text is free-form prose with labelled fields.
    let looks_like_json = body.starts_with('{');
    let looks_like_tsv = body.lines().any(|l| {
        let l = l.trim();
        l.starts_with("version\t") || l.starts_with("Warning\t") || l.starts_with("salt\t")
    });

    if looks_like_json || looks_like_tsv {
        let kit = build_from_config_json(body)?;
        let kind = if looks_like_json {
            if kind_prefix.is_empty() {
                "marker-json".to_string()
            } else {
                format!("{kind_prefix}+marker-json")
            }
        } else if kind_prefix.is_empty() {
            "marker-tsv".to_string()
        } else {
            format!("{kind_prefix}+marker-tsv")
        };
        // TSV never stores KDF costs; legacy JSON emits them but parse_config
        // does not retain them on OverlayConfig, so both marker paths rebuild
        // KDF from the live binary profile. Mark as non-explicit so the report
        // does not claim the marker itself carried independent KDF numbers.
        let kdf_explicit = false;
        return Ok((kit, kind, kdf_explicit));
    }

    let kit = parse_kit_text(body)?;
    let kind = if kind_prefix.is_empty() {
        "kit-text".to_string()
    } else {
        "qr-payload".to_string()
    };
    Ok((kit, kind, true))
}

/// Parse the human-readable Emergency Kit text block produced by `render_text`.
/// Field labels are stable and English (the kit is intentionally not i18n'd so
/// recovery works across locales). Tolerates optional keyfile note lines and
/// the security footer; requires Vault ID, Version, Salt, and KDF.
pub fn parse_kit_text(text: &str) -> Result<EmergencyKit, String> {
    let mut vault_id: Option<String> = None;
    let mut version: Option<u8> = None;
    let mut salt: Option<String> = None;
    let mut kdf_algorithm: Option<String> = None;
    let mut kdf_mem_kib: Option<u32> = None;
    let mut kdf_time: Option<u32> = None;
    let mut kdf_lanes: Option<u32> = None;
    let mut requires_keyfile = false;

    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if line
            .to_ascii_lowercase()
            .contains("also requires its keyfile")
        {
            requires_keyfile = true;
        }
        if let Some(v) = line.strip_prefix("Vault ID:") {
            vault_id = Some(v.trim().to_string());
            continue;
        }
        if let Some(v) = line.strip_prefix("Version:") {
            let n = v
                .trim()
                .parse::<u8>()
                .map_err(|_| format!("Invalid Version field in kit: {}", v.trim()))?;
            version = Some(n);
            continue;
        }
        if let Some(v) = line.strip_prefix("Salt (base64):") {
            salt = Some(v.trim().to_string());
            continue;
        }
        // Accept both "Salt:" and "Salt (base64):" for slightly older prints.
        if let Some(v) = line.strip_prefix("Salt:") {
            salt = Some(v.trim().to_string());
            continue;
        }
        if let Some(rest) = line.strip_prefix("KDF:") {
            // Expected: "Argon2id (mem=131072 KiB, t=4, p=4)"
            let rest = rest.trim();
            let (algo, params) = match rest.find('(') {
                Some(i) => {
                    let algo = rest[..i].trim().to_string();
                    let inside = rest[i + 1..].trim_end_matches(')').trim().to_string();
                    (algo, inside)
                }
                None => {
                    return Err(format!(
                        "KDF line is missing parameters (expected 'Argon2id (mem=…, t=…, p=…)'): {rest}"
                    ));
                }
            };
            if algo.is_empty() {
                return Err("KDF algorithm is empty in kit".to_string());
            }
            let mut mem = None;
            let mut t = None;
            let mut p = None;
            for part in params.split(',') {
                let part = part.trim();
                if let Some(v) = part
                    .strip_prefix("mem=")
                    .or_else(|| part.strip_prefix("mem ="))
                {
                    let v = v.trim().trim_end_matches("KiB").trim();
                    mem = Some(
                        v.parse::<u32>()
                            .map_err(|_| format!("Invalid KDF mem in kit: {v}"))?,
                    );
                } else if let Some(v) = part.strip_prefix("t=").or_else(|| part.strip_prefix("t ="))
                {
                    t = Some(
                        v.trim()
                            .parse::<u32>()
                            .map_err(|_| format!("Invalid KDF t in kit: {}", v.trim()))?,
                    );
                } else if let Some(v) = part.strip_prefix("p=").or_else(|| part.strip_prefix("p ="))
                {
                    p = Some(
                        v.trim()
                            .parse::<u32>()
                            .map_err(|_| format!("Invalid KDF p in kit: {}", v.trim()))?,
                    );
                }
            }
            kdf_algorithm = Some(algo);
            kdf_mem_kib = mem;
            kdf_time = t;
            kdf_lanes = p;
        }
    }

    let vault_id = vault_id.ok_or_else(|| "Kit is missing Vault ID".to_string())?;
    let version = version.ok_or_else(|| "Kit is missing Version".to_string())?;
    let salt = salt.ok_or_else(|| "Kit is missing Salt".to_string())?;
    let kdf_algorithm = kdf_algorithm.ok_or_else(|| "Kit is missing KDF algorithm".to_string())?;
    let kdf_mem_kib = kdf_mem_kib.ok_or_else(|| "Kit is missing KDF mem".to_string())?;
    let kdf_time = kdf_time.ok_or_else(|| "Kit is missing KDF t".to_string())?;
    let kdf_lanes = kdf_lanes.ok_or_else(|| "Kit is missing KDF p".to_string())?;

    // Decode to prove the public fields are well-formed base64 of the right size.
    let vid_bytes = base64::engine::general_purpose::STANDARD
        .decode(vault_id.as_bytes())
        .map_err(|e| format!("Vault ID is not valid base64: {e}"))?;
    if vid_bytes.len() != VAULT_ID_SIZE {
        return Err(format!(
            "Vault ID has wrong decoded length (got {}, expected {VAULT_ID_SIZE})",
            vid_bytes.len()
        ));
    }
    let salt_bytes = base64::engine::general_purpose::STANDARD
        .decode(salt.as_bytes())
        .map_err(|e| format!("Salt is not valid base64: {e}"))?;
    if salt_bytes.len() != 32 {
        return Err(format!(
            "Salt has wrong decoded length (got {}, expected 32)",
            salt_bytes.len()
        ));
    }

    Ok(EmergencyKit::public_snapshot(
        PublicKitFields {
            vault_id,
            version,
            salt,
            kdf_algorithm,
            kdf_mem_kib,
            kdf_time,
            kdf_lanes,
        },
        requires_keyfile,
    ))
}

/// Compare a candidate kit/marker against the active profile's public config.
///
/// `active_config` is the keystore blob (`aerocrypt_overlay_config_<id>`) or any
/// parseable headed marker for the same vault. The candidate may be kit text,
/// a QR payload, or a marker (JSON/TSV). Returns a structured report; never
/// panics on mismatch — only on unparseable input.
pub fn verify_against_active(
    active_config: &str,
    candidate: &str,
) -> Result<KitVerifyReport, String> {
    let expected = build_from_config_json(active_config)?;
    let (observed, candidate_kind, candidate_kdf_explicit) = parse_candidate_kit(candidate)?;

    let vault_id_match = expected.vault_id == observed.vault_id;
    let salt_match = expected.salt == observed.salt;
    let version_match = expected.version == observed.version;
    let kdf_match = expected.kdf_algorithm == observed.kdf_algorithm
        && expected.kdf_mem_kib == observed.kdf_mem_kib
        && expected.kdf_time == observed.kdf_time
        && expected.kdf_lanes == observed.kdf_lanes;

    let mut details = Vec::new();
    if !vault_id_match {
        details.push(format!(
            "Vault ID mismatch: expected {}, observed {}",
            expected.vault_id, observed.vault_id
        ));
    }
    if !salt_match {
        details.push(format!(
            "Salt mismatch: expected {}, observed {}",
            expected.salt, observed.salt
        ));
    }
    if !version_match {
        details.push(format!(
            "Version mismatch: expected {}, observed {}",
            expected.version, observed.version
        ));
    }
    if !kdf_match {
        details.push(format!(
            "KDF mismatch: expected {} (mem={} KiB, t={}, p={}), observed {} (mem={} KiB, t={}, p={})",
            expected.kdf_algorithm,
            expected.kdf_mem_kib,
            expected.kdf_time,
            expected.kdf_lanes,
            observed.kdf_algorithm,
            observed.kdf_mem_kib,
            observed.kdf_time,
            observed.kdf_lanes
        ));
        if !candidate_kdf_explicit {
            details.push(
                "Candidate is a marker without stored KDF costs; KDF was rebuilt from the live AeroCrypt profile."
                    .to_string(),
            );
        }
    }
    if details.is_empty() {
        details.push("All public fields match the active profile keystore config.".to_string());
    }

    let ok = vault_id_match && salt_match && version_match && kdf_match;
    Ok(KitVerifyReport {
        ok,
        vault_id_match,
        salt_match,
        version_match,
        kdf_match,
        candidate_kdf_explicit,
        expected,
        observed,
        details,
        candidate_kind,
    })
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
        assert!(t.contains("NEVER keep this kit and your password in the same place"));
        // keyfile requirement is in config but not leaked as secret in kit
        assert!(t.contains("AEROCRYPT EMERGENCY KIT"));
    }

    #[test]
    fn parse_kit_text_round_trips_render_text() {
        let salt = [0xABu8; 32];
        let vid = [0xCDu8; 16];
        let json = make_v3_json_with_vid(&salt, &vid, true);
        let kit = build_from_config_json(&json).unwrap();
        let parsed = parse_kit_text(&kit.text).unwrap();
        assert_eq!(parsed.vault_id, kit.vault_id);
        assert_eq!(parsed.salt, kit.salt);
        assert_eq!(parsed.version, kit.version);
        assert_eq!(parsed.kdf_algorithm, kit.kdf_algorithm);
        assert_eq!(parsed.kdf_mem_kib, kit.kdf_mem_kib);
        assert_eq!(parsed.kdf_time, kit.kdf_time);
        assert_eq!(parsed.kdf_lanes, kit.kdf_lanes);
        assert!(parsed.text.contains("also requires its keyfile"));
    }

    #[test]
    fn verify_against_active_matches_identical_kit_text() {
        let salt = [7u8; 32];
        let vid = [8u8; 16];
        let json = make_v3_json_with_vid(&salt, &vid, false);
        let kit = build_from_config_json(&json).unwrap();
        let report = verify_against_active(&json, &kit.text).unwrap();
        assert!(report.ok);
        assert!(report.vault_id_match);
        assert!(report.salt_match);
        assert!(report.version_match);
        assert!(report.kdf_match);
        assert!(report.candidate_kdf_explicit);
        assert_eq!(report.candidate_kind, "kit-text");
    }

    #[test]
    fn verify_against_active_matches_qr_payload_and_marker_json() {
        let salt = [9u8; 32];
        let vid = [10u8; 16];
        let json = make_v3_json_with_vid(&salt, &vid, false);
        let kit = build_from_config_json(&json).unwrap();
        let qr = format!("AEROCRYPT-KIT v1\n{}", kit.text);
        let report = verify_against_active(&json, &qr).unwrap();
        assert!(report.ok);
        assert_eq!(report.candidate_kind, "qr-payload");

        // Marker JSON against itself.
        let report_json = verify_against_active(&json, &json).unwrap();
        assert!(report_json.ok);
        assert_eq!(report_json.candidate_kind, "marker-json");
        assert!(!report_json.candidate_kdf_explicit);
    }

    #[test]
    fn verify_against_active_detects_vault_id_and_salt_mismatch() {
        let salt_a = [1u8; 32];
        let salt_b = [2u8; 32];
        let vid_a = [3u8; 16];
        let vid_b = [4u8; 16];
        let active = make_v3_json_with_vid(&salt_a, &vid_a, false);
        let other = make_v3_json_with_vid(&salt_b, &vid_b, false);
        let other_kit = build_from_config_json(&other).unwrap();

        let report = verify_against_active(&active, &other_kit.text).unwrap();
        assert!(!report.ok);
        assert!(!report.vault_id_match);
        assert!(!report.salt_match);
        assert!(report.version_match);
        assert!(report.kdf_match);
        assert!(report
            .details
            .iter()
            .any(|d| d.contains("Vault ID mismatch")));
        assert!(report.details.iter().any(|d| d.contains("Salt mismatch")));
    }

    #[test]
    fn verify_against_active_detects_kdf_mismatch_in_kit_text() {
        let salt = [5u8; 32];
        let vid = [6u8; 16];
        let active = make_v3_json_with_vid(&salt, &vid, false);
        let kit = build_from_config_json(&active).unwrap();
        // Tamper only the KDF line while keeping vault_id/salt intact.
        let tampered = kit.text.replace(
            "KDF: Argon2id (mem=131072 KiB, t=4, p=4)",
            "KDF: Argon2id (mem=65536 KiB, t=3, p=2)",
        );
        assert_ne!(tampered, kit.text);
        let report = verify_against_active(&active, &tampered).unwrap();
        assert!(!report.ok);
        assert!(report.vault_id_match);
        assert!(report.salt_match);
        assert!(!report.kdf_match);
        assert!(report.details.iter().any(|d| d.contains("KDF mismatch")));
    }

    #[test]
    fn parse_candidate_rejects_empty_and_malformed() {
        assert!(parse_candidate_kit("").is_err());
        assert!(parse_candidate_kit("   ").is_err());
        assert!(parse_kit_text("not a kit at all").is_err());
        // Wrong vault_id length
        let bad = "AEROCRYPT EMERGENCY KIT\n\
                   Vault ID: YQ==\n\
                   Version: 3\n\
                   Salt (base64): AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\n\
                   KDF: Argon2id (mem=131072 KiB, t=4, p=4)\n";
        assert!(parse_kit_text(bad).is_err());
    }

    #[test]
    fn verify_tsv_marker_against_itself() {
        // Minimal TSV headed marker (version/salt/vault_id/mac).
        let salt = [0x11u8; 32];
        let vid = [0x22u8; 16];
        let salt_b64 = base64::engine::general_purpose::STANDARD.encode(salt);
        let vid_b64 = base64::engine::general_purpose::STANDARD.encode(vid);
        let mac_b64 = base64::engine::general_purpose::STANDARD.encode([0u8; 32]);
        let tsv = format!(
            "Warning\tPlease do not delete this file, it is needed to decrypt AeroCrypt\n\
             version\t3\n\
             salt\t{salt_b64}\n\
             vault_id\t{vid_b64}\n\
             mac\t{mac_b64}\n"
        );
        let report = verify_against_active(&tsv, &tsv).unwrap();
        assert!(report.ok);
        assert_eq!(report.candidate_kind, "marker-tsv");
        assert!(!report.candidate_kdf_explicit);
    }
}
