//! AeroCrypt transparent overlay codec (file format `AECR`).
//!
//! One local file maps to one remote object: per-file encrypted blobs with
//! obfuscated names, the shape that keeps an encrypted scope browsable and
//! syncable object by object (master plan 3.7).
//!
//! - **v3 (current object codec)** is built on the shared [`crate::aerocrypt`]
//!   engine: AES-256-GCM-SIV content under a per-file random DEK that is wrapped
//!   with AES-256-KW under the Argon2id-128 master KEK. Every block binds the
//!   block index **and the total block count** as AAD, and the total count is
//!   also carried (authenticated) in the header, so truncation and append are
//!   detected and fail closed. The config carries a key-bound MAC so a tampered
//!   `version`/`salt` is rejected on unlock (closes the unauthenticated-config
//!   downgrade and gives a clean wrong-password signal).
//! - **v4 (keyslots)** replaces only the key-management layer: a Volume Key
//!   wrapped by independent slots (passphrase / keyfile / recovery / ...). The
//!   per-file object layout stays the v3 codec under OMK (09 §7a). Parser is
//!   F9-hardened; `config_mac` covers exact stored header bytes.
//! - **v2 / v1 (legacy)** stay **read-only**: existing overlays keep decrypting
//!   transparently, but new objects are always written as v3. v2 = GCM-SIV per
//!   the shared engine without the length binding; v1 = original plain
//!   AES-256-GCM (HKDF per-file key, Argon2id-64).
//!
//! Filenames use AES-256-SIV via [`crate::aerocrypt::names`] in every version.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce as AesNonce};
use base64::Engine as _;
use hkdf::Hkdf;
use sha2::Sha256;
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

use super::keyslots::{
    AndMember, Argon2Params, Slot, SlotBinding, SlotType, MAX_ARGON2_LANES, MAX_ARGON2_MEM_KIB,
    MAX_ARGON2_TIME, MAX_SLOTS, MIN_ARGON2_LANES, MIN_ARGON2_MEM_KIB, MIN_ARGON2_TIME, VERSION_V4,
};
use super::{
    decrypt_with_aad, derive_base_kek, encrypt_with_aad, hkdf_expand, random_array, unwrap_key,
    wrap_key, KEY_SIZE, NONCE_SIZE, SALT_SIZE, WRAPPED_KEY_SIZE,
};

/// Magic bytes for an AeroCrypt-encrypted file.
pub const MAGIC: &[u8; 4] = b"AECR";
/// Legacy plain-GCM format (read-only).
pub const VERSION_V1: u8 = 1;
/// Legacy GCM-SIV format without length binding (read-only).
pub const VERSION_V2: u8 = 2;
/// Current format: GCM-SIV + AES-KW + per-block index/total binding.
pub const VERSION_V3: u8 = 3;
// VERSION_V4 lives in keyslots.rs (shared with AAD tags); re-export is the const import above.
/// Streaming block size (64 KiB plaintext per AEAD block).
const BLOCK_SIZE: usize = 64 * 1024;
const SALT_V1_SIZE: usize = 16;
const SALT_V2_SIZE: usize = 32; // == super::SALT_SIZE
const SALT_V3_SIZE: usize = 32;
/// Config MAC output length (HKDF-Expand, 32 bytes) for v3 and v4.
pub(crate) const CONFIG_MAC_SIZE: usize = 32;
/// Max base64 characters accepted for a single field before decode (F9 OOM guard).
const MAX_B64_FIELD_LEN: usize = 512;
/// Max base64 characters for a wrap blob (nonce || ct || tag is ~60 raw bytes).
const MAX_B64_WRAP_LEN: usize = 256;
/// Max base64 characters for a salt / MAC field (32 raw bytes).
const MAX_B64_SALT_LEN: usize = 128;
/// Max base64 characters for vault_id (16 raw bytes).
const MAX_B64_VAULT_ID_LEN: usize = 64;
/// Random per-vault identifier length (bytes). Emitted in every new v3 config
/// from Tier 1 onward; seeds rollback pinning, Emergency Kits, and diagnostics.
pub const VAULT_ID_SIZE: usize = 16;

/// Current write name for the headed marker (lean TSV format).
pub const CRYPT_CONFIG_WRITE_NAME: &str = ".aerocrypt.tsv";
/// Legacy read name (still supported forever for read-both migration window).
pub const CRYPT_CONFIG_LEGACY_NAME: &str = ".aeroftp-crypt.json";

/// Salt source mode for a v3 vault.
/// - PerVault (default, absent in serialised form): fresh random salt per vault (current behaviour).
/// - DefaultV1: opt-in public constant salt (AEROCRYPT_DEFAULT_SALT_V1). Password alone
///   reconstructs the master key (rclone-analog headerless portability).
///
/// The mode is recorded in the keystore profile for headerless vaults and (when DefaultV1)
/// emitted in the headed marker. It is bound into the config MAC when DefaultV1 so a
/// downgrade/upgrade is tamper-evident.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SaltMode {
    #[default]
    PerVault,
    DefaultV1,
}

impl SaltMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            SaltMode::PerVault => "per-vault",
            SaltMode::DefaultV1 => "default-v1",
        }
    }
}

impl std::str::FromStr for SaltMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "per-vault" | "PerVault" => Ok(SaltMode::PerVault),
            "default-v1" | "DefaultV1" => Ok(SaltMode::DefaultV1),
            other => Err(format!("unknown salt_mode: {}", other)),
        }
    }
}

/// Legacy Argon2id parameters for v1 overlays (balanced 64 MiB / t3 / p4).
const ARGON2_V1_MEM_KIB: u32 = 65536;
const ARGON2_V1_TIME: u32 = 3;
const ARGON2_V1_LANES: u32 = 4;

/// Domain-separating AAD prefix for v2 content blocks; the block index is
/// appended so blocks cannot be reordered within a file.
const V2_BLOCK_AAD_PREFIX: &[u8] = b"AeroCrypt overlay v2 block";
/// Domain-separating AAD prefix for v3 content blocks; the block index **and**
/// the total block count are appended so reorder, truncation, and append all
/// fail closed.
const V3_BLOCK_AAD_PREFIX: &[u8] = b"AeroCrypt overlay v3 block";
/// Domain-separating label for the v3 config MAC (key-bound config integrity).
const V3_CONFIG_MAC_LABEL: &[u8] = b"AeroCrypt overlay v3 config MAC";
/// FROZEN suffix appended to the v3 config-MAC info string for KEYFILE vaults
/// only (F3). Password-only vaults keep the original info string byte-for-byte,
/// so their MAC is unchanged and old readers keep verifying. No pre-keyfile
/// reader can open a keyfile vault, so extending the info here is back-compat
/// safe and binds the keyfile requirement + vault_id against tampering.
const V3_KEYFILE_MAC_SUFFIX: &[u8] = b"|kdf_inputs=password+keyfile|vault_id=";
/// Domain-separating label for the v4 config-mac key derivation (spec 08 §8).
/// `mac_key = HKDF-Expand(VK, this_label || vault_id)`.
const V4_CONFIG_MAC_KEY_LABEL: &[u8] = b"aerocrypt v4 config-mac";

/// GCM tag length added to every AEAD block.
const GCM_TAG: usize = 16;

/// A parsed `.aeroftp-crypt.json` / `.aerocrypt.tsv` overlay configuration.
#[derive(Debug, Clone)]
pub enum OverlayConfig {
    V1 {
        salt: [u8; SALT_V1_SIZE],
    },
    V2 {
        salt: [u8; SALT_V2_SIZE],
    },
    V3 {
        salt: [u8; SALT_V3_SIZE],
        mac: [u8; CONFIG_MAC_SIZE],
        /// Present in every config written from Tier 1 on; `None` for older v3
        /// configs. Authenticated by the config MAC only for keyfile vaults.
        vault_id: Option<[u8; VAULT_ID_SIZE]>,
        /// True when `kdf_inputs` includes a keyfile, i.e. unlock needs the
        /// keyfile digest in addition to the password.
        requires_keyfile: bool,
        /// Salt source. Absent in on-disk legacy v3 markers => PerVault (back-compat).
        /// When DefaultV1 the salt bytes are the public constant (reconstructible).
        salt_mode: SaltMode,
    },
    /// AECR v4 keyslot vault (Tier 2). Object codec stays v3 under OMK; key
    /// management is the slot layer (spec 08, migration 09 §7a).
    V4 {
        vault_id: [u8; VAULT_ID_SIZE],
        epoch: u32,
        /// Nonce-prefixed GCM-SIV(epoch_key -> VK).
        vk_wrap: Vec<u8>,
        /// Nonce-prefixed GCM-SIV(HKDF(VK, omk-wrap) -> OMK); immutable across epochs.
        omk_wrap: Vec<u8>,
        /// Known slots only; unknown type tags are dropped from this list but
        /// remain covered by `config_mac` over the exact stored header bytes.
        slots: Vec<Slot>,
        /// Top-level config MAC over exact stored bytes with this field excised
        /// (spec 08 section 8). Verified after unlock obtains VK (T4).
        config_mac: [u8; CONFIG_MAC_SIZE],
    },
}

impl OverlayConfig {
    pub fn version(&self) -> u8 {
        match self {
            OverlayConfig::V1 { .. } => VERSION_V1,
            OverlayConfig::V2 { .. } => VERSION_V2,
            OverlayConfig::V3 { .. } => VERSION_V3,
            OverlayConfig::V4 { .. } => VERSION_V4,
        }
    }

    /// True for legacy formats that are kept readable but never written.
    pub fn is_read_only(&self) -> bool {
        !matches!(self, OverlayConfig::V3 { .. } | OverlayConfig::V4 { .. })
    }

    /// True when this overlay requires a keyfile in addition to the password.
    /// Always false for legacy v1/v2. For v4, unlock (T4) inspects slots; this
    /// helper stays v3-only so callers do not assume a single keyfile factor.
    pub fn requires_keyfile(&self) -> bool {
        matches!(
            self,
            OverlayConfig::V3 {
                requires_keyfile: true,
                ..
            }
        )
    }

    /// The vault id, when present (v3 from Tier 1 on; always for v4).
    pub fn vault_id(&self) -> Option<[u8; VAULT_ID_SIZE]> {
        match self {
            OverlayConfig::V3 { vault_id, .. } => *vault_id,
            OverlayConfig::V4 { vault_id, .. } => Some(*vault_id),
            _ => None,
        }
    }

    /// Build a bare v3 config carrying only the salt, for the internal
    /// derive-then-init bootstrap (the MAC and metadata are filled in when the
    /// real config JSON is written). Never persisted.
    pub fn v3_bootstrap(salt: [u8; SALT_V3_SIZE]) -> Self {
        OverlayConfig::V3 {
            salt,
            mac: [0u8; CONFIG_MAC_SIZE],
            vault_id: None,
            requires_keyfile: false,
            salt_mode: SaltMode::PerVault,
        }
    }
}

