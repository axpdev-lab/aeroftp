// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! AeroCrypt v4 keyslot primitives (Tier 2).
//!
//! Slot-key derivation, canonical hand-built AAD for the VK / slot / OMK wraps,
//! and thin wrap/unwrap helpers over the shared AES-256-GCM-SIV codec.
//! Format layout and migration live in later modules; this file owns only the
//! pure crypto building blocks (spec 08 F6-F9, migration-build 09 section 7a).

use super::overlay::VAULT_ID_SIZE;
use super::{
    decrypt_with_aad, derive_base_kek, derive_base_kek_with_keyfile, encrypt_with_aad,
    keyfile_digest_from_file, KEY_SIZE, SALT_SIZE,
};

/// Maximum number of slots in a v4 header (F9 DoS cap).
pub const MAX_SLOTS: usize = 16;

/// Floor for Argon2id memory (KiB) accepted by the hardened parser (F9).
pub const MIN_ARGON2_MEM_KIB: u32 = 8 * 1024;
/// Cap for Argon2id memory (KiB) accepted by the hardened parser (F9).
pub const MAX_ARGON2_MEM_KIB: u32 = 1024 * 1024;
/// Floor for Argon2id time cost (passes).
pub const MIN_ARGON2_TIME: u32 = 1;
/// Cap for Argon2id time cost (passes).
pub const MAX_ARGON2_TIME: u32 = 16;
/// Floor for Argon2id parallelism (lanes).
pub const MIN_ARGON2_LANES: u32 = 1;
/// Cap for Argon2id parallelism (lanes).
pub const MAX_ARGON2_LANES: u32 = 8;

/// Wire version byte bound into every v4 AAD (marker version 4).
pub const VERSION_V4: u8 = 4;

/// Canonical AAD label for the epoch_key -> VK wrap.
const AAD_LABEL_VK: &[u8] = b"aerocrypt v4 vk-wrap";
/// Canonical AAD label for the slot_key -> epoch_key wrap.
const AAD_LABEL_SLOT: &[u8] = b"aerocrypt v4 slot-wrap";
/// Canonical AAD label for the OMK wrap (immutable; no epoch).
const AAD_LABEL_OMK: &[u8] = b"aerocrypt v4 omk-wrap";

/// Slot factor kind, emitted as a fixed 1-byte tag in AAD (never the JSON string).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SlotType {
    Passphrase = 1,
    /// Legacy scheme `aecr-t1-combined-v1` (password + keyfile digest).
    Keyfile = 2,
    Recovery = 3,
    /// Reserved: gated on the FIDO2 cross-platform spike (T8).
    Fido2Hmac = 4,
    /// F8 multi-factor composition (built in a later task).
    And = 5,
    /// Reserved for a future Shamir K-of-N; not built in v4.0.
    Threshold = 6,
}

impl SlotType {
    /// Wire tag for AAD and on-disk encoding.
    pub fn tag(self) -> u8 {
        self as u8
    }

    /// Parse a wire tag. Unknown tags yield `None` (F9: skip for unlock, keep MAC-covered).
    pub fn from_tag(tag: u8) -> Option<Self> {
        match tag {
            1 => Some(SlotType::Passphrase),
            2 => Some(SlotType::Keyfile),
            3 => Some(SlotType::Recovery),
            4 => Some(SlotType::Fido2Hmac),
            5 => Some(SlotType::And),
            6 => Some(SlotType::Threshold),
            _ => None,
        }
    }
}

/// Argon2id parameters stored in a KDF slot. Derivation always uses the v3
/// profile helpers (`argon2_mem_kib` / `argon2_time` / `argon2_lanes`); the
/// stored values are bound into AAD and floored/capped by the F9 parser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Argon2Params {
    pub m_kib: u32,
    pub t: u32,
    pub p: u32,
}

impl Argon2Params {
    /// The audited v3 profile (128 MiB / t=4 / p=4).
    pub fn v3_profile() -> Self {
        Self {
            m_kib: super::argon2_mem_kib(),
            t: super::argon2_time(),
            p: super::argon2_lanes(),
        }
    }
}

