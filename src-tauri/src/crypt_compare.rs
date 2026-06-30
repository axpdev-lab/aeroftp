//! Single source of truth for crypt-overlay AeroSync Compare.
//!
//! The GUI (`provider_compare_directories`), the CLI (`check` / `reconcile`),
//! and MCP (`check_tree`) all compare a plaintext local tree against an
//! encrypted remote. Without decryption every remote file carries an encrypted
//! name (and, for rclone-crypt, a ciphertext size), so every local file looks
//! "missing on remote" and is re-uploaded on every run (Ehud, discussion
//! #364). These helpers decrypt the remote relative paths and map rclone-crypt
//! ciphertext sizes back to plaintext so Compare matches.
//!
//! Everything here only needs unlocked keys, never Tauri state, so all three
//! callers share the exact same crypto. The GUI works on a
//! `HashMap<String, FileInfo>` and reuses the pure `decrypt_rel_*` /
//! `rclone_decrypted_size` helpers directly; the CLI and MCP work on the
//! `sync_core::RemoteEntry` vector and reuse [`normalize_remote_entries`].

use crate::providers::StorageProvider;
use crate::rclone_crypt::RcloneCryptKeys;
use zeroize::Zeroize;

/// rclone-crypt content overhead, inverted.
///
/// An rclone-crypt object is a 32-byte file header followed by 64 KiB
/// plaintext data chunks, each chunk carrying a 16-byte Poly1305 tag. Given the
/// ciphertext object size, recover the plaintext size so a size-policy Compare
/// matches the local plaintext. Constants mirror `rclone_crypt.rs:22-37`.
pub fn rclone_decrypted_size(enc: u64) -> u64 {
    const HEADER: u64 = 32;
    const CHUNK_DATA: u64 = 65_536;
    const CHUNK_TAG: u64 = 16;
    const CHUNK_CIPHER: u64 = CHUNK_DATA + CHUNK_TAG;

    if enc <= HEADER {
        return 0;
    }
    let data = enc - HEADER;
    let full_chunks = data / CHUNK_CIPHER;
    let remainder = data % CHUNK_CIPHER;
    let remainder_plain = if remainder == 0 {
        0
    } else {
        remainder.saturating_sub(CHUNK_TAG)
    };
    full_chunks * CHUNK_DATA + remainder_plain
}

/// Decrypt every encrypted segment of an rclone-crypt relative path.
///
/// When `directory_name_encryption` is off only the leaf filename is
/// encrypted, so intermediate directory segments pass through verbatim.
/// Returns `None` when any encrypted segment fails to decrypt: the caller drops
/// such rows rather than treating ciphertext as a fake plaintext name.
pub fn decrypt_rel_rclone(keys: &RcloneCryptKeys, rel_path: &str) -> Option<String> {
    let segments: Vec<&str> = rel_path.split('/').collect();
    let last = segments.len().saturating_sub(1);
    let mut out = Vec::with_capacity(segments.len());
    for (index, segment) in segments.iter().enumerate() {
        if segment.is_empty() {
            out.push(String::new());
            continue;
        }
        if keys.directory_name_encryption || index == last {
            out.push(crate::rclone_crypt::decrypt_one_name(keys, segment)?);
        } else {
            out.push((*segment).to_string());
        }
    }
    Some(out.join("/"))
}

/// Decrypt every segment of an AeroCrypt relative path. Returns `None` when any
/// segment is not a valid AeroCrypt name (foreign / undecryptable), so the
/// caller drops the row.
pub fn decrypt_rel_aerocrypt(master_key: &[u8; 32], rel_path: &str) -> Option<String> {
    let mut out = Vec::new();
    for segment in rel_path.split('/') {
        if segment.is_empty() {
            out.push(String::new());
            continue;
        }
        out.push(crate::aerocrypt::names::decrypt_filename(
            master_key, segment,
        )?);
    }
    Some(out.join("/"))
}

/// Unlocked keys for one crypt-compare pass, independent of Tauri state.
///
/// Built from the GUI's already-unlocked vault state, or unlocked on demand for
/// the CLI / MCP via [`unlock_overlay_keys`].
pub enum CryptCompareKeys {
    Rclone(RcloneCryptKeys),
    /// AeroCrypt master key. Content-size mapping is deferred (see
    /// [`CryptCompareKeys::decrypted_size`]), so Compare is name-aware only.
    AeroCrypt([u8; 32]),
}