// --- Key derivation -------------------------------------------------------

/// Legacy v1 master-key derivation (Argon2id 64 MiB / t3 / p4, 16-byte salt).
fn derive_master_key_v1(
    password: &str,
    salt: &[u8; SALT_V1_SIZE],
) -> Result<[u8; KEY_SIZE], String> {
    let params = argon2::Params::new(
        ARGON2_V1_MEM_KIB,
        ARGON2_V1_TIME,
        ARGON2_V1_LANES,
        Some(KEY_SIZE),
    )
    .map_err(|e| format!("Argon2 params: {e}"))?;
    let argon = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let mut key = [0u8; KEY_SIZE];
    argon
        .hash_password_into(password.as_bytes(), salt, &mut key)
        .map_err(|e| format!("Argon2 derive: {e}"))?;
    Ok(key)
}

/// Derive the overlay master key for the given config version (password only).
pub fn derive_master_key(cfg: &OverlayConfig, password: &str) -> Result<[u8; KEY_SIZE], String> {
    derive_master_key_with_keyfile(cfg, password, None)
}

/// Derive the overlay master key with an OPTIONAL keyfile digest mixed into the
/// KDF (Tier 1). `None` is byte-identical to the password-only path, so existing
/// vaults are unaffected. Keyfiles apply to v3 only; a v1/v2 config ignores the
/// digest (callers reject `--keyfile` against legacy overlays upstream).
///
/// v4 vaults unlock via keyslots (slot_key -> epoch_key -> VK -> OMK); this path
/// fails closed so a mistaken call never derives a silent wrong key.
pub fn derive_master_key_with_keyfile(
    cfg: &OverlayConfig,
    password: &str,
    keyfile_digest: Option<&[u8; KEY_SIZE]>,
) -> Result<[u8; KEY_SIZE], String> {
    match cfg {
        OverlayConfig::V1 { salt } => derive_master_key_v1(password, salt),
        OverlayConfig::V2 { salt } | OverlayConfig::V3 { salt, .. } => match keyfile_digest {
            // No keyfile: byte-identical to the classic password-only KDF.
            None => derive_base_kek(password, salt),
            some => super::derive_base_kek_with_keyfile(password, some, salt),
        },
        OverlayConfig::V4 { .. } => {
            Err("v4 vaults unlock via keyslots, not derive_master_key".to_string())
        }
    }
}

/// Verify the key-bound config MAC for a v3 overlay. Returns `Err` when the
/// password is wrong or the config (`version`/`salt`) was tampered with.
/// Legacy v1/v2 carry no MAC and always pass (they fail closed later on the
/// AEAD instead).
///
/// v4 uses a stored-bytes MAC verified after keyslot unlock; call
/// [`verify_config_mac_v4`] with the raw header and VK instead.
pub fn verify_config_mac(cfg: &OverlayConfig, master_key: &[u8; KEY_SIZE]) -> Result<(), String> {
    match cfg {
        OverlayConfig::V3 {
            salt,
            mac,
            vault_id,
            requires_keyfile,
            salt_mode,
        } => {
            let expected = compute_config_mac_v3(
                master_key,
                salt,
                *requires_keyfile,
                vault_id.as_ref(),
                *salt_mode,
            )?;
            if expected.ct_eq(mac).into() {
                Ok(())
            } else if *requires_keyfile {
                Err("wrong password, wrong keyfile, or tampered crypt config".to_string())
            } else {
                Err("wrong password or tampered crypt config".to_string())
            }
        }
        OverlayConfig::V4 { .. } => Err(
            "v4 vaults verify config_mac via verify_config_mac_v4 after keyslot unlock".to_string(),
        ),
        _ => Ok(()),
    }
}

/// Compute the v3 config MAC: a key-bound PRF over the security-relevant
/// parameters (label, version, block size, Argon2 profile, salt). Because the
/// master key already depends on the salt, an attacker who rewrites the config
/// cannot forge a matching MAC without the password.
///
/// For KEYFILE vaults (`requires_keyfile`) the info string is extended with a
/// FROZEN suffix binding the keyfile requirement and the vault_id (F3). This is
/// back-compat safe: password-only vaults produce the exact original MAC, and no
/// pre-keyfile reader can open a keyfile vault anyway.
///
/// For default-salt vaults we append a second FROZEN suffix binding the mode.
/// This makes a mode flip (default <-> per-vault) on an existing marker
/// tamper-evident. Per-vault vaults emit the exact pre-default-salt MAC bytes.
fn compute_config_mac_v3(
    master_key: &[u8; KEY_SIZE],
    salt: &[u8; SALT_V3_SIZE],
    requires_keyfile: bool,
    vault_id: Option<&[u8; VAULT_ID_SIZE]>,
    salt_mode: SaltMode,
) -> Result<[u8; CONFIG_MAC_SIZE], String> {
    let mut info = Vec::with_capacity(V3_CONFIG_MAC_LABEL.len() + 1 + 4 + 12 + SALT_V3_SIZE);
    info.extend_from_slice(V3_CONFIG_MAC_LABEL);
    info.push(VERSION_V3);
    info.extend_from_slice(&(BLOCK_SIZE as u32).to_le_bytes());
    info.extend_from_slice(&super::argon2_mem_kib().to_le_bytes());
    info.extend_from_slice(&super::argon2_time().to_le_bytes());
    info.extend_from_slice(&super::argon2_lanes().to_le_bytes());
    info.extend_from_slice(salt);
    if requires_keyfile {
        let vid = vault_id.ok_or("keyfile vault requires a vault_id for the config MAC")?;
        info.extend_from_slice(V3_KEYFILE_MAC_SUFFIX);
        info.extend_from_slice(vid);
    }
    if salt_mode == SaltMode::DefaultV1 {
        // FROZEN suffix for default-salt binding (domain-separated, versioned).
        // Per-vault vaults must produce identical MAC bytes as before this feature.
        const V3_DEFAULT_SALT_MAC_SUFFIX: &[u8] = b"|salt_mode=default-v1";
        info.extend_from_slice(V3_DEFAULT_SALT_MAC_SUFFIX);
    }
    hkdf_expand::<CONFIG_MAC_SIZE>(master_key, &info)
}

/// Compute the v4 config MAC over the exact stored header bytes with the
/// `config_mac` field already excised (spec 08 section 8).
///
/// ```text
/// mac_key     = HKDF-Expand(VK, "aerocrypt v4 config-mac" || vault_id)
/// config_mac  = HKDF-Expand(mac_key, stored_header_without_mac)
/// ```
///
/// Hand-built only: never re-serialize the header. Callers pass the output of
/// [`excise_config_mac_field`] (or an equivalent fixed-rule excision).
pub fn compute_config_mac_v4(
    vk: &[u8; KEY_SIZE],
    vault_id: &[u8; VAULT_ID_SIZE],
    stored_header_without_mac: &[u8],
) -> Result<[u8; CONFIG_MAC_SIZE], String> {
    let mut mac_key_info = Vec::with_capacity(V4_CONFIG_MAC_KEY_LABEL.len() + VAULT_ID_SIZE);
    mac_key_info.extend_from_slice(V4_CONFIG_MAC_KEY_LABEL);
    mac_key_info.extend_from_slice(vault_id);
    let mac_key = hkdf_expand::<KEY_SIZE>(vk, &mac_key_info)?;
    hkdf_expand::<CONFIG_MAC_SIZE>(&mac_key, stored_header_without_mac)
}

/// Verify a v4 config_mac against the raw stored header (before or after any
/// parse). Bootstrap circularity (08 §5 step 5): call only after unlock yields VK.
pub fn verify_config_mac_v4(
    vk: &[u8; KEY_SIZE],
    vault_id: &[u8; VAULT_ID_SIZE],
    stored_header: &str,
    expected_mac: &[u8; CONFIG_MAC_SIZE],
) -> Result<(), String> {
    let without = excise_config_mac_field(stored_header)?;
    let expected = compute_config_mac_v4(vk, vault_id, without.as_bytes())?;
    if expected.ct_eq(expected_mac).into() {
        Ok(())
    } else {
        Err("wrong credential or tampered v4 crypt config".to_string())
    }
}

/// Excise the `config_mac` field from a stored v4 JSON header by a fixed textual
/// rule (not re-serialization). The MAC covers the exact remaining bytes.
///
/// Rule (JSON):
/// 1. Locate the first occurrence of the key substring `"config_mac"`.
/// 2. From that key's opening quote, consume optional whitespace, `:`, optional
///    whitespace, then a JSON string value (`"..."` with no escape handling for
///    base64 payloads which never need escapes).
/// 3. If a comma immediately follows the value (after optional whitespace),
///    remove that trailing comma with the field.
/// 4. Else, if a comma immediately precedes the key (after optional whitespace
///    walking backward), remove that preceding comma so objects stay valid.
/// 5. All other bytes (including whitespace) are left intact.
fn excise_config_mac_field(raw: &str) -> Result<String, String> {
    const KEY: &str = "\"config_mac\"";
    let key_pos = raw
        .find(KEY)
        .ok_or_else(|| "v4 header missing config_mac field for MAC excision".to_string())?;

    // Walk forward past key, colon, and the string value.
    let after_key = key_pos + KEY.len();
    let bytes = raw.as_bytes();
    let mut i = after_key;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= bytes.len() || bytes[i] != b':' {
        return Err("v4 config_mac field missing ':' after key".to_string());
    }
    i += 1;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= bytes.len() || bytes[i] != b'"' {
        return Err("v4 config_mac value is not a JSON string".to_string());
    }
    i += 1; // open quote of value
    while i < bytes.len() && bytes[i] != b'"' {
        // Base64 values have no escapes; reject backslash to stay strict.
        if bytes[i] == b'\\' {
            return Err("v4 config_mac value has unexpected escape".to_string());
        }
        i += 1;
    }
    if i >= bytes.len() {
        return Err("v4 config_mac value string not terminated".to_string());
    }
    i += 1; // close quote of value
    let value_end = i;

    // Prefer removing a trailing comma after the value.
    let mut trail = value_end;
    while trail < bytes.len() && bytes[trail].is_ascii_whitespace() {
        trail += 1;
    }
    let (field_start, field_end) = if trail < bytes.len() && bytes[trail] == b',' {
        (key_pos, trail + 1)
    } else {
        // No trailing comma: remove a preceding comma if present.
        let mut pre = key_pos;
        while pre > 0 && bytes[pre - 1].is_ascii_whitespace() {
            pre -= 1;
        }
        if pre > 0 && bytes[pre - 1] == b',' {
            (pre - 1, value_end)
        } else {
            (key_pos, value_end)
        }
    };

    let mut out = String::with_capacity(raw.len() - (field_end - field_start));
    out.push_str(&raw[..field_start]);
    out.push_str(&raw[field_end..]);
    Ok(out)
}

