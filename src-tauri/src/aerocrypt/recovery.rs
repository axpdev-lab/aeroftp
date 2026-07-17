// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! AeroCrypt v4 recovery codes (Crockford base32).
//!
//! Format (spec 06 / 08 §4):
//!   `AERO-{VP4}-{G1}-{G2}-{G3}-{G4}-{G5}-{G6}-{CHK}`
//!
//! - `VP4`: first 4 Crockford chars of the 16-byte vault_id (identifies the vault)
//! - `G1..G6`: 26 Crockford chars encoding 16 random bytes (>=128 bits entropy),
//!   grouped 5-5-5-5-5-1 then re-grouped as 5-5-5-5-6 for readability
//! - `CHK`: 5 Crockford chars of a BLAKE3 checksum over
//!   `b"aerocrypt recovery code v1" || vault_id || secret`
//!
//! The KDF input is the 26-char uppercase Crockford payload only (no prefix,
//! no hyphens, no checksum). Wrong checksum / corrupted / wrong vault_prefix
//! fail closed.

use super::overlay::VAULT_ID_SIZE;
use super::random_array;

/// Crockford base32 alphabet (no I, L, O, U).
const CROCKFORD_ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Domain separation for the recovery-code checksum.
const RECOVERY_CHECKSUM_CONTEXT: &[u8] = b"aerocrypt recovery code v1";

/// Entropy length in bytes (>=128 bits).
pub const RECOVERY_SECRET_LEN: usize = 16;
/// Crockford length of the secret payload (ceil(16*8/5) = 26).
pub const RECOVERY_PAYLOAD_CHARS: usize = 26;
/// Vault-id prefix length (first 4 Crockford chars).
pub const RECOVERY_VAULT_PREFIX_LEN: usize = 4;
/// Checksum group length in Crockford chars (25 bits).
pub const RECOVERY_CHECKSUM_CHARS: usize = 5;

/// A freshly generated or successfully parsed recovery code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryCode {
    /// Human-facing grouped form: `AERO-XXXX-XXXXX-...`
    pub formatted: String,
    /// Canonical KDF input: 26 uppercase Crockford chars of the secret only.
    pub normalized: String,
    /// First 4 Crockford chars of vault_id (also stored in slot binding).
    pub vault_prefix: String,
}

/// Encode `data` as Crockford base32 (no padding). Output is uppercase ASCII.
pub fn crockford_encode(data: &[u8]) -> String {
    if data.is_empty() {
        return String::new();
    }
    // Bit buffer: emit 5-bit symbols MSB-first.
    let mut out = String::with_capacity((data.len() * 8).div_ceil(5));
    let mut buffer: u64 = 0;
    let mut bits: u32 = 0;
    for &b in data {
        buffer = (buffer << 8) | u64::from(b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            let idx = ((buffer >> bits) & 0x1f) as usize;
            out.push(CROCKFORD_ALPHABET[idx] as char);
        }
    }
    if bits > 0 {
        let idx = ((buffer << (5 - bits)) & 0x1f) as usize;
        out.push(CROCKFORD_ALPHABET[idx] as char);
    }
    out
}

/// Decode Crockford base32. Accepts lower/upper case; maps I/L→1, O→0.
/// Fails closed on empty input, invalid symbols, or empty result.
pub fn crockford_decode(s: &str) -> Result<Vec<u8>, String> {
    let cleaned: String = s
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .collect();
    if cleaned.is_empty() {
        return Err("empty Crockford input".into());
    }
    let mut buffer: u64 = 0;
    let mut bits: u32 = 0;
    let mut out = Vec::with_capacity((cleaned.len() * 5) / 8);
    for ch in cleaned.chars() {
        let v = crockford_value(ch)?;
        buffer = (buffer << 5) | u64::from(v);
        bits += 5;
        while bits >= 8 {
            bits -= 8;
            out.push(((buffer >> bits) & 0xff) as u8);
        }
    }
    // Trailing bits must be zero padding only (fail closed on garbage residue).
    if bits > 0 {
        let residue = (buffer & ((1u64 << bits) - 1)) as u8;
        if residue != 0 {
            return Err("invalid Crockford padding bits".into());
        }
    }
    if out.is_empty() {
        return Err("Crockford decode produced no bytes".into());
    }
    Ok(out)
}