impl Drop for CryptCompareKeys {
    fn drop(&mut self) {
        // Zeroize the raw AeroCrypt master key on drop, matching the zeroize-on-
        // drop guarantee of AeroCryptKeys / RcloneCryptKeys. The Rclone variant
        // holds a RcloneCryptKeys, which zeroizes its own key material via its
        // own Drop, so only the bare master-key array needs wiping here.
        if let Self::AeroCrypt(master_key) = self {
            master_key.zeroize();
        }
    }
}

impl CryptCompareKeys {
    /// Decrypt one remote relative path to its plaintext form, or `None` when
    /// the row is foreign / undecryptable and should be dropped.
    pub fn decrypt_rel(&self, rel: &str) -> Option<String> {
        match self {
            Self::Rclone(keys) => decrypt_rel_rclone(keys, rel),
            Self::AeroCrypt(master_key) => decrypt_rel_aerocrypt(master_key, rel),
        }
    }

    /// Map a remote ciphertext size to the plaintext size for Compare.
    ///
    /// `sync_core::RemoteEntry` rows are files only (directories are recursed,
    /// never emitted), so the rclone mapping applies to every row. AeroCrypt
    /// content-size decryption needs the versioned overlay container decoder
    /// and is deliberately deferred: AeroCrypt Compare matches by name, and a
    /// size-policy compare may still re-flag AeroCrypt files until the
    /// follow-up lands.
    pub fn decrypted_size(&self, size: u64) -> u64 {
        match self {
            Self::Rclone(_) => rclone_decrypted_size(size),
            Self::AeroCrypt(_) => size,
        }
    }

    /// Detect a wrong rclone-crypt overlay password from an all-drop result.
    ///
    /// rclone-crypt has no config MAC, so `unlock_overlay_keys` cannot verify
    /// the password up front: any non-empty password derives valid-shaped keys.
    /// A wrong password then decrypts NOTHING, so every remote row is dropped by
    /// [`normalize_remote_entries`] and Compare would silently re-flag the whole
    /// tree as missing (the #364 symptom, for a wrong key). When a non-empty
    /// remote normalizes to zero rows under an rclone overlay, the caller fails
    /// closed instead. AeroCrypt is already verified by its config MAC at unlock
    /// time, so it never reports here (and its config file legitimately drops,
    /// which would false-positive this heuristic).
    pub fn wrong_key_suspected(&self, raw_len: usize, kept_len: usize) -> bool {
        matches!(self, Self::Rclone(_)) && raw_len > 0 && kept_len == 0
    }
}

/// Normalize a scanned remote tree for a crypt-overlay Compare.
///
/// Decrypts each `rel_path`, maps the size, drops foreign / undecryptable rows,
/// and clears any server-side checksum (the remote hash is over ciphertext and
/// would false-conflict against the plaintext local hash). Shared by the CLI
/// `check` / `reconcile` and MCP `check_tree`, which all feed the result into
/// `sync_core::compare_trees`.
pub fn normalize_remote_entries(
    entries: Vec<crate::sync_core::RemoteEntry>,
    keys: &CryptCompareKeys,
) -> Vec<crate::sync_core::RemoteEntry> {
    let mut out = Vec::with_capacity(entries.len());
    for mut entry in entries {
        let Some(plain) = keys.decrypt_rel(&entry.rel_path) else {
            continue;
        };
        entry.size = keys.decrypted_size(entry.size);
        entry.rel_path = plain;
        // Never compare a ciphertext checksum against the plaintext local hash.
        entry.checksum_alg = None;
        entry.checksum_hex = None;
        out.push(entry);
    }
    out
}

/// Overlay binding fields the CLI / MCP need to unlock crypt-compare keys
/// outside the GUI (the GUI already has unlocked vault state).
pub struct OverlayUnlockParams {
    /// "rclone-crypt" or "aerocrypt".
    pub kind: String,
    /// Remote scope (absolute remote path) where the AeroCrypt overlay config
    /// `.aeroftp-crypt.json` lives. Ignored for rclone-crypt.
    pub remote_scope: String,
    /// rclone-crypt filename encryption mode ("standard" / "obfuscate" /
    /// "off"). Ignored for aerocrypt.
    pub filename_encryption: String,
    /// rclone-crypt directory-name encryption (default true). Ignored for
    /// aerocrypt.
    pub directory_name_encryption: bool,
    /// rclone-crypt name-off suffix override (`None` = rclone default ".bin").
    pub off_suffix: Option<String>,
}