// --- v1 legacy content path (read-only) -----------------------------------

fn derive_file_key_v1(
    master_key: &[u8; KEY_SIZE],
    nonce: &[u8; NONCE_SIZE],
) -> Result<Zeroizing<[u8; KEY_SIZE]>, String> {
    let hk = Hkdf::<Sha256>::new(Some(nonce), master_key);
    let mut file_key = Zeroizing::new([0u8; KEY_SIZE]);
    hk.expand(b"aeroftp-crypt-file-key", file_key.as_mut())
        .map_err(|_| "HKDF file-key expand failed".to_string())?;
    Ok(file_key)
}

fn decrypt_data_v1(master_key: &[u8; KEY_SIZE], ciphertext: &[u8]) -> Result<Vec<u8>, String> {
    let header = 4 + 1 + NONCE_SIZE;
    if ciphertext.len() < header {
        return Err("AeroCrypt v1 data too short".into());
    }
    let master_nonce: [u8; NONCE_SIZE] = ciphertext[5..header]
        .try_into()
        .map_err(|_| "AeroCrypt v1 header truncated".to_string())?;
    let file_key = derive_file_key_v1(master_key, &master_nonce)?;
    let cipher = Aes256Gcm::new((&*file_key).into());

    let data = &ciphertext[header..];
    let block_cipher_size = BLOCK_SIZE + GCM_TAG;
    let mut plaintext = Vec::with_capacity(data.len());
    let mut block_idx = 0usize;
    let mut pos = 0usize;
    while pos < data.len() {
        let end = (pos + block_cipher_size).min(data.len());
        let block = &data[pos..end];
        let mut block_nonce = master_nonce;
        let idx_bytes = (block_idx as u32).to_le_bytes();
        for i in 0..4 {
            block_nonce[i] ^= idx_bytes[i];
        }
        let nonce = AesNonce::from_slice(&block_nonce);
        let decrypted = cipher
            .decrypt(nonce, block)
            .map_err(|_| format!("AeroCrypt v1 decrypt failed at block {block_idx}"))?;
        plaintext.extend_from_slice(&decrypted);
        pos = end;
        block_idx += 1;
    }
    Ok(plaintext)
}

// --- v2 legacy content path (read-only) -----------------------------------

fn v2_block_aad(block_index: u64) -> Vec<u8> {
    let mut aad = Vec::with_capacity(V2_BLOCK_AAD_PREFIX.len() + 8);
    aad.extend_from_slice(V2_BLOCK_AAD_PREFIX);
    aad.extend_from_slice(&block_index.to_le_bytes());
    aad
}

/// Ciphertext length of a full GCM-SIV block: per-block nonce + plaintext + tag.
const FULL_BLOCK_CIPHER: usize = NONCE_SIZE + BLOCK_SIZE + GCM_TAG;

/// Recover the plaintext length of a v3 AeroCrypt container from its on-wire
/// (ciphertext) length, WITHOUT reading the object.
///
/// The v3 layout is a fixed [`V3_HEADER_LEN`] header followed by GCM-SIV blocks;
/// each block adds a per-block nonce + tag ([`FULL_BLOCK_CIPHER`] for a full
/// [`BLOCK_SIZE`] block), so the mapping is deterministic and reversible from the
/// length alone (mirrors rclone's deterministic overhead map). This lets `stat`
/// and `size` report the DECRYPTED size for a v3 overlay, so the browser size
/// column and AeroSync size compares converge instead of re-flagging on every run.
///
/// Legacy v1/v2 containers use a different header and are NOT handled here (they
/// are read-only and their size stays deferred). A length that cannot correspond
/// to a well-formed v3 container (too short, or a final block below the per-block
/// overhead) is clamped rather than erroring: this feeds size-only UI/compare and
/// must never panic on a foreign or truncated object.
pub fn v3_decrypted_size(ciphertext_len: u64) -> u64 {
    let header = V3_HEADER_LEN as u64;
    let full_block = FULL_BLOCK_CIPHER as u64;
    let block = BLOCK_SIZE as u64;
    let per_block_overhead = (NONCE_SIZE + GCM_TAG) as u64;
    let Some(data) = ciphertext_len.checked_sub(header) else {
        // Too short to be a v3 container: leave the length unchanged.
        return ciphertext_len;
    };
    let full_blocks = data / full_block;
    let rem = data % full_block;
    let mut plain = full_blocks.saturating_mul(block);
    if rem > 0 {
        // Final partial block = nonce + partial_plaintext + tag; clamp a
        // malformed sub-overhead remainder to zero rather than underflowing.
        plain += rem.saturating_sub(per_block_overhead);
    }
    plain
}

fn decrypt_data_v2(master_key: &[u8; KEY_SIZE], ciphertext: &[u8]) -> Result<Vec<u8>, String> {
    let header = 4 + 1 + WRAPPED_KEY_SIZE;
    if ciphertext.len() < header {
        return Err("AeroCrypt v2 data too short".into());
    }
    let wrapped: [u8; WRAPPED_KEY_SIZE] = ciphertext[5..header]
        .try_into()
        .map_err(|_| "AeroCrypt v2 header truncated".to_string())?;
    let dek = Zeroizing::new(unwrap_key(master_key, &wrapped)?);

    let data = &ciphertext[header..];
    let mut plaintext = Vec::with_capacity(data.len());
    let mut block_idx = 0u64;
    let mut pos = 0usize;
    while pos < data.len() {
        let end = (pos + FULL_BLOCK_CIPHER).min(data.len());
        let block = &data[pos..end];
        let pt = decrypt_with_aad(&dek, block, &v2_block_aad(block_idx))?;
        plaintext.extend_from_slice(&pt);
        pos = end;
        block_idx += 1;
    }
    Ok(plaintext)
}

// --- v3 content path (current; length-bound) -------------------------------

fn v3_block_aad(block_index: u64, total_blocks: u64) -> Vec<u8> {
    let mut aad = Vec::with_capacity(V3_BLOCK_AAD_PREFIX.len() + 16);
    aad.extend_from_slice(V3_BLOCK_AAD_PREFIX);
    aad.extend_from_slice(&block_index.to_le_bytes());
    aad.extend_from_slice(&total_blocks.to_le_bytes());
    aad
}

/// v3 header: MAGIC(4) + VERSION(1) + total_blocks(8 LE) + wrapped DEK(40).
const V3_HEADER_LEN: usize = 4 + 1 + 8 + WRAPPED_KEY_SIZE;

fn encrypt_data_v3(master_key: &[u8; KEY_SIZE], plaintext: &[u8]) -> Result<Vec<u8>, String> {
    let dek = Zeroizing::new(random_array::<KEY_SIZE>());
    let wrapped = wrap_key(master_key, &dek)?;

    // ceil(len / BLOCK_SIZE); an empty file has zero blocks.
    let total_blocks = plaintext.len().div_ceil(BLOCK_SIZE) as u64;

    let mut output = Vec::with_capacity(V3_HEADER_LEN + plaintext.len() + FULL_BLOCK_CIPHER);
    output.extend_from_slice(MAGIC);
    output.push(VERSION_V3);
    output.extend_from_slice(&total_blocks.to_le_bytes());
    output.extend_from_slice(&wrapped);

    for (block_idx, chunk) in plaintext.chunks(BLOCK_SIZE).enumerate() {
        let block = encrypt_with_aad(&dek, chunk, &v3_block_aad(block_idx as u64, total_blocks))?;
        output.extend_from_slice(&block);
    }
    Ok(output)
}

fn decrypt_data_v3(master_key: &[u8; KEY_SIZE], ciphertext: &[u8]) -> Result<Vec<u8>, String> {
    if ciphertext.len() < V3_HEADER_LEN {
        return Err("AeroCrypt v3 data too short".into());
    }
    let total_blocks = u64::from_le_bytes(
        ciphertext[5..13]
            .try_into()
            .map_err(|_| "AeroCrypt v3 header truncated".to_string())?,
    );
    let wrapped: [u8; WRAPPED_KEY_SIZE] = ciphertext[13..V3_HEADER_LEN]
        .try_into()
        .map_err(|_| "AeroCrypt v3 header truncated".to_string())?;
    let dek = Zeroizing::new(unwrap_key(master_key, &wrapped)?);

    let data = &ciphertext[V3_HEADER_LEN..];
    let mut plaintext = Vec::with_capacity(data.len());
    let mut block_idx = 0u64;
    let mut pos = 0usize;
    while pos < data.len() {
        let end = (pos + FULL_BLOCK_CIPHER).min(data.len());
        let block = &data[pos..end];
        let pt = decrypt_with_aad(&dek, block, &v3_block_aad(block_idx, total_blocks))?;
        plaintext.extend_from_slice(&pt);
        pos = end;
        block_idx += 1;
    }
    // Length binding: the number of blocks actually present must equal the
    // authenticated count, with no trailing bytes. This rejects silent
    // truncation (fewer blocks) and append (extra bytes / blocks).
    if block_idx != total_blocks {
        return Err(format!(
            "AeroCrypt v3 truncated: expected {total_blocks} blocks, found {block_idx}"
        ));
    }
    if pos != data.len() {
        return Err("AeroCrypt v3 trailing data after final block".into());
    }
    Ok(plaintext)
}

// --- Public content API (version-dispatched) ------------------------------

/// Encrypt a file's bytes. New objects are always written as v3 wire format;
/// v4 keyslot vaults reuse the same object codec under OMK (09 §7a). Legacy
/// v1/v2 overlays are read-only and return an error so a downgraded or stale
/// config can never produce weaker ciphertext.
pub fn encrypt_data(
    cfg: &OverlayConfig,
    master_key: &[u8; KEY_SIZE],
    plaintext: &[u8],
) -> Result<Vec<u8>, String> {
    match cfg {
        // v4: master_key is OMK recovered via keyslots (T4); object layout is v3.
        OverlayConfig::V3 { .. } | OverlayConfig::V4 { .. } => {
            encrypt_data_v3(master_key, plaintext)
        }
        OverlayConfig::V2 { .. } | OverlayConfig::V1 { .. } => Err(format!(
            "legacy AeroCrypt v{} overlay is read-only; create a new overlay to add files",
            cfg.version()
        )),
    }
}

/// Decrypt an AeroCrypt blob, dispatching on its embedded version byte so a
/// reader transparently handles v1, v2, and v3 objects.
pub fn decrypt_data(master_key: &[u8; KEY_SIZE], ciphertext: &[u8]) -> Result<Vec<u8>, String> {
    if ciphertext.len() < 5 {
        return Err("AeroCrypt data too short".into());
    }
    if &ciphertext[0..4] != MAGIC {
        return Err("Not an AeroCrypt encrypted file".into());
    }
    match ciphertext[4] {
        VERSION_V1 => decrypt_data_v1(master_key, ciphertext),
        VERSION_V2 => decrypt_data_v2(master_key, ciphertext),
        VERSION_V3 => decrypt_data_v3(master_key, ciphertext),
        other => Err(format!("Unsupported AeroCrypt version {other}")),
    }
}

