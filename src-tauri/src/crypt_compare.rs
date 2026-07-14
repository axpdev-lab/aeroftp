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
    /// Saved profile id that owns the AeroCrypt metadata, when a profile-backed
    /// caller can identify it. Headerless AeroCrypt stores its public config
    /// under `aerocrypt_overlay_config_<id>` instead of on the remote.
    pub profile_id: Option<String>,
    /// Scoped, preloaded headerless AeroCrypt config JSON from
    /// `aerocrypt_overlay_config_<id>`. Callers that support per-user
    /// credential partitions pass this so unlock honors their selected user.
    pub local_config_json: Option<String>,
    /// Scoped salt of record from `aerocrypt_overlay_salt_<id>`, used to detect
    /// local keystore divergence before deriving from `local_config_json`.
    pub local_config_salt: Option<String>,
}

/// AeroCrypt overlay config filename, written at the scope root by `crypt init`.
#[allow(dead_code)]
const AEROCRYPT_CONFIG_FILENAME: &str = crate::aerocrypt::overlay::CRYPT_CONFIG_WRITE_NAME;

/// Unlock crypt-compare keys for the CLI / MCP from an overlay binding plus the
/// secret(s) read from the vault (or env).
///
/// rclone-crypt derives keys from password + salt (scrypt), no remote round
/// trip. AeroCrypt downloads the remote `.aeroftp-crypt.json` config from
/// `remote_scope`, derives the master key, and verifies the config MAC so a
/// wrong password fails closed instead of silently dropping every row.
///
/// `keyfile_digest` is the Tier 1 keyfile second factor, reconciled against
/// what the downloaded config requires exactly like the encrypting unlock
/// (`crypt_overlay_provider::unlock_overlay_keys_encrypting`): a keyfile vault
/// never unlocks password-only, a password-only vault rejects a spurious
/// keyfile, and rclone-crypt (which has no keyfiles) rejects one outright.
pub async fn unlock_overlay_keys(
    provider: &mut dyn StorageProvider,
    params: &OverlayUnlockParams,
    password: &str,
    salt: &str,
    keyfile_digest: Option<&[u8; 32]>,
) -> Result<CryptCompareKeys, String> {
    match params.kind.as_str() {
        "rclone-crypt" => {
            if keyfile_digest.is_some() {
                // Keyfiles are an AeroCrypt-only feature; a keyfile stored on an
                // rclone-crypt binding is a misconfiguration that must surface,
                // never be silently ignored.
                return Err(
                    "keyfiles are an AeroCrypt feature; this overlay is rclone-crypt".to_string(),
                );
            }
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
            let new_name = overlay::CRYPT_CONFIG_WRITE_NAME;
            let legacy_name = overlay::CRYPT_CONFIG_LEGACY_NAME;
            let new_path = format!("{}/{}", scope, new_name);
            let legacy_path = format!("{}/{}", scope, legacy_name);

            // read-both for D5
            let present_new = provider.exists(&new_path).await.unwrap_or(false);
            let present_legacy = if !present_new {
                provider.exists(&legacy_path).await.unwrap_or(false)
            } else {
                false
            };
            let present = present_new || present_legacy;
            let config_path = if present_new {
                new_path
            } else if present_legacy {
                legacy_path
            } else {
                new_path
            };

            let config_str = if present {
                let config_bytes = provider
                    .download_to_bytes(&config_path)
                    .await
                    .map_err(|e| format!("Cannot read AeroCrypt overlay config: {}", e))?;
                String::from_utf8_lossy(&config_bytes).into_owned()
            } else {
                let local = params
                    .local_config_json
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .ok_or_else(|| {
                        format!("Cannot read AeroCrypt overlay config: no overlay at {config_path}")
                    })?;
                crate::crypt_overlay_provider::validate_headerless_config_salt(
                    params.profile_id.as_deref().unwrap_or(""),
                    &local,
                    params.local_config_salt.as_deref(),
                )?;
                local
            };
            let cfg = overlay::parse_config(&config_str)
                .map_err(|e| format!("Invalid AeroCrypt overlay config: {}", e))?;
            // Reconcile the supplied keyfile against what the config requires
            // (v1/v2 never require one) before deriving.
            let keyfile_digest = match (cfg.requires_keyfile(), keyfile_digest) {
                (true, None) => {
                    return Err(
                        "this AeroCrypt overlay requires a keyfile (none was provided)".to_string(),
                    )
                }
                (false, Some(_)) => {
                    return Err(
                        "this AeroCrypt overlay was not created with a keyfile (remove the keyfile to unlock)"
                            .to_string(),
                    )
                }
                (true, kd) => kd,
                (false, _) => None,
            };
            let master_key =
                overlay::derive_master_key_with_keyfile(&cfg, password, keyfile_digest)
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

    /// Minimal read-only provider for `unlock_overlay_keys`: serves the remote
    /// `.aeroftp-crypt.json` config via `download_to_bytes` and nothing else
    /// (the read-side unlock never writes, unlike the encrypting twin's
    /// MemProvider in `crypt_overlay_provider` tests).
    struct ConfigProvider {
        files: std::collections::HashMap<String, Vec<u8>>,
    }

    #[async_trait::async_trait]
    impl StorageProvider for ConfigProvider {
        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
        fn provider_type(&self) -> crate::providers::ProviderType {
            crate::providers::ProviderType::Ftp
        }
        fn display_name(&self) -> String {
            "cfg-mem".into()
        }
        async fn connect(&mut self) -> Result<(), crate::providers::ProviderError> {
            Ok(())
        }
        async fn disconnect(&mut self) -> Result<(), crate::providers::ProviderError> {
            Ok(())
        }
        fn is_connected(&self) -> bool {
            true
        }
        async fn list(
            &mut self,
            _path: &str,
        ) -> Result<Vec<crate::providers::RemoteEntry>, crate::providers::ProviderError> {
            Ok(Vec::new())
        }
        async fn pwd(&mut self) -> Result<String, crate::providers::ProviderError> {
            Ok("/".into())
        }
        async fn cd(&mut self, _p: &str) -> Result<(), crate::providers::ProviderError> {
            Ok(())
        }
        async fn cd_up(&mut self) -> Result<(), crate::providers::ProviderError> {
            Ok(())
        }
        async fn download(
            &mut self,
            remote: &str,
            _local: &str,
            _cb: Option<Box<dyn Fn(u64, u64) + Send>>,
        ) -> Result<(), crate::providers::ProviderError> {
            Err(crate::providers::ProviderError::NotFound(remote.into()))
        }
        async fn download_to_bytes(
            &mut self,
            remote: &str,
        ) -> Result<Vec<u8>, crate::providers::ProviderError> {
            self.files
                .get(remote)
                .cloned()
                .ok_or_else(|| crate::providers::ProviderError::NotFound(remote.into()))
        }
        async fn upload(
            &mut self,
            _local: &str,
            remote: &str,
            _cb: Option<Box<dyn Fn(u64, u64) + Send>>,
        ) -> Result<(), crate::providers::ProviderError> {
            Err(crate::providers::ProviderError::NotFound(remote.into()))
        }
        async fn mkdir(&mut self, _p: &str) -> Result<(), crate::providers::ProviderError> {
            Ok(())
        }
        async fn delete(&mut self, _p: &str) -> Result<(), crate::providers::ProviderError> {
            Ok(())
        }
        async fn rmdir(&mut self, _p: &str) -> Result<(), crate::providers::ProviderError> {
            Ok(())
        }
        async fn rmdir_recursive(
            &mut self,
            _p: &str,
        ) -> Result<(), crate::providers::ProviderError> {
            Ok(())
        }
        async fn rename(
            &mut self,
            _from: &str,
            _to: &str,
        ) -> Result<(), crate::providers::ProviderError> {
            Ok(())
        }
        async fn stat(
            &mut self,
            path: &str,
        ) -> Result<crate::providers::RemoteEntry, crate::providers::ProviderError> {
            Err(crate::providers::ProviderError::NotFound(path.into()))
        }
        async fn size(&mut self, path: &str) -> Result<u64, crate::providers::ProviderError> {
            self.files
                .get(path)
                .map(|d| d.len() as u64)
                .ok_or_else(|| crate::providers::ProviderError::NotFound(path.into()))
        }
        async fn exists(&mut self, path: &str) -> Result<bool, crate::providers::ProviderError> {
            Ok(self.files.contains_key(path))
        }
        async fn keep_alive(&mut self) -> Result<(), crate::providers::ProviderError> {
            Ok(())
        }
        async fn server_info(&mut self) -> Result<String, crate::providers::ProviderError> {
            Ok("cfg-mem".into())
        }
    }

    /// Provider holding a v3 config at `<scope>/.aeroftp-crypt.json`: a keyfile
    /// vault when `keyfile_digest` is `Some` (kdf_inputs + vault_id, MAC over
    /// the extended info string), a plain password-only vault otherwise.
    fn provider_with_v3_config(
        scope: &str,
        password: &str,
        keyfile_digest: Option<&[u8; 32]>,
    ) -> ConfigProvider {
        use crate::aerocrypt::overlay;
        let salt = overlay::random_salt_v3();
        let tmp = overlay::OverlayConfig::v3_bootstrap(salt);
        let master_key = overlay::derive_master_key_with_keyfile(&tmp, password, keyfile_digest)
            .expect("derive fixture master key");
        let json = match keyfile_digest {
            Some(_) => overlay::init_config_v3_with_keyfile(
                &salt,
                &master_key,
                &overlay::random_vault_id(),
                None,
                overlay::SaltMode::PerVault,
            ),
            None => overlay::init_config_v3(&salt, &master_key),
        }
        .expect("build fixture config");
        let mut files = std::collections::HashMap::new();
        files.insert(
            format!("{}/{}", scope, AEROCRYPT_CONFIG_FILENAME),
            json.into_bytes(),
        );
        ConfigProvider { files }
    }

    fn aerocrypt_params(scope: &str) -> OverlayUnlockParams {
        OverlayUnlockParams {
            kind: "aerocrypt".to_string(),
            remote_scope: scope.to_string(),
            filename_encryption: String::new(),
            directory_name_encryption: true,
            off_suffix: None,
            profile_id: None,
            local_config_json: None,
            local_config_salt: None,
        }
    }

    /// `expect_err` for unlock results. [`CryptCompareKeys`] intentionally has
    /// no `Debug` impl (key material), so `Result::expect_err` cannot be used.
    fn unlock_err(res: Result<CryptCompareKeys, String>, msg: &str) -> String {
        match res {
            Ok(_) => panic!("{msg}"),
            Err(e) => e,
        }
    }

    #[tokio::test]
    async fn unlock_overlay_keys_reconciles_keyfile_and_fails_closed() {
        // Read-side twin of crypt_overlay_provider's
        // `aerocrypt_keyfile_vault_reconciles_and_fails_closed`: the Compare /
        // check_tree unlock must enforce the exact same keyfile reconcile.
        let digest = crate::aerocrypt::keyfile_digest_from_file(
            crate::aerocrypt::generate_keyfile_v1().as_bytes(),
        )
        .unwrap();
        let params = aerocrypt_params("/KfVault");
        let mut kf_vault = provider_with_v3_config("/KfVault", "pw", Some(&digest));

        // Right password + right digest unlocks (config MAC verifies).
        let keys = unlock_overlay_keys(&mut kf_vault, &params, "pw", "", Some(&digest))
            .await
            .expect("correct password + keyfile must unlock");
        assert!(matches!(keys, CryptCompareKeys::AeroCrypt(_)));

        // Password-only on a keyfile vault fails closed with the frozen string.
        let err = unlock_err(
            unlock_overlay_keys(&mut kf_vault, &params, "pw", "", None).await,
            "password-only on a keyfile vault must fail closed",
        );
        assert!(
            err.contains("requires a keyfile"),
            "clear reconcile error: {err}"
        );

        // A wrong keyfile fails closed at the MAC check.
        let wrong = crate::aerocrypt::keyfile_digest(b"not the keyfile");
        let res = unlock_overlay_keys(&mut kf_vault, &params, "pw", "", Some(&wrong)).await;
        assert!(res.is_err(), "a wrong keyfile must fail closed");

        // A keyfile-only vault (EMPTY password, legal by design) unlocks.
        let mut kf_only = provider_with_v3_config("/KfVault", "", Some(&digest));
        unlock_overlay_keys(&mut kf_only, &params, "", "", Some(&digest))
            .await
            .expect("empty password + keyfile must unlock a keyfile-only vault");

        // A spurious keyfile on a password-only vault is rejected with the
        // frozen string, never a confusing derived-key mismatch.
        let pw_params = aerocrypt_params("/PwVault");
        let mut pw_vault = provider_with_v3_config("/PwVault", "pw", None);
        let err = unlock_err(
            unlock_overlay_keys(&mut pw_vault, &pw_params, "pw", "", Some(&digest)).await,
            "a spurious keyfile on a password-only vault must be rejected",
        );
        assert!(
            err.contains("was not created with a keyfile"),
            "clear reconcile error: {err}"
        );
        unlock_overlay_keys(&mut pw_vault, &pw_params, "pw", "", None)
            .await
            .expect("password-only vault still unlocks without a keyfile");
    }

    #[tokio::test]
    async fn unlock_overlay_keys_rclone_rejects_keyfile() {
        // Keyfiles are an AeroCrypt feature; a keyfile stored against an
        // rclone-crypt binding is a misconfiguration that must surface.
        let mut mem = ConfigProvider {
            files: std::collections::HashMap::new(),
        };
        let params = OverlayUnlockParams {
            kind: "rclone-crypt".to_string(),
            remote_scope: String::new(),
            filename_encryption: "standard".to_string(),
            directory_name_encryption: true,
            off_suffix: None,
            profile_id: None,
            local_config_json: None,
            local_config_salt: None,
        };
        let digest = crate::aerocrypt::keyfile_digest(b"kf");
        let err = unlock_err(
            unlock_overlay_keys(&mut mem, &params, "pw", "salt", Some(&digest)).await,
            "rclone-crypt must reject a keyfile",
        );
        assert!(err.contains("AeroCrypt feature"), "clear error: {err}");
    }

    /// Headerless AeroCrypt parity (v4.1.4 headerless-default flip): the MCP /
    /// Compare unlock must open a headerless overlay from the local keystore
    /// config alone, with NO remote marker on the wire, and stay fail-closed.
    /// The marker-PRESENT branch is covered by the tests above; this exercises
    /// the local-config fallback branch (`exists()` false) end to end.
    #[tokio::test]
    async fn unlock_overlay_keys_headerless_from_local_config() {
        use crate::aerocrypt::overlay;
        use base64::Engine as _;

        // Build a valid v3 config marker the way `provider_with_v3_config` does,
        // but keep it OUT of the provider so `exists()` returns false: this is a
        // headerless overlay whose config lives only in the local keystore.
        let salt = overlay::random_salt_v3();
        let tmp = overlay::OverlayConfig::v3_bootstrap(salt);
        let master_key =
            overlay::derive_master_key_with_keyfile(&tmp, "pw", None).expect("derive master key");
        let config_text =
            overlay::init_config_v3(&salt, &master_key).expect("build headerless config");
        let config_salt = base64::engine::general_purpose::STANDARD.encode(salt);

        // Empty provider -> the marker `<scope>/.aeroftp-crypt.json` is absent.
        let empty = || ConfigProvider {
            files: std::collections::HashMap::new(),
        };

        // 1) Headerless unlock succeeds from the local config alone.
        let mut params = aerocrypt_params("/Headerless");
        params.local_config_json = Some(config_text.clone());
        params.local_config_salt = Some(config_salt.clone());
        params.profile_id = Some("srv_headerless_test".to_string());
        let mut prov = empty();
        let keys = unlock_overlay_keys(&mut prov, &params, "pw", "", None)
            .await
            .expect("headerless overlay must unlock from local_config_json");
        assert!(matches!(keys, CryptCompareKeys::AeroCrypt(_)));
        // The unlock must NOT have written a marker (headerless stays headerless).
        assert!(
            prov.files.is_empty(),
            "headerless unlock must leave no remote footprint"
        );

        // 2) No config anywhere -> fail-closed with the frozen "no overlay at".
        let mut params_none = aerocrypt_params("/Headerless");
        params_none.profile_id = Some("srv_headerless_test".to_string());
        let mut prov_none = empty();
        let err = unlock_err(
            unlock_overlay_keys(&mut prov_none, &params_none, "pw", "", None).await,
            "missing config everywhere must fail closed",
        );
        assert!(err.contains("no overlay at"), "fail-closed error: {err}");

        // 3) Salt mismatch -> fail-closed (a divergent keystore never unlocks).
        let mut params_bad = aerocrypt_params("/Headerless");
        params_bad.local_config_json = Some(config_text);
        params_bad.local_config_salt = Some("deadbeef-not-the-config-salt".to_string());
        params_bad.profile_id = Some("srv_headerless_test".to_string());
        let mut prov_bad = empty();
        let err = unlock_err(
            unlock_overlay_keys(&mut prov_bad, &params_bad, "pw", "", None).await,
            "a config/salt divergence must fail closed",
        );
        assert!(err.contains("does not match"), "salt-mismatch error: {err}");
    }
}