/// AeroCrypt overlay config filename, written at the scope root by `crypt init`.
const AEROCRYPT_CONFIG_FILENAME: &str = ".aeroftp-crypt.json";

/// Unlock crypt-compare keys for the CLI / MCP from an overlay binding plus the
/// secret(s) read from the vault (or env).
///
/// rclone-crypt derives keys from password + salt (scrypt), no remote round
/// trip. AeroCrypt downloads the remote `.aeroftp-crypt.json` config from
/// `remote_scope`, derives the master key, and verifies the config MAC so a
/// wrong password fails closed instead of silently dropping every row.
pub async fn unlock_overlay_keys(
    provider: &mut dyn StorageProvider,
    params: &OverlayUnlockParams,
    password: &str,
    salt: &str,
) -> Result<CryptCompareKeys, String> {
    match params.kind.as_str() {
        "rclone-crypt" => {
            let (name_key, data_key, name_tweak) =
                crate::rclone_crypt::derive_keys_with_tweak(password, salt)?;
            let filename_encryption = match params.filename_encryption.as_str() {
                "off" => crate::rclone_crypt::FilenameEncryption::Off,
                "obfuscate" => crate::rclone_crypt::FilenameEncryption::Obfuscate,
                _ => crate::rclone_crypt::FilenameEncryption::Standard,
            };
            let off_suffix = crate::rclone_crypt::resolve_off_suffix(params.off_suffix.as_deref());
            Ok(CryptCompareKeys::Rclone(RcloneCryptKeys {
                name_key,
                data_key,
                name_tweak,
                filename_encryption,
                off_suffix,
                directory_name_encryption: params.directory_name_encryption,
            }))
        }
        "aerocrypt" => {
            use crate::aerocrypt::overlay;
            let scope = params.remote_scope.trim_end_matches('/');
            let config_path = format!("{}/{}", scope, AEROCRYPT_CONFIG_FILENAME);
            let config_bytes = provider
                .download_to_bytes(&config_path)
                .await
                .map_err(|e| format!("Cannot read AeroCrypt overlay config: {}", e))?;
            let config_str = String::from_utf8_lossy(&config_bytes);
            let cfg = overlay::parse_config(&config_str)
                .map_err(|e| format!("Invalid AeroCrypt overlay config: {}", e))?;
            let master_key = overlay::derive_master_key(&cfg, password)
                .map_err(|e| format!("AeroCrypt key derivation failed: {}", e))?;
            overlay::verify_config_mac(&cfg, &master_key)
                .map_err(|e| format!("AeroCrypt unlock failed: {}", e))?;
            Ok(CryptCompareKeys::AeroCrypt(master_key))
        }
        other => Err(format!("Unsupported crypt overlay kind: {}", other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rclone_crypt::{
        derive_keys, derive_keys_with_tweak, encrypt_file_content, encrypt_name, FilenameEncryption,
    };
    use crate::sync_core::RemoteEntry;

    fn rclone_keys(directory_name_encryption: bool) -> RcloneCryptKeys {
        let (name_key, data_key, name_tweak) =
            derive_keys_with_tweak("compare-pass", "compare-salt").unwrap();
        RcloneCryptKeys {
            name_key,
            data_key,
            name_tweak,
            filename_encryption: FilenameEncryption::Standard,
            off_suffix: ".bin".to_string(),
            directory_name_encryption,
        }
    }

    fn entry(rel_path: &str, size: u64) -> RemoteEntry {
        RemoteEntry {
            rel_path: rel_path.to_string(),
            size,
            mtime: None,
            checksum_alg: Some("sha256".to_string()),
            checksum_hex: Some("ciphertext-hash".to_string()),
        }
    }

    #[test]
    fn rclone_decrypted_size_round_trips_encrypted_content_lengths() {
        let (_, data_key) = derive_keys("compare-size", "salt").unwrap();
        for size in [0usize, 1, 65_535, 65_536, 65_537, 200_000] {
            let plaintext: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
            let encrypted = encrypt_file_content(&plaintext, &data_key).unwrap();
            assert_eq!(
                rclone_decrypted_size(encrypted.len() as u64),
                plaintext.len() as u64,
                "encrypted length {} should map back to plaintext length {}",
                encrypted.len(),
                plaintext.len()
            );
        }
    }

    #[test]
    fn decrypt_rel_rclone_decrypts_all_segments_when_directory_names_are_encrypted() {
        let keys = rclone_keys(true);
        let encrypted_rel = ["alpha", "beta", "report.txt"]
            .into_iter()
            .map(|segment| encrypt_name(&keys.name_key, &keys.name_tweak, segment).unwrap())
            .collect::<Vec<_>>()
            .join("/");
        assert_eq!(
            decrypt_rel_rclone(&keys, &encrypted_rel).as_deref(),
            Some("alpha/beta/report.txt")
        );
    }

    #[test]
    fn decrypt_rel_rclone_decrypts_only_leaf_when_directory_names_are_plain() {
        let keys = rclone_keys(false);
        let encrypted_leaf = encrypt_name(&keys.name_key, &keys.name_tweak, "report.txt").unwrap();
        let mixed_rel = format!("alpha/beta/{}", encrypted_leaf);
        assert_eq!(
            decrypt_rel_rclone(&keys, &mixed_rel).as_deref(),
            Some("alpha/beta/report.txt")
        );
    }

    #[test]
    fn normalize_remote_entries_rclone_drops_foreign_names_and_maps_size() {
        let keys = rclone_keys(true);
        let encrypted_leaf = encrypt_name(&keys.name_key, &keys.name_tweak, "report.txt").unwrap();
        let encrypted_blob = encrypt_file_content(b"report body", &keys.data_key).unwrap();
        let entries = vec![
            entry(&encrypted_leaf, encrypted_blob.len() as u64),
            entry("not-base32-!!!", 999),
        ];

        let normalized =
            normalize_remote_entries(entries, &CryptCompareKeys::Rclone(rclone_keys(true)));

        assert_eq!(normalized.len(), 1);
        assert_eq!(normalized[0].rel_path, "report.txt");
        assert_eq!(normalized[0].size, 11);
        assert_eq!(normalized[0].checksum_alg, None);
        assert_eq!(normalized[0].checksum_hex, None);
        // `keys` kept alive only to build the fixtures above.
        drop(keys);
    }

    #[test]
    fn wrong_key_suspected_only_fires_for_rclone_total_drop() {
        let rclone = CryptCompareKeys::Rclone(rclone_keys(true));
        // N>0 remote rows, none decrypted: wrong rclone password.
        assert!(rclone.wrong_key_suspected(5, 0));
        // Some rows kept: key is good.
        assert!(!rclone.wrong_key_suspected(5, 3));
        // Empty remote: nothing to upload, not a wrong-key signal.
        assert!(!rclone.wrong_key_suspected(0, 0));
        // AeroCrypt is MAC-verified at unlock and its config row legitimately
        // drops, so the all-drop heuristic must never fire for it.
        let aero = CryptCompareKeys::AeroCrypt([7u8; 32]);
        assert!(!aero.wrong_key_suspected(5, 0));
    }

    #[test]
    fn normalize_remote_entries_aerocrypt_decrypts_names_and_defers_size() {
        let master_key = [7u8; 32];
        let encrypted_rel = ["alpha", "beta", "report.txt"]
            .into_iter()
            .map(|segment| crate::aerocrypt::names::encrypt_filename(&master_key, segment).unwrap())
            .collect::<Vec<_>>()
            .join("/");
        let entries = vec![entry(&encrypted_rel, 123), entry("not-base64-$$$", 999)];

        let normalized =
            normalize_remote_entries(entries, &CryptCompareKeys::AeroCrypt(master_key));

        assert_eq!(normalized.len(), 1);
        assert_eq!(normalized[0].rel_path, "alpha/beta/report.txt");
        // AeroCrypt size is deferred: the row keeps its raw ciphertext size.
        assert_eq!(normalized[0].size, 123);
        assert_eq!(normalized[0].checksum_alg, None);
        assert_eq!(normalized[0].checksum_hex, None);
    }
}