// --- Config (`.aeroftp-crypt.json`) ---------------------------------------

/// Generate a fresh 32-byte salt for a new v3 overlay.
pub fn random_salt_v3() -> [u8; SALT_V3_SIZE] {
    random_array::<SALT_V3_SIZE>()
}

/// Generate a fresh random vault id for a new overlay.
pub fn random_vault_id() -> [u8; VAULT_ID_SIZE] {
    random_array::<VAULT_ID_SIZE>()
}

/// Build the v3 config JSON for a PASSWORD-ONLY overlay. Emits a fresh
/// `vault_id` (unauthenticated, Axis-6) but keeps the original MAC info string
/// so the config MAC is byte-identical to what a pre-Tier-1 client would write.
pub fn init_config_v3(
    salt: &[u8; SALT_V3_SIZE],
    master_key: &[u8; KEY_SIZE],
) -> Result<String, String> {
    init_config_v3_with_vault_id(salt, master_key, &random_vault_id(), SaltMode::PerVault)
}

/// Build a password-only v3 config while preserving an existing vault id.
/// Used by headed/headerless metadata migration, where changing vault identity
/// would make an otherwise reversible conversion lossy.
pub fn init_config_v3_with_vault_id(
    salt: &[u8; SALT_V3_SIZE],
    master_key: &[u8; KEY_SIZE],
    vault_id: &[u8; VAULT_ID_SIZE],
    salt_mode: SaltMode,
) -> Result<String, String> {
    build_config_v3_tsv(salt, master_key, vault_id, false, None, salt_mode)
}

/// Build the v3 config JSON for a KEYFILE overlay. The caller supplies the
/// `vault_id` (so it can also be recorded locally / in an Emergency Kit). The
/// MAC binds `kdf_inputs` + `vault_id` via the extended info string.
/// `keyfile_hint` is an OPTIONAL, non-sensitive display hint; pass `None` to omit
/// it entirely (the recommended default, F5), or a basename-only string.
pub fn init_config_v3_with_keyfile(
    salt: &[u8; SALT_V3_SIZE],
    master_key: &[u8; KEY_SIZE],
    vault_id: &[u8; VAULT_ID_SIZE],
    keyfile_hint: Option<&str>,
    salt_mode: SaltMode,
) -> Result<String, String> {
    build_config_v3_tsv(salt, master_key, vault_id, true, keyfile_hint, salt_mode)
}

/// Rebuild a headed v3 marker from parsed local metadata and a verified key.
/// The salt, keyfile requirement, and vault id are preserved. Older v3
/// password-only configs without a vault id receive one because current marker
/// writers always emit it; keyfile configs already require one at parse time.
pub fn rebuild_config_v3(
    config: &OverlayConfig,
    master_key: &[u8; KEY_SIZE],
) -> Result<String, String> {
    match config {
        OverlayConfig::V3 {
            salt,
            vault_id,
            requires_keyfile,
            salt_mode,
            ..
        } => {
            let vault_id = vault_id.unwrap_or_else(random_vault_id);
            if *requires_keyfile {
                // keyfile + default-salt is coherent (orthogonal factors); keep the mode
                init_config_v3_with_keyfile(salt, master_key, &vault_id, None, *salt_mode)
            } else {
                init_config_v3_with_vault_id(salt, master_key, &vault_id, *salt_mode)
            }
        }
        other => Err(format!(
            "AeroCrypt v{} metadata migration is not supported; only v3 vaults can migrate",
            other.version()
        )),
    }
}

#[allow(dead_code)]
fn build_config_v3_json(
    salt: &[u8; SALT_V3_SIZE],
    master_key: &[u8; KEY_SIZE],
    vault_id: &[u8; VAULT_ID_SIZE],
    requires_keyfile: bool,
    keyfile_hint: Option<&str>,
    salt_mode: SaltMode,
) -> Result<String, String> {
    let mac = compute_config_mac_v3(
        master_key,
        salt,
        requires_keyfile,
        Some(vault_id),
        salt_mode,
    )?;
    let mut obj = serde_json::json!({
        "version": VERSION_V3,
        "cipher": "AES-256-GCM-SIV",
        "filename_cipher": "AES-256-SIV",
        "key_wrap": "AES-256-KW",
        "kdf": "Argon2id",
        "kdf_mem_kib": super::argon2_mem_kib(),
        "kdf_time": super::argon2_time(),
        "kdf_lanes": super::argon2_lanes(),
        "salt": base64::engine::general_purpose::STANDARD.encode(salt),
        "vault_id": base64::engine::general_purpose::STANDARD.encode(vault_id),
        "block_size": BLOCK_SIZE,
        "mac": base64::engine::general_purpose::STANDARD.encode(mac),
    });
    if requires_keyfile {
        obj["kdf_inputs"] = serde_json::json!(["password", "keyfile"]);
        if let Some(hint) = keyfile_hint {
            obj["keyfile_hint"] = serde_json::json!(hint);
        }
    }
    // Emit salt_mode ONLY for the non-default (opt-in) case so that every
    // existing per-vault marker stays byte-for-byte identical on disk.
    if salt_mode == SaltMode::DefaultV1 {
        obj["salt_mode"] = serde_json::json!(salt_mode.as_str());
    }
    Ok(obj.to_string())
}

fn build_config_v3_tsv(
    salt: &[u8; SALT_V3_SIZE],
    master_key: &[u8; KEY_SIZE],
    vault_id: &[u8; VAULT_ID_SIZE],
    requires_keyfile: bool,
    keyfile_hint: Option<&str>,
    salt_mode: SaltMode,
) -> Result<String, String> {
    let mac = compute_config_mac_v3(
        master_key,
        salt,
        requires_keyfile,
        Some(vault_id),
        salt_mode,
    )?;
    let mut lines = vec![
        "Warning\tPlease do not delete this file, it is needed to decrypt AeroCrypt".to_string(),
        format!("version\t{}", VERSION_V3),
        format!(
            "salt\t{}",
            base64::engine::general_purpose::STANDARD.encode(salt)
        ),
        format!(
            "vault_id\t{}",
            base64::engine::general_purpose::STANDARD.encode(vault_id)
        ),
        format!(
            "mac\t{}",
            base64::engine::general_purpose::STANDARD.encode(mac)
        ),
    ];
    if requires_keyfile {
        lines.push("kdf_inputs\tpassword,keyfile".to_string());
        if let Some(h) = keyfile_hint {
            lines.push(format!("keyfile_hint\t{}", h));
        }
    }
    if salt_mode == SaltMode::DefaultV1 {
        lines.push(format!("salt_mode\t{}", salt_mode.as_str()));
    }
    Ok(lines.join("\n") + "\n")
}

fn parse_config_tsv(config: &str) -> Result<OverlayConfig, String> {
    let mut version: Option<u64> = None;
    let mut salt_b64: Option<&str> = None;
    let mut mac_b64: Option<&str> = None;
    let mut vault_id_b64: Option<&str> = None;
    let mut kdf_inputs: Option<&str> = None;
    let mut salt_mode_str: Option<&str> = None;

    for line in config.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("Warning") {
            continue;
        }
        if let Some((k, v)) = line.split_once('\t') {
            match k {
                "version" => version = v.parse().ok(),
                "salt" => salt_b64 = Some(v),
                "mac" => mac_b64 = Some(v),
                "vault_id" => vault_id_b64 = Some(v),
                "kdf_inputs" => kdf_inputs = Some(v),
                "salt_mode" => salt_mode_str = Some(v),
                _ => {}
            }
        }
    }

    let version = version.ok_or("missing version in tsv crypt config")?;
    if version == 4 {
        // T2: v4 headed markers are JSON-first; TSV layout lands with T3 builders.
        return Err("v4 TSV not yet supported".into());
    }
    let salt_bytes = base64::engine::general_purpose::STANDARD
        .decode(salt_b64.ok_or("missing salt in tsv")?)
        .map_err(|e| format!("invalid salt: {e}"))?;

    if version != 3 || salt_bytes.len() != SALT_V3_SIZE {
        return Err("only v3 TSV supported".into());
    }
    let mut salt = [0u8; SALT_V3_SIZE];
    salt.copy_from_slice(&salt_bytes);

    let mac_bytes = base64::engine::general_purpose::STANDARD
        .decode(mac_b64.ok_or("missing mac in tsv")?)
        .map_err(|e| format!("invalid mac: {e}"))?;
    let mut mac = [0u8; CONFIG_MAC_SIZE];
    mac.copy_from_slice(&mac_bytes);

    let vault_id = if let Some(v) = vault_id_b64 {
        let b = base64::engine::general_purpose::STANDARD
            .decode(v)
            .map_err(|e| format!("bad vault_id: {e}"))?;
        if b.len() != VAULT_ID_SIZE {
            return Err("bad vault_id len".into());
        }
        let mut vid = [0u8; VAULT_ID_SIZE];
        vid.copy_from_slice(&b);
        Some(vid)
    } else {
        None
    };

    let requires_keyfile = kdf_inputs.is_some_and(|s| s.contains("keyfile"));
    let salt_mode: SaltMode = salt_mode_str
        .unwrap_or("per-vault")
        .parse()
        .unwrap_or(SaltMode::PerVault);

    if requires_keyfile && vault_id.is_none() {
        return Err("keyfile tsv missing vault_id".into());
    }

    Ok(OverlayConfig::V3 {
        salt,
        mac,
        vault_id,
        requires_keyfile,
        salt_mode,
    })
}

/// Parse an overlay config. Supports both legacy JSON (starts with `{`) and the new
/// lean TSV format. A missing or unknown `version` is a hard error.
pub fn parse_config(config: &str) -> Result<OverlayConfig, String> {
    let trimmed = config.trim_start();
    if trimmed.starts_with('{') {
        // Legacy JSON format (read-both support)
        return parse_config_json(config);
    }
    // New TSV format
    parse_config_tsv(config)
}

fn parse_config_json(config_json: &str) -> Result<OverlayConfig, String> {
    let val: serde_json::Value =
        serde_json::from_str(config_json).map_err(|e| format!("invalid crypt config: {e}"))?;
    let version = val
        .get("version")
        .and_then(|v| v.as_u64())
        .ok_or("missing version in crypt config")?;
    match version {
        1..=3 => parse_config_json_v1_v2_v3(version, &val),
        4 => parse_config_json_v4(&val),
        other => Err(format!(
            "unsupported crypt config version {other}: this vault was created by a newer AeroFTP, please update"
        )),
    }
}

