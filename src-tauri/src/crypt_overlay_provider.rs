// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Crypt-overlay decorator: a [`StorageProvider`] that wraps an inner provider
//! and transparently maps/encrypts every path and content method through a
//! crypt overlay (rclone-crypt or native AeroCrypt).
//!
//! # Why
//!
//! Crypt-overlay profiles bind a plaintext remote subtree to an encrypted layout
//! on the wire: directory and file NAMES are encrypted per segment, file CONTENT
//! is encrypted whole. Historically only a handful of dedicated commands
//! (`rclone_crypt_provider_*` / `aerocrypt_provider_*`) and an interim AeroSync
//! shim knew how to do this; every other read/write surface (browse, transfer,
//! agent, MCP) called the raw `Box<dyn StorageProvider>` with plaintext names,
//! so writes injected plaintext into the crypt store (silent corruption) and
//! reads failed or showed ciphertext (audit 2026-06-30, 16 surfaces).
//!
//! This decorator is the single chokepoint that fixes all of them: wrap the
//! inner provider once at the resolver, and every caller keeps speaking
//! plaintext while the wire stays fully encrypted. The wiring into the resolvers
//! is done in later phases; this module is the foundation (Phase 0) and is not
//! referenced by any resolver yet.
//!
//! # Design contract
//!
//! 1. **Path domain.** The decorator presents DECRYPTED (plaintext) paths and
//!    names to its callers, exactly like a non-crypt provider. Internally it maps
//!    a caller plaintext path to the encrypted remote path (the bound anchor
//!    stays cleartext, the tail below it is encrypted segment-by-segment) and
//!    decrypts names returned by `list`. Content is encrypted on every write and
//!    decrypted on every read.
//! 2. **Scope / anchor.** The decorator holds the bound plaintext anchor
//!    ([`CryptOverlayProvider::scope`], `""` = the whole remote is crypt). All
//!    path mapping is relative to it. A caller path outside the anchor is refused
//!    (fail-closed), never silently re-encrypted.
//! 3. **`is_dir`-aware names.** Intermediate path components are always
//!    directories; only the leaf takes the per-operation `is_dir`. This matters
//!    for rclone `off` mode (file names get a `.bin` suffix, directory names do
//!    not) and for rclone `directory_name_encryption = false` (directory names
//!    pass through cleartext). For rclone `standard`/`obfuscate` and for
//!    AeroCrypt (AES-256-SIV), `is_dir` does not change the encoding.
//! 4. **Decrypted size.** `stat`/`size` return the DECRYPTED size where it is
//!    deterministic: rclone-crypt has an exact ciphertext->plaintext size map;
//!    AeroCrypt defers (returns the on-wire ciphertext length) until the
//!    container size decoder lands. Mirrors [`crate::crypt_compare`].
//! 5. **Fail-closed.** If a profile carries a crypt binding but the keys cannot
//!    be unlocked (wrong password, locked vault, unreadable config), the wrap
//!    factory returns an error and the operation is refused. The raw provider is
//!    NEVER used for a crypt-bound profile.
//!
//! # Conservatively disabled surfaces
//!
//! Capabilities whose mapping is not byte-exact through a crypt overlay are
//! turned OFF (advertised as unsupported, so callers fall back to whole-file
//! paths instead of corrupting data):
//! - **Offset/streaming**: resume download/upload, multipart upload, byte-range
//!   reads / delta sync. A plaintext offset does not map linearly to a ciphertext
//!   offset (rclone chunks 64 KiB -> 64 KiB + 16-byte tag).
//! - **Server-side digest**: `checksum` (the server hashes ciphertext, which
//!   never matches the plaintext hash).
//! - **Server-fetch writes**: `remote_upload` and `import_link` (the server
//!   would write attacker/plaintext bytes straight into the crypt store without
//!   passing through our content encryption).
//! - **Name-pattern / opaque surfaces**: `find` (the server matches against
//!   encrypted names), thumbnails, change tracking, share links, locking,
//!   permissions.
//!
//! Everything else (list, download, upload, mkdir, delete, rmdir(_recursive),
//! rename, stat, size, exists, delete_permanent, chmod, disk_usage,
//! server-side copy, and file versions) is mapped exactly.

use async_trait::async_trait;
use zeroize::Zeroize;

use crate::aerocrypt::overlay::{self, OverlayConfig};
use crate::aerocrypt::{names, KEY_SIZE};
use crate::crypt_compare::{rclone_decrypted_size, OverlayUnlockParams};
use crate::providers::{FileVersion, ProviderError, ProviderType, RemoteEntry, StorageProvider};
use crate::rclone_crypt::{self, FilenameEncryption, RcloneCryptKeys};

/// AeroCrypt overlay config filename, written at the scope root by `crypt init`.
/// Skipped from every decrypted listing (it is plaintext JSON, never an overlay
/// entry).
const AEROCRYPT_CONFIG_NAME: &str = ".aeroftp-crypt.json";

/// Per-directory IV sentinels some rclone-crypt layouts carry. Our overlays use
/// the global `name_tweak` so they are normally absent, but skip them defensively
/// so a foreign rclone vault never leaks an undecryptable row into a listing.
const RCLONE_DIRIV_SENTINELS: &[&str] = &["dirIV", ".diriv", "diriv"];

// ── Overlay keys (encryption-capable) ────────────────────────────────────────

/// Unlocked key material for one crypt overlay, encryption-capable.
///
/// Distinct from [`crate::crypt_compare::CryptCompareKeys`], which is
/// decryption-only: the AeroCrypt variant here additionally carries the parsed
/// [`OverlayConfig`] required by [`overlay::encrypt_data`]. Key material is
/// zeroized on drop (the `RcloneCryptKeys` via its own `Drop`, the AeroCrypt
/// master key here).
pub enum OverlayKeys {
    /// rclone-crypt interop keys (scrypt-derived, EME/AES-256 names,
    /// XSalsa20-Poly1305 content).
    Rclone(RcloneCryptKeys),
    /// Native AeroCrypt overlay (Argon2id master key, AES-256-SIV names,
    /// versioned AEAD content container).
    AeroCrypt {
        master_key: [u8; KEY_SIZE],
        config: OverlayConfig,
    },
}

impl Drop for OverlayKeys {
    fn drop(&mut self) {
        if let Self::AeroCrypt { master_key, .. } = self {
            master_key.zeroize();
        }
    }
}

impl OverlayKeys {
    /// Whether an rclone name with this kind/mode is encrypted on the wire for a
    /// given `is_dir`. Off mode and directory-name-encryption-off both leave a
    /// (sub)set of names cleartext. AeroCrypt always encrypts.
    fn rclone_name_is_encrypted(keys: &RcloneCryptKeys, is_dir: bool) -> bool {
        keys.filename_encryption != FilenameEncryption::Off
            && (!is_dir || keys.directory_name_encryption)
    }

    /// Encrypt one plaintext path component to its on-wire form.
    fn encode_name(&self, plain: &str, is_dir: bool) -> Result<String, String> {
        match self {
            Self::AeroCrypt { master_key, .. } => names::encrypt_filename(master_key, plain),
            Self::Rclone(keys) => {
                // Mirror of lib.rs `rclone_overlay_encode_name`: gate cleartext
                // passthrough first, then dispatch on the filename mode.
                if keys.filename_encryption != FilenameEncryption::Off
                    && !Self::rclone_name_is_encrypted(keys, is_dir)
                {
                    return Ok(plain.to_string());
                }
                match keys.filename_encryption {
                    FilenameEncryption::Off => {
                        if is_dir || keys.off_suffix.is_empty() {
                            Ok(plain.to_string())
                        } else {
                            Ok(format!("{}{}", plain, keys.off_suffix))
                        }
                    }
                    FilenameEncryption::Standard => {
                        rclone_crypt::encrypt_name(&keys.name_key, &keys.name_tweak, plain)
                    }
                    FilenameEncryption::Obfuscate => {
                        rclone_crypt::obfuscate_name(&keys.name_tweak, plain)
                    }
                }
            }
        }
    }

    /// Decrypt one on-wire path component back to plaintext, or `None` when it is
    /// not a valid name for this overlay (foreign entry, wrong key, sentinel).
    fn decode_name(&self, encoded: &str, is_dir: bool) -> Option<String> {
        match self {
            Self::AeroCrypt { master_key, .. } => names::decrypt_filename(master_key, encoded),
            Self::Rclone(keys) => {
                // Mirror of lib.rs `rclone_overlay_decode_name`.
                if keys.filename_encryption != FilenameEncryption::Off
                    && !Self::rclone_name_is_encrypted(keys, is_dir)
                {
                    return Some(encoded.to_string());
                }
                match keys.filename_encryption {
                    FilenameEncryption::Off => {
                        if is_dir || keys.off_suffix.is_empty() {
                            Some(encoded.to_string())
                        } else {
                            encoded
                                .strip_suffix(keys.off_suffix.as_str())
                                .map(|name| name.to_string())
                        }
                    }
                    FilenameEncryption::Standard => {
                        rclone_crypt::decrypt_name(&keys.name_key, &keys.name_tweak, encoded).ok()
                    }
                    FilenameEncryption::Obfuscate => {
                        rclone_crypt::deobfuscate_name(&keys.name_tweak, encoded).ok()
                    }
                }
            }
        }
    }