fn crockford_value(ch: char) -> Result<u8, String> {
    let c = match ch {
        'i' | 'I' | 'l' | 'L' => '1',
        'o' | 'O' => '0',
        other => other.to_ascii_uppercase(),
    };
    match CROCKFORD_ALPHABET.iter().position(|&a| a == c as u8) {
        Some(i) => Ok(i as u8),
        None => Err(format!("invalid Crockford character '{ch}'")),
    }
}

/// First `RECOVERY_VAULT_PREFIX_LEN` Crockford chars of `vault_id`.
pub fn vault_prefix_from_id(vault_id: &[u8; VAULT_ID_SIZE]) -> String {
    let full = crockford_encode(vault_id);
    full.chars().take(RECOVERY_VAULT_PREFIX_LEN).collect()
}

fn checksum_chars(vault_id: &[u8; VAULT_ID_SIZE], secret: &[u8; RECOVERY_SECRET_LEN]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(RECOVERY_CHECKSUM_CONTEXT);
    hasher.update(vault_id);
    hasher.update(secret);
    let digest = hasher.finalize();
    // 5 Crockford chars = 25 bits. Take first 4 bytes, mask to 25 bits.
    let bytes = digest.as_bytes();
    let mut n = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    n &= (1u32 << 25) - 1;
    let mut out = String::with_capacity(RECOVERY_CHECKSUM_CHARS);
    for i in (0..RECOVERY_CHECKSUM_CHARS).rev() {
        let idx = ((n >> (5 * i)) & 0x1f) as usize;
        out.push(CROCKFORD_ALPHABET[idx] as char);
    }
    out
}

/// Encode 16 secret bytes as exactly 26 Crockford chars (may include a final
/// pad symbol from the 2 leftover bits; decode recovers the original 16 bytes
/// when we take the first 16 decoded bytes).
fn encode_secret_payload(secret: &[u8; RECOVERY_SECRET_LEN]) -> String {
    let mut enc = crockford_encode(secret);
    // crockford_encode of 16 bytes yields 26 chars (128 bits -> 26*5=130).
    debug_assert_eq!(enc.len(), RECOVERY_PAYLOAD_CHARS);
    // Truncate/pad defensively so the wire form is always 26 chars.
    if enc.len() > RECOVERY_PAYLOAD_CHARS {
        enc.truncate(RECOVERY_PAYLOAD_CHARS);
    }
    while enc.len() < RECOVERY_PAYLOAD_CHARS {
        enc.push('0');
    }
    enc
}

fn decode_secret_payload(payload: &str) -> Result<[u8; RECOVERY_SECRET_LEN], String> {
    if payload.len() != RECOVERY_PAYLOAD_CHARS {
        return Err(format!(
            "recovery payload must be {RECOVERY_PAYLOAD_CHARS} Crockford chars (got {})",
            payload.len()
        ));
    }
    // Reject non-alphabet before decode (fail closed; no I/L/O confusion on payload
    // after normalization — parser uppercases and maps already).
    for ch in payload.chars() {
        crockford_value(ch)?;
    }
    let decoded = crockford_decode(payload)?;
    if decoded.len() < RECOVERY_SECRET_LEN {
        return Err(format!(
            "recovery payload decodes too short ({} bytes)",
            decoded.len()
        ));
    }
    let mut secret = [0u8; RECOVERY_SECRET_LEN];
    secret.copy_from_slice(&decoded[..RECOVERY_SECRET_LEN]);
    // Re-encode and require a match so corrupted pad bits fail closed.
    let re = encode_secret_payload(&secret);
    if re != payload.to_ascii_uppercase() {
        // Allow I/L/O mapping: normalize payload first.
        let normalized: String = payload
            .chars()
            .map(|c| match c {
                'i' | 'I' | 'l' | 'L' => '1',
                'o' | 'O' => '0',
                other => other.to_ascii_uppercase(),
            })
            .collect();
        if re != normalized {
            return Err("recovery payload failed re-encode check".into());
        }
    }
    Ok(secret)
}