fn parse_config_json_v1_v2_v3(
    version: u64,
    val: &serde_json::Value,
) -> Result<OverlayConfig, String> {
    let salt_b64 = val
        .get("salt")
        .and_then(|v| v.as_str())
        .ok_or("missing salt in crypt config")?;
    if salt_b64.len() > MAX_B64_SALT_LEN {
        return Err("salt field too long".into());
    }
    let salt_bytes = base64::engine::general_purpose::STANDARD
        .decode(salt_b64)
        .map_err(|e| format!("invalid salt: {e}"))?;
    match version {
        1 => {
            if salt_bytes.len() != SALT_V1_SIZE {
                return Err(format!("v1 salt must be {SALT_V1_SIZE} bytes"));
            }
            let mut salt = [0u8; SALT_V1_SIZE];
            salt.copy_from_slice(&salt_bytes);
            Ok(OverlayConfig::V1 { salt })
        }
        2 => {
            if salt_bytes.len() != SALT_V2_SIZE {
                return Err(format!("v2 salt must be {SALT_V2_SIZE} bytes"));
            }
            let mut salt = [0u8; SALT_V2_SIZE];
            salt.copy_from_slice(&salt_bytes);
            Ok(OverlayConfig::V2 { salt })
        }
        3 => {
            if salt_bytes.len() != SALT_V3_SIZE {
                return Err(format!("v3 salt must be {SALT_V3_SIZE} bytes"));
            }
            let mac_b64 = val
                .get("mac")
                .and_then(|v| v.as_str())
                .ok_or("missing mac in v3 crypt config")?;
            if mac_b64.len() > MAX_B64_SALT_LEN {
                return Err("mac field too long".into());
            }
            let mac_bytes = base64::engine::general_purpose::STANDARD
                .decode(mac_b64)
                .map_err(|e| format!("invalid mac: {e}"))?;
            if mac_bytes.len() != CONFIG_MAC_SIZE {
                return Err(format!("v3 mac must be {CONFIG_MAC_SIZE} bytes"));
            }
            let mut salt = [0u8; SALT_V3_SIZE];
            salt.copy_from_slice(&salt_bytes);
            let mut mac = [0u8; CONFIG_MAC_SIZE];
            mac.copy_from_slice(&mac_bytes);

            // Optional vault_id (present from Tier 1 on).
            let vault_id = match val.get("vault_id").and_then(|v| v.as_str()) {
                Some(vid_b64) => {
                    if vid_b64.len() > MAX_B64_VAULT_ID_LEN {
                        return Err("vault_id field too long".into());
                    }
                    let vid_bytes = base64::engine::general_purpose::STANDARD
                        .decode(vid_b64)
                        .map_err(|e| format!("invalid vault_id: {e}"))?;
                    if vid_bytes.len() != VAULT_ID_SIZE {
                        return Err(format!("v3 vault_id must be {VAULT_ID_SIZE} bytes"));
                    }
                    let mut vid = [0u8; VAULT_ID_SIZE];
                    vid.copy_from_slice(&vid_bytes);
                    Some(vid)
                }
                None => None,
            };

            // `kdf_inputs` decides whether a keyfile is required. Absent (or
            // exactly ["password"]) means password-only.
            let requires_keyfile = match val.get("kdf_inputs").and_then(|v| v.as_array()) {
                Some(arr) => arr.iter().any(|v| v.as_str() == Some("keyfile")),
                None => false,
            };
            if requires_keyfile && vault_id.is_none() {
                return Err("keyfile crypt config is missing its vault_id".to_string());
            }

            // salt_mode: optional, absent or "per-vault" => PerVault (back-compat for all pre-default-salt markers)
            let salt_mode = match val.get("salt_mode").and_then(|v| v.as_str()) {
                Some(s) => s.parse().unwrap_or(SaltMode::PerVault),
                None => SaltMode::PerVault,
            };

            Ok(OverlayConfig::V3 {
                salt,
                mac,
                vault_id,
                requires_keyfile,
                salt_mode,
            })
        }
        _ => unreachable!("parse_config_json_v1_v2_v3 called with non-legacy version"),
    }
}

/// Parse a v4 keyslot header (JSON). Applies F9 bounds BEFORE any Argon2id:
/// slot cap, duplicate ids, Argon2 floor/cap, fixed KDF salt sizes, base64
/// length bounds, unknown type skip (MAC covers stored bytes, not this list).
fn parse_config_json_v4(val: &serde_json::Value) -> Result<OverlayConfig, String> {
    let vault_id = decode_b64_fixed::<VAULT_ID_SIZE>(
        val.get("vault_id")
            .and_then(|v| v.as_str())
            .ok_or("missing vault_id in v4 crypt config")?,
        MAX_B64_VAULT_ID_LEN,
        "vault_id",
    )?;

    let epoch = val
        .get("epoch")
        .and_then(|v| v.as_u64())
        .ok_or("missing epoch in v4 crypt config")?;
    if epoch == 0 || epoch > u32::MAX as u64 {
        return Err("v4 epoch must be a non-zero u32".into());
    }
    let epoch = epoch as u32;

    let vk_wrap = decode_b64_bounded(
        val.get("vk_wrap")
            .and_then(|v| v.as_str())
            .ok_or("missing vk_wrap in v4 crypt config")?,
        MAX_B64_WRAP_LEN,
        "vk_wrap",
    )?;
    let omk_wrap = decode_b64_bounded(
        val.get("omk_wrap")
            .and_then(|v| v.as_str())
            .ok_or("missing omk_wrap in v4 crypt config")?,
        MAX_B64_WRAP_LEN,
        "omk_wrap",
    )?;

    let config_mac = decode_b64_fixed::<CONFIG_MAC_SIZE>(
        val.get("config_mac")
            .and_then(|v| v.as_str())
            .ok_or("missing config_mac in v4 crypt config")?,
        MAX_B64_SALT_LEN,
        "config_mac",
    )?;

    let slots_val = val
        .get("slots")
        .and_then(|v| v.as_array())
        .ok_or("missing slots array in v4 crypt config")?;
    if slots_val.len() > MAX_SLOTS {
        return Err(format!(
            "v4 crypt config has {} slots; max is {MAX_SLOTS}",
            slots_val.len()
        ));
    }

    let mut slots: Vec<Slot> = Vec::with_capacity(slots_val.len());
    let mut seen_ids: Vec<u32> = Vec::with_capacity(slots_val.len());
    for (idx, slot_val) in slots_val.iter().enumerate() {
        let id = slot_val
            .get("id")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| format!("v4 slot[{idx}] missing id"))?;
        if id > u32::MAX as u64 {
            return Err(format!("v4 slot[{idx}] id out of u32 range"));
        }
        let id = id as u32;
        if seen_ids.contains(&id) {
            return Err(format!("v4 crypt config has duplicate slot id {id}"));
        }
        seen_ids.push(id);

        let type_str = slot_val
            .get("type")
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("v4 slot[{idx}] missing type"))?;
        let Some(kind) = parse_slot_type_str(type_str) else {
            // F9: unknown type skipped for unlock; config_mac still covers raw bytes.
            continue;
        };

        let salt = decode_b64_bounded(
            slot_val
                .get("salt")
                .and_then(|v| v.as_str())
                .ok_or_else(|| format!("v4 slot[{idx}] missing salt"))?,
            MAX_B64_SALT_LEN,
            "slot salt",
        )?;
        // KDF slots require a fixed 32-byte salt; other reserved types keep salt
        // as variable for forward-compat (still length-bounded above).
        if matches!(
            kind,
            SlotType::Passphrase | SlotType::Keyfile | SlotType::Recovery
        ) && salt.len() != SALT_SIZE
        {
            return Err(format!(
                "v4 slot[{idx}] salt must be {SALT_SIZE} bytes for KDF slot types"
            ));
        }

        let kdf = parse_slot_kdf(slot_val.get("kdf"), kind, idx)?;
        let binding = parse_slot_binding(slot_val.get("binding"), kind, idx)?;
        let wrapped = decode_b64_bounded(
            slot_val
                .get("wrapped")
                .and_then(|v| v.as_str())
                .ok_or_else(|| format!("v4 slot[{idx}] missing wrapped"))?,
            MAX_B64_WRAP_LEN,
            "slot wrapped",
        )?;

        slots.push(Slot {
            id,
            kind,
            salt,
            kdf,
            binding,
            wrapped,
        });
    }

    Ok(OverlayConfig::V4 {
        vault_id,
        epoch,
        vk_wrap,
        omk_wrap,
        slots,
        config_mac,
    })
}

fn parse_slot_type_str(s: &str) -> Option<SlotType> {
    match s {
        "passphrase" => Some(SlotType::Passphrase),
        // Legacy Tier-1 combined scheme is the keyfile slot type on the wire.
        "keyfile" | "aecr-t1-combined-v1" => Some(SlotType::Keyfile),
        "recovery" => Some(SlotType::Recovery),
        "fido2-hmac" => Some(SlotType::Fido2Hmac),
        "and" => Some(SlotType::And),
        "threshold" => Some(SlotType::Threshold),
        _ => None,
    }
}

/// Parse and floor/cap Argon2id params (F9). Absent kdf is allowed for
/// non-KDF types (fido2-hmac); KDF types require the object.
fn parse_slot_kdf(
    kdf_val: Option<&serde_json::Value>,
    kind: SlotType,
    idx: usize,
) -> Result<Option<Argon2Params>, String> {
    let needs_kdf = matches!(
        kind,
        SlotType::Passphrase | SlotType::Keyfile | SlotType::Recovery
    );
    match kdf_val {
        None | Some(serde_json::Value::Null) => {
            if needs_kdf {
                Err(format!("v4 slot[{idx}] missing kdf for KDF slot type"))
            } else {
                Ok(None)
            }
        }
        Some(obj) => {
            let m_kib = obj
                .get("m_kib")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| format!("v4 slot[{idx}] kdf.m_kib missing"))?;
            let t = obj
                .get("t")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| format!("v4 slot[{idx}] kdf.t missing"))?;
            let p = obj
                .get("p")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| format!("v4 slot[{idx}] kdf.p missing"))?;
            if m_kib > u32::MAX as u64 || t > u32::MAX as u64 || p > u32::MAX as u64 {
                return Err(format!("v4 slot[{idx}] kdf param out of u32 range"));
            }
            // Floor + cap (do not reject): protects the honest client from DoS
            // or under-strength params in a hostile header (F9).
            Ok(Some(Argon2Params {
                m_kib: (m_kib as u32).clamp(MIN_ARGON2_MEM_KIB, MAX_ARGON2_MEM_KIB),
                t: (t as u32).clamp(MIN_ARGON2_TIME, MAX_ARGON2_TIME),
                p: (p as u32).clamp(MIN_ARGON2_LANES, MAX_ARGON2_LANES),
            }))
        }
    }
}