    /// Encrypt whole file content for upload.
    fn encrypt_content(&self, plaintext: &[u8]) -> Result<Vec<u8>, String> {
        match self {
            Self::Rclone(keys) => rclone_crypt::encrypt_file_content(plaintext, &keys.data_key),
            Self::AeroCrypt { master_key, config } => {
                overlay::encrypt_data(config, master_key, plaintext)
            }
        }
    }

    /// Decrypt whole file content after download.
    fn decrypt_content(&self, ciphertext: &[u8]) -> Result<Vec<u8>, String> {
        match self {
            Self::Rclone(keys) => rclone_crypt::decrypt_file_content(ciphertext, &keys.data_key),
            Self::AeroCrypt { master_key, .. } => overlay::decrypt_data(master_key, ciphertext),
        }
    }

    /// Map an on-wire (ciphertext) file size back to the plaintext size.
    ///
    /// rclone-crypt has a deterministic overhead map; AeroCrypt defers and
    /// returns the input unchanged (the AEAD tag is the integrity guarantee,
    /// size-policy compares may re-flag until the container decoder lands).
    fn decrypted_size(&self, size: u64) -> u64 {
        match self {
            Self::Rclone(_) => rclone_decrypted_size(size),
            Self::AeroCrypt { .. } => size,
        }
    }

    /// Whether a listing entry name is an overlay sentinel that must never be
    /// surfaced as a decrypted file (config file, rclone dirIV markers).
    fn is_sentinel(&self, name: &str) -> bool {
        match self {
            Self::AeroCrypt { .. } => name == AEROCRYPT_CONFIG_NAME,
            Self::Rclone(_) => RCLONE_DIRIV_SENTINELS.contains(&name),
        }
    }
}

// ── Path mapping (generic over the overlay kind) ─────────────────────────────

/// Normalize a plaintext crypt anchor: `""`/`"/"` => whole-remote scope; else an
/// absolute, slash-trimmed `/Foo/Bar`.
fn norm_anchor(scope: &str) -> String {
    let s = scope.trim();
    if s.is_empty() || s == "/" {
        return String::new();
    }
    format!("/{}", s.trim_matches('/'))
}

/// Normalize an absolute path: leading slash, no trailing slash.
fn norm_abs(p: &str) -> String {
    let trimmed = p.trim_end_matches('/');
    if trimmed.is_empty() {
        return "/".to_string();
    }
    format!("/{}", trimmed.trim_start_matches('/'))
}

/// Encrypt a plaintext relative (or absolute) path component-by-component.
///
/// Intermediate components are encoded as directories; the leaf takes
/// `is_dir_leaf`. `.` segments are dropped, `..` and NUL are rejected (no
/// traversal in crypt paths). The leading slash is preserved for an absolute
/// input.
fn encode_rel_path(keys: &OverlayKeys, rel: &str, is_dir_leaf: bool) -> Result<String, String> {
    let absolute = rel.starts_with('/');
    let comps: Vec<&str> = rel
        .split('/')
        .filter(|c| !c.is_empty() && *c != ".")
        .collect();
    for c in &comps {
        if *c == ".." || c.contains('\0') {
            return Err("Invalid crypt path component".to_string());
        }
    }
    let last = comps.len().saturating_sub(1);
    let mut parts = Vec::with_capacity(comps.len());
    for (i, c) in comps.iter().enumerate() {
        let is_dir = if i == last { is_dir_leaf } else { true };
        parts.push(keys.encode_name(c, is_dir)?);
    }
    let joined = parts.join("/");
    if absolute {
        Ok(format!("/{}", joined))
    } else {
        Ok(joined)
    }
}

/// Map a caller plaintext target to the on-wire encrypted path, keeping the
/// cleartext anchor prefix and encrypting only the tail below it. FAIL-CLOSED:
/// an absolute target that is not at/under the anchor is refused.
///
/// `scope` is the already-normalized anchor (`""` = whole remote). A relative
/// target is encrypted in full (it resolves against the current in-scope dir).
fn encode_plain_target(
    keys: &OverlayKeys,
    scope: &str,
    target: &str,
    is_dir_leaf: bool,
) -> Result<String, String> {
    if !target.starts_with('/') {
        return encode_rel_path(keys, target, is_dir_leaf);
    }
    if scope.is_empty() {
        return encode_rel_path(keys, target, is_dir_leaf);
    }
    let t = norm_abs(target);
    if t == scope {
        // The cleartext anchor root itself, no sub-components below it.
        return Ok(scope.to_string());
    }
    if let Some(below) = t.strip_prefix(&format!("{}/", scope)) {
        let enc_below = encode_rel_path(keys, below, is_dir_leaf)?;
        return Ok(format!("{}/{}", scope, enc_below));
    }
    Err(format!(
        "crypt target {:?} is outside the overlay scope {:?}",
        target, scope
    ))
}

/// Decrypt an on-wire path for display, decoding each component as a directory
/// (every prefix of a path is a directory) and leaving undecryptable components
/// verbatim. Used to render `pwd` and rebuild listing paths in the plaintext
/// domain.
fn decode_path(keys: &OverlayKeys, encrypted_path: &str) -> String {
    if encrypted_path.is_empty() || encrypted_path == "." || encrypted_path == "/" {
        return encrypted_path.to_string();
    }
    let absolute = encrypted_path.starts_with('/');
    let parts: Vec<String> = encrypted_path
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .map(|part| {
            keys.decode_name(part, true)
                .unwrap_or_else(|| part.to_string())
        })
        .collect();
    let joined = parts.join("/");
    if absolute {
        format!("/{}", joined)
    } else {
        joined
    }
}

// ── The decorator ────────────────────────────────────────────────────────────

/// A [`StorageProvider`] that wraps an inner provider and applies a crypt
/// overlay (rclone-crypt or AeroCrypt) to every path and content method. See the
/// module docs for the full contract.
pub struct CryptOverlayProvider {
    inner: Box<dyn StorageProvider>,
    keys: OverlayKeys,
    /// Normalized plaintext anchor (`""` = whole remote is crypt).
    scope: String,
}

impl CryptOverlayProvider {
    /// Wrap `inner` with the unlocked `keys` bound to the plaintext `scope`
    /// (`""`/`"/"` = the whole remote). The scope is normalized internally.
    pub fn new(inner: Box<dyn StorageProvider>, keys: OverlayKeys, scope: &str) -> Self {
        Self {
            inner,
            keys,
            scope: norm_anchor(scope),
        }
    }

    /// Map a caller plaintext path to the on-wire encrypted path (fail-closed on
    /// an out-of-scope target).
    fn map(&self, plain: &str, is_dir_leaf: bool) -> Result<String, ProviderError> {
        encode_plain_target(&self.keys, &self.scope, plain, is_dir_leaf)
            .map_err(ProviderError::InvalidPath)
    }

    fn crypt_err(context: &str, e: String) -> ProviderError {
        ProviderError::TransferFailed(format!("{context}: {e}"))
    }

    /// The plaintext anchor this overlay is bound to (`""` = whole remote).
    pub fn scope(&self) -> &str {
        &self.scope
    }

    /// Detach and return the wrapped raw provider, leaving this decorator husk
    /// inert (a [`DetachedProvider`] placeholder whose methods are never called:
    /// the husk is dropped immediately after the caller swaps the detached inner
    /// back into its slot). The overlay keys are dropped with the husk (zeroized
    /// via `OverlayKeys`'s `Drop`). Used by the GUI on-demand
    /// `provider_clear_crypt_overlay` / scope-cross-out path to revert
    /// `ProviderState` to the live raw connection without reconnecting, so the
    /// browser can show plaintext outside the encrypted scope exactly as the
    /// pre-decorator command layer did.
    pub fn take_inner(&mut self) -> Box<dyn StorageProvider> {
        std::mem::replace(&mut self.inner, Box::new(DetachedProvider))
    }
}

/// Inert placeholder swapped into a [`CryptOverlayProvider`] husk by
/// [`CryptOverlayProvider::take_inner`]. It is NEVER used for I/O: the husk is
/// dropped the instant after its real inner is detached. Every method fails
/// closed so that, even in the impossible event the husk outlives the swap, it
/// can never touch a backend.
struct DetachedProvider;