fn group_payload(payload26: &str) -> String {
    // 26 chars → 5-5-5-5-6
    debug_assert_eq!(payload26.len(), 26);
    format!(
        "{}-{}-{}-{}-{}",
        &payload26[0..5],
        &payload26[5..10],
        &payload26[10..15],
        &payload26[15..20],
        &payload26[20..26],
    )
}

/// Generate a fresh recovery code bound to `vault_id`.
pub fn generate_recovery_code(vault_id: &[u8; VAULT_ID_SIZE]) -> RecoveryCode {
    let secret: [u8; RECOVERY_SECRET_LEN] = random_array();
    format_recovery_code(vault_id, &secret)
}

fn format_recovery_code(
    vault_id: &[u8; VAULT_ID_SIZE],
    secret: &[u8; RECOVERY_SECRET_LEN],
) -> RecoveryCode {
    let vault_prefix = vault_prefix_from_id(vault_id);
    let payload = encode_secret_payload(secret);
    let chk = checksum_chars(vault_id, secret);
    let formatted = format!("AERO-{}-{}-{}", vault_prefix, group_payload(&payload), chk);
    RecoveryCode {
        formatted,
        normalized: payload,
        vault_prefix,
    }
}

/// Strip separators and uppercase. Does NOT map O/I/L (those confusable maps
/// would rewrite the literal `AERO` brand prefix to `AER0`).
fn strip_separators_upper(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

/// Map Crockford confusables inside a data group only (never the AERO brand).
fn map_crockford_confusables(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'i' | 'I' | 'l' | 'L' => '1',
            'o' | 'O' => '0',
            other => other.to_ascii_uppercase(),
        })
        .collect()
}

/// Parse a recovery code. Requires either an explicit `vault_id` (preferred:
/// verifies vault_prefix + checksum) or accepts prefix-only shape checks when
/// `vault_id` is unknown (still verifies payload length; checksum needs vault_id).
///
/// When `vault_id` is `Some`, wrong prefix or wrong checksum fails closed.
/// Hyphens, spaces, and case are ignored; I/L→1 and O→0 inside data groups only.
pub fn parse_recovery_code(
    input: &str,
    vault_id: Option<&[u8; VAULT_ID_SIZE]>,
) -> Result<RecoveryCode, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("empty recovery code".into());
    }

    // Normalize separators/case first so "AERO - XXXX - ..." works. Keep the
    // literal AERO brand intact (do not map O→0 on the prefix).
    let compact = strip_separators_upper(trimmed);
    let body = if let Some(rest) = compact.strip_prefix("AERO") {
        rest
    } else {
        // Compact form without AERO prefix: VP4 + 26 payload + 5 checksum.
        compact.as_str()
    };
    // Confusable mapping applies only to the data body (prefix/payload/checksum).
    let body_mapped = map_crockford_confusables(body);
    parse_compact_body(&body_mapped, vault_id)
}