fn parse_slot_binding(
    binding_val: Option<&serde_json::Value>,
    kind: SlotType,
    idx: usize,
) -> Result<SlotBinding, String> {
    match kind {
        SlotType::Passphrase | SlotType::Keyfile => Ok(SlotBinding::None),
        SlotType::Recovery => {
            let prefix = binding_val
                .and_then(|b| b.get("vault_prefix"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if prefix.len() > 64 {
                return Err(format!("v4 slot[{idx}] recovery vault_prefix too long"));
            }
            Ok(SlotBinding::Recovery {
                vault_prefix: prefix,
            })
        }
        SlotType::Fido2Hmac => {
            let b = binding_val.ok_or_else(|| format!("v4 slot[{idx}] fido2 missing binding"))?;
            let credential_id = decode_b64_bounded(
                b.get("credential_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| format!("v4 slot[{idx}] fido2 missing credential_id"))?,
                MAX_B64_FIELD_LEN,
                "credential_id",
            )?;
            let hmac_salt = decode_b64_bounded(
                b.get("hmac_salt")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| format!("v4 slot[{idx}] fido2 missing hmac_salt"))?,
                MAX_B64_SALT_LEN,
                "hmac_salt",
            )?;
            Ok(SlotBinding::Fido2 {
                credential_id,
                hmac_salt,
            })
        }
        SlotType::And => {
            let members_val = binding_val
                .and_then(|b| b.get("members"))
                .and_then(|v| v.as_array())
                .ok_or_else(|| format!("v4 slot[{idx}] and binding missing members"))?;
            if members_val.len() > MAX_SLOTS {
                return Err(format!("v4 slot[{idx}] and members exceed MAX_SLOTS"));
            }
            let mut members = Vec::with_capacity(members_val.len());
            for (mi, m) in members_val.iter().enumerate() {
                let type_str = m
                    .get("type")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| format!("v4 slot[{idx}] and member[{mi}] missing type"))?;
                let mkind = parse_slot_type_str(type_str).ok_or_else(|| {
                    format!("v4 slot[{idx}] and member[{mi}] unknown type {type_str}")
                })?;
                let salt = decode_b64_bounded(
                    m.get("salt")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| format!("v4 slot[{idx}] and member[{mi}] missing salt"))?,
                    MAX_B64_SALT_LEN,
                    "and member salt",
                )?;
                let kdf = parse_slot_kdf(m.get("kdf"), mkind, idx)?;
                let (credential_id, hmac_salt) = if mkind == SlotType::Fido2Hmac {
                    (
                        Some(decode_b64_bounded(
                            m.get("credential_id")
                                .and_then(|v| v.as_str())
                                .ok_or_else(|| {
                                    format!("v4 slot[{idx}] and member[{mi}] missing credential_id")
                                })?,
                            MAX_B64_FIELD_LEN,
                            "credential_id",
                        )?),
                        Some(decode_b64_bounded(
                            m.get("hmac_salt").and_then(|v| v.as_str()).ok_or_else(|| {
                                format!("v4 slot[{idx}] and member[{mi}] missing hmac_salt")
                            })?,
                            MAX_B64_SALT_LEN,
                            "hmac_salt",
                        )?),
                    )
                } else {
                    (None, None)
                };
                members.push(AndMember {
                    kind: mkind,
                    salt,
                    kdf,
                    credential_id,
                    hmac_salt,
                });
            }
            Ok(SlotBinding::And { members })
        }
        SlotType::Threshold => {
            // Reserved: accept empty binding; unlock will fail closed later.
            Ok(SlotBinding::None)
        }
    }
}

fn decode_b64_bounded(b64: &str, max_len: usize, field: &str) -> Result<Vec<u8>, String> {
    if b64.len() > max_len {
        return Err(format!("{field} field too long"));
    }
    base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| format!("invalid {field}: {e}"))
}