#[async_trait]
impl StorageProvider for DetachedProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
    fn provider_type(&self) -> ProviderType {
        ProviderType::Ftp
    }
    fn display_name(&self) -> String {
        "detached".to_string()
    }
    async fn connect(&mut self) -> Result<(), ProviderError> {
        Err(ProviderError::NotConnected)
    }
    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        Ok(())
    }
    fn is_connected(&self) -> bool {
        false
    }
    async fn list(&mut self, _path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        Err(ProviderError::NotConnected)
    }
    async fn pwd(&mut self) -> Result<String, ProviderError> {
        Err(ProviderError::NotConnected)
    }
    async fn cd(&mut self, _path: &str) -> Result<(), ProviderError> {
        Err(ProviderError::NotConnected)
    }
    async fn cd_up(&mut self) -> Result<(), ProviderError> {
        Err(ProviderError::NotConnected)
    }
    async fn download(
        &mut self,
        _remote_path: &str,
        _local_path: &str,
        _on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        Err(ProviderError::NotConnected)
    }
    async fn download_to_bytes(&mut self, _remote_path: &str) -> Result<Vec<u8>, ProviderError> {
        Err(ProviderError::NotConnected)
    }
    async fn upload(
        &mut self,
        _local_path: &str,
        _remote_path: &str,
        _on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        Err(ProviderError::NotConnected)
    }
    async fn mkdir(&mut self, _path: &str) -> Result<(), ProviderError> {
        Err(ProviderError::NotConnected)
    }
    async fn delete(&mut self, _path: &str) -> Result<(), ProviderError> {
        Err(ProviderError::NotConnected)
    }
    async fn rmdir(&mut self, _path: &str) -> Result<(), ProviderError> {
        Err(ProviderError::NotConnected)
    }
    async fn rmdir_recursive(&mut self, _path: &str) -> Result<(), ProviderError> {
        Err(ProviderError::NotConnected)
    }
    async fn rename(&mut self, _from: &str, _to: &str) -> Result<(), ProviderError> {
        Err(ProviderError::NotConnected)
    }
    async fn stat(&mut self, _path: &str) -> Result<RemoteEntry, ProviderError> {
        Err(ProviderError::NotConnected)
    }
    async fn size(&mut self, _path: &str) -> Result<u64, ProviderError> {
        Err(ProviderError::NotConnected)
    }
    async fn exists(&mut self, _path: &str) -> Result<bool, ProviderError> {
        Err(ProviderError::NotConnected)
    }
    async fn keep_alive(&mut self) -> Result<(), ProviderError> {
        Err(ProviderError::NotConnected)
    }
    async fn server_info(&mut self) -> Result<String, ProviderError> {
        Err(ProviderError::NotConnected)
    }
}

/// Write decrypted plaintext atomically: stage to a sibling `.aerotmp` file then
/// rename over the target, so an interrupted decrypt never leaves a partial or
/// 0-byte plaintext file.
async fn write_plaintext_atomic(output_path: &str, plaintext: &[u8]) -> Result<(), ProviderError> {
    crate::filesystem::validate_path(output_path).map_err(ProviderError::InvalidPath)?;
    if let Some(parent) = std::path::Path::new(output_path).parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    let tmp = format!("{output_path}.aerotmp");
    tokio::fs::write(&tmp, plaintext)
        .await
        .map_err(ProviderError::IoError)?;
    if let Err(e) = tokio::fs::rename(&tmp, output_path).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(ProviderError::IoError(e));
    }
    Ok(())
}

/// Encrypt `plaintext` and stage it to a unique temp file, returning its path.
/// The caller is responsible for removing it after the upload.
async fn stage_ciphertext_temp(
    keys: &OverlayKeys,
    plaintext: &[u8],
) -> Result<std::path::PathBuf, ProviderError> {
    let ciphertext = keys
        .encrypt_content(plaintext)
        .map_err(|e| CryptOverlayProvider::crypt_err("content encrypt", e))?;
    let temp_path = std::env::temp_dir().join(format!(
        "aeroftp_crypt_overlay_{}_{}.bin",
        chrono::Utc::now().timestamp_millis(),
        uuid::Uuid::new_v4()
    ));
    tokio::fs::write(&temp_path, &ciphertext)
        .await
        .map_err(ProviderError::IoError)?;
    Ok(temp_path)
}