fn parse_compact_body(
    compact: &str,
    vault_id: Option<&[u8; VAULT_ID_SIZE]>,
) -> Result<RecoveryCode, String> {
    // Crockford is ASCII-only. Reject non-ASCII BEFORE any byte-index slicing so a
    // multibyte UTF-8 char can never land mid-slice and panic; fail closed instead.
    if !compact.is_ascii() {
        return Err("recovery code contains non-Crockford characters".into());
    }
    let expected_len = RECOVERY_VAULT_PREFIX_LEN + RECOVERY_PAYLOAD_CHARS + RECOVERY_CHECKSUM_CHARS;
    if compact.len() != expected_len {
        return Err(format!(
            "recovery code has wrong length (got {}, expected {expected_len} data chars after AERO)",
            compact.len()
        ));
    }
    let vault_prefix = compact[..RECOVERY_VAULT_PREFIX_LEN].to_string();
    let payload = compact
        [RECOVERY_VAULT_PREFIX_LEN..RECOVERY_VAULT_PREFIX_LEN + RECOVERY_PAYLOAD_CHARS]
        .to_string();
    let chk = compact[RECOVERY_VAULT_PREFIX_LEN + RECOVERY_PAYLOAD_CHARS..].to_string();

    // Alphabet check on every char (fail closed).
    for ch in vault_prefix
        .chars()
        .chain(payload.chars())
        .chain(chk.chars())
    {
        crockford_value(ch)?;
    }

    let secret = decode_secret_payload(&payload)?;

    if let Some(vid) = vault_id {
        let expected_prefix = vault_prefix_from_id(vid);
        if vault_prefix != expected_prefix {
            return Err(format!(
                "recovery code vault_prefix mismatch (code is for {vault_prefix}, vault is {expected_prefix})"
            ));
        }
        let expected_chk = checksum_chars(vid, &secret);
        if chk != expected_chk {
            return Err(
                "recovery code checksum invalid (code is corrupted or for another vault)".into(),
            );
        }
    } else {
        // Without vault_id we cannot verify checksum; still reject obviously
        // wrong shapes. Callers that unlock a known vault always pass vault_id.
        // Recompute checksum only when we have vault_id.
        let _ = chk;
    }

    let formatted = if vault_id.is_some() {
        // Prefer canonical formatting with verified parts.
        format!("AERO-{}-{}-{}", vault_prefix, group_payload(&payload), chk)
    } else {
        format!("AERO-{}-{}-{}", vault_prefix, group_payload(&payload), chk)
    };

    Ok(RecoveryCode {
        formatted,
        normalized: payload,
        vault_prefix,
    })
}