/// One member of an AND-composition slot (F8). Nested AND is not expressed here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AndMember {
    pub kind: SlotType,
    pub salt: Vec<u8>,
    pub kdf: Option<Argon2Params>,
    /// Present for fido2-hmac members only.
    pub credential_id: Option<Vec<u8>>,
    /// Present for fido2-hmac members only.
    pub hmac_salt: Option<Vec<u8>>,
}

/// Type-specific public binding carried in the slot and bound into AAD_SLOT.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotBinding {
    /// Passphrase / keyfile: no public binding.
    None,
    /// Recovery code vault prefix (first chars of Crockford-base32 vault_id).
    Recovery { vault_prefix: String },
    /// FIDO2 hmac-secret public data (gated; reserved shape).
    Fido2 {
        credential_id: Vec<u8>,
        hmac_salt: Vec<u8>,
    },
    /// AND-composition member list (F8).
    And { members: Vec<AndMember> },
}

/// One keyslot record (in-memory; on-disk encoding is T2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slot {
    pub id: u32,
    pub kind: SlotType,
    pub salt: Vec<u8>,
    pub kdf: Option<Argon2Params>,
    pub binding: SlotBinding,
    /// Nonce-prefixed GCM-SIV ciphertext of epoch_key under slot_key.
    pub wrapped: Vec<u8>,
}