#[async_trait]
impl StorageProvider for CryptOverlayProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        // Honest downcast target: this IS a CryptOverlayProvider. Callers that
        // need provider-specific operations on the inner transport must reach it
        // before wrapping (the wrap happens at the resolver chokepoint).
        self
    }

    fn provider_type(&self) -> ProviderType {
        self.inner.provider_type()
    }

    fn display_name(&self) -> String {
        self.inner.display_name()
    }

    fn account_email(&self) -> Option<String> {
        self.inner.account_email()
    }

    async fn connect(&mut self) -> Result<(), ProviderError> {
        self.inner.connect().await
    }

    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        self.inner.disconnect().await
    }

    fn is_connected(&self) -> bool {
        self.inner.is_connected()
    }

    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        let enc_path = if path.is_empty() || path == "." {
            path.to_string()
        } else {
            self.map(path, true)?
        };
        let raw = self.inner.list(&enc_path).await?;
        let mut out = Vec::with_capacity(raw.len());
        for entry in raw {
            if self.keys.is_sentinel(&entry.name) {
                continue;
            }
            // Drop foreign / undecryptable rows rather than surface ciphertext as
            // if it were a plaintext name.
            let Some(plain_name) = self.keys.decode_name(&entry.name, entry.is_dir) else {
                continue;
            };
            let plain_path = decode_path(&self.keys, &entry.path);
            let size = if entry.is_dir {
                0
            } else {
                self.keys.decrypted_size(entry.size)
            };
            out.push(RemoteEntry {
                name: plain_name,
                path: plain_path,
                size,
                ..entry
            });
        }
        Ok(out)
    }

    async fn pwd(&mut self) -> Result<String, ProviderError> {
        let enc = self.inner.pwd().await?;
        Ok(decode_path(&self.keys, &enc))
    }

    async fn cd(&mut self, path: &str) -> Result<(), ProviderError> {
        if path == ".." {
            return self.inner.cd_up().await;
        }
        let enc = self.map(path, true)?;
        self.inner.cd(&enc).await
    }

    async fn cd_up(&mut self) -> Result<(), ProviderError> {
        self.inner.cd_up().await
    }

    async fn download(
        &mut self,
        remote_path: &str,
        local_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let enc = self.map(remote_path, false)?;
        // Stage the ciphertext to a temp file (streaming the network leg with the
        // caller's progress callback), then decrypt into the final plaintext path.
        let temp_path = std::env::temp_dir().join(format!(
            "aeroftp_crypt_overlay_dl_{}_{}.bin",
            chrono::Utc::now().timestamp_millis(),
            uuid::Uuid::new_v4()
        ));
        let temp_str = temp_path.to_string_lossy().to_string();
        self.inner.download(&enc, &temp_str, on_progress).await?;
        let ciphertext = tokio::fs::read(&temp_path)
            .await
            .map_err(ProviderError::IoError)?;
        let _ = tokio::fs::remove_file(&temp_path).await;
        let plaintext = self
            .keys
            .decrypt_content(&ciphertext)
            .map_err(|e| Self::crypt_err("content decrypt", e))?;
        write_plaintext_atomic(local_path, &plaintext).await
    }

    async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>, ProviderError> {
        let enc = self.map(remote_path, false)?;
        let ciphertext = self.inner.download_to_bytes(&enc).await?;
        self.keys
            .decrypt_content(&ciphertext)
            .map_err(|e| Self::crypt_err("content decrypt", e))
    }

    async fn upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        crate::filesystem::validate_path(local_path).map_err(ProviderError::InvalidPath)?;
        let plaintext = tokio::fs::read(local_path)
            .await
            .map_err(ProviderError::IoError)?;
        let temp_path = stage_ciphertext_temp(&self.keys, &plaintext).await?;
        let enc = match self.map(remote_path, false) {
            Ok(p) => p,
            Err(e) => {
                let _ = tokio::fs::remove_file(&temp_path).await;
                return Err(e);
            }
        };
        let result = self
            .inner
            .upload(&temp_path.to_string_lossy(), &enc, on_progress)
            .await;
        let _ = tokio::fs::remove_file(&temp_path).await;
        result
    }

    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError> {
        let enc = self.map(path, true)?;
        self.inner.mkdir(&enc).await
    }

    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        let enc = self.map(path, false)?;
        self.inner.delete(&enc).await
    }

    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        let enc = self.map(path, true)?;
        self.inner.rmdir(&enc).await
    }

    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        let enc = self.map(path, true)?;
        self.inner.rmdir_recursive(&enc).await
    }

    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        // is_dir is unknown for a bare rename; encode with file-leaf semantics.
        // Exact for rclone standard/obfuscate and AeroCrypt (is_dir-independent);
        // a directory rename under rclone `off`-suffix mode is a known edge.
        let enc_from = self.map(from, false)?;
        let enc_to = self.map(to, false)?;
        self.inner.rename(&enc_from, &enc_to).await
    }

    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        let enc = self.map(path, false)?;
        let mut entry = self.inner.stat(&enc).await?;
        let plain_name = self
            .keys
            .decode_name(&entry.name, entry.is_dir)
            .unwrap_or(entry.name);
        let plain_path = decode_path(&self.keys, &entry.path);
        let size = if entry.is_dir {
            0
        } else {
            self.keys.decrypted_size(entry.size)
        };
        entry.name = plain_name;
        entry.path = plain_path;
        entry.size = size;
        Ok(entry)
    }

    async fn size(&mut self, path: &str) -> Result<u64, ProviderError> {
        let enc = self.map(path, false)?;
        let enc_size = self.inner.size(&enc).await?;
        Ok(self.keys.decrypted_size(enc_size))
    }

    async fn exists(&mut self, path: &str) -> Result<bool, ProviderError> {
        let enc = self.map(path, false)?;
        self.inner.exists(&enc).await
    }

    async fn keep_alive(&mut self) -> Result<(), ProviderError> {
        self.inner.keep_alive().await
    }

    async fn server_info(&mut self) -> Result<String, ProviderError> {
        self.inner.server_info().await
    }

    async fn delete_permanent(&mut self, path: &str) -> Result<bool, ProviderError> {
        let enc = self.map(path, false)?;
        self.inner.delete_permanent(&enc).await
    }

    fn supports_chmod(&self) -> bool {
        self.inner.supports_chmod()
    }

    async fn chmod(&mut self, path: &str, mode: u32) -> Result<(), ProviderError> {
        let enc = self.map(path, false)?;
        self.inner.chmod(&enc, mode).await
    }

    fn supports_symlinks(&self) -> bool {
        self.inner.supports_symlinks()
    }

    fn supports_server_copy(&self) -> bool {
        self.inner.supports_server_copy()
    }

    fn supports_server_side_copy(&self) -> bool {
        self.inner.supports_server_side_copy()
    }

    async fn server_copy(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        // The ciphertext blob is self-contained (the content nonce is embedded,
        // not derived from the name), so copying it verbatim to a new encrypted
        // name stays decryptable. Map both ends; the content is untouched.
        let enc_from = self.map(from, false)?;
        let enc_to = self.map(to, false)?;
        self.inner.server_copy(&enc_from, &enc_to).await
    }

    async fn server_side_copy(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        let enc_from = self.map(from, false)?;
        let enc_to = self.map(to, false)?;
        self.inner.server_side_copy(&enc_from, &enc_to).await
    }

    async fn storage_info(&mut self) -> Result<crate::providers::StorageInfo, ProviderError> {
        // Byte totals are over ciphertext; surfaced as-is (the per-file overhead
        // is tiny relative to quota figures).
        self.inner.storage_info().await
    }

    async fn disk_usage(&mut self, path: &str) -> Result<u64, ProviderError> {
        let enc = self.map(path, true)?;
        self.inner.disk_usage(&enc).await
    }

    async fn set_speed_limit(
        &mut self,
        upload_kb: u64,
        download_kb: u64,
    ) -> Result<(), ProviderError> {
        self.inner.set_speed_limit(upload_kb, download_kb).await
    }

    async fn get_speed_limit(&mut self) -> Result<(u64, u64), ProviderError> {
        self.inner.get_speed_limit().await
    }

    fn supports_versions(&self) -> bool {
        self.inner.supports_versions()
    }

    async fn list_versions(&mut self, path: &str) -> Result<Vec<FileVersion>, ProviderError> {
        let enc = self.map(path, false)?;
        let mut versions = self.inner.list_versions(&enc).await?;
        for v in &mut versions {
            v.size = self.keys.decrypted_size(v.size);
        }
        Ok(versions)
    }

    async fn download_version(
        &mut self,
        path: &str,
        version_id: &str,
        local_path: &str,
    ) -> Result<(), ProviderError> {
        let enc = self.map(path, false)?;
        let temp_path = std::env::temp_dir().join(format!(
            "aeroftp_crypt_overlay_ver_{}_{}.bin",
            chrono::Utc::now().timestamp_millis(),
            uuid::Uuid::new_v4()
        ));
        let temp_str = temp_path.to_string_lossy().to_string();
        self.inner
            .download_version(&enc, version_id, &temp_str)
            .await?;
        let ciphertext = tokio::fs::read(&temp_path)
            .await
            .map_err(ProviderError::IoError)?;
        let _ = tokio::fs::remove_file(&temp_path).await;
        let plaintext = self
            .keys
            .decrypt_content(&ciphertext)
            .map_err(|e| Self::crypt_err("version decrypt", e))?;
        write_plaintext_atomic(local_path, &plaintext).await
    }

    async fn restore_version(&mut self, path: &str, version_id: &str) -> Result<(), ProviderError> {
        let enc = self.map(path, false)?;
        self.inner.restore_version(&enc, version_id).await
    }

    // ── Conservatively disabled surfaces (see module docs) ───────────────────
    //
    // Each is advertised unsupported so the runner/caller falls back to a
    // whole-file mapped path instead of an offset/digest/server-fetch operation
    // that a crypt overlay cannot perform byte-exactly. The methods keep their
    // trait defaults (NotSupported).

    fn supports_resume(&self) -> bool {
        false
    }

    fn supports_find(&self) -> bool {
        false
    }

    fn supports_thumbnails(&self) -> bool {
        false
    }

    fn supports_checksum(&self) -> bool {
        false
    }

    fn supports_remote_upload(&self) -> bool {
        false
    }

    fn supports_import_link(&self) -> bool {
        false
    }

    fn supports_change_tracking(&self) -> bool {
        false
    }

    fn supports_delta_sync(&self) -> bool {
        false
    }

    fn supports_locking(&self) -> bool {
        false
    }

    fn supports_share_links(&self) -> bool {
        false
    }

    fn supports_permissions(&self) -> bool {
        false
    }

    fn transfer_optimization_hints(&self) -> crate::providers::TransferOptimizationHints {
        // Force a conservative single-stream shape; never inherit the inner
        // provider's multipart/parallel hints (they do not survive whole-file
        // crypt framing).
        crate::providers::TransferOptimizationHints::default()
    }

    fn transfer_capabilities(&self) -> crate::transfer_dag::TransferCapabilities {
        // Conservative: no file parallelism, multipart, range, or server digest.
        crate::transfer_dag::TransferCapabilities::default()
    }
}

// ── Factory: wrap a provider when the profile carries a crypt binding ─────────

/// Wrap `inner` with a [`CryptOverlayProvider`] when `binding` is present,
/// unlocking the overlay keys from the supplied secret(s); otherwise return
/// `inner` unchanged.
///
/// FAIL-CLOSED: when a binding is present but the keys cannot be unlocked (wrong
/// password, locked vault, unreadable/invalid config), this returns `Err` and
/// the raw provider is NEVER handed back. Callers must propagate the error and
/// refuse the operation, never fall back to the plaintext provider.
///
/// `binding` reuses [`OverlayUnlockParams`]: `kind` selects the overlay,
/// `remote_scope` is the plaintext anchor (and, for AeroCrypt, where the config
/// lives), and the rclone fields configure name encryption. `password`/`salt`
/// are the already-resolved secrets (the profile -> vault lookup is wired at the
/// resolver in a later phase).
pub async fn wrap_provider_with_overlay_if_bound(
    mut inner: Box<dyn StorageProvider>,
    binding: Option<&OverlayUnlockParams>,
    password: &str,
    salt: &str,
) -> Result<Box<dyn StorageProvider>, String> {
    let Some(params) = binding else {
        return Ok(inner);
    };
    let keys = unlock_overlay_keys_encrypting(&mut *inner, params, password, salt).await?;
    let provider = CryptOverlayProvider::new(inner, keys, &params.remote_scope);
    Ok(Box::new(provider))
}