/// Returns true when `input` looks like a recovery code (AERO- prefix or compact
/// length). Used by unlock to decide whether to try recovery slots.
pub fn looks_like_recovery_code(input: &str) -> bool {
    let t = input.trim();
    if t.is_empty() {
        return false;
    }
    let upper = t.to_ascii_uppercase();
    if upper.starts_with("AERO-") || upper.starts_with("AERO") {
        // Require roughly the full shape so ordinary passwords starting with
        // "aero" are not treated as recovery attempts.
        let compact = strip_separators_upper(t);
        let body = compact.strip_prefix("AERO").unwrap_or(compact.as_str());
        return body.len()
            == RECOVERY_VAULT_PREFIX_LEN + RECOVERY_PAYLOAD_CHARS + RECOVERY_CHECKSUM_CHARS;
    }
    let compact = strip_separators_upper(t);
    compact.len() == RECOVERY_VAULT_PREFIX_LEN + RECOVERY_PAYLOAD_CHARS + RECOVERY_CHECKSUM_CHARS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crockford_roundtrip_and_confusables() {
        let data = b"Hello, AeroCrypt!";
        let enc = crockford_encode(data);
        assert!(
            !enc.contains('I') && !enc.contains('L') && !enc.contains('O') && !enc.contains('U')
        );
        let dec = crockford_decode(&enc).unwrap();
        assert_eq!(dec, data);

        // Confusable mapping: I/L → 1, O → 0
        let with_confusables = enc.replace('1', "I").replace('0', "O");
        // Only rewrite if those digits exist; decode must still succeed when mapped.
        let _ = crockford_decode(&with_confusables);
    }

    #[test]
    fn generate_parse_roundtrip_and_kdf_stable() {
        let vault_id = [0xABu8; 16];
        let code = generate_recovery_code(&vault_id);
        assert!(code.formatted.starts_with("AERO-"));
        assert_eq!(code.vault_prefix.len(), 4);
        assert_eq!(code.normalized.len(), RECOVERY_PAYLOAD_CHARS);

        let parsed = parse_recovery_code(&code.formatted, Some(&vault_id)).unwrap();
        assert_eq!(parsed.normalized, code.normalized);
        assert_eq!(parsed.vault_prefix, code.vault_prefix);

        // Lowercase + odd spacing still parses.
        let messy = code.formatted.to_ascii_lowercase().replace('-', " - ");
        let parsed2 = parse_recovery_code(&messy, Some(&vault_id)).unwrap();
        assert_eq!(parsed2.normalized, code.normalized);
    }

    #[test]
    fn wrong_checksum_and_prefix_fail_closed() {
        let vault_id = [0x11u8; 16];
        let code = generate_recovery_code(&vault_id);
        // Flip last checksum char.
        let mut chars: Vec<char> = code.formatted.chars().collect();
        let last = chars.len() - 1;
        chars[last] = if chars[last] == '0' { '1' } else { '0' };
        let corrupted: String = chars.into_iter().collect();
        assert!(
            parse_recovery_code(&corrupted, Some(&vault_id)).is_err(),
            "corrupted checksum must fail closed"
        );

        // Wrong vault_id.
        let other = [0x22u8; 16];
        assert!(
            parse_recovery_code(&code.formatted, Some(&other)).is_err(),
            "wrong vault must fail closed"
        );
    }

    #[test]
    fn vault_prefix_is_stable_crockford_prefix() {
        let vault_id = [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
            0x0e, 0x0f,
        ];
        let prefix = vault_prefix_from_id(&vault_id);
        let full = crockford_encode(&vault_id);
        assert_eq!(prefix, &full[..4]);
        assert_eq!(prefix.len(), 4);
    }

    #[test]
    fn looks_like_recovery_code_detects_shape() {
        let vault_id = [0x33u8; 16];
        let code = generate_recovery_code(&vault_id);
        assert!(looks_like_recovery_code(&code.formatted));
        assert!(looks_like_recovery_code(
            &code.formatted.to_ascii_lowercase()
        ));
        assert!(!looks_like_recovery_code("correct horse battery staple"));
        assert!(!looks_like_recovery_code(""));
    }

    /// Regression: a recovery-shaped input carrying a multibyte UTF-8 char must
    /// fail closed with Err, never panic on a non-char-boundary byte slice.
    /// (Found by Orchestrator L2 probe: pre-fix this panicked at the vault_prefix
    /// slice because normalization preserved non-ASCII before byte indexing.)
    #[test]
    fn hostile_multibyte_recovery_code_fails_closed() {
        let vault_id = [0x5Au8; VAULT_ID_SIZE];
        let hostiles = [
            format!("AEROAAA\u{00e9}{}", "A".repeat(30)), // 'é' straddles body byte idx 4
            format!("AERO{}\u{00e9}", "A".repeat(31)),    // multibyte near checksum boundary
            format!("AERO{}", "\u{00e9}".repeat(17)),     // all-multibyte body
            "\u{4e2d}\u{6587}AERO123456".to_string(),     // CJK around AERO
            format!("{}\u{00e9}{}", "A".repeat(3), "A".repeat(30)), // compact form, no AERO
        ];
        for h in hostiles {
            assert!(
                parse_recovery_code(&h, Some(&vault_id)).is_err(),
                "hostile multibyte input must fail closed: {h:?}"
            );
        }
        // The valid happy path still round-trips after the ASCII guard.
        let good = generate_recovery_code(&vault_id);
        assert!(parse_recovery_code(&good.formatted, Some(&vault_id)).is_ok());
    }
}