/// Credential material used to derive a slot key. Lifetime-bound to caller data.
#[derive(Debug, Clone, Copy)]
pub enum SlotFactor<'a> {
    /// Password / passphrase for a passphrase slot.
    Passphrase(&'a str),
    /// Keyfile factor: password + raw keyfile bytes (canonicalized + digested).
    Keyfile {
        password: &'a str,
        keyfile_bytes: &'a [u8],
    },
    /// Keyfile factor with a precomputed digest (avoids re-reading the file).
    KeyfileDigest {
        password: &'a str,
        digest: &'a [u8; KEY_SIZE],
    },
    /// Recovery code (caller supplies the normalized form).
    Recovery(&'a str),
}

/// Derive the 32-byte slot key for `kind` from `factor` and `salt`.
///
/// KDF slots always derive with the shared v3 Argon2id profile
/// ([`derive_base_kek`] / [`derive_base_kek_with_keyfile`]). Stored `kdf` params
/// are not used for derivation here; they are metadata for AAD and the F9 parser.
/// Fido2Hmac / And / Threshold return an explicit "not built" error in T1.
pub fn derive_slot_key(
    kind: SlotType,
    factor: &SlotFactor<'_>,
    salt: &[u8; SALT_SIZE],
    _kdf: Option<&Argon2Params>,
) -> Result<[u8; KEY_SIZE], String> {
    match kind {
        SlotType::Passphrase => match factor {
            SlotFactor::Passphrase(password) => derive_base_kek(password, salt),
            _ => Err("factor does not match passphrase slot".to_string()),
        },
        SlotType::Keyfile => match factor {
            SlotFactor::Keyfile {
                password,
                keyfile_bytes,
            } => {
                let digest = keyfile_digest_from_file(keyfile_bytes)?;
                derive_base_kek_with_keyfile(password, Some(&digest), salt)
            }
            SlotFactor::KeyfileDigest { password, digest } => {
                derive_base_kek_with_keyfile(password, Some(digest), salt)
            }
            _ => Err("factor does not match keyfile slot".to_string()),
        },
        SlotType::Recovery => match factor {
            SlotFactor::Recovery(code) => derive_base_kek(code, salt),
            _ => Err("factor does not match recovery slot".to_string()),
        },
        SlotType::Fido2Hmac => Err("fido2-hmac slot type is not built yet (T8)".to_string()),
        SlotType::And => Err("and slot type is not built yet (T7/F8)".to_string()),
        SlotType::Threshold => Err("threshold slot type is reserved and not built".to_string()),
    }
}

/// Canonical AAD for wrapping VK under epoch_key:
/// `b"aerocrypt v4 vk-wrap" || vault_id(16) || 0x04 || u32_le(epoch)`.
pub fn aad_vk(vault_id: &[u8; VAULT_ID_SIZE], epoch: u32) -> Vec<u8> {
    let mut aad = Vec::with_capacity(AAD_LABEL_VK.len() + VAULT_ID_SIZE + 1 + 4);
    aad.extend_from_slice(AAD_LABEL_VK);
    aad.extend_from_slice(vault_id);
    aad.push(VERSION_V4);
    aad.extend_from_slice(&epoch.to_le_bytes());
    aad
}

/// Canonical AAD for wrapping OMK under the VK-derived OMK wrap key.
/// No epoch: OMK is immutable across revocation (09 section 7a).
/// `b"aerocrypt v4 omk-wrap" || vault_id(16) || 0x04`.
pub fn aad_omk(vault_id: &[u8; VAULT_ID_SIZE]) -> Vec<u8> {
    let mut aad = Vec::with_capacity(AAD_LABEL_OMK.len() + VAULT_ID_SIZE + 1);
    aad.extend_from_slice(AAD_LABEL_OMK);
    aad.extend_from_slice(vault_id);
    aad.push(VERSION_V4);
    aad
}

/// Canonical AAD for wrapping epoch_key under a slot_key (spec 08 section 3).
///
/// ```text
/// b"aerocrypt v4 slot-wrap" || vault_id(16) || 0x04 || u32_le(epoch)
///   || u32_le(id) || tag || u32_le(salt.len) || salt
///   || (kdf: u32_le(m) || u32_le(t) || u32_le(p))?
///   || u32_le(binding.len) || binding_bytes
/// ```
pub fn aad_slot(
    vault_id: &[u8; VAULT_ID_SIZE],
    epoch: u32,
    id: u32,
    kind: SlotType,
    salt: &[u8],
    kdf: Option<&Argon2Params>,
    binding: &SlotBinding,
) -> Vec<u8> {
    let binding_bytes = encode_binding(binding);
    let mut aad = Vec::with_capacity(
        AAD_LABEL_SLOT.len()
            + VAULT_ID_SIZE
            + 1
            + 4
            + 4
            + 1
            + 4
            + salt.len()
            + if kdf.is_some() { 12 } else { 0 }
            + 4
            + binding_bytes.len(),
    );
    aad.extend_from_slice(AAD_LABEL_SLOT);
    aad.extend_from_slice(vault_id);
    aad.push(VERSION_V4);
    aad.extend_from_slice(&epoch.to_le_bytes());
    aad.extend_from_slice(&id.to_le_bytes());
    aad.push(kind.tag());
    aad.extend_from_slice(&(salt.len() as u32).to_le_bytes());
    aad.extend_from_slice(salt);
    if let Some(params) = kdf {
        aad.extend_from_slice(&params.m_kib.to_le_bytes());
        aad.extend_from_slice(&params.t.to_le_bytes());
        aad.extend_from_slice(&params.p.to_le_bytes());
    }
    aad.extend_from_slice(&(binding_bytes.len() as u32).to_le_bytes());
    aad.extend_from_slice(&binding_bytes);
    aad
}

/// Encode a [`SlotBinding`] as the variable-length payload bound into AAD_SLOT.
///
/// Layout is hand-built binary (length-prefix variable fields with `u32_le`),
/// never JSON. Both ends rebuild this from the parsed slot; it is not parsed
/// from the AAD itself.
fn encode_binding(binding: &SlotBinding) -> Vec<u8> {
    match binding {
        SlotBinding::None => Vec::new(),
        SlotBinding::Recovery { vault_prefix } => vault_prefix.as_bytes().to_vec(),
        SlotBinding::Fido2 {
            credential_id,
            hmac_salt,
        } => {
            let mut out = Vec::with_capacity(4 + credential_id.len() + 4 + hmac_salt.len());
            out.extend_from_slice(&(credential_id.len() as u32).to_le_bytes());
            out.extend_from_slice(credential_id);
            out.extend_from_slice(&(hmac_salt.len() as u32).to_le_bytes());
            out.extend_from_slice(hmac_salt);
            out
        }
        SlotBinding::And { members } => {
            let mut out = Vec::new();
            out.extend_from_slice(&(members.len() as u32).to_le_bytes());
            for m in members {
                out.push(m.kind.tag());
                out.extend_from_slice(&(m.salt.len() as u32).to_le_bytes());
                out.extend_from_slice(&m.salt);
                match m.kdf {
                    Some(params) => {
                        out.push(1);
                        out.extend_from_slice(&params.m_kib.to_le_bytes());
                        out.extend_from_slice(&params.t.to_le_bytes());
                        out.extend_from_slice(&params.p.to_le_bytes());
                    }
                    None => out.push(0),
                }
                match (&m.credential_id, &m.hmac_salt) {
                    (Some(cid), Some(hs)) => {
                        out.push(1);
                        out.extend_from_slice(&(cid.len() as u32).to_le_bytes());
                        out.extend_from_slice(cid);
                        out.extend_from_slice(&(hs.len() as u32).to_le_bytes());
                        out.extend_from_slice(hs);
                    }
                    _ => out.push(0),
                }
            }
            out
        }
    }
}

/// Wrap `plaintext` under `slot_key` with domain-separating `aad` (AES-256-GCM-SIV).
pub fn wrap(slot_key: &[u8; KEY_SIZE], plaintext: &[u8], aad: &[u8]) -> Result<Vec<u8>, String> {
    encrypt_with_aad(slot_key, plaintext, aad)
}

/// Unwrap a nonce-prefixed payload under `slot_key` binding `aad`.
pub fn unwrap(slot_key: &[u8; KEY_SIZE], wrapped: &[u8], aad: &[u8]) -> Result<Vec<u8>, String> {
    decrypt_with_aad(slot_key, wrapped, aad)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aerocrypt::overlay::{self, OverlayConfig};

    /// Load-bearing migration invariant (09 section 7a): slot 0 with the v3 salt
    /// and passphrase yields the same key as `derive_base_kek` and as the v3
    /// overlay master key.
    #[test]
    fn slot0_key_equals_v3_master_key() {
        let salt = [0x42u8; SALT_SIZE];
        let password = "migration-invariant-password";
        let kdf = Argon2Params::v3_profile();

        let slot_key = derive_slot_key(
            SlotType::Passphrase,
            &SlotFactor::Passphrase(password),
            &salt,
            Some(&kdf),
        )
        .expect("derive_slot_key");
        let base = derive_base_kek(password, &salt).expect("derive_base_kek");
        let master = overlay::derive_master_key(&OverlayConfig::v3_bootstrap(salt), password)
            .expect("derive_master_key");

        assert_eq!(
            slot_key, base,
            "slot0 key must equal derive_base_kek(password, v3_salt)"
        );
        assert_eq!(
            slot_key, master,
            "slot0 key must equal overlay::derive_master_key for the same salt/password"
        );
    }

    #[test]
    fn aad_binding_rejects_tamper() {
        let vault_id = [0x11u8; VAULT_ID_SIZE];
        let epoch = 1u32;
        let id = 0u32;
        let salt = vec![0x99u8; SALT_SIZE];
        let kdf = Argon2Params::v3_profile();
        let binding = SlotBinding::None;
        let slot_key = [0x07u8; KEY_SIZE];
        let epoch_key = [0x03u8; KEY_SIZE];

        let aad = aad_slot(
            &vault_id,
            epoch,
            id,
            SlotType::Passphrase,
            &salt,
            Some(&kdf),
            &binding,
        );
        let wrapped = wrap(&slot_key, &epoch_key, &aad).expect("wrap");
        assert_eq!(
            unwrap(&slot_key, &wrapped, &aad).expect("unwrap same AAD"),
            epoch_key
        );

        // Wrong slot id.
        let aad_id = aad_slot(
            &vault_id,
            epoch,
            1,
            SlotType::Passphrase,
            &salt,
            Some(&kdf),
            &binding,
        );
        assert!(
            unwrap(&slot_key, &wrapped, &aad_id).is_err(),
            "tampered slot id must fail"
        );

        // Wrong vault_id.
        let mut bad_vid = vault_id;
        bad_vid[0] ^= 0xff;
        let aad_vid = aad_slot(
            &bad_vid,
            epoch,
            id,
            SlotType::Passphrase,
            &salt,
            Some(&kdf),
            &binding,
        );
        assert!(
            unwrap(&slot_key, &wrapped, &aad_vid).is_err(),
            "tampered vault_id must fail"
        );

        // Wrong epoch.
        let aad_epoch = aad_slot(
            &vault_id,
            2,
            id,
            SlotType::Passphrase,
            &salt,
            Some(&kdf),
            &binding,
        );
        assert!(
            unwrap(&slot_key, &wrapped, &aad_epoch).is_err(),
            "tampered epoch must fail"
        );

        // Wrong slot-type tag (passphrase -> keyfile).
        let aad_tag = aad_slot(
            &vault_id,
            epoch,
            id,
            SlotType::Keyfile,
            &salt,
            Some(&kdf),
            &binding,
        );
        assert!(
            unwrap(&slot_key, &wrapped, &aad_tag).is_err(),
            "tampered slot-type tag must fail"
        );
    }

    #[test]
    fn wrap_unwrap_round_trip() {
        let vault_id = [0xABu8; VAULT_ID_SIZE];
        let epoch_key = [0x02u8; KEY_SIZE];
        let vk = [0x05u8; KEY_SIZE];
        let omk = [0x06u8; KEY_SIZE];
        let omk_kek = [0x04u8; KEY_SIZE];

        // VK wrap under epoch_key with AAD_VK.
        let aad_v = aad_vk(&vault_id, 1);
        let wrapped_vk = wrap(&epoch_key, &vk, &aad_v).expect("wrap VK");
        assert_eq!(
            unwrap(&epoch_key, &wrapped_vk, &aad_v).expect("unwrap VK"),
            vk
        );
        // Epoch mismatch on AAD_VK fails closed.
        let aad_v_bad = aad_vk(&vault_id, 2);
        assert!(unwrap(&epoch_key, &wrapped_vk, &aad_v_bad).is_err());

        // OMK wrap (no epoch in AAD).
        let aad_o = aad_omk(&vault_id);
        let wrapped_omk = wrap(&omk_kek, &omk, &aad_o).expect("wrap OMK");
        assert_eq!(
            unwrap(&omk_kek, &wrapped_omk, &aad_o).expect("unwrap OMK"),
            omk
        );
        // vault_id mismatch fails closed.
        let mut bad_vid = vault_id;
        bad_vid[0] ^= 0x01;
        let aad_o_bad = aad_omk(&bad_vid);
        assert!(unwrap(&omk_kek, &wrapped_omk, &aad_o_bad).is_err());
    }

    #[test]
    fn slot_type_tag_round_trip() {
        for kind in [
            SlotType::Passphrase,
            SlotType::Keyfile,
            SlotType::Recovery,
            SlotType::Fido2Hmac,
            SlotType::And,
            SlotType::Threshold,
        ] {
            assert_eq!(SlotType::from_tag(kind.tag()), Some(kind));
        }
        assert_eq!(SlotType::from_tag(0), None);
        assert_eq!(SlotType::from_tag(7), None);
        assert_eq!(SlotType::from_tag(255), None);
    }

    #[test]
    fn fido2_and_threshold_not_built() {
        let salt = [0u8; SALT_SIZE];
        let factor = SlotFactor::Passphrase("x");
        assert!(derive_slot_key(SlotType::Fido2Hmac, &factor, &salt, None)
            .unwrap_err()
            .contains("not built"));
        assert!(derive_slot_key(SlotType::And, &factor, &salt, None)
            .unwrap_err()
            .contains("not built"));
        assert!(derive_slot_key(SlotType::Threshold, &factor, &salt, None)
            .unwrap_err()
            .contains("reserved"));
    }
}