/// Resolve a saved profile's `aeroCryptOverlay` binding (+ its per-profile vault
/// secrets) and wrap a freshly-connected provider via
/// [`wrap_provider_with_overlay_if_bound`]. Shared by the cross-profile / agent
/// resolver (`ai_tools::create_temp_provider`) and the MCP connection pool so
/// every non-CLI provider resolution closes the same crypt gap the CLI
/// chokepoint (`cli_apply_crypt_overlay`) does, with identical fail-closed
/// semantics.
///
/// FAIL-CLOSED: a profile WITH an enabled binding but no usable stored password
/// returns `Err` (the operation is refused); the raw provider is never handed
/// back. A profile without a binding returns `inner` byte-identical.
///
/// The binding (kind / scope / name-encryption modes) reuses the profile's
/// `aeroCryptOverlay` JSON; the secrets reuse the generic per-profile vault keys
/// `aerocrypt_overlay_pw_<id>` / `aerocrypt_overlay_salt_<id>` shared by both
/// overlay kinds (lib.rs ~15610). Mirrors `mcp::pool::resolve_overlay_secrets`
/// and the CLI `cli_apply_crypt_overlay`. The whole session is wrapped, so the
/// binding's own `remoteScope` is the authoritative plaintext anchor
/// ('' / unset = whole-remote crypt).
pub async fn wrap_connected_provider_for_profile(
    inner: Box<dyn StorageProvider>,
    profile: &serde_json::Value,
    store: &crate::credential_store::CredentialStore,
) -> Result<Box<dyn StorageProvider>, String> {
    let Some(params) = overlay_binding_from_profile(profile) else {
        return Ok(inner);
    };
    let id = profile.get("id").and_then(|v| v.as_str()).unwrap_or("");

    let password = crate::user_partitions::resolve_active_credential(
        store,
        &format!("aerocrypt_overlay_pw_{}", id),
    )
    .ok()
    .flatten()
    .map(|s| s.to_string())
    .filter(|s| !s.is_empty())
    .or_else(|| std::env::var("AEROFTP_CRYPT_OVERLAY_PASSWORD").ok())
    .ok_or_else(|| {
        "Crypt overlay profile has no stored password. Store it in the AeroFTP GUI, or set AEROFTP_CRYPT_OVERLAY_PASSWORD."
            .to_string()
    })?;
    let salt = crate::user_partitions::resolve_active_credential(
        store,
        &format!("aerocrypt_overlay_salt_{}", id),
    )
    .ok()
    .flatten()
    .map(|s| s.to_string())
    .or_else(|| std::env::var("AEROFTP_CRYPT_OVERLAY_SALT").ok())
    .unwrap_or_default();

    wrap_provider_with_overlay_if_bound(inner, Some(&params), &password, &salt).await
}

/// Extract the [`OverlayUnlockParams`] binding from a saved profile's
/// `aeroCryptOverlay` JSON, or `None` when the profile carries no enabled
/// overlay. Pure (no vault access): the secret lookup is the caller's job. The
/// whole session is wrapped at the resolver, so the binding's own `remoteScope`
/// is the authoritative plaintext anchor ('' / unset = whole-remote crypt),
/// matching the CLI chokepoint and `mcp::pool::resolve_overlay_secrets`.
pub(crate) fn overlay_binding_from_profile(
    profile: &serde_json::Value,
) -> Option<OverlayUnlockParams> {
    let overlay = profile.get("aeroCryptOverlay")?;
    if !overlay
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return None;
    }
    Some(OverlayUnlockParams {
        kind: overlay
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or("aerocrypt")
            .to_string(),
        remote_scope: overlay
            .get("remoteScope")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        filename_encryption: overlay
            .get("filenameEncryption")
            .and_then(|v| v.as_str())
            .unwrap_or("standard")
            .to_string(),
        directory_name_encryption: overlay
            .get("directoryNameEncryption")
            .and_then(|v| v.as_bool())
            .unwrap_or(true),
        off_suffix: None,
    })
}

/// Unlock encryption-capable [`OverlayKeys`] from an overlay binding plus the
/// secret(s). The AeroCrypt arm retains the parsed [`OverlayConfig`] (needed for
/// content encryption), which [`crate::crypt_compare::unlock_overlay_keys`]
/// discards. rclone derives keys offline; AeroCrypt reads + verifies the remote
/// config (fail-closed on a wrong password via the config MAC).
async fn unlock_overlay_keys_encrypting(
    provider: &mut dyn StorageProvider,
    params: &OverlayUnlockParams,
    password: &str,
    salt: &str,
) -> Result<OverlayKeys, String> {
    match params.kind.as_str() {
        "rclone-crypt" => {
            let (name_key, data_key, name_tweak) =
                rclone_crypt::derive_keys_with_tweak(password, salt)?;
            let filename_encryption = match params.filename_encryption.as_str() {
                "off" => FilenameEncryption::Off,
                "obfuscate" => FilenameEncryption::Obfuscate,
                _ => FilenameEncryption::Standard,
            };
            let off_suffix = rclone_crypt::resolve_off_suffix(params.off_suffix.as_deref());
            Ok(OverlayKeys::Rclone(RcloneCryptKeys {
                name_key,
                data_key,
                name_tweak,
                filename_encryption,
                off_suffix,
                directory_name_encryption: params.directory_name_encryption,
            }))
        }
        "aerocrypt" => {
            let scope = params.remote_scope.trim_end_matches('/');
            let config_path = format!("{}/{}", scope, AEROCRYPT_CONFIG_NAME);
            let config_bytes = provider
                .download_to_bytes(&config_path)
                .await
                .map_err(|e| format!("Cannot read AeroCrypt overlay config: {e}"))?;
            let config_str = String::from_utf8_lossy(&config_bytes);
            let config = overlay::parse_config(&config_str)
                .map_err(|e| format!("Invalid AeroCrypt overlay config: {e}"))?;
            let master_key = overlay::derive_master_key(&config, password)
                .map_err(|e| format!("AeroCrypt key derivation failed: {e}"))?;
            overlay::verify_config_mac(&config, &master_key)
                .map_err(|e| format!("AeroCrypt unlock failed: {e}"))?;
            Ok(OverlayKeys::AeroCrypt { master_key, config })
        }
        other => Err(format!("Unsupported crypt overlay kind: {other}")),
    }
}

// ── GUI on-demand wrap/unwrap of a live provider slot ─────────────────────────
//
// The GUI keeps a single live connection in `ProviderState::provider` and toggles
// the crypt overlay on it at runtime (auto-unlock on connect, ad-hoc activation,
// badge lock/unlock, and cross-scope-boundary navigation), instead of choosing a
// crypt-vs-plain command per operation as the retired `*_provider_*` layer did.
// `apply_overlay_in_place` wraps the live raw provider; `clear_overlay_in_place`
// reverts it to raw (showing plaintext outside the encrypted scope, exactly like
// the old command layer). Both operate on the same `Option<Box<dyn ...>>` slot.

/// Apply a crypt overlay to a live provider slot in place. Any existing overlay
/// is reverted first (re-anchor / refresh / refresh-after-scope-change), so the
/// slot is never double-wrapped. The keys are derived against the live
/// connection; FAIL-CLOSED: on any unlock error the slot keeps the untouched raw
/// provider (the borrow is released without taking), so a failed unlock never
/// drops the session. Returns the normalized plaintext scope on success.
pub async fn apply_overlay_in_place(
    slot: &mut Option<Box<dyn StorageProvider>>,
    binding: &OverlayUnlockParams,
    password: &str,
    salt: &str,
) -> Result<String, String> {
    // Revert any prior overlay so a re-apply (re-anchor / scope change) can never
    // stack a second decorator on top of the first.
    clear_overlay_in_place(slot);
    let provider = slot
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    // Derive keys against the live raw connection. On error the slot still holds
    // the raw provider (we borrowed via `&mut`, never took), so the session is
    // preserved and the caller surfaces the unlock error.
    let keys = unlock_overlay_keys_encrypting(&mut **provider, binding, password, salt).await?;
    let raw = slot
        .take()
        .expect("provider present after a successful unlock");
    let wrapped = CryptOverlayProvider::new(raw, keys, &binding.remote_scope);
    *slot = Some(Box::new(wrapped));
    Ok(norm_anchor(&binding.remote_scope))
}