fn decode_b64_fixed<const N: usize>(
    b64: &str,
    max_len: usize,
    field: &str,
) -> Result<[u8; N], String> {
    let bytes = decode_b64_bounded(b64, max_len, field)?;
    if bytes.len() != N {
        return Err(format!("{field} must be {N} bytes"));
    }
    let mut out = [0u8; N];
    out.copy_from_slice(&bytes);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sizes_to_test() -> Vec<usize> {
        vec![
            0,
            1,
            100,
            BLOCK_SIZE - 1,
            BLOCK_SIZE,
            BLOCK_SIZE + 1,
            BLOCK_SIZE * 3 + 77,
        ]
    }

    // Legacy-format writers, kept only for read-compatibility tests.
    fn encrypt_data_v1(master_key: &[u8; KEY_SIZE], plaintext: &[u8]) -> Vec<u8> {
        let master_nonce = random_array::<NONCE_SIZE>();
        let file_key = derive_file_key_v1(master_key, &master_nonce).unwrap();
        let cipher = Aes256Gcm::new((&*file_key).into());
        let mut output = Vec::new();
        output.extend_from_slice(MAGIC);
        output.push(VERSION_V1);
        output.extend_from_slice(&master_nonce);
        for (block_idx, chunk) in plaintext.chunks(BLOCK_SIZE).enumerate() {
            let mut block_nonce = master_nonce;
            let idx_bytes = (block_idx as u32).to_le_bytes();
            for i in 0..4 {
                block_nonce[i] ^= idx_bytes[i];
            }
            let nonce = AesNonce::from_slice(&block_nonce);
            output.extend_from_slice(&cipher.encrypt(nonce, chunk).unwrap());
        }
        output
    }

    fn encrypt_data_v2(master_key: &[u8; KEY_SIZE], plaintext: &[u8]) -> Vec<u8> {
        let dek = random_array::<KEY_SIZE>();
        let wrapped = wrap_key(master_key, &dek).unwrap();
        let mut output = Vec::new();
        output.extend_from_slice(MAGIC);
        output.push(VERSION_V2);
        output.extend_from_slice(&wrapped);
        for (block_idx, chunk) in plaintext.chunks(BLOCK_SIZE).enumerate() {
            output.extend_from_slice(
                &encrypt_with_aad(&dek, chunk, &v2_block_aad(block_idx as u64)).unwrap(),
            );
        }
        output
    }

    fn v3_cfg() -> (OverlayConfig, [u8; KEY_SIZE]) {
        let salt = [9u8; SALT_V3_SIZE];
        let master = derive_base_kek("correct horse battery staple", &salt).unwrap();
        let mac = compute_config_mac_v3(&master, &salt, false, None, SaltMode::PerVault).unwrap();
        (
            OverlayConfig::V3 {
                salt,
                mac,
                vault_id: None,
                requires_keyfile: false,
                salt_mode: SaltMode::PerVault,
            },
            master,
        )
    }

    #[test]
    fn v3_round_trip_all_sizes() {
        let (cfg, master) = v3_cfg();
        for n in sizes_to_test() {
            let pt: Vec<u8> = (0..n).map(|i| (i * 37 + 11) as u8).collect();
            let blob = encrypt_data(&cfg, &master, &pt).unwrap();
            assert_eq!(&blob[0..4], MAGIC);
            assert_eq!(blob[4], VERSION_V3);
            assert_eq!(
                decrypt_data(&master, &blob).unwrap(),
                pt,
                "v3 round trip {n}"
            );
        }
    }

    #[test]
    fn v3_decrypted_size_inverts_ciphertext_length() {
        let (cfg, master) = v3_cfg();
        for n in sizes_to_test() {
            let pt: Vec<u8> = (0..n).map(|i| (i * 37 + 11) as u8).collect();
            let blob = encrypt_data(&cfg, &master, &pt).unwrap();
            assert_eq!(
                v3_decrypted_size(blob.len() as u64),
                n as u64,
                "v3 size map for {n} plaintext bytes (ciphertext {})",
                blob.len()
            );
        }
        // Header-only (empty file, zero blocks) maps to 0.
        assert_eq!(v3_decrypted_size(V3_HEADER_LEN as u64), 0);
        // A length shorter than the header is left unchanged (foreign object).
        assert_eq!(v3_decrypted_size(10), 10);
    }

    #[test]
    fn v3_truncation_is_rejected() {
        let (cfg, master) = v3_cfg();
        let pt: Vec<u8> = (0..BLOCK_SIZE * 3 + 10).map(|i| i as u8).collect();
        let blob = encrypt_data(&cfg, &master, &pt).unwrap();
        // Drop the final (partial) block: this is the silent-truncation attack.
        let truncated = &blob[..blob.len() - (NONCE_SIZE + 10 + GCM_TAG)];
        assert!(
            decrypt_data(&master, truncated).is_err(),
            "tail truncation must fail closed"
        );
        // Drop a whole trailing full block too.
        let truncated2 = &blob[..V3_HEADER_LEN + FULL_BLOCK_CIPHER];
        assert!(decrypt_data(&master, truncated2).is_err());
    }

    #[test]
    fn v3_append_is_rejected() {
        let (cfg, master) = v3_cfg();
        let pt: Vec<u8> = (0..BLOCK_SIZE + 5).map(|i| i as u8).collect();
        let mut blob = encrypt_data(&cfg, &master, &pt).unwrap();
        blob.extend_from_slice(&[0u8; 8]); // trailing garbage
        assert!(decrypt_data(&master, &blob).is_err());
    }

    #[test]
    fn v3_block_reorder_is_rejected() {
        let (cfg, master) = v3_cfg();
        let pt: Vec<u8> = (0..BLOCK_SIZE * 2 + 10).map(|i| i as u8).collect();
        let blob = encrypt_data(&cfg, &master, &pt).unwrap();
        let h = V3_HEADER_LEN;
        let b1 = h + FULL_BLOCK_CIPHER;
        let b2 = h + 2 * FULL_BLOCK_CIPHER;
        let mut swapped = blob[..h].to_vec();
        swapped.extend_from_slice(&blob[b1..b2]);
        swapped.extend_from_slice(&blob[h..b1]);
        swapped.extend_from_slice(&blob[b2..]);
        assert!(decrypt_data(&master, &swapped).is_err());
    }

    #[test]
    fn v3_wrong_password_fails_closed() {
        let (cfg, master) = v3_cfg();
        let blob = encrypt_data(&cfg, &master, b"secret").unwrap();
        let salt = [9u8; SALT_V3_SIZE];
        let wrong = derive_base_kek("wrong", &salt).unwrap();
        assert!(decrypt_data(&wrong, &blob).is_err());
    }

    #[test]
    fn v3_single_bit_flip_is_rejected() {
        let (cfg, master) = v3_cfg();
        let pt: Vec<u8> = (0..1000).map(|i| i as u8).collect();
        let mut blob = encrypt_data(&cfg, &master, &pt).unwrap();
        let last = blob.len() - 1;
        blob[last] ^= 0x01;
        assert!(decrypt_data(&master, &blob).is_err());
    }

    #[test]
    fn legacy_v1_and_v2_still_read() {
        let salt1 = [3u8; SALT_V1_SIZE];
        let m1 = derive_master_key(&OverlayConfig::V1 { salt: salt1 }, "legacy").unwrap();
        let pt = b"legacy AeroCrypt payload".repeat(5000);
        let blob1 = encrypt_data_v1(&m1, &pt);
        assert_eq!(blob1[4], VERSION_V1);
        assert_eq!(decrypt_data(&m1, &blob1).unwrap(), pt);

        let salt2 = [4u8; SALT_V2_SIZE];
        let m2 = derive_master_key(&OverlayConfig::V2 { salt: salt2 }, "legacy").unwrap();
        let blob2 = encrypt_data_v2(&m2, &pt);
        assert_eq!(blob2[4], VERSION_V2);
        assert_eq!(decrypt_data(&m2, &blob2).unwrap(), pt);
    }

    #[test]
    fn legacy_overlays_are_write_frozen() {
        let m = [7u8; KEY_SIZE];
        assert!(encrypt_data(
            &OverlayConfig::V1 {
                salt: [0u8; SALT_V1_SIZE]
            },
            &m,
            b"x"
        )
        .is_err());
        assert!(encrypt_data(
            &OverlayConfig::V2 {
                salt: [0u8; SALT_V2_SIZE]
            },
            &m,
            b"x"
        )
        .is_err());
    }

    #[test]
    fn cross_version_key_mismatch_fails_closed() {
        // A v3 blob must not decrypt under a key derived for a different salt.
        let (cfg, master) = v3_cfg();
        let blob = encrypt_data(&cfg, &master, b"hello world").unwrap();
        let other = derive_base_kek("correct horse battery staple", &[1u8; SALT_V3_SIZE]).unwrap();
        assert!(decrypt_data(&other, &blob).is_err());
    }

    #[test]
    fn v3_config_mac_detects_tamper_and_wrong_password() {
        let salt = random_salt_v3();
        let master = derive_base_kek("right-password", &salt).unwrap();
        let json = init_config_v3(&salt, &master).unwrap();
        let cfg = parse_config(&json).unwrap();
        assert_eq!(cfg.version(), VERSION_V3);
        // Correct password verifies.
        assert!(verify_config_mac(&cfg, &master).is_ok());
        // Wrong password fails the MAC.
        let wrong = derive_base_kek("wrong-password", &salt).unwrap();
        assert!(verify_config_mac(&cfg, &wrong).is_err());
        // Tampered salt (attacker swaps salt) fails: the derived key no longer
        // matches the stored MAC.
        if let OverlayConfig::V3 { mac, .. } = cfg {
            let evil_salt = [2u8; SALT_V3_SIZE];
            let evil_cfg = OverlayConfig::V3 {
                salt: evil_salt,
                mac,
                vault_id: None,
                requires_keyfile: false,
                salt_mode: SaltMode::PerVault,
            };
            let evil_key = derive_base_kek("right-password", &evil_salt).unwrap();
            assert!(verify_config_mac(&evil_cfg, &evil_key).is_err());
        }
    }

    #[test]
    fn keyfile_vault_round_trips_and_binds_requirement() {
        let salt = random_salt_v3();
        let vault_id = random_vault_id();
        let kf = super::super::keyfile_digest(b"my-keyfile-payload");
        // Build a keyfile vault (password + keyfile).
        let master =
            derive_master_key_with_keyfile(&OverlayConfig::v3_bootstrap(salt), "pw", Some(&kf))
                .unwrap();
        let json = init_config_v3_with_keyfile(&salt, &master, &vault_id, None, SaltMode::PerVault)
            .unwrap();
        let cfg = parse_config(&json).unwrap();
        assert!(cfg.requires_keyfile());
        assert_eq!(cfg.vault_id(), Some(vault_id));

        // Right password + right keyfile verifies.
        let m_ok = derive_master_key_with_keyfile(&cfg, "pw", Some(&kf)).unwrap();
        assert!(verify_config_mac(&cfg, &m_ok).is_ok());
        // Right password, WRONG keyfile fails closed.
        let kf_wrong = super::super::keyfile_digest(b"other");
        let m_bad = derive_master_key_with_keyfile(&cfg, "pw", Some(&kf_wrong)).unwrap();
        assert!(verify_config_mac(&cfg, &m_bad).is_err());
        // Password only (keyfile stripped) fails closed.
        let m_nopass = derive_master_key_with_keyfile(&cfg, "pw", None).unwrap();
        assert!(verify_config_mac(&cfg, &m_nopass).is_err());
    }

    #[test]
    fn password_only_config_mac_is_unchanged_by_vault_id() {
        // Back-compat: a password-only vault's MAC must not depend on vault_id
        // (old readers ignore the field and still verify).
        let salt = [4u8; SALT_V3_SIZE];
        let master = derive_base_kek("pw", &salt).unwrap();
        let with_vid = compute_config_mac_v3(
            &master,
            &salt,
            false,
            Some(&[7u8; VAULT_ID_SIZE]),
            SaltMode::PerVault,
        );
        let without = compute_config_mac_v3(&master, &salt, false, None, SaltMode::PerVault);
        assert_eq!(with_vid.unwrap(), without.unwrap());
    }

    #[test]
    fn rebuild_config_v3_preserves_password_vault_identity_and_mac() {
        let salt = [12u8; SALT_V3_SIZE];
        let vault_id = [34u8; VAULT_ID_SIZE];
        let master = derive_base_kek("migration-password", &salt).unwrap();
        let original =
            init_config_v3_with_vault_id(&salt, &master, &vault_id, SaltMode::PerVault).unwrap();
        let original_cfg = parse_config(&original).unwrap();

        let rebuilt = rebuild_config_v3(&original_cfg, &master).unwrap();
        let rebuilt_cfg = parse_config(&rebuilt).unwrap();

        assert_eq!(rebuilt_cfg.vault_id(), Some(vault_id));
        assert!(!rebuilt_cfg.requires_keyfile());
        verify_config_mac(&rebuilt_cfg, &master).unwrap();
    }

    #[test]
    fn rebuild_config_v3_preserves_keyfile_requirement_identity_and_mac() {
        let salt = [56u8; SALT_V3_SIZE];
        let vault_id = [78u8; VAULT_ID_SIZE];
        let keyfile = super::super::keyfile_digest(b"migration-keyfile");
        let bootstrap = OverlayConfig::v3_bootstrap(salt);
        let master =
            derive_master_key_with_keyfile(&bootstrap, "migration-password", Some(&keyfile))
                .unwrap();
        let original = init_config_v3_with_keyfile(
            &salt,
            &master,
            &vault_id,
            Some("not-preserved"),
            SaltMode::PerVault,
        )
        .unwrap();
        let original_cfg = parse_config(&original).unwrap();

        let rebuilt = rebuild_config_v3(&original_cfg, &master).unwrap();
        let rebuilt_cfg = parse_config(&rebuilt).unwrap();

        assert_eq!(rebuilt_cfg.vault_id(), Some(vault_id));
        assert!(rebuilt_cfg.requires_keyfile());
        verify_config_mac(&rebuilt_cfg, &master).unwrap();
        // TSV format (D5): check raw content instead of JSON
        assert!(rebuilt.contains("kdf_inputs\tpassword,keyfile"));
        assert!(!rebuilt.contains("keyfile_hint"));
    }

    #[test]
    fn parse_config_rejects_missing_and_unknown_version() {
        // Missing version is a hard error (no silent v1 fallback).
        let no_ver = serde_json::json!({
            "salt": base64::engine::general_purpose::STANDARD.encode([1u8; SALT_V1_SIZE]),
        })
        .to_string();
        assert!(parse_config(&no_ver).is_err());
        // Unknown version is rejected.
        let bad_ver = serde_json::json!({
            "version": 99,
            "salt": base64::engine::general_purpose::STANDARD.encode([1u8; SALT_V3_SIZE]),
        })
        .to_string();
        assert!(parse_config(&bad_ver).is_err());
        // v3 without a mac is rejected.
        let no_mac = serde_json::json!({
            "version": 3,
            "salt": base64::engine::general_purpose::STANDARD.encode([1u8; SALT_V3_SIZE]),
        })
        .to_string();
        assert!(parse_config(&no_mac).is_err());
    }

    #[test]
    fn default_salt_and_tsv_roundtrip_and_mac_binding() {
        // DefaultV1 uses the public constant; MAC includes the frozen suffix.
        let vid = random_vault_id();
        let master_def = derive_base_kek(
            "correct-horse-battery-staple-123456789012345678901234567890",
            &crate::aerocrypt::AEROCRYPT_DEFAULT_SALT_V1,
        )
        .unwrap();
        let tsv_def = init_config_v3_with_vault_id(
            &crate::aerocrypt::AEROCRYPT_DEFAULT_SALT_V1,
            &master_def,
            &vid,
            SaltMode::DefaultV1,
        )
        .unwrap();
        assert!(tsv_def.contains("salt_mode\tdefault-v1"));
        let cfg_def = parse_config(&tsv_def).unwrap();
        if let OverlayConfig::V3 { salt_mode, .. } = &cfg_def {
            assert_eq!(*salt_mode, SaltMode::DefaultV1);
        }
        verify_config_mac(&cfg_def, &master_def).unwrap();

        // Per-vault TSV has no salt_mode field, MAC without suffix.
        let salt_per = [7u8; SALT_V3_SIZE];
        let master_per =
            derive_base_kek("another-strong-pw-12345678901234567890", &salt_per).unwrap();
        let tsv_per =
            init_config_v3_with_vault_id(&salt_per, &master_per, &vid, SaltMode::PerVault).unwrap();
        assert!(!tsv_per.contains("salt_mode"));
        let cfg_per = parse_config(&tsv_per).unwrap();
        verify_config_mac(&cfg_per, &master_per).unwrap();
    }

    #[test]
    fn tsv_and_legacy_json_are_both_parsable_for_read_both() {
        let salt = [5u8; SALT_V3_SIZE];
        let vid = random_vault_id();
        let master = derive_base_kek("pw-for-read-both", &salt).unwrap();
        // Current init emits TSV
        let tsv = init_config_v3_with_vault_id(&salt, &master, &vid, SaltMode::PerVault).unwrap();
        let cfg_from_tsv = parse_config(&tsv).unwrap();
        verify_config_mac(&cfg_from_tsv, &master).unwrap();

        // Simulate legacy JSON (what old files have)
        let legacy_json =
            build_config_v3_json(&salt, &master, &vid, false, None, SaltMode::PerVault).unwrap();
        let cfg_from_json = parse_config(&legacy_json).unwrap();
        verify_config_mac(&cfg_from_json, &master).unwrap();
    }

    // --- v4 keyslot parser (F9) + stored-bytes config_mac -------------------

    fn b64(bytes: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    /// Minimal valid v4 JSON fixture (no real crypto wraps; parse-only).
    struct V4Fixture {
        n_slots: usize,
        kdf: (u32, u32, u32),
        salt_len: usize,
        duplicate_id: bool,
    }

    impl Default for V4Fixture {
        fn default() -> Self {
            Self {
                n_slots: 1,
                kdf: (128 * 1024, 4, 4),
                salt_len: SALT_SIZE,
                duplicate_id: false,
            }
        }
    }

    impl V4Fixture {
        fn to_json(&self) -> String {
            let vault_id = [0xABu8; VAULT_ID_SIZE];
            let wrap = vec![0x11u8; 60]; // nonce(12)+ct(32)+tag(16) shape
            let salt = vec![0x22u8; self.salt_len];
            let (kdf_m, kdf_t, kdf_p) = self.kdf;
            let mut slots = String::from("[");
            for i in 0..self.n_slots {
                let id = if self.duplicate_id && i == 1 {
                    0
                } else {
                    i as u32
                };
                if i > 0 {
                    slots.push(',');
                }
                slots.push_str(&format!(
                    r#"{{"id":{id},"type":"passphrase","salt":"{}","kdf":{{"m_kib":{kdf_m},"t":{kdf_t},"p":{kdf_p}}},"wrapped":"{}"}}"#,
                    b64(&salt),
                    b64(&wrap),
                ));
            }
            slots.push(']');
            format!(
                r#"{{"version":4,"vault_id":"{}","epoch":1,"vk_wrap":"{}","omk_wrap":"{}","slots":{slots},"config_mac":"{}"}}"#,
                b64(&vault_id),
                b64(&wrap),
                b64(&wrap),
                b64(&[0u8; CONFIG_MAC_SIZE]),
            )
        }
    }

    #[test]
    fn v4_rejects_more_than_max_slots() {
        let json = V4Fixture {
            n_slots: MAX_SLOTS + 1,
            ..Default::default()
        }
        .to_json();
        let err = parse_config(&json).unwrap_err();
        assert!(
            err.contains("max is") || err.contains(&MAX_SLOTS.to_string()),
            "expected MAX_SLOTS reject, got: {err}"
        );
    }

    #[test]
    fn v4_floors_and_caps_argon2_params() {
        // Too-weak params are floored; no Argon2id runs during parse.
        let weak = V4Fixture {
            kdf: (1, 0, 0),
            ..Default::default()
        }
        .to_json();
        let cfg = parse_config(&weak).expect("weak kdf should parse with floor");
        if let OverlayConfig::V4 { slots, .. } = cfg {
            let kdf = slots[0].kdf.expect("kdf present");
            assert_eq!(kdf.m_kib, MIN_ARGON2_MEM_KIB);
            assert_eq!(kdf.t, MIN_ARGON2_TIME);
            assert_eq!(kdf.p, MIN_ARGON2_LANES);
        } else {
            panic!("expected V4");
        }

        // Huge params are capped.
        let huge = V4Fixture {
            kdf: (u32::MAX, u32::MAX, u32::MAX),
            ..Default::default()
        }
        .to_json();
        let cfg = parse_config(&huge).expect("huge kdf should parse with cap");
        if let OverlayConfig::V4 { slots, .. } = cfg {
            let kdf = slots[0].kdf.expect("kdf present");
            assert_eq!(kdf.m_kib, MAX_ARGON2_MEM_KIB);
            assert_eq!(kdf.t, MAX_ARGON2_TIME);
            assert_eq!(kdf.p, MAX_ARGON2_LANES);
        } else {
            panic!("expected V4");
        }
    }

    #[test]
    fn v4_rejects_wrong_salt_length() {
        let json = V4Fixture {
            salt_len: 16, // wrong: KDF slots need 32
            ..Default::default()
        }
        .to_json();
        let err = parse_config(&json).unwrap_err();
        assert!(
            err.contains("salt must be"),
            "expected salt length reject, got: {err}"
        );
    }

    #[test]
    fn v4_skips_unknown_slot_type_but_mac_covers_stored_bytes() {
        let vault_id = [0xABu8; VAULT_ID_SIZE];
        let wrap = vec![0x11u8; 60];
        let salt = vec![0x22u8; SALT_SIZE];
        // Placeholder MAC; we recompute after excision.
        let placeholder_mac = [0u8; CONFIG_MAC_SIZE];
        let unknown_slot = format!(
            r#"{{"id":1,"type":"future-widget","salt":"{}","wrapped":"{}"}}"#,
            b64(&salt),
            b64(&wrap),
        );
        let json = format!(
            r#"{{"version":4,"vault_id":"{}","epoch":1,"vk_wrap":"{}","omk_wrap":"{}","slots":[{{"id":0,"type":"passphrase","salt":"{}","kdf":{{"m_kib":131072,"t":4,"p":4}},"wrapped":"{}"}},{unknown_slot}],"config_mac":"{}"}}"#,
            b64(&vault_id),
            b64(&wrap),
            b64(&wrap),
            b64(&salt),
            b64(&wrap),
            b64(&placeholder_mac),
        );

        let cfg = parse_config(&json).expect("unknown type must not fail parse");
        if let OverlayConfig::V4 { slots, .. } = &cfg {
            assert_eq!(slots.len(), 1, "only known slot kept in-memory");
            assert_eq!(slots[0].id, 0);
            assert_eq!(slots[0].kind, SlotType::Passphrase);
        } else {
            panic!("expected V4");
        }

        // MAC over stored bytes: unknown slot remains in the raw header, so a
        // MAC computed on the full excised header covers it.
        let vk = [0x77u8; KEY_SIZE];
        let without = excise_config_mac_field(&json).unwrap();
        assert!(
            without.contains("future-widget"),
            "excised header must still contain the unknown slot"
        );
        assert!(
            !without.contains("config_mac"),
            "excised header must not contain config_mac key"
        );
        let mac = compute_config_mac_v4(&vk, &vault_id, without.as_bytes()).unwrap();
        // Rebuild header with real MAC and verify.
        let with_mac = format!(
            r#"{{"version":4,"vault_id":"{}","epoch":1,"vk_wrap":"{}","omk_wrap":"{}","slots":[{{"id":0,"type":"passphrase","salt":"{}","kdf":{{"m_kib":131072,"t":4,"p":4}},"wrapped":"{}"}},{unknown_slot}],"config_mac":"{}"}}"#,
            b64(&vault_id),
            b64(&wrap),
            b64(&wrap),
            b64(&salt),
            b64(&wrap),
            b64(&mac),
        );
        verify_config_mac_v4(&vk, &vault_id, &with_mac, &mac).expect("MAC must verify");
        // Tamper unknown slot content: MAC fails.
        let tampered = with_mac.replace("future-widget", "future-widgetX");
        assert!(verify_config_mac_v4(&vk, &vault_id, &tampered, &mac).is_err());
    }

    #[test]
    fn v4_rejects_duplicate_slot_ids() {
        let json = V4Fixture {
            n_slots: 2,
            duplicate_id: true,
            ..Default::default()
        }
        .to_json();
        let err = parse_config(&json).unwrap_err();
        assert!(
            err.contains("duplicate slot id"),
            "expected duplicate id reject, got: {err}"
        );
    }

    #[test]
    fn v4_version_helpers_and_derive_fail_closed() {
        let json = V4Fixture::default().to_json();
        let cfg = parse_config(&json).expect("valid v4 fixture");
        assert_eq!(cfg.version(), VERSION_V4);
        assert_eq!(cfg.version(), 4);
        assert!(!cfg.is_read_only());
        assert!(!cfg.requires_keyfile()); // v4 inspects slots at unlock (T4)
        assert_eq!(cfg.vault_id(), Some([0xABu8; VAULT_ID_SIZE]));
        let err = derive_master_key(&cfg, "anything").unwrap_err();
        assert!(
            err.contains("keyslots"),
            "derive_master_key must fail closed on v4: {err}"
        );
        assert!(verify_config_mac(&cfg, &[0u8; KEY_SIZE]).is_err());
    }

    #[test]
    fn v4_unknown_version_still_hard_errors() {
        let bad = serde_json::json!({
            "version": 99,
            "salt": b64(&[1u8; SALT_V3_SIZE]),
        })
        .to_string();
        let err = parse_config(&bad).unwrap_err();
        assert!(
            err.contains("unsupported crypt config version 99") && err.contains("please update"),
            "expected update message, got: {err}"
        );
    }

    #[test]
    fn v4_config_mac_round_trip_and_excise() {
        let vk = [0x42u8; KEY_SIZE];
        let vault_id = [0x11u8; VAULT_ID_SIZE];
        // Hand-built header: config_mac in the middle so both comma cases work.
        let body = format!(
            r#"{{"version":4,"vault_id":"{}","epoch":1,"vk_wrap":"{}","config_mac":"PLACEHOLDER","omk_wrap":"{}","slots":[]}}"#,
            b64(&vault_id),
            b64(&[0u8; 60]),
            b64(&[0u8; 60]),
        );
        // First compute MAC over a header with a zero MAC field excised.
        let zero_mac = [0u8; CONFIG_MAC_SIZE];
        let with_zero = body.replace("PLACEHOLDER", &b64(&zero_mac));
        let without = excise_config_mac_field(&with_zero).unwrap();
        assert!(!without.contains("config_mac"));
        assert!(without.contains("\"omk_wrap\""));
        let mac = compute_config_mac_v4(&vk, &vault_id, without.as_bytes()).unwrap();
        let with_mac = body.replace("PLACEHOLDER", &b64(&mac));
        verify_config_mac_v4(&vk, &vault_id, &with_mac, &mac).unwrap();

        // Wrong VK fails.
        let wrong_vk = [0x43u8; KEY_SIZE];
        assert!(verify_config_mac_v4(&wrong_vk, &vault_id, &with_mac, &mac).is_err());

        // Trailing-comma form: config_mac last before closing brace.
        let trailing = format!(
            r#"{{"version":4,"vault_id":"{}","epoch":2,"vk_wrap":"{}","omk_wrap":"{}","slots":[],"config_mac":"{}"}}"#,
            b64(&vault_id),
            b64(&[1u8; 60]),
            b64(&[1u8; 60]),
            b64(&zero_mac),
        );
        let w = excise_config_mac_field(&trailing).unwrap();
        assert!(!w.contains("config_mac"));
        let mac2 = compute_config_mac_v4(&vk, &vault_id, w.as_bytes()).unwrap();
        let trailing_ok = format!(
            r#"{{"version":4,"vault_id":"{}","epoch":2,"vk_wrap":"{}","omk_wrap":"{}","slots":[],"config_mac":"{}"}}"#,
            b64(&vault_id),
            b64(&[1u8; 60]),
            b64(&[1u8; 60]),
            b64(&mac2),
        );
        verify_config_mac_v4(&vk, &vault_id, &trailing_ok, &mac2).unwrap();
    }

    #[test]
    fn v4_tsv_not_yet_supported() {
        let tsv = "version\t4\nvault_id\tabc\n";
        let err = parse_config(tsv).unwrap_err();
        assert!(err.contains("v4 TSV not yet supported"), "got: {err}");
    }
}