/// Revert a live provider slot to its raw inner when it currently holds a
/// [`CryptOverlayProvider`]. The overlay keys are dropped (zeroized) with the
/// husk. Returns true when an overlay was removed, false when the slot was
/// already raw / empty. Idempotent.
pub fn clear_overlay_in_place(slot: &mut Option<Box<dyn StorageProvider>>) -> bool {
    if let Some(boxed) = slot.as_mut() {
        if let Some(dec) = boxed.as_any_mut().downcast_mut::<CryptOverlayProvider>() {
            let raw = dec.take_inner();
            *slot = Some(raw);
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    // ── Key builders ─────────────────────────────────────────────────────────

    fn rclone_keys(mode: FilenameEncryption, dir_name_enc: bool, off_suffix: &str) -> OverlayKeys {
        let (name_key, data_key, name_tweak) =
            rclone_crypt::derive_keys_with_tweak("overlay-pass", "overlay-salt").unwrap();
        OverlayKeys::Rclone(RcloneCryptKeys {
            name_key,
            data_key,
            name_tweak,
            filename_encryption: mode,
            off_suffix: off_suffix.to_string(),
            directory_name_encryption: dir_name_enc,
        })
    }

    fn aerocrypt_keys() -> OverlayKeys {
        let salt = overlay::random_salt_v3();
        let tmp = OverlayConfig::V3 {
            salt,
            mac: [0u8; 32],
        };
        let master_key = overlay::derive_master_key(&tmp, "overlay-pass").unwrap();
        let json = overlay::init_config_v3(&salt, &master_key).unwrap();
        let config = overlay::parse_config(&json).unwrap();
        OverlayKeys::AeroCrypt { master_key, config }
    }

    fn both_kinds() -> Vec<(&'static str, OverlayKeys)> {
        vec![
            (
                "rclone-standard",
                rclone_keys(FilenameEncryption::Standard, true, ".bin"),
            ),
            ("aerocrypt", aerocrypt_keys()),
        ]
    }

    // ── Name round-trips ─────────────────────────────────────────────────────

    #[test]
    fn encode_decode_name_roundtrip_both_kinds() {
        for (label, keys) in both_kinds() {
            for is_dir in [false, true] {
                let enc = keys.encode_name("Report 2026.pdf", is_dir).unwrap();
                assert_ne!(enc, "Report 2026.pdf", "{label}: name must be encrypted");
                let dec = keys.decode_name(&enc, is_dir).unwrap();
                assert_eq!(dec, "Report 2026.pdf", "{label}: name round-trip");
            }
        }
    }

    #[test]
    fn decode_name_rejects_foreign_entry() {
        for (label, keys) in both_kinds() {
            // A name that is not valid ciphertext for this overlay decodes to None
            // (so listings drop it instead of surfacing ciphertext).
            assert!(
                keys.decode_name("definitely not a crypt name!!", false)
                    .is_none(),
                "{label}: foreign name must not decode"
            );
        }
    }

    // ── Path mapping ─────────────────────────────────────────────────────────

    #[test]
    fn encode_plain_target_relative_encrypts_full() {
        for (label, keys) in both_kinds() {
            let enc = encode_plain_target(&keys, "", "sub/file.txt", false).unwrap();
            assert!(!enc.starts_with('/'), "{label}: relative stays relative");
            let parts: Vec<&str> = enc.split('/').collect();
            assert_eq!(parts.len(), 2, "{label}: two encoded segments");
            assert_eq!(
                keys.decode_name(parts[0], true).unwrap(),
                "sub",
                "{label}: dir segment"
            );
            assert_eq!(
                keys.decode_name(parts[1], false).unwrap(),
                "file.txt",
                "{label}: file leaf"
            );
        }
    }

    #[test]
    fn encode_plain_target_absolute_keeps_anchor_cleartext() {
        for (label, keys) in both_kinds() {
            let enc = encode_plain_target(&keys, "/Vault", "/Vault/dir/note.md", false).unwrap();
            assert!(
                enc.starts_with("/Vault/"),
                "{label}: anchor stays cleartext, got {enc}"
            );
            let tail: Vec<&str> = enc.trim_start_matches("/Vault/").split('/').collect();
            assert_eq!(tail.len(), 2, "{label}: encrypted tail");
            assert_eq!(keys.decode_name(tail[0], true).unwrap(), "dir");
            assert_eq!(keys.decode_name(tail[1], false).unwrap(), "note.md");
        }
    }

    #[test]
    fn encode_plain_target_anchor_root_is_cleartext() {
        for (_label, keys) in both_kinds() {
            let enc = encode_plain_target(&keys, "/Vault", "/Vault", true).unwrap();
            assert_eq!(enc, "/Vault");
            let enc2 = encode_plain_target(&keys, "/Vault", "/Vault/", true).unwrap();
            assert_eq!(enc2, "/Vault");
        }
    }

    #[test]
    fn encode_plain_target_outside_anchor_is_refused() {
        for (label, keys) in both_kinds() {
            let res = encode_plain_target(&keys, "/Vault", "/Other/secret.txt", false);
            assert!(res.is_err(), "{label}: out-of-scope must fail closed");
        }
    }

    #[test]
    fn encode_plain_target_whole_remote_encrypts_all() {
        for (label, keys) in both_kinds() {
            let enc = encode_plain_target(&keys, "", "/a/b.txt", false).unwrap();
            assert!(enc.starts_with('/'), "{label}: absolute preserved");
            let parts: Vec<&str> = enc.trim_start_matches('/').split('/').collect();
            assert_eq!(keys.decode_name(parts[0], true).unwrap(), "a");
            assert_eq!(keys.decode_name(parts[1], false).unwrap(), "b.txt");
        }
    }

    #[test]
    fn encode_rel_path_rejects_dotdot() {
        for (_label, keys) in both_kinds() {
            assert!(encode_rel_path(&keys, "a/../b", false).is_err());
            assert!(encode_rel_path(&keys, "../escape", false).is_err());
        }
    }

    #[test]
    fn decode_path_roundtrips_and_passes_undecryptable() {
        for (_label, keys) in both_kinds() {
            let enc = encode_plain_target(&keys, "/Vault", "/Vault/x/y", true).unwrap();
            let dec = decode_path(&keys, &enc);
            assert_eq!(dec, "/Vault/x/y");
            // A foreign component is left verbatim, not dropped, in display paths.
            let mixed = format!("{}/foreign", enc);
            let dec_mixed = decode_path(&keys, &mixed);
            assert!(dec_mixed.starts_with("/Vault/x/y/"));
        }
    }

    // ── Content round-trips ──────────────────────────────────────────────────

    #[test]
    fn encrypt_decrypt_content_roundtrip_both_kinds() {
        for (label, keys) in both_kinds() {
            for size in [0usize, 1, 100, 65_536, 65_537, 200_000] {
                let plaintext: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
                let ct = keys.encrypt_content(&plaintext).unwrap();
                if size > 0 {
                    assert_ne!(ct, plaintext, "{label}/{size}: content must be encrypted");
                }
                let pt = keys.decrypt_content(&ct).unwrap();
                assert_eq!(pt, plaintext, "{label}/{size}: content round-trip");
            }
        }
    }

    // ── Decrypted size mapping ───────────────────────────────────────────────

    #[test]
    fn decrypted_size_rclone_maps_aerocrypt_passes_through() {
        let rclone = rclone_keys(FilenameEncryption::Standard, true, ".bin");
        let aero = aerocrypt_keys();
        // Header-only ciphertext -> 0 bytes plaintext for rclone.
        assert_eq!(rclone.decrypted_size(32), 0);
        // 32 header + (100 + 16 tag) -> 100 plaintext.
        assert_eq!(rclone.decrypted_size(32 + 100 + 16), 100);
        // AeroCrypt defers: returns the input.
        assert_eq!(aero.decrypted_size(12_345), 12_345);
    }

    #[test]
    fn decrypted_size_matches_real_rclone_ciphertext() {
        let keys = rclone_keys(FilenameEncryption::Standard, true, ".bin");
        for size in [1usize, 100, 65_536, 65_537, 200_000] {
            let plaintext = vec![7u8; size];
            let ct = keys.encrypt_content(&plaintext).unwrap();
            assert_eq!(
                keys.decrypted_size(ct.len() as u64),
                size as u64,
                "size map for {size}"
            );
        }
    }

    // ── rclone mode specifics ────────────────────────────────────────────────

    #[test]
    fn rclone_off_mode_suffixes_files_not_dirs() {
        let keys = rclone_keys(FilenameEncryption::Off, true, ".bin");
        // File leaf gets the suffix; directory does not.
        let file = keys.encode_name("data.txt", false).unwrap();
        assert_eq!(file, "data.txt.bin");
        assert_eq!(keys.decode_name(&file, false).unwrap(), "data.txt");
        let dir = keys.encode_name("folder", true).unwrap();
        assert_eq!(dir, "folder");
        assert_eq!(keys.decode_name(&dir, true).unwrap(), "folder");
    }

    #[test]
    fn rclone_off_mode_path_suffixes_only_leaf() {
        let keys = rclone_keys(FilenameEncryption::Off, true, ".bin");
        let enc = encode_plain_target(&keys, "", "dir/sub/file.txt", false).unwrap();
        assert_eq!(enc, "dir/sub/file.txt.bin");
    }

    #[test]
    fn rclone_obfuscate_mode_roundtrip() {
        let keys = rclone_keys(FilenameEncryption::Obfuscate, true, ".bin");
        let enc = keys.encode_name("Secret.doc", false).unwrap();
        assert_ne!(enc, "Secret.doc");
        assert_eq!(keys.decode_name(&enc, false).unwrap(), "Secret.doc");
    }

    #[test]
    fn rclone_dir_name_encryption_off_passes_dirs_through() {
        let keys = rclone_keys(FilenameEncryption::Standard, false, ".bin");
        // Directory name passes through cleartext; file leaf is still encrypted.
        let dir = keys.encode_name("Photos", true).unwrap();
        assert_eq!(dir, "Photos");
        let file = keys.encode_name("img.jpg", false).unwrap();
        assert_ne!(file, "img.jpg");
        assert_eq!(keys.decode_name(&file, false).unwrap(), "img.jpg");
        // A full path: cleartext dir + encrypted leaf.
        let enc = encode_plain_target(&keys, "", "Photos/img.jpg", false).unwrap();
        let parts: Vec<&str> = enc.split('/').collect();
        assert_eq!(parts[0], "Photos");
        assert_eq!(keys.decode_name(parts[1], false).unwrap(), "img.jpg");
    }

    // ── End-to-end through the decorator over an in-memory inner provider ─────

    /// Minimal in-memory provider: a flat object store keyed by on-wire path.
    /// Lets the decorator tests assert that the raw store only ever holds
    /// encrypted names + encrypted content (no plaintext leak), and that reads
    /// come back decrypted.
    struct MemProvider {
        files: Mutex<HashMap<String, Vec<u8>>>,
        dirs: Mutex<Vec<String>>,
    }

    impl MemProvider {
        fn new() -> Self {
            Self {
                files: Mutex::new(HashMap::new()),
                dirs: Mutex::new(Vec::new()),
            }
        }
        fn raw_paths(&self) -> Vec<String> {
            self.files.lock().unwrap().keys().cloned().collect()
        }
        fn raw_bytes(&self, path: &str) -> Option<Vec<u8>> {
            self.files.lock().unwrap().get(path).cloned()
        }
    }

    #[async_trait]
    impl StorageProvider for MemProvider {
        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
        fn provider_type(&self) -> ProviderType {
            ProviderType::Ftp
        }
        fn display_name(&self) -> String {
            "mem".into()
        }
        async fn connect(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn disconnect(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        fn is_connected(&self) -> bool {
            true
        }
        async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
            let prefix = if path.is_empty() || path == "." || path == "/" {
                "/".to_string()
            } else {
                format!("{}/", path.trim_end_matches('/'))
            };
            let mut out = Vec::new();
            let files = self.files.lock().unwrap();
            for (p, data) in files.iter() {
                if let Some(rest) = p.strip_prefix(&prefix) {
                    if !rest.contains('/') {
                        out.push(RemoteEntry {
                            name: rest.to_string(),
                            path: p.clone(),
                            is_dir: false,
                            size: data.len() as u64,
                            modified: None,
                            permissions: None,
                            owner: None,
                            group: None,
                            is_symlink: false,
                            link_target: None,
                            mime_type: None,
                            metadata: HashMap::new(),
                        });
                    }
                }
            }
            Ok(out)
        }
        async fn pwd(&mut self) -> Result<String, ProviderError> {
            Ok("/".into())
        }
        async fn cd(&mut self, _p: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn cd_up(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn download(
            &mut self,
            remote: &str,
            local: &str,
            _cb: Option<Box<dyn Fn(u64, u64) + Send>>,
        ) -> Result<(), ProviderError> {
            let data = self
                .files
                .lock()
                .unwrap()
                .get(remote)
                .cloned()
                .ok_or_else(|| ProviderError::NotFound(remote.to_string()))?;
            std::fs::write(local, &data).map_err(ProviderError::IoError)?;
            Ok(())
        }
        async fn download_to_bytes(&mut self, remote: &str) -> Result<Vec<u8>, ProviderError> {
            self.files
                .lock()
                .unwrap()
                .get(remote)
                .cloned()
                .ok_or_else(|| ProviderError::NotFound(remote.to_string()))
        }
        async fn upload(
            &mut self,
            local: &str,
            remote: &str,
            _cb: Option<Box<dyn Fn(u64, u64) + Send>>,
        ) -> Result<(), ProviderError> {
            let data = std::fs::read(local).map_err(ProviderError::IoError)?;
            self.files.lock().unwrap().insert(remote.to_string(), data);
            Ok(())
        }
        async fn mkdir(&mut self, p: &str) -> Result<(), ProviderError> {
            self.dirs.lock().unwrap().push(p.to_string());
            Ok(())
        }
        async fn delete(&mut self, p: &str) -> Result<(), ProviderError> {
            self.files.lock().unwrap().remove(p);
            Ok(())
        }
        async fn rmdir(&mut self, _p: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn rmdir_recursive(&mut self, _p: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
            let mut files = self.files.lock().unwrap();
            if let Some(data) = files.remove(from) {
                files.insert(to.to_string(), data);
            }
            Ok(())
        }
        async fn stat(&mut self, p: &str) -> Result<RemoteEntry, ProviderError> {
            let data = self
                .files
                .lock()
                .unwrap()
                .get(p)
                .cloned()
                .ok_or_else(|| ProviderError::NotFound(p.to_string()))?;
            let name = p.rsplit('/').next().unwrap_or(p).to_string();
            Ok(RemoteEntry {
                name,
                path: p.to_string(),
                is_dir: false,
                size: data.len() as u64,
                modified: None,
                permissions: None,
                owner: None,
                group: None,
                is_symlink: false,
                link_target: None,
                mime_type: None,
                metadata: HashMap::new(),
            })
        }
        async fn size(&mut self, p: &str) -> Result<u64, ProviderError> {
            self.files
                .lock()
                .unwrap()
                .get(p)
                .map(|d| d.len() as u64)
                .ok_or_else(|| ProviderError::NotFound(p.to_string()))
        }
        async fn exists(&mut self, p: &str) -> Result<bool, ProviderError> {
            Ok(self.files.lock().unwrap().contains_key(p))
        }
        async fn keep_alive(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn server_info(&mut self) -> Result<String, ProviderError> {
            Ok("mem".into())
        }
    }

    async fn roundtrip_through_decorator(keys: OverlayKeys, scope: &str) {
        let inner = Box::new(MemProvider::new());
        // Keep a raw handle by leaking a second pointer through Box::into_raw is
        // unsafe; instead, downcast back after wrapping is not possible once
        // boxed as dyn. So build the MemProvider, wrap, and assert via the
        // decorator's own list/download plus a raw inspection helper exposed by
        // re-borrowing through as_any_mut.
        let mut provider = CryptOverlayProvider::new(inner, keys, scope);

        // Upload a plaintext file through the decorator.
        let dir = std::env::temp_dir().join(format!("crypt_ovl_test_{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let local = dir.join("hello.txt");
        let payload = b"the quick brown fox jumps over the lazy dog";
        tokio::fs::write(&local, payload).await.unwrap();

        let remote_plain = if scope.is_empty() {
            "/notes/hello.txt".to_string()
        } else {
            format!("{}/notes/hello.txt", scope.trim_end_matches('/'))
        };
        provider
            .upload(local.to_str().unwrap(), &remote_plain, None)
            .await
            .unwrap();

        // Inspect the raw store: name must be encrypted, content must be
        // encrypted (no plaintext leak).
        let mem = provider
            .as_any_mut()
            .downcast_mut::<CryptOverlayProvider>()
            .unwrap()
            .inner
            .as_any_mut()
            .downcast_mut::<MemProvider>()
            .unwrap();
        let raw_paths = mem.raw_paths();
        assert_eq!(raw_paths.len(), 1, "one object stored");
        let raw_path = &raw_paths[0];
        assert!(
            !raw_path.contains("hello.txt") && !raw_path.contains("notes"),
            "raw path must be encrypted: {raw_path}"
        );
        let raw_bytes = mem.raw_bytes(raw_path).unwrap();
        assert_ne!(raw_bytes, payload, "raw content must be encrypted");

        // Read it back through the decorator: name + content decrypted.
        let listed = provider
            .list(&format!(
                "{}/notes",
                if scope.is_empty() {
                    ""
                } else {
                    scope.trim_end_matches('/')
                }
            ))
            .await
            .unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "hello.txt");
        // rclone maps to the exact plaintext size; AeroCrypt defers (reports the
        // ciphertext length), so only the lower bound holds for both kinds here.
        // Exact rclone size mapping is covered by `decrypted_size_*` tests.
        assert!(
            listed[0].size >= payload.len() as u64,
            "decrypted size lower bound"
        );

        let got = provider.download_to_bytes(&remote_plain).await.unwrap();
        assert_eq!(got, payload, "decrypted content round-trip");

        // download() to a file path too.
        let out = dir.join("out.txt");
        provider
            .download(&remote_plain, out.to_str().unwrap(), None)
            .await
            .unwrap();
        assert_eq!(tokio::fs::read(&out).await.unwrap(), payload);

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn end_to_end_rclone_whole_remote() {
        roundtrip_through_decorator(rclone_keys(FilenameEncryption::Standard, true, ".bin"), "")
            .await;
    }

    #[tokio::test]
    async fn end_to_end_aerocrypt_scoped() {
        roundtrip_through_decorator(aerocrypt_keys(), "/Vault").await;
    }

    /// Phase 3 on-demand model: applying an overlay to a live slot wraps the raw
    /// provider (writes become encrypted), clearing reverts it to the SAME raw
    /// provider (showing the encrypted store verbatim), and a re-apply never
    /// stacks a second decorator (one clear fully reverts to raw).
    #[tokio::test]
    async fn apply_clear_overlay_in_place_wraps_reverts_and_reanchors() {
        let mut slot: Option<Box<dyn StorageProvider>> = Some(Box::new(MemProvider::new()));
        let binding = OverlayUnlockParams {
            kind: "rclone-crypt".to_string(),
            remote_scope: String::new(),
            filename_encryption: "standard".to_string(),
            directory_name_encryption: true,
            off_suffix: None,
        };

        // No-op on a raw slot.
        assert!(
            !clear_overlay_in_place(&mut slot),
            "raw slot has no overlay"
        );

        // Apply: the slot now holds a decorator.
        let scope = apply_overlay_in_place(&mut slot, &binding, "pw", "salt")
            .await
            .unwrap();
        assert_eq!(scope, "");
        assert!(
            slot.as_mut()
                .unwrap()
                .as_any_mut()
                .downcast_mut::<CryptOverlayProvider>()
                .is_some(),
            "slot is wrapped after apply"
        );

        // Re-apply (re-anchor): must revert the prior overlay first, never stack.
        apply_overlay_in_place(&mut slot, &binding, "pw", "salt")
            .await
            .unwrap();

        // Write a plaintext file through the wrapped slot.
        let dir = std::env::temp_dir().join(format!("crypt_ovl_apply_{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let local = dir.join("secret.txt");
        tokio::fs::write(&local, b"plaintext payload")
            .await
            .unwrap();
        slot.as_mut()
            .unwrap()
            .upload(local.to_str().unwrap(), "/secret.txt", None)
            .await
            .unwrap();

        // A SINGLE clear reverts straight to the raw MemProvider (no nested
        // decorator survives the re-apply).
        assert!(clear_overlay_in_place(&mut slot), "overlay removed");
        let mem = slot
            .as_mut()
            .unwrap()
            .as_any_mut()
            .downcast_mut::<MemProvider>()
            .expect("slot reverted to the raw MemProvider");

        // The raw store kept the object under an ENCRYPTED name (the wrapped
        // upload encrypted it; clearing does not delete or decrypt it).
        let raw_paths = mem.raw_paths();
        assert_eq!(raw_paths.len(), 1, "one stored object");
        assert!(
            !raw_paths[0].contains("secret.txt"),
            "raw name stays encrypted after clear: {}",
            raw_paths[0]
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn out_of_scope_operation_fails_closed() {
        let inner = Box::new(MemProvider::new());
        let mut provider = CryptOverlayProvider::new(inner, aerocrypt_keys(), "/Vault");
        // A path outside the bound anchor is refused, never re-encrypted blindly.
        let err = provider.download_to_bytes("/Elsewhere/x.txt").await;
        assert!(matches!(err, Err(ProviderError::InvalidPath(_))));
    }

    #[tokio::test]
    async fn factory_passes_through_without_binding() {
        let inner: Box<dyn StorageProvider> = Box::new(MemProvider::new());
        let wrapped = wrap_provider_with_overlay_if_bound(inner, None, "", "")
            .await
            .unwrap();
        // No binding -> the same provider kind comes back (not wrapped).
        assert_eq!(wrapped.display_name(), "mem");
    }

    #[tokio::test]
    async fn factory_wraps_rclone_binding() {
        let inner: Box<dyn StorageProvider> = Box::new(MemProvider::new());
        let binding = OverlayUnlockParams {
            kind: "rclone-crypt".to_string(),
            remote_scope: String::new(),
            filename_encryption: "standard".to_string(),
            directory_name_encryption: true,
            off_suffix: None,
        };
        let mut wrapped = wrap_provider_with_overlay_if_bound(inner, Some(&binding), "pw", "salt")
            .await
            .unwrap();
        // The wrapper is a CryptOverlayProvider.
        assert!(wrapped
            .as_any_mut()
            .downcast_mut::<CryptOverlayProvider>()
            .is_some());
    }

    #[tokio::test]
    async fn factory_aerocrypt_fails_closed_on_missing_config() {
        let inner: Box<dyn StorageProvider> = Box::new(MemProvider::new());
        let binding = OverlayUnlockParams {
            kind: "aerocrypt".to_string(),
            remote_scope: "/Vault".to_string(),
            filename_encryption: String::new(),
            directory_name_encryption: true,
            off_suffix: None,
        };
        // No config on the (empty) remote -> unlock fails, no raw provider handed
        // back.
        let res = wrap_provider_with_overlay_if_bound(inner, Some(&binding), "pw", "").await;
        assert!(res.is_err(), "missing config must fail closed");
    }

    #[tokio::test]
    async fn range_reads_and_delta_disabled_so_dedupe_hashes_plaintext() {
        // dedupe hashes large files via `read_range`; a crypt overlay cannot map
        // a plaintext offset to a ciphertext offset, so the decorator must refuse
        // ranged reads (NotSupported) and report no delta-sync. That forces the
        // dedupe hasher to fall back to a full `download_to_bytes`, which yields
        // DECRYPTED bytes, so identical plaintext dedupes even though rclone
        // gives each file a random per-file nonce (identical plaintext -> DIFFERENT
        // ciphertext). Without this guard dedupe would hash ciphertext ranges and
        // never find a duplicate. Pin the invariant for both overlay kinds.
        for (label, keys) in both_kinds() {
            let inner: Box<dyn StorageProvider> = Box::new(MemProvider::new());
            let mut provider = CryptOverlayProvider::new(inner, keys, "");
            assert!(
                !provider.supports_delta_sync(),
                "{label}: delta sync must be off"
            );
            let ranged = provider.read_range("/whatever.bin", 0, 16).await;
            assert!(
                matches!(ranged, Err(ProviderError::NotSupported(_))),
                "{label}: ranged read must be NotSupported, got {ranged:?}"
            );
        }
    }

    // ── Phase 2: profile -> binding extraction (cross-profile / agent / MCP) ──

    #[test]
    fn binding_none_when_no_overlay_or_disabled() {
        // A plain profile and a profile with an explicitly-disabled overlay both
        // yield no binding, so the resolver returns the raw provider untouched.
        assert!(overlay_binding_from_profile(&serde_json::json!({ "id": "p1" })).is_none());
        assert!(overlay_binding_from_profile(&serde_json::json!({
            "id": "p1",
            "aeroCryptOverlay": { "enabled": false, "kind": "rclone-crypt" }
        }))
        .is_none());
        // `enabled` absent defaults to false (fail-safe: do not wrap by accident).
        assert!(overlay_binding_from_profile(&serde_json::json!({
            "id": "p1",
            "aeroCryptOverlay": { "kind": "rclone-crypt" }
        }))
        .is_none());
    }

    #[test]
    fn binding_rclone_reads_fields_and_anchor() {
        let params = overlay_binding_from_profile(&serde_json::json!({
            "id": "p1",
            "aeroCryptOverlay": {
                "enabled": true,
                "kind": "rclone-crypt",
                "remoteScope": "/enc",
                "filenameEncryption": "off",
                "directoryNameEncryption": false
            }
        }))
        .expect("enabled overlay must yield a binding");
        assert_eq!(params.kind, "rclone-crypt");
        assert_eq!(params.remote_scope, "/enc");
        assert_eq!(params.filename_encryption, "off");
        assert!(!params.directory_name_encryption);
    }

    #[test]
    fn binding_defaults_whole_remote_and_standard() {
        // Minimal enabled overlay: kind defaults to aerocrypt, scope to whole-
        // remote (""), filename encryption to standard, dir-name encryption on.
        let params = overlay_binding_from_profile(&serde_json::json!({
            "id": "p1",
            "aeroCryptOverlay": { "enabled": true }
        }))
        .expect("enabled overlay must yield a binding");
        assert_eq!(params.kind, "aerocrypt");
        assert_eq!(params.remote_scope, "");
        assert_eq!(params.filename_encryption, "standard");
        assert!(params.directory_name_encryption);
    }
}
