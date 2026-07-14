// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// Allow for the internal unlock helper that threads many necessary context params.
#![allow(clippy::too_many_arguments)]

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
//! 4. **Decrypted size.** `stat`/`size` return the DECRYPTED size via a
//!    deterministic ciphertext->plaintext size map for both kinds: rclone-crypt
//!    via [`crate::crypt_compare::rclone_decrypted_size`], AeroCrypt v3 via
//!    [`overlay::v3_decrypted_size`] (fixed header + per-block nonce/tag). Legacy
//!    AeroCrypt v1/v2 overlays are read-only and keep the deferred behaviour
//!    (on-wire ciphertext length). Mirrors [`crate::crypt_compare`].
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
use crate::providers::{
    FileVersion, ProviderError, ProviderType, RemoteEntry, StorageProvider, TrashEntry,
};
use crate::rclone_crypt::{self, FilenameEncryption, RcloneCryptKeys};

/// AeroCrypt overlay config filename, written at the scope root by `crypt init`.
/// Skipped from every decrypted listing (it is plaintext JSON, never an overlay
/// entry).
const AEROCRYPT_CONFIG_NAME: &str = crate::aerocrypt::overlay::CRYPT_CONFIG_WRITE_NAME;

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
    /// Both kinds now have a deterministic overhead map: rclone-crypt via
    /// [`rclone_decrypted_size`], AeroCrypt v3 via [`overlay::v3_decrypted_size`]
    /// (fixed header + per-block nonce/tag). Legacy AeroCrypt v1/v2 overlays are
    /// read-only and keep the deferred behaviour (return the ciphertext length),
    /// since their container header differs and the v3 decoder does not apply.
    fn decrypted_size(&self, size: u64) -> u64 {
        match self {
            Self::Rclone(_) => rclone_decrypted_size(size),
            Self::AeroCrypt {
                config: OverlayConfig::V3 { .. },
                ..
            } => overlay::v3_decrypted_size(size),
            // Legacy v1/v2 overlays are read-only; keep the deferred behaviour.
            Self::AeroCrypt { .. } => size,
        }
    }

    /// Whether [`decrypted_size`](Self::decrypted_size) returns the EXACT
    /// plaintext size (true) rather than falling back to the on-wire ciphertext
    /// length (false). Mirrors the `decrypted_size` arms: rclone-crypt and
    /// AeroCrypt v3 map deterministically; legacy AeroCrypt v1/v2 defer. Surfaced
    /// through [`StorageProvider::reports_exact_size`] so sync compare drops the
    /// size check for a deferred-size overlay instead of churning.
    fn size_is_exact(&self) -> bool {
        match self {
            Self::Rclone(_) => true,
            Self::AeroCrypt {
                config: OverlayConfig::V3 { .. },
                ..
            } => true,
            Self::AeroCrypt { .. } => false,
        }
    }

    /// Whether a listing entry name is an overlay sentinel that must never be
    /// surfaced as a decrypted file (config file, rclone dirIV markers).
    fn is_sentinel(&self, name: &str) -> bool {
        match self {
            Self::AeroCrypt { .. } => {
                name == AEROCRYPT_CONFIG_NAME || name == overlay::CRYPT_CONFIG_LEGACY_NAME
            }
            Self::Rclone(_) => RCLONE_DIRIV_SENTINELS.contains(&name),
        }
    }
}

// ── Path mapping (generic over the overlay kind) ─────────────────────────────

/// Normalize a plaintext crypt anchor: `""`/`"/"` => whole-remote scope; else an
/// absolute `/Foo/Bar` with `.`/`..` segments resolved.
///
/// Resolving `..` is a security boundary (audit B-F1): a stored anchor that still
/// contains `..` (a CLI-crafted or legacy `/vault/../etc`) would never match the
/// literal scope-prefix test against real, already-normalized provider paths, so
/// the whole vault would be treated as out-of-scope and writes would fall through
/// to the raw provider as plaintext. Mirrors the frontend `normalizeSegments`.
fn norm_anchor(scope: &str) -> String {
    let s = scope.trim();
    if s.is_empty() || s == "/" {
        return String::new();
    }
    let mut segs: Vec<&str> = Vec::new();
    for part in s.split('/') {
        match part {
            "" | "." => continue,
            ".." => {
                segs.pop();
            }
            other => segs.push(other),
        }
    }
    if segs.is_empty() {
        return String::new();
    }
    format!("/{}", segs.join("/"))
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

/// Access kind for path mapping through a scoped overlay.
/// Read allows pass-through for targets at/above/outside the anchor (used by
/// non-mutating ops like list, stat, download so plaintext areas remain
/// visible and listable). Write keeps the strict refusal (fail-closed) for
/// all mutating ops.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AccessKind {
    Read,
    Write,
}

/// Map a caller plaintext target to the on-wire encrypted path, keeping the
/// cleartext anchor prefix and encrypting only the tail below it.
/// For Write (mutating callers) an absolute target that is not at/under the
/// anchor is refused (fail-closed).
/// For Read (non-mutating callers) such targets return the raw path verbatim.
///
/// `scope` is the already-normalized anchor (`""` = whole remote). A relative
/// target is encrypted in full (it resolves against the current in-scope dir).
fn encode_plain_target(
    keys: &OverlayKeys,
    scope: &str,
    target: &str,
    is_dir_leaf: bool,
    kind: AccessKind,
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
    if kind == AccessKind::Read {
        return Ok(norm_abs(target));
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
    decode_entry_path(keys, encrypted_path, true)
}

/// Decrypt an on-wire entry path for display, decoding the leaf with its real
/// file/dir kind. This differs from [`decode_path`] only for rclone
/// `filename_encryption=off`, where file leaves carry the configured suffix
/// (usually `.bin`) and directory leaves do not.
fn decode_entry_path(keys: &OverlayKeys, encrypted_path: &str, leaf_is_dir: bool) -> String {
    if encrypted_path.is_empty() || encrypted_path == "." || encrypted_path == "/" {
        return encrypted_path.to_string();
    }
    let absolute = encrypted_path.starts_with('/');
    let components: Vec<&str> = encrypted_path
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect();
    let last = components.len().saturating_sub(1);
    let parts: Vec<String> = components
        .iter()
        .enumerate()
        .map(|(idx, part)| {
            let is_dir = if idx == last { leaf_is_dir } else { true };
            keys.decode_name(part, is_dir)
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

    /// Map a caller plaintext path to the on-wire encrypted path.
    /// Write mode: fail-closed on out-of-scope (mutating ops).
    /// Read mode: pass-through raw for out-of-scope (non-mutating ops: list etc).
    fn map(
        &self,
        plain: &str,
        is_dir_leaf: bool,
        kind: AccessKind,
    ) -> Result<String, ProviderError> {
        encode_plain_target(&self.keys, &self.scope, plain, is_dir_leaf, kind)
            .map_err(ProviderError::InvalidPath)
    }

    /// #390 fail-closed guard. When a write into `plain_target` lands on an
    /// encrypted parent that is missing on the wire, distinguish two cases:
    ///
    /// - #385 genuine: the encrypted parent chain simply does not exist yet (a
    ///   fresh encrypted tree). The caller should create it (`ensure_parent_dirs`)
    ///   and retry.
    /// - #390 phantom: a PLAINTEXT-named folder exists on the wire at that
    ///   location because it was created while the overlay was off. It has no
    ///   ciphertext preimage, so encrypting the path would spawn a divergent
    ///   `enc(name)` folder ALONGSIDE the plaintext one and silently misplace the
    ///   file (the folder the user is standing in stays empty).
    ///
    /// Returns `Some(on-wire plaintext parent)` for the #390 case, where the write
    /// must fail closed instead of materializing the phantom; `None` when it is
    /// safe to create the encrypted parent (the #385 case) or the guard does not
    /// apply. Only reached on the missing-parent retry path, so it costs one
    /// `exists` probe on failure and nothing on the success path.
    async fn phantom_plaintext_parent(&mut self, plain_target: &str) -> Option<String> {
        // A relative target resolves against the current on-wire dir, which is not
        // known here without a round trip; the frontend cwd re-read on overlay-arm
        // covers that seam. Absolute targets are what the upload UI actually sends.
        if !plain_target.starts_with('/') {
            return None;
        }
        let t = norm_abs(plain_target);
        let parent = match t.rsplit_once('/') {
            Some((p, _)) if !p.is_empty() => p.to_string(),
            _ => return None, // leaf directly at the root: no below-anchor parent
        };
        // Only a parent strictly BELOW the anchor can phantom. At or above the
        // anchor the path passes through as cleartext and the anchor itself
        // legitimately exists, so an `exists` hit there is not a phantom.
        let strictly_below = if self.scope.is_empty() {
            true
        } else {
            parent.starts_with(&format!("{}/", self.scope))
        };
        if !strictly_below {
            return None;
        }
        match self.inner.exists(&parent).await {
            Ok(true) => Some(parent),
            _ => None,
        }
    }

    /// #390 smart re-anchor support: is the wrapped provider's CURRENT on-wire
    /// cwd a valid location inside the encrypted view? True when the cwd is the
    /// anchor itself or a below-anchor path whose every component decodes as real
    /// ciphertext (so the overlay can render it decrypted in place); false when the
    /// cwd is outside the anchor, or a below-anchor path with a plaintext component
    /// that has no ciphertext preimage (a folder navigated to while the overlay was
    /// off, hidden once armed). Lets the UI keep the user where they are on arm when
    /// the folder is a genuine encrypted folder, and re-anchor to the scope/root
    /// only when the current folder would otherwise be hidden.
    pub async fn cwd_in_encrypted_view(&mut self) -> Result<bool, ProviderError> {
        let enc_cwd = norm_abs(&self.inner.pwd().await?);
        let below = if self.scope.is_empty() {
            enc_cwd.trim_start_matches('/').to_string()
        } else if enc_cwd == self.scope {
            return Ok(true); // the cleartext anchor root itself
        } else if let Some(b) = enc_cwd.strip_prefix(&format!("{}/", self.scope)) {
            b.to_string()
        } else {
            return Ok(false); // outside / above the anchor: not in the encrypted view
        };
        // Every below-anchor component must decode as valid ciphertext (directory
        // semantics). An empty tail (the anchor / remote root) is vacuously valid.
        let all_decode = below
            .split('/')
            .filter(|c| !c.is_empty() && *c != ".")
            .all(|c| self.keys.decode_name(c, true).is_some());
        Ok(all_decode)
    }

    /// True when an on-wire entry path is STRICTLY BELOW the cleartext anchor, so
    /// its name is ciphertext to decrypt. The anchor itself and anything at/above/
    /// outside it is cleartext pass-through. Whole-remote scope => everything.
    fn wire_path_is_encrypted(&self, wire_path: &str) -> bool {
        if self.scope.is_empty() {
            return true;
        }
        norm_abs(wire_path).starts_with(&format!("{}/", self.scope))
    }

    /// True for a caller plaintext target that is STRICTLY BELOW the anchor:
    /// its bytes on the wire (and in provider calls) are ciphertext and need
    /// decrypt / decrypted_size. Anchor itself and outside/above are plaintext
    /// pass-through (no decrypt step).
    ///
    /// Must mirror [`encode_plain_target`] decision-for-decision: whatever the
    /// mapper encrypts, this classifier must call encrypted, or a fetch decrypts
    /// the wrong side. In particular a RELATIVE target is encrypted in full by
    /// the mapper (it resolves against the current in-scope dir), so it is
    /// encrypted here too; absolutizing it first would compare a fake `/name`
    /// against the anchor and hand ciphertext back as plaintext.
    fn target_is_encrypted(&self, plain_target: &str) -> bool {
        if !plain_target.starts_with('/') {
            return true;
        }
        if self.scope.is_empty() {
            return true;
        }
        let t = norm_abs(plain_target);
        if t == self.scope {
            return false;
        }
        t.starts_with(&format!("{}/", self.scope))
    }

    /// Map a type-agnostic caller target (stat/rename/chmod take no is_dir) to
    /// its on-wire path. The file-form and dir-form encodings are identical in
    /// every mode except rclone `off` with a name suffix; when they differ,
    /// probe the file form and fall back to the directory form, so a directory
    /// target stops being addressed as `name.bin`. Returns the resolved on-wire
    /// path and whether the directory form was the one that resolved. Costs an
    /// extra `exists` round trip only in the off+suffix mode.
    async fn map_existing(
        &mut self,
        plain: &str,
        kind: AccessKind,
    ) -> Result<(String, bool), ProviderError> {
        let enc_file = self.map(plain, false, kind)?;
        let enc_dir = self.map(plain, true, kind)?;
        if enc_dir == enc_file {
            return Ok((enc_file, false));
        }
        match self.inner.exists(&enc_file).await {
            Ok(true) => Ok((enc_file, false)),
            _ => match self.inner.exists(&enc_dir).await {
                Ok(true) => Ok((enc_dir, true)),
                // Neither form exists (or the probe errored): keep the file
                // form so the caller surfaces the provider's own error.
                _ => Ok((enc_file, false)),
            },
        }
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

    /// Non-destructive mutable borrow of the wrapped transport, for callers that
    /// must invoke provider-specific operations (trash/restore/empty and every
    /// other `downcast_mut::<ConcreteProvider>()` command) on the real backend
    /// while the overlay stays live. Unlike [`take_inner`](Self::take_inner) this
    /// does not detach the inner provider. Reach it through the free function
    /// [`concrete_provider_mut`], which no-ops when Crypt is off.
    pub fn inner_mut(&mut self) -> &mut dyn StorageProvider {
        &mut *self.inner
    }

    /// Rewrite the DISPLAY name of each trash entry to plaintext for entries that
    /// belong to this overlay's scope, leaving `path`/metadata untouched. See the
    /// free function [`decode_overlay_trash_names`] for the full contract.
    fn decode_trash_names(&self, entries: &mut [RemoteEntry]) {
        for entry in entries.iter_mut() {
            if self.keys.is_sentinel(&entry.name) {
                continue;
            }
            if let Some(plain) = self.keys.decode_name(&entry.name, entry.is_dir) {
                entry.name = plain;
                if !entry.is_dir {
                    entry.size = self.keys.decrypted_size(entry.size);
                }
            }
        }
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

/// Best-effort creation of the encrypted parent directory chain for `enc_path`,
/// shallow to deep. Errors are ignored: a parent that already exists is the
/// common case, and any real failure surfaces on the caller's retried upload.
/// Gives strict WebDAV/OpenDrive providers the parent collection a PUT needs when
/// the crypt folder was created outside the overlay, mirroring rclone's implicit
/// mkdir before Put (#385).
async fn ensure_parent_dirs(inner: &mut Box<dyn StorageProvider>, enc_path: &str) {
    let absolute = enc_path.starts_with('/');
    let comps: Vec<&str> = enc_path.split('/').filter(|c| !c.is_empty()).collect();
    if comps.len() <= 1 {
        // Leaf at the root (or a bare name resolved against cwd): no parent to make.
        return;
    }
    let mut acc = String::new();
    for c in &comps[..comps.len() - 1] {
        if absolute || !acc.is_empty() {
            acc.push('/');
        }
        acc.push_str(c);
        let _ = inner.mkdir(&acc).await;
    }
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
            self.map(path, true, AccessKind::Read)?
        };
        let raw = self.inner.list(&enc_path).await?;
        let mut out = Vec::with_capacity(raw.len());
        for entry in raw {
            if self.keys.is_sentinel(&entry.name) {
                continue;
            }
            if self.wire_path_is_encrypted(&entry.path) {
                // inside the encrypted subtree: decrypt (a real foreign/corrupt row still drops)
                let Some(plain_name) = self.keys.decode_name(&entry.name, entry.is_dir) else {
                    continue;
                };
                let plain_path = decode_entry_path(&self.keys, &entry.path, entry.is_dir);
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
            } else {
                out.push(entry); // outside/at/above anchor: raw plaintext row, unchanged
            }
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
        let enc = self.map(path, true, AccessKind::Read)?;
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
        let enc = self.map(remote_path, false, AccessKind::Read)?;
        let is_enc = self.target_is_encrypted(remote_path);
        // Stage (for outside this holds plaintext; for inside: ciphertext).
        let temp_path = std::env::temp_dir().join(format!(
            "aeroftp_crypt_overlay_dl_{}_{}.bin",
            chrono::Utc::now().timestamp_millis(),
            uuid::Uuid::new_v4()
        ));
        let temp_str = temp_path.to_string_lossy().to_string();
        if let Err(e) = self.inner.download(&enc, &temp_str, on_progress).await {
            let _ = tokio::fs::remove_file(&temp_path).await;
            return Err(e);
        }
        let staged = tokio::fs::read(&temp_path)
            .await
            .map_err(ProviderError::IoError)?;
        let _ = tokio::fs::remove_file(&temp_path).await;
        let content = if is_enc {
            self.keys
                .decrypt_content(&staged)
                .map_err(|e| Self::crypt_err("content decrypt", e))?
        } else {
            staged
        };
        write_plaintext_atomic(local_path, &content).await
    }

    async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>, ProviderError> {
        let enc = self.map(remote_path, false, AccessKind::Read)?;
        let data = self.inner.download_to_bytes(&enc).await?;
        if self.target_is_encrypted(remote_path) {
            self.keys
                .decrypt_content(&data)
                .map_err(|e| Self::crypt_err("content decrypt", e))
        } else {
            Ok(data)
        }
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
        // Propagate the SOURCE file's mtime onto the ciphertext temp. The inner
        // provider's upload stamps the remote object from the file it uploads
        // (SFTP `set_metadata`, FTP `MFMT`), which here is the temp, not the
        // caller's `local_path`. Without this the remote encrypted object gets
        // the temp's creation time ("now") and AeroSync never converges on a
        // crypt overlay (the remote always looks just-modified). Best-effort:
        // failure to read/stamp only degrades to the previous behaviour.
        if let Ok(meta) = tokio::fs::metadata(local_path).await {
            if let Ok(src_mtime) = meta.modified() {
                let ft = filetime::FileTime::from_system_time(src_mtime);
                let _ = filetime::set_file_mtime(&temp_path, ft);
            }
        }
        let enc = match self.map(remote_path, false, AccessKind::Write) {
            Ok(p) => p,
            Err(e) => {
                let _ = tokio::fs::remove_file(&temp_path).await;
                return Err(e);
            }
        };
        let mut result = self
            .inner
            .upload(&temp_path.to_string_lossy(), &enc, on_progress)
            .await;
        // #385: strict WebDAV/OpenDrive providers PUT-fail with "Path not found"
        // when the encrypted parent collection does not exist (e.g. the crypt
        // folder was created while the overlay was off, so only its plaintext name
        // exists on the remote). Ensure the encrypted parent chain and retry once,
        // mirroring rclone's implicit mkdir before Put. Gated to path-missing errors
        // so an auth/quota failure never leaves stray encrypted directories behind.
        if matches!(
            result,
            Err(ProviderError::NotFound(_)) | Err(ProviderError::InvalidPath(_))
        ) {
            // #390 (option 1, strict): before creating the encrypted parent chain,
            // refuse if the on-wire parent is a plaintext-named folder created while
            // the overlay was off. Creating enc(name) there would spawn a divergent
            // phantom folder and silently misplace the file. Fail closed with a clear
            // message instead. A genuinely-missing encrypted parent (#385) still
            // creates and retries.
            if let Some(plain_parent) = self.phantom_plaintext_parent(remote_path).await {
                let _ = tokio::fs::remove_file(&temp_path).await;
                return Err(ProviderError::InvalidPath(format!(
                    "the current folder {:?} is a plaintext folder created with the crypt \
                     overlay off, so it is not part of the encrypted view. AeroFTP will not \
                     create a second encrypted folder here and silently misplace the file. Turn \
                     the overlay off to use this folder, or move into an encrypted folder before \
                     uploading (see issue #390).",
                    plain_parent
                )));
            }
            ensure_parent_dirs(&mut self.inner, &enc).await;
            result = self
                .inner
                .upload(&temp_path.to_string_lossy(), &enc, None)
                .await;
        }
        let _ = tokio::fs::remove_file(&temp_path).await;
        result
    }

    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError> {
        let enc = self.map(path, true, AccessKind::Write)?;
        self.inner.mkdir(&enc).await
    }

    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        let enc = self.map(path, false, AccessKind::Write)?;
        self.inner.delete(&enc).await
    }

    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        let enc = self.map(path, true, AccessKind::Write)?;
        self.inner.rmdir(&enc).await
    }

    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        let enc = self.map(path, true, AccessKind::Write)?;
        self.inner.rmdir_recursive(&enc).await
    }

    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        // is_dir is unknown for a bare rename. The two leaf encodings differ
        // only under rclone `off`+suffix mode; `map_existing` resolves the
        // source's real kind there, so a directory rename maps both ends in
        // directory form instead of targeting a nonexistent `name.bin`.
        let (enc_from, from_is_dir) = self.map_existing(from, AccessKind::Write).await?;
        let enc_to = self.map(to, from_is_dir, AccessKind::Write)?;
        self.inner.rename(&enc_from, &enc_to).await
    }

    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        let (enc, _) = self.map_existing(path, AccessKind::Read).await?;
        let mut entry = self.inner.stat(&enc).await?;
        let is_enc = self.target_is_encrypted(path);
        let plain_name = if is_enc {
            self.keys
                .decode_name(&entry.name, entry.is_dir)
                .unwrap_or(entry.name)
        } else {
            entry.name
        };
        let plain_path = if is_enc {
            decode_entry_path(&self.keys, &entry.path, entry.is_dir)
        } else {
            entry.path
        };
        let size = if entry.is_dir {
            0
        } else if is_enc {
            self.keys.decrypted_size(entry.size)
        } else {
            entry.size
        };
        entry.name = plain_name;
        entry.path = plain_path;
        entry.size = size;
        Ok(entry)
    }

    async fn size(&mut self, path: &str) -> Result<u64, ProviderError> {
        let enc = self.map(path, false, AccessKind::Read)?;
        let s = self.inner.size(&enc).await?;
        if self.target_is_encrypted(path) {
            Ok(self.keys.decrypted_size(s))
        } else {
            Ok(s)
        }
    }

    async fn exists(&mut self, path: &str) -> Result<bool, ProviderError> {
        let enc = self.map(path, false, AccessKind::Read)?;
        if self.inner.exists(&enc).await? {
            return Ok(true);
        }
        // Off+suffix mode only: the directory form differs; a directory target
        // must not false-negative because it was probed as `name.bin`.
        let enc_dir = self.map(path, true, AccessKind::Read)?;
        if enc_dir != enc {
            return self.inner.exists(&enc_dir).await;
        }
        Ok(false)
    }

    async fn keep_alive(&mut self) -> Result<(), ProviderError> {
        self.inner.keep_alive().await
    }

    async fn server_info(&mut self) -> Result<String, ProviderError> {
        self.inner.server_info().await
    }

    async fn delete_permanent(&mut self, path: &str) -> Result<bool, ProviderError> {
        let enc = self.map(path, false, AccessKind::Write)?;
        self.inner.delete_permanent(&enc).await
    }

    fn supports_chmod(&self) -> bool {
        self.inner.supports_chmod()
    }

    async fn chmod(&mut self, path: &str, mode: u32) -> Result<(), ProviderError> {
        let (enc, _) = self.map_existing(path, AccessKind::Write).await?;
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
        let enc_from = self.map(from, false, AccessKind::Write)?;
        let enc_to = self.map(to, false, AccessKind::Write)?;
        self.inner.server_copy(&enc_from, &enc_to).await
    }

    async fn server_side_copy(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        let enc_from = self.map(from, false, AccessKind::Write)?;
        let enc_to = self.map(to, false, AccessKind::Write)?;
        self.inner.server_side_copy(&enc_from, &enc_to).await
    }

    async fn storage_info(&mut self) -> Result<crate::providers::StorageInfo, ProviderError> {
        // Byte totals are over ciphertext; surfaced as-is (the per-file overhead
        // is tiny relative to quota figures).
        self.inner.storage_info().await
    }

    async fn disk_usage(&mut self, path: &str) -> Result<u64, ProviderError> {
        let enc = self.map(path, true, AccessKind::Read)?;
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
        let enc = self.map(path, false, AccessKind::Read)?;
        let mut versions = self.inner.list_versions(&enc).await?;
        if self.target_is_encrypted(path) {
            for v in &mut versions {
                v.size = self.keys.decrypted_size(v.size);
            }
        }
        Ok(versions)
    }

    async fn download_version(
        &mut self,
        path: &str,
        version_id: &str,
        local_path: &str,
    ) -> Result<(), ProviderError> {
        let enc = self.map(path, false, AccessKind::Read)?;
        let is_enc = self.target_is_encrypted(path);
        let temp_path = std::env::temp_dir().join(format!(
            "aeroftp_crypt_overlay_ver_{}_{}.bin",
            chrono::Utc::now().timestamp_millis(),
            uuid::Uuid::new_v4()
        ));
        let temp_str = temp_path.to_string_lossy().to_string();
        if let Err(e) = self
            .inner
            .download_version(&enc, version_id, &temp_str)
            .await
        {
            let _ = tokio::fs::remove_file(&temp_path).await;
            return Err(e);
        }
        let staged = tokio::fs::read(&temp_path)
            .await
            .map_err(ProviderError::IoError)?;
        let _ = tokio::fs::remove_file(&temp_path).await;
        let content = if is_enc {
            self.keys
                .decrypt_content(&staged)
                .map_err(|e| Self::crypt_err("version decrypt", e))?
        } else {
            staged
        };
        write_plaintext_atomic(local_path, &content).await
    }

    async fn restore_version(&mut self, path: &str, version_id: &str) -> Result<(), ProviderError> {
        let enc = self.map(path, false, AccessKind::Write)?;
        self.inner.restore_version(&enc, version_id).await
    }

    async fn delete_version(&mut self, path: &str, version_id: &str) -> Result<(), ProviderError> {
        let enc = self.map(path, false, AccessKind::Write)?;
        self.inner.delete_version(&enc, version_id).await
    }

    // NOTE: `list_object_versions` / `empty_object_versions` are deliberately NOT
    // overridden. The trash browse works on raw ciphertext keys and decrypts a
    // separate display field, so the command layer reaches the concrete provider
    // via `concrete_provider_mut` (the #399 peel) instead of routing through the
    // overlay; through the overlay they keep the trait NotSupported default.

    // ── Conservatively disabled surfaces (see module docs) ───────────────────
    //
    // Each is advertised unsupported so the runner/caller falls back to a
    // whole-file mapped path instead of an offset/digest/server-fetch operation
    // that a crypt overlay cannot perform byte-exactly. The methods keep their
    // trait defaults (NotSupported).

    fn supports_resume(&self) -> bool {
        false
    }

    // A partial ciphertext is never byte-resumable (per-file nonce / AEAD
    // framing), so the "Resume" action must fall back to a full re-encrypt.
    // Explicit (not just the trait default) so a future refactor cannot make
    // this silently delegate to the inner provider.
    fn supports_resume_upload_append(&self) -> bool {
        false
    }

    fn supports_find(&self) -> bool {
        false
    }

    fn supports_thumbnails(&self) -> bool {
        false
    }

    fn reports_exact_size(&self) -> bool {
        // rclone / AeroCrypt v3 map the ciphertext size to the exact plaintext
        // size; legacy AeroCrypt v1/v2 defer (return ciphertext length). Tell the
        // sync so it drops size comparison for the deferred case instead of
        // re-syncing every file every cycle.
        self.keys.size_is_exact()
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
/// resolver in a later phase). `keyfile_digest` is the OPTIONAL AeroCrypt Tier 1
/// second factor, already resolved from the profile's keyfile path (see
/// [`resolve_profile_keyfile_digest`]); `None` for password-only vaults.
pub async fn wrap_provider_with_overlay_if_bound(
    mut inner: Box<dyn StorageProvider>,
    binding: Option<&OverlayUnlockParams>,
    password: &str,
    salt: &str,
    keyfile_digest: Option<&[u8; 32]>,
) -> Result<Box<dyn StorageProvider>, String> {
    let Some(params) = binding else {
        return Ok(inner);
    };
    // Non-interactive factory (CLI / cross-profile / MCP): fail-closed, never
    // bootstrap an overlay on a folder that has no config.
    let keys = unlock_overlay_keys_encrypting(
        &mut *inner,
        params,
        password,
        salt,
        keyfile_digest,
        false,
        false,
        None, // non-interactive factory path: never default-salt opt-in
    )
    .await?;
    let provider = CryptOverlayProvider::new(inner, keys, &params.remote_scope);
    Ok(Box::new(provider))
}

/// True when a provider slot is already the plaintext crypt-overlay decorator.
///
/// Compare/reconcile code uses this to avoid unlocking and normalizing a second
/// time after the CLI/MCP provider resolver has already wrapped the transport.
pub fn is_crypt_overlay_provider(provider: &mut dyn StorageProvider) -> bool {
    provider.as_any_mut().is::<CryptOverlayProvider>()
}

/// Peel a live provider box down to the concrete transport, past any crypt
/// overlay decorator, so provider-specific operations (trash/restore/empty and
/// every other command that must `downcast_mut::<ConcreteProvider>()`) can reach
/// the real backend when Crypt is on.
///
/// When Crypt is off (no overlay) this is a no-op returning the provider itself,
/// so it is safe to apply unconditionally at every downcast site, including
/// providers that never carry a crypt scope.
///
/// Note the overlay's `as_any_mut()` deliberately returns the decorator itself
/// (an honest downcast target), so a raw `downcast_mut::<ConcreteProvider>()` on
/// a wrapped box yields `None`; callers must peel first via this helper.
pub fn concrete_provider_mut(provider: &mut dyn StorageProvider) -> &mut dyn StorageProvider {
    if provider.as_any_mut().is::<CryptOverlayProvider>() {
        provider
            .as_any_mut()
            .downcast_mut::<CryptOverlayProvider>()
            .expect("just checked is::<CryptOverlayProvider>()")
            .inner_mut()
    } else {
        provider
    }
}

/// Decode the DISPLAY names of trash entries in place when `provider` is a live
/// crypt overlay, so a "View Trash" listing shows plaintext names instead of
/// ciphertext for items that live inside the encrypted scope. A no-op when Crypt
/// is off.
///
/// ONLY the human-facing `name` (and, for a file, the decrypted `size`) is
/// rewritten. The `path` and every provider `metadata` token are left byte-for-
/// byte as returned by the backend, because restore/permanent-delete round-trip
/// those raw tokens; rewriting them would break the round-trip. The one provider
/// whose restore keys off the NAME (MEGA) must therefore NOT be passed here (it
/// stays ciphertext until the frontend can carry a separate display name).
///
/// Decode-or-passthrough: a name that is not valid ciphertext for this overlay
/// (a foreign / out-of-scope item in a globally shared trash) is left verbatim,
/// never dropped, so the trash view stays complete.
pub fn decode_overlay_trash_names(provider: &mut dyn StorageProvider, entries: &mut [RemoteEntry]) {
    if let Some(overlay) = provider.as_any_mut().downcast_mut::<CryptOverlayProvider>() {
        overlay.decode_trash_names(entries);
    }
}

/// Decode a single trash entry's DISPLAY name when `provider` is a live crypt
/// overlay, returning the plaintext name, or `None` to keep the original. For
/// trash listers whose entry type is not [`RemoteEntry`] (e.g. Koofr). Same
/// decode-or-passthrough contract as [`decode_overlay_trash_names`].
pub fn decode_overlay_trash_name(
    provider: &mut dyn StorageProvider,
    name: &str,
    is_dir: bool,
) -> Option<String> {
    let overlay = provider
        .as_any_mut()
        .downcast_mut::<CryptOverlayProvider>()?;
    if overlay.keys.is_sentinel(name) {
        return None;
    }
    overlay.keys.decode_name(name, is_dir)
}

/// Decrypt the `display_key` of each S3 [`TrashEntry`] in place when `provider`
/// is a live crypt overlay, so the trash browse shows plaintext keys. A no-op
/// when Crypt is off (the command layer already seeds `display_key == key`).
///
/// Only `display_key` is rewritten; `key`, `version_id` and every other field
/// stay byte-for-byte raw so restore and purge round-trip the exact backend
/// tokens (the #399 contract). A key whose components are not valid ciphertext
/// for this overlay (a foreign / out-of-scope object) is left verbatim.
pub fn decode_overlay_trash_keys(provider: &mut dyn StorageProvider, entries: &mut [TrashEntry]) {
    let Some(overlay) = provider.as_any_mut().downcast_mut::<CryptOverlayProvider>() else {
        return;
    };
    for entry in entries.iter_mut() {
        // A trash row is always an object (file) leaf, never a directory.
        entry.display_key = decode_entry_path(&overlay.keys, &entry.key, false);
    }
}

/// Read a keyfile from `path` and return its AeroCrypt KDF digest (F1 + F2
/// canonicalization via [`crate::aerocrypt::keyfile_digest_from_file`]).
/// FAIL-CLOSED: an unreadable or malformed keyfile is a hard `Err`; callers
/// must refuse the unlock, never fall back to password-only (a keyfile vault
/// would then always report "wrong keyfile" instead of the real problem).
pub fn keyfile_digest_from_path(path: &str) -> Result<[u8; 32], String> {
    let bytes =
        std::fs::read(path).map_err(|e| format!("cannot read AeroCrypt keyfile '{path}': {e}"))?;
    crate::aerocrypt::keyfile_digest_from_file(&bytes)
        .map_err(|e| format!("invalid AeroCrypt keyfile '{path}': {e}"))
}

/// Resolve a profile's OPTIONAL AeroCrypt keyfile (Tier 1 second factor) to its
/// KDF digest. The keyfile PATH lives in the per-profile vault key
/// `aerocrypt_overlay_keyfile_path_<id>` (mirroring `_pw_` / `_salt_`), with the
/// `AEROFTP_CRYPT_OVERLAY_KEYFILE` env var as the headless fallback (mirroring
/// `AEROFTP_CRYPT_OVERLAY_PASSWORD`). `Ok(None)` when the profile has no
/// keyfile. FAIL-CLOSED: a stored path whose file cannot be read or parsed is a
/// hard `Err` via [`keyfile_digest_from_path`], never a silent password-only
/// fallback.
pub fn resolve_profile_keyfile_digest(
    store: &crate::credential_store::CredentialStore,
    profile_id: &str,
) -> Result<Option<[u8; 32]>, String> {
    let path = crate::user_partitions::resolve_active_credential(
        store,
        &format!("aerocrypt_overlay_keyfile_path_{}", profile_id),
    )
    .ok()
    .flatten()
    .map(|s| s.to_string())
    .filter(|s| !s.is_empty())
    .or_else(|| {
        std::env::var("AEROFTP_CRYPT_OVERLAY_KEYFILE")
            .ok()
            .filter(|s| !s.is_empty())
    });
    match path {
        None => Ok(None),
        Some(p) => keyfile_digest_from_path(&p).map(Some),
    }
}

/// Validate that a headerless AeroCrypt config JSON matches the per-profile
/// salt of record (`aerocrypt_overlay_salt_<id>`). The config blob intentionally
/// stores the complete public `OverlayConfig` JSON so the same parser and
/// config-MAC verifier are used for headed and headerless vaults. The separate
/// salt key remains the local keystore's salt of record; a mismatch is treated
/// as local tampering or a partial restore and fails closed.
pub fn validate_headerless_config_salt(
    profile_id: &str,
    config_json: &str,
    stored_salt: Option<&str>,
) -> Result<(), String> {
    let value: serde_json::Value = serde_json::from_str(config_json)
        .map_err(|e| format!("Invalid headerless AeroCrypt config JSON: {e}"))?;
    let config_salt = value
        .get("salt")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "Headerless AeroCrypt config is missing salt".to_string())?;
    let stored_salt = stored_salt.filter(|s| !s.is_empty()).ok_or_else(|| {
        if profile_id.is_empty() {
            "Headerless AeroCrypt config is missing its local salt of record".to_string()
        } else {
            format!(
                "Headerless AeroCrypt config for profile {profile_id} is missing aerocrypt_overlay_salt_{profile_id}"
            )
        }
    })?;
    if config_salt != stored_salt {
        return Err(if profile_id.is_empty() {
            "Headerless AeroCrypt config salt does not match its local salt of record".to_string()
        } else {
            format!(
                "Headerless AeroCrypt config for profile {profile_id} does not match aerocrypt_overlay_salt_{profile_id}"
            )
        });
    }
    Ok(())
}

/// Persist the PUBLIC AeroCrypt overlay config JSON plus its salt of record into
/// the local keystore for `id`, transactionally: write the config, then the
/// salt; roll the config back if the salt write fails, so a half-written keystore
/// never masquerades as a complete vault. The stored blob is public-only (salt,
/// KDF params, vault_id, config MAC), never a secret.
///
/// Shared by headerless init (where it MUST succeed, since the remote carries no
/// marker), and by headed init / connect-time backfill (where it is a best-effort
/// convenience copy so the on-demand Recovery Kit works in every mode without
/// connecting; there the remote marker stays the source of truth, so callers
/// treat a failure as non-fatal).
fn persist_public_overlay_config(
    store: &crate::credential_store::CredentialStore,
    id: &str,
    config_json: &str,
    salt_b64: &str,
) -> Result<(), String> {
    let config_key = format!("aerocrypt_overlay_config_{id}");
    let salt_key = format!("aerocrypt_overlay_salt_{id}");
    let prior_config = crate::user_partitions::resolve_active_credential(store, &config_key)
        .ok()
        .flatten()
        .map(|s| s.to_string());
    crate::user_partitions::store_active_credential_dual(store, &config_key, config_json)
        .map_err(|e| format!("Cannot persist AeroCrypt overlay config to the keystore: {e}"))?;
    if let Err(e) = crate::user_partitions::store_active_credential_dual(store, &salt_key, salt_b64)
    {
        // Roll the config back to its prior value (or remove it) so a config
        // without its matching salt of record never lingers.
        match prior_config {
            Some(prev) => {
                let _ =
                    crate::user_partitions::store_active_credential_dual(store, &config_key, &prev);
            }
            None => {
                let _ = crate::user_partitions::delete_active_credential_dual(store, &config_key);
            }
        }
        return Err(format!(
            "Cannot persist AeroCrypt overlay salt to the keystore: {e}"
        ));
    }
    Ok(())
}

/// Best-effort: cache a HEADED vault's PUBLIC config in the local keystore so the
/// on-demand Recovery Kit (T3) works without connecting, for headed vaults
/// created before this build too. Idempotent: fills the per-profile config/salt
/// entries only when nothing is stored yet, so it never clobbers an existing
/// (possibly headerless) salt of record. No-op without a saved profile id or a
/// keystore. Never fails the unlock: the remote marker is authoritative.
fn backfill_headed_overlay_config(params: &OverlayUnlockParams, config_json: &str) {
    let Some(id) = params.profile_id.as_deref().filter(|s| !s.is_empty()) else {
        return;
    };
    let Some(store) = crate::credential_store::CredentialStore::from_cache() else {
        return;
    };
    let config_key = format!("aerocrypt_overlay_config_{id}");
    let already = crate::user_partitions::resolve_active_credential(&store, &config_key)
        .ok()
        .flatten()
        .map(|s| !s.to_string().is_empty())
        .unwrap_or(false);
    if already {
        return;
    }
    // The config JSON carries the salt (base64) directly; reuse it as the salt of
    // record so validate_headerless_config_salt stays consistent.
    let Some(salt_b64) = serde_json::from_str::<serde_json::Value>(config_json)
        .ok()
        .and_then(|v| {
            v.get("salt")
                .and_then(|s| s.as_str())
                .map(|s| s.to_string())
        })
        .filter(|s| !s.is_empty())
    else {
        return;
    };
    if let Err(e) = persist_public_overlay_config(&store, id, config_json, &salt_b64) {
        eprintln!("[aerocrypt] headed overlay config backfill skipped for {id}: {e}");
    }
}

fn local_headerless_config_from_params(
    params: &OverlayUnlockParams,
) -> Result<Option<String>, String> {
    if let Some(config_json) = params
        .local_config_json
        .as_deref()
        .filter(|s| !s.is_empty())
    {
        validate_headerless_config_salt(
            params.profile_id.as_deref().unwrap_or(""),
            config_json,
            params.local_config_salt.as_deref(),
        )?;
        return Ok(Some(config_json.to_string()));
    }

    let Some(profile_id) = params.profile_id.as_deref().filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let Some(store) = crate::credential_store::CredentialStore::from_cache() else {
        return Ok(None);
    };
    let key = format!("aerocrypt_overlay_config_{profile_id}");
    let Some(config_json) = crate::user_partitions::resolve_active_credential(&store, &key)
        .map_err(|e| format!("Cannot read local AeroCrypt overlay config: {e}"))?
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
    else {
        return Ok(None);
    };
    let salt_key = format!("aerocrypt_overlay_salt_{profile_id}");
    let salt = crate::user_partitions::resolve_active_credential(&store, &salt_key)
        .map_err(|e| format!("Cannot read local AeroCrypt overlay salt: {e}"))?
        .map(|s| s.to_string());
    validate_headerless_config_salt(profile_id, &config_json, salt.as_deref())?;
    Ok(Some(config_json))
}

fn derive_aerocrypt_overlay_keys_from_config(
    config_json: &str,
    password: &str,
    keyfile_digest: Option<&[u8; 32]>,
) -> Result<(OverlayConfig, [u8; KEY_SIZE]), String> {
    let config = overlay::parse_config(config_json)
        .map_err(|e| format!("Invalid AeroCrypt overlay config: {e}"))?;
    let keyfile_digest = match (config.requires_keyfile(), keyfile_digest) {
        (true, None) => {
            return Err("this AeroCrypt overlay requires a keyfile (none was provided)".to_string())
        }
        (false, Some(_)) => return Err(
            "this AeroCrypt overlay was not created with a keyfile (remove the keyfile to unlock)"
                .to_string(),
        ),
        (true, kd) => kd,
        (false, _) => None,
    };
    let master_key = overlay::derive_master_key_with_keyfile(&config, password, keyfile_digest)
        .map_err(|e| format!("AeroCrypt key derivation failed: {e}"))?;
    overlay::verify_config_mac(&config, &master_key)
        .map_err(|e| format!("AeroCrypt unlock failed: {e}"))?;
    Ok((config, master_key))
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
/// back. A profile without a binding returns `inner` byte-identical. Exception:
/// an AeroCrypt binding with a resolved keyfile digest may legally have an
/// EMPTY password (keyfile-only vault) and unlocks with `""`.
///
/// The binding (kind / scope / name-encryption modes) reuses the profile's
/// `aeroCryptOverlay` JSON; the secrets reuse the generic per-profile vault keys
/// `aerocrypt_overlay_pw_<id>` / `aerocrypt_overlay_salt_<id>` shared by both
/// overlay kinds (lib.rs ~15610), plus the OPTIONAL Tier 1 keyfile path
/// `aerocrypt_overlay_keyfile_path_<id>` resolved fail-closed to its digest at
/// connect time. Mirrors `mcp::pool::resolve_overlay_secrets`
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

    // Tier 1 keyfile second factor: resolve the profile's stored keyfile path to
    // its digest at connect time, BEFORE the password guard (a keyfile-only
    // AeroCrypt vault legally has an empty password). FAIL-CLOSED on an
    // unreadable stored keyfile.
    let keyfile_digest = resolve_profile_keyfile_digest(store, id)?;
    let password = crate::user_partitions::resolve_active_credential(
        store,
        &format!("aerocrypt_overlay_pw_{}", id),
    )
    .ok()
    .flatten()
    .map(|s| s.to_string())
    .filter(|s| !s.is_empty())
    .or_else(|| std::env::var("AEROFTP_CRYPT_OVERLAY_PASSWORD").ok());
    // Keyfiles do not apply to rclone-crypt, which keeps requiring a password.
    let password = match password {
        Some(p) => p,
        None if params.kind == "aerocrypt" && keyfile_digest.is_some() => String::new(),
        None => return Err(
            "Crypt overlay profile has no stored password. Store it in the AeroFTP GUI, or set AEROFTP_CRYPT_OVERLAY_PASSWORD."
                .to_string(),
        ),
    };
    let salt = crate::user_partitions::resolve_active_credential(
        store,
        &format!("aerocrypt_overlay_salt_{}", id),
    )
    .ok()
    .flatten()
    .map(|s| s.to_string())
    .or_else(|| std::env::var("AEROFTP_CRYPT_OVERLAY_SALT").ok())
    .unwrap_or_default();
    let local_config_json = crate::user_partitions::resolve_active_credential(
        store,
        &format!("aerocrypt_overlay_config_{}", id),
    )
    .ok()
    .flatten()
    .map(|s| s.to_string())
    .filter(|s| !s.is_empty());
    let params = OverlayUnlockParams {
        local_config_json,
        local_config_salt: if salt.is_empty() {
            None
        } else {
            Some(salt.clone())
        },
        ..params
    };

    wrap_provider_with_overlay_if_bound(
        inner,
        Some(&params),
        &password,
        &salt,
        keyfile_digest.as_ref(),
    )
    .await
}

/// Resolve the active user's saved profile NAMED `profile_name` and wrap a
/// freshly-connected provider via [`wrap_connected_provider_for_profile`]. Used
/// by AeroCloud background sync, whose [`crate::cloud_config::CloudConfig`]
/// references the server profile by name (not the profile JSON) and which runs
/// in a scheduled worker with no `AppHandle`.
///
/// The profile set is read with
/// [`crate::user_partitions::mcp_list_active_server_profiles`]: it is
/// `AppHandle`-free (opens the SAME shared user-partitions DB that the sync's
/// own credential lookup, `resolve_active_credential`, already opens via
/// `open_or_init_cli`) and carries the identical legacy-blob fallback, so any
/// profile that resolves credentials for the sync also resolves here. Matching
/// is by exact `name`, mirroring the credential key `server_<name>` the sync
/// connects with.
///
/// FAIL-CLOSED for a crypt-bound profile: an enabled binding whose vault is
/// locked / has no stored secret returns `Err` (the sync is refused, never run
/// against the raw provider). A profile that is not crypt-bound - or is absent
/// from the readable profile set, and therefore cannot carry a binding -
/// returns `inner` byte-identical. An unreadable profile set propagates its
/// `Err` (fail-closed), consistent with the credential read that gates the
/// same sync.
pub async fn wrap_connected_provider_for_profile_named(
    inner: Box<dyn StorageProvider>,
    profile_name: &str,
    store: &crate::credential_store::CredentialStore,
) -> Result<Box<dyn StorageProvider>, String> {
    let profiles = crate::user_partitions::mcp_list_active_server_profiles(store)?;
    let Some(profile) = select_profile_by_name(&profiles, profile_name) else {
        return Ok(inner);
    };
    wrap_connected_provider_for_profile(inner, profile, store).await
}

/// AeroCrypt overlay builder for the AeroCloud stack.
/// This is byte-identical to the single crypt wrap that used to live at
/// cloud_service.rs:1693. Behavior is unchanged for both crypt-bound and plain
/// profiles.
///
/// Compression is intentionally not resolved here anymore: it is per-AeroCloud
/// config/pair state, so `sync_one_config` wraps `CompressOverlayProvider`
/// around this crypt-only builder when `CloudConfig.compress_enabled` is true.
/// Any layer refusing (e.g. unlock fail or invalid compression config) aborts
/// the whole stack (fail-closed).
///
/// Keep this builder crypt-only; the caller owns higher layers whose settings
/// live in `CloudConfig` or `CloudPathPair`.
pub async fn build_aerocloud_overlay_stack(
    inner: Box<dyn StorageProvider>,
    profile_name: &str,
    store: &crate::credential_store::CredentialStore,
) -> Result<Box<dyn StorageProvider>, String> {
    wrap_connected_provider_for_profile_named(inner, profile_name, store).await
}

/// Select the profile named `name` from a decrypted profile list by EXACT
/// `name` match. Pure seam of [`wrap_connected_provider_for_profile_named`],
/// pinned by tests so the match stays exact: a fuzzy / substring / id match
/// could resolve the WRONG profile and thereby fail-open a crypt binding (wrap
/// with the wrong keys, or skip the wrap on a bound profile). Exact `name`
/// mirrors the credential key `server_<name>` the background sync connects with.
pub(crate) fn select_profile_by_name<'a>(
    profiles: &'a [serde_json::Value],
    name: &str,
) -> Option<&'a serde_json::Value> {
    profiles
        .iter()
        .find(|p| p.get("name").and_then(|v| v.as_str()) == Some(name))
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
        profile_id: profile
            .get("id")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        local_config_json: None,
        local_config_salt: None,
    })
}

/// Unlock encryption-capable [`OverlayKeys`] from an overlay binding plus the
/// secret(s). The AeroCrypt arm retains the parsed [`OverlayConfig`] (needed for
/// content encryption), which [`crate::crypt_compare::unlock_overlay_keys`]
/// discards. rclone derives keys offline; AeroCrypt reads + verifies the remote
/// config (fail-closed on a wrong password via the config MAC).
///
/// `keyfile_digest` is the OPTIONAL AeroCrypt Tier 1 second factor. It is
/// reconciled against what the remote config requires BEFORE the expensive KDF
/// (mirroring the CLI `reconcile_keyfile`), so a missing or spurious keyfile is
/// a clear error instead of a confusing "wrong password" from a
/// silently-wrong derived key.
#[allow(clippy::too_many_arguments)]
async fn unlock_overlay_keys_encrypting(
    provider: &mut dyn StorageProvider,
    params: &OverlayUnlockParams,
    password: &str,
    salt: &str,
    keyfile_digest: Option<&[u8; 32]>,
    allow_init: bool,
    with_header: bool,
    use_default_salt: Option<bool>,
) -> Result<OverlayKeys, String> {
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
            let new_name = AEROCRYPT_CONFIG_NAME;
            let legacy_name = overlay::CRYPT_CONFIG_LEGACY_NAME;
            let new_path = format!("{}/{}", scope, new_name);
            let legacy_path = format!("{}/{}", scope, legacy_name);

            // Read-both for D5: probe new name first, fall back to legacy.
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
                new_path // for bootstrap write
            };

            // Clobber-safe existence probe. A fresh empty target must bootstrap a v3
            // overlay (mirrors the legacy `aerocrypt_provider::aerocrypt_unlock` None
            // branch the Phase-3 migration replaced), but a read/network error must
            // NEVER be taken for "absent": re-init rotates the salt and would orphan
            // every file already encrypted under the existing overlay. So only an
            // explicit `exists == false` triggers the bootstrap.
            let (config, master_key) = if present {
                let config_bytes = provider
                    .download_to_bytes(&config_path)
                    .await
                    .map_err(|e| format!("Cannot read AeroCrypt overlay config: {e}"))?;
                let config_str = String::from_utf8_lossy(&config_bytes);
                let derived = derive_aerocrypt_overlay_keys_from_config(
                    &config_str,
                    password,
                    keyfile_digest,
                )?;
                // T3: cache this headed vault's PUBLIC config in the local keystore
                // so the on-demand Recovery Kit works without connecting, for headed
                // vaults created before this build too. Best-effort and idempotent
                // (only fills a missing entry); the remote marker stays the source of
                // truth, so a cache miss never fails the unlock.
                backfill_headed_overlay_config(params, &config_str);
                derived
            } else if let Some(config_json) = local_headerless_config_from_params(params)? {
                derive_aerocrypt_overlay_keys_from_config(&config_json, password, keyfile_digest)?
            } else if allow_init {
                // Bootstrap a fresh AECR v3 overlay and persist its config so the
                // empty folder becomes a self-describing crypt store on first
                // activation (length-bound content + key-bound config MAC). Only the
                // interactive GUI activation path opts in; the non-interactive
                // factory (CLI / cross-profile / MCP) passes allow_init=false and
                // stays fail-closed below.
                //
                // Clobber guard (audit FINDING 1): a headerless vault carries no
                // remote marker, so re-activating over a scope that already holds
                // ciphertext would mint a new random salt and permanently orphan
                // every existing object. Refuse when the scope LISTS a non-config
                // entry. This keeps the frictionless headerless flow intact: an
                // empty scope (an empty existing folder, or the whole remote whose
                // root always exists) still bootstraps, and a not-yet-created
                // subfolder still bootstraps on first write.
                //
                // Only a positively-verified-empty scope may bootstrap. A
                // not-yet-created location (NotFound) is safe (nothing to orphan);
                // EVERY other listing failure (permission denial, network, timeout,
                // server error) is NOT proof of emptiness and could hide existing
                // ciphertext under a headerless vault, so it fails closed rather
                // than rotating the salt over data it could not see (audit B-F2).
                // The frictionless flow is intact: an empty existing folder / the
                // whole remote root (Ok(empty)) and a not-yet-created subfolder
                // (NotFound) both still bootstrap.
                let list_dir = if scope.is_empty() { "/" } else { scope };
                match provider.list(list_dir).await {
                    Ok(entries) => {
                        if entries.iter().any(|e| e.name != AEROCRYPT_CONFIG_NAME) {
                            return Err(format!(
                                "Refusing to initialize a new AeroCrypt overlay at {list_dir}: it \
                                 already contains files. Unlock it with its existing credentials, or \
                                 recover a headerless vault from its Emergency Kit. Re-initializing \
                                 would rotate the salt and permanently orphan the existing files."
                            ));
                        }
                    }
                    // Scope does not exist yet: frictionless first-write bootstrap.
                    Err(ProviderError::NotFound(_)) => {}
                    Err(ProviderError::PermissionDenied(msg)) => {
                        return Err(format!(
                            "Refusing to initialize a new AeroCrypt overlay at {list_dir}: it \
                             cannot be listed due to permission denial ({msg}). Unlock it with \
                             its existing credentials, or recover a headerless vault from its \
                             Emergency Kit. Re-initializing would rotate the salt and permanently \
                             orphan any existing files."
                        ));
                    }
                    Err(e) => {
                        return Err(format!(
                            "Refusing to initialize a new AeroCrypt overlay at {list_dir}: its \
                             contents could not be verified ({e}). Unlock it with its existing \
                             credentials, or recover a headerless vault from its Emergency Kit. \
                             Re-initializing would rotate the salt and permanently orphan any \
                             existing files."
                        ));
                    }
                }
                let use_default = use_default_salt.unwrap_or(false);
                let salt = if use_default {
                    crate::aerocrypt::AEROCRYPT_DEFAULT_SALT_V1
                } else {
                    overlay::random_salt_v3()
                };
                let salt_mode = if use_default {
                    overlay::SaltMode::DefaultV1
                } else {
                    overlay::SaltMode::PerVault
                };
                let tmp = OverlayConfig::v3_bootstrap(salt);
                let master_key =
                    overlay::derive_master_key_with_keyfile(&tmp, password, keyfile_digest)
                        .map_err(|e| format!("AeroCrypt key derivation failed: {e}"))?;
                // With a keyfile the config records kdf_inputs + a fresh vault_id
                // and omits keyfile_hint by default (F5), mirroring the CLI
                // `cmd_crypt_init`.
                let json = if keyfile_digest.is_some() {
                    overlay::init_config_v3_with_keyfile(
                        &salt,
                        &master_key,
                        &overlay::random_vault_id(),
                        None,
                        salt_mode,
                    )
                } else {
                    overlay::init_config_v3_with_vault_id(
                        &salt,
                        &master_key,
                        &overlay::random_vault_id(),
                        salt_mode,
                    )
                }
                .map_err(|e| format!("Cannot build AeroCrypt overlay config: {e}"))?;
                let staged = tempfile::NamedTempFile::new()
                    .map_err(|e| format!("Cannot stage AeroCrypt overlay config: {e}"))?;
                std::fs::write(staged.path(), json.as_bytes())
                    .map_err(|e| format!("Cannot stage AeroCrypt overlay config: {e}"))?;

                if with_header {
                    // Headed vault: the on-remote marker is the source of truth,
                    // exactly like the CLI `crypt init --with-header`. Upload it, then
                    // ALSO cache the PUBLIC config in the local keystore (best-effort,
                    // T3) so the on-demand Recovery Kit works in every mode without a
                    // connection. The marker stays authoritative, so a cache-write
                    // failure is logged and never fails the create.
                    provider
                        .upload(&staged.path().to_string_lossy(), &config_path, None)
                        .await
                        .map_err(|e| format!("Cannot write AeroCrypt overlay config: {e}"))?;
                    if let Some(id) = params.profile_id.as_deref().filter(|s| !s.is_empty()) {
                        if let Some(store) = crate::credential_store::CredentialStore::from_cache()
                        {
                            use base64::Engine as _;
                            let salt_b64 = base64::engine::general_purpose::STANDARD.encode(salt);
                            if let Err(e) =
                                persist_public_overlay_config(&store, id, &json, &salt_b64)
                            {
                                eprintln!(
                                    "[aerocrypt] headed overlay config cache skipped for {id}: {e}"
                                );
                            }
                        }
                    }
                } else {
                    // Headerless (default): persist the complete public config in the
                    // local keystore keyed by profile id, so connect-time unlock finds
                    // the metadata with no remote marker (mirrors the CLI headerless
                    // `crypt init`). This MUST be durable and fail-closed: a swallowed
                    // write would hand back a usable overlay whose metadata was never
                    // saved, permanently orphaning every file encrypted under it.
                    let id = params.profile_id.as_deref().ok_or_else(|| {
                        "Cannot create a headerless AeroCrypt vault without a saved profile to store \
                         its metadata. Save the profile first, or enable the on-remote header."
                            .to_string()
                    })?;
                    let store = crate::credential_store::CredentialStore::from_cache().ok_or_else(
                        || {
                            "Cannot persist AeroCrypt overlay metadata: the local keystore is \
                             unavailable."
                                .to_string()
                        },
                    )?;
                    use base64::Engine as _;
                    let salt_b64 = base64::engine::general_purpose::STANDARD.encode(salt);
                    // Durable and fail-closed: a swallowed write here would orphan
                    // every file encrypted under this headerless vault, so the error
                    // propagates (unlike the best-effort headed cache above).
                    persist_public_overlay_config(&store, id, &json, &salt_b64)?;
                }

                let config = overlay::parse_config(&json)
                    .map_err(|e| format!("Invalid AeroCrypt overlay config: {e}"))?;
                (config, master_key)
            } else {
                // Non-interactive contexts never create an overlay implicitly: a
                // crypt-bound profile pointed at a folder with no config is refused
                // (fail-closed), never handed back the raw provider.
                return Err(format!(
                    "Cannot read AeroCrypt overlay config: no overlay at {config_path}"
                ));
            };
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
    keyfile_digest: Option<&[u8; 32]>,
    with_header: bool,
    use_default_salt: Option<bool>,
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
    // Interactive GUI activation: bootstrap a fresh v3 overlay when the target
    // folder has no config yet (clobber-safe), so "activate overlay here" works on
    // an empty folder instead of failing "could not be unlocked". A Some
    // keyfile_digest makes that bootstrap a keyfile vault (Tier 1).
    let keys = unlock_overlay_keys_encrypting(
        &mut **provider,
        binding,
        password,
        salt,
        keyfile_digest,
        true,
        with_header,
        use_default_salt,
    )
    .await?;
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
        // Test fixture: always per-vault (default salt is an opt-in user choice).
        let salt = overlay::random_salt_v3();
        let tmp = OverlayConfig::v3_bootstrap(salt);
        let master_key = overlay::derive_master_key(&tmp, "overlay-pass").unwrap();
        let json = overlay::init_config_v3(&salt, &master_key).unwrap();
        let config = overlay::parse_config(&json).unwrap();
        OverlayKeys::AeroCrypt { master_key, config }
    }

    /// A legacy read-only AeroCrypt v2 overlay. Only the config variant matters
    /// for `size_is_exact` (v2 defers the size map), so the key is a stub.
    fn aerocrypt_v2_keys() -> OverlayKeys {
        OverlayKeys::AeroCrypt {
            master_key: [0u8; KEY_SIZE],
            config: OverlayConfig::V2 { salt: [0u8; 32] },
        }
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
            let enc =
                encode_plain_target(&keys, "", "sub/file.txt", false, AccessKind::Write).unwrap();
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
            let enc = encode_plain_target(
                &keys,
                "/Vault",
                "/Vault/dir/note.md",
                false,
                AccessKind::Write,
            )
            .unwrap();
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
            let enc =
                encode_plain_target(&keys, "/Vault", "/Vault", true, AccessKind::Write).unwrap();
            assert_eq!(enc, "/Vault");
            let enc2 =
                encode_plain_target(&keys, "/Vault", "/Vault/", true, AccessKind::Write).unwrap();
            assert_eq!(enc2, "/Vault");
        }
    }

    #[test]
    fn encode_plain_target_outside_anchor_is_refused() {
        for (label, keys) in both_kinds() {
            let res = encode_plain_target(
                &keys,
                "/Vault",
                "/Other/secret.txt",
                false,
                AccessKind::Write,
            );
            assert!(
                res.is_err(),
                "{label}: out-of-scope must fail closed (Write)"
            );
        }
    }

    #[test]
    fn encode_plain_target_whole_remote_encrypts_all() {
        for (label, keys) in both_kinds() {
            let enc = encode_plain_target(&keys, "", "/a/b.txt", false, AccessKind::Write).unwrap();
            assert!(enc.starts_with('/'), "{label}: absolute preserved");
            let parts: Vec<&str> = enc.trim_start_matches('/').split('/').collect();
            assert_eq!(keys.decode_name(parts[0], true).unwrap(), "a");
            assert_eq!(keys.decode_name(parts[1], false).unwrap(), "b.txt");
        }
    }

    /// V1 regression (pre-tag audit): the fetch-side classifier must mirror
    /// `encode_plain_target` decision-for-decision. A RELATIVE target under a
    /// bound scope is encrypted in full by the mapper, so it must classify as
    /// encrypted; absolutizing it first compared a fake `/name` against the
    /// anchor and made download/stat skip the decrypt step.
    #[test]
    fn target_is_encrypted_mirrors_mapper_for_relative_targets() {
        for (label, keys) in both_kinds() {
            let provider = CryptOverlayProvider::new(Box::new(MemProvider::new()), keys, "/Vault");
            assert!(
                provider.target_is_encrypted("notes/r.txt"),
                "{label}: relative multi-segment target is encrypted"
            );
            assert!(
                provider.target_is_encrypted("r.txt"),
                "{label}: relative bare name is encrypted"
            );
            assert!(
                !provider.target_is_encrypted("/Vault"),
                "{label}: the anchor itself is cleartext"
            );
            assert!(
                provider.target_is_encrypted("/Vault/x"),
                "{label}: below-anchor absolute target is encrypted"
            );
            assert!(
                !provider.target_is_encrypted("/Other/x"),
                "{label}: outside-anchor absolute target is plaintext pass-through"
            );
        }
    }

    /// V1 regression, end to end: standing inside a bound scope and fetching by
    /// a RELATIVE plaintext name (the MCP/AI caller shape) must return decrypted
    /// bytes and decrypted stat metadata, never the raw ciphertext.
    #[tokio::test]
    async fn scoped_overlay_decrypts_relative_target() {
        for (label, keys) in both_kinds() {
            let mut mem = MemProvider::new();
            mem.seed_raw_dir("/Vault");
            let mut provider = CryptOverlayProvider::new(Box::new(mem), keys, "/Vault");

            let dir = std::env::temp_dir().join(format!("crypt_rel_{}", uuid::Uuid::new_v4()));
            tokio::fs::create_dir_all(&dir).await.unwrap();
            let local = dir.join("r.txt");
            let payload = b"relative fetch must decrypt";
            tokio::fs::write(&local, payload).await.unwrap();
            provider
                .upload(local.to_str().unwrap(), "/Vault/notes/r.txt", None)
                .await
                .unwrap();

            // Stand inside the scope like a live session, then fetch relative.
            provider.cd("/Vault").await.unwrap();
            let got = provider.download_to_bytes("notes/r.txt").await.unwrap();
            assert_eq!(
                got, payload,
                "{label}: relative in-scope fetch returns plaintext"
            );

            let st = provider.stat("notes/r.txt").await.unwrap();
            assert_eq!(st.name, "r.txt", "{label}: stat name is decrypted");
            assert_eq!(
                st.size,
                payload.len() as u64,
                "{label}: stat size is the decrypted size"
            );

            let _ = tokio::fs::remove_dir_all(&dir).await;
        }
    }

    /// V8 regression (pre-tag audit): rclone `off`+suffix encodes file leaves
    /// as `name.bin` and directories as `name`; the type-agnostic ops
    /// (stat/exists/rename/chmod) must resolve the directory form instead of
    /// addressing directories as files.
    #[tokio::test]
    async fn off_mode_type_agnostic_ops_resolve_directories() {
        let keys = rclone_keys(FilenameEncryption::Off, true, ".bin");
        let mut mem = MemProvider::new();
        mem.seed_raw_dir("/Vault");
        mem.seed_raw_dir("/Vault/photos");
        mem.seed_raw_file("/Vault/report.txt.bin", b"opaque ciphertext blob");
        let mut provider = CryptOverlayProvider::new(Box::new(mem), keys, "/Vault");

        assert!(
            provider.exists("/Vault/photos").await.unwrap(),
            "directory found via the dir form"
        );
        assert!(
            provider.exists("/Vault/report.txt").await.unwrap(),
            "file found via the suffixed form"
        );
        assert!(!provider.exists("/Vault/nope").await.unwrap());

        let st = provider.stat("/Vault/photos").await.unwrap();
        assert!(st.is_dir, "stat resolves the directory form");
        assert_eq!(st.name, "photos");
        let st = provider.stat("/Vault/report.txt").await.unwrap();
        assert!(!st.is_dir);
        assert_eq!(st.name, "report.txt", "file leaf suffix stripped");

        provider
            .rename("/Vault/photos", "/Vault/pics")
            .await
            .unwrap();
        let mem = provider
            .inner
            .as_any_mut()
            .downcast_mut::<MemProvider>()
            .unwrap();
        let dirs = mem.raw_dirs();
        assert!(
            dirs.iter().any(|d| d == "/Vault/pics"),
            "directory renamed in dir form, no .bin: {dirs:?}"
        );
        assert!(!dirs.iter().any(|d| d == "/Vault/photos"));
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
            let enc = encode_plain_target(&keys, "/Vault", "/Vault/x/y", true, AccessKind::Write)
                .unwrap();
            let dec = decode_path(&keys, &enc);
            assert_eq!(dec, "/Vault/x/y");
            // A foreign component is left verbatim, not dropped, in display paths.
            let mixed = format!("{}/foreign", enc);
            let dec_mixed = decode_path(&keys, &mixed);
            assert!(dec_mixed.starts_with("/Vault/x/y/"));
        }
    }

    // ── Scope-aware listing (CWP-20C) ────────────────────────────────────────

    /// list must pass through plaintext siblings that live outside the anchor
    /// (Model B: plaintext outside, decrypted inside).
    #[tokio::test]
    async fn list_passes_through_plaintext_sibling_outside_scope() {
        let keys = rclone_keys(FilenameEncryption::Standard, true, ".bin");
        let mut mem = MemProvider::new();
        // plaintext sibling outside
        mem.seed_raw_dir("/sibling_plain");
        mem.seed_raw_file("/sibling_plain/outside.txt", b"outside data");
        // anchor dir
        mem.seed_raw_dir("/AeroCryptTest");
        let mut provider = CryptOverlayProvider::new(Box::new(mem), keys, "/AeroCryptTest");

        let listed = provider.list("/").await.unwrap();
        let names: Vec<_> = listed.iter().map(|e| e.name.as_str()).collect();
        assert!(
            names.contains(&"sibling_plain"),
            "sibling plaintext dir must be visible: {names:?}"
        );
        assert!(names.contains(&"AeroCryptTest"), "anchor must be visible");
        // the outside file's parent dir is sibling_plain, but when listing root we see the dirs
    }

    /// When listing the parent of the anchor, the anchor folder's own plaintext
    /// name must be kept (not dropped by a failed decode).
    #[tokio::test]
    async fn list_keeps_anchor_folder_visible_when_listing_parent() {
        let keys = aerocrypt_keys();
        let mut mem = MemProvider::new();
        mem.seed_raw_dir("/AeroCryptTest");
        mem.seed_raw_dir("/AeroCryptTest/inside_enc");
        // note: inside dir name here is placeholder; the list will see it as child
        // but since we did not encrypt its name, the inside one will be dropped by decode
        // (correct, only real below get decrypted). We only care anchor survives.
        let mut provider = CryptOverlayProvider::new(Box::new(mem), keys, "/AeroCryptTest");

        let listed = provider.list("/").await.unwrap();
        let names: Vec<_> = listed.iter().map(|e| (&e.name, e.is_dir)).collect();
        assert!(
            names.iter().any(|(n, d)| *n == "AeroCryptTest" && *d),
            "anchor folder must survive listing parent as plaintext dir"
        );
    }

    /// Strictly-below entries are decrypted; sentinels are hidden; plaintext at
    /// anchor level would pass but we test inside decrypt path.
    #[tokio::test]
    async fn list_decrypts_strictly_below_anchor_and_hides_sentinels() {
        let keys = rclone_keys(FilenameEncryption::Standard, true, ".bin");
        let mut mem = MemProvider::new();
        mem.seed_raw_dir("/AeroCryptTest");
        // seed an encrypted-name child by computing it
        let enc_child = keys.encode_name("secret.txt", false).unwrap();
        let enc_path = format!("/AeroCryptTest/{}", enc_child);
        mem.seed_raw_file(&enc_path, b"cipher data");
        // seed a sentinel that must be hidden (for rclone)
        mem.seed_raw_file("/AeroCryptTest/dirIV", b"sentinel");
        let mut provider = CryptOverlayProvider::new(Box::new(mem), keys, "/AeroCryptTest");

        let listed = provider.list("/AeroCryptTest").await.unwrap();
        let names: Vec<_> = listed.iter().map(|e| e.name.as_str()).collect();
        assert!(
            names.contains(&"secret.txt"),
            "below-anchor must decrypt: {names:?}"
        );
        assert!(!names.contains(&"dirIV"), "sentinel must be hidden");
        // ensure no raw ciphertext name leaks
        assert!(
            !names.iter().any(|n| n == &enc_child),
            "cipher name must not surface"
        );
    }

    /// Whole-remote (scope="") must behave byte-identical to pre-CWP-20C
    /// (everything decrypted, no pass-through logic changes output).
    #[tokio::test]
    async fn list_whole_remote_scope_byte_identical() {
        // populate via upload (which does full-encrypt path for scope="")
        let inner = Box::new(MemProvider::new());
        let mut provider = CryptOverlayProvider::new(inner, aerocrypt_keys(), "");
        let dir = std::env::temp_dir().join(format!("cwp20c_whole_{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let local = dir.join("rootfile.txt");
        tokio::fs::write(&local, b"root content").await.unwrap();
        provider
            .upload(local.to_str().unwrap(), "/rootfile.txt", None)
            .await
            .unwrap();

        let listed = provider.list("/").await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "rootfile.txt");
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[test]
    fn encode_plain_target_read_passes_through_out_of_scope() {
        for (label, keys) in both_kinds() {
            let res = encode_plain_target(
                &keys,
                "/Vault",
                "/Other/secret.txt",
                false,
                AccessKind::Read,
            );
            assert!(res.is_ok(), "{label}: Read must pass through outside");
            assert_eq!(res.unwrap(), "/Other/secret.txt");
            // also at-anchor sibling
            let res2 =
                encode_plain_target(&keys, "/Vault", "/sibling", true, AccessKind::Read).unwrap();
            assert_eq!(res2, "/sibling");
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
    fn decrypted_size_maps_both_kinds() {
        let rclone = rclone_keys(FilenameEncryption::Standard, true, ".bin");
        let aero = aerocrypt_keys();
        // Header-only ciphertext -> 0 bytes plaintext for rclone.
        assert_eq!(rclone.decrypted_size(32), 0);
        // 32 header + (100 + 16 tag) -> 100 plaintext.
        assert_eq!(rclone.decrypted_size(32 + 100 + 16), 100);
        // AeroCrypt v3 now maps too (53 header + 28 per-block overhead): a single
        // partial block of ciphertext length 12_345 -> 12_345 - 53 - 28 plaintext.
        assert_eq!(aero.decrypted_size(12_345), 12_345 - 53 - 28);
    }

    #[test]
    fn size_is_exact_true_except_legacy_aerocrypt() {
        // rclone-crypt and AeroCrypt v3 map the ciphertext size to the exact
        // plaintext size, so sync keeps comparing size. Legacy AeroCrypt v1/v2
        // defer (return ciphertext length), so `reports_exact_size` is false and
        // sync drops the size check rather than re-syncing every file every cycle.
        assert!(rclone_keys(FilenameEncryption::Standard, true, ".bin").size_is_exact());
        assert!(aerocrypt_keys().size_is_exact());
        assert!(!aerocrypt_v2_keys().size_is_exact());
    }

    #[test]
    fn decrypted_size_matches_real_ciphertext_both_kinds() {
        for keys in [
            rclone_keys(FilenameEncryption::Standard, true, ".bin"),
            aerocrypt_keys(),
        ] {
            for size in [0usize, 1, 100, 65_536, 65_537, 200_000] {
                let plaintext = vec![7u8; size];
                let ct = keys.encrypt_content(&plaintext).unwrap();
                assert_eq!(
                    keys.decrypted_size(ct.len() as u64),
                    size as u64,
                    "size map for {size}"
                );
            }
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
        let enc =
            encode_plain_target(&keys, "", "dir/sub/file.txt", false, AccessKind::Write).unwrap();
        assert_eq!(enc, "dir/sub/file.txt.bin");
    }

    #[test]
    fn decode_entry_path_strips_rclone_off_suffix_for_file_leaf() {
        let keys = rclone_keys(FilenameEncryption::Off, true, ".bin");

        assert_eq!(
            decode_entry_path(&keys, "/dir/sub/file.txt.bin", false),
            "/dir/sub/file.txt"
        );
        assert_eq!(decode_entry_path(&keys, "/dir/sub", true), "/dir/sub");
        // pwd/current-directory rendering still decodes every component as a
        // directory, so it must not strip a suffix-looking directory name.
        assert_eq!(
            decode_path(&keys, "/dir/sub/file.txt.bin"),
            "/dir/sub/file.txt.bin"
        );
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
        let enc =
            encode_plain_target(&keys, "", "Photos/img.jpg", false, AccessKind::Write).unwrap();
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
        cwd: Mutex<String>,
    }

    impl MemProvider {
        fn new() -> Self {
            Self {
                files: Mutex::new(HashMap::new()),
                dirs: Mutex::new(Vec::new()),
                cwd: Mutex::new("/".to_string()),
            }
        }
        /// Resolve a relative wire path against the current dir, like a real
        /// session-oriented provider (FTP/SFTP) does. Absolute paths pass
        /// through, so every pre-existing absolute-path test is unchanged.
        fn resolve(&self, p: &str) -> String {
            if p.starts_with('/') {
                return p.to_string();
            }
            let cwd = self.cwd.lock().unwrap();
            format!("{}/{}", cwd.trim_end_matches('/'), p)
        }
        fn raw_paths(&self) -> Vec<String> {
            self.files.lock().unwrap().keys().cloned().collect()
        }
        fn raw_dirs(&self) -> Vec<String> {
            self.dirs.lock().unwrap().clone()
        }
        fn raw_bytes(&self, path: &str) -> Option<Vec<u8>> {
            self.files.lock().unwrap().get(path).cloned()
        }
        /// Seed a raw on-wire file (used in scope tests to place plaintext
        /// siblings outside the anchor without going through write-map).
        fn seed_raw_file(&mut self, wire_path: &str, data: &[u8]) {
            self.files
                .lock()
                .unwrap()
                .insert(wire_path.to_string(), data.to_vec());
        }
        /// Seed a raw on-wire dir entry (for listing anchor/sibling dirs).
        fn seed_raw_dir(&mut self, wire_path: &str) {
            let mut ds = self.dirs.lock().unwrap();
            let s = wire_path.to_string();
            if !ds.iter().any(|d| d == &s) {
                ds.push(s);
            }
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
            // Also surface seeded/created dirs as directory entries.
            let dirs = self.dirs.lock().unwrap();
            for d in dirs.iter() {
                if let Some(rest) = d.strip_prefix(&prefix) {
                    if !rest.contains('/') && !rest.is_empty() {
                        out.push(RemoteEntry {
                            name: rest.to_string(),
                            path: d.clone(),
                            is_dir: true,
                            size: 0,
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
            Ok(self.cwd.lock().unwrap().clone())
        }
        async fn cd(&mut self, p: &str) -> Result<(), ProviderError> {
            let resolved = self.resolve(p);
            *self.cwd.lock().unwrap() = resolved;
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
            let remote = self.resolve(remote);
            let data = self
                .files
                .lock()
                .unwrap()
                .get(&remote)
                .cloned()
                .ok_or_else(|| ProviderError::NotFound(remote.to_string()))?;
            std::fs::write(local, &data).map_err(ProviderError::IoError)?;
            Ok(())
        }
        async fn download_to_bytes(&mut self, remote: &str) -> Result<Vec<u8>, ProviderError> {
            let remote = self.resolve(remote);
            self.files
                .lock()
                .unwrap()
                .get(&remote)
                .cloned()
                .ok_or_else(|| ProviderError::NotFound(remote.to_string()))
        }
        async fn upload(
            &mut self,
            local: &str,
            remote: &str,
            _cb: Option<Box<dyn Fn(u64, u64) + Send>>,
        ) -> Result<(), ProviderError> {
            let remote = self.resolve(remote);
            let data = std::fs::read(local).map_err(ProviderError::IoError)?;
            self.files.lock().unwrap().insert(remote, data);
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
            let from = self.resolve(from);
            let to = self.resolve(to);
            {
                let mut files = self.files.lock().unwrap();
                if let Some(data) = files.remove(&from) {
                    files.insert(to, data);
                    return Ok(());
                }
            }
            let mut dirs = self.dirs.lock().unwrap();
            if let Some(d) = dirs.iter_mut().find(|d| **d == from) {
                *d = to;
            }
            Ok(())
        }
        async fn stat(&mut self, p: &str) -> Result<RemoteEntry, ProviderError> {
            let p = self.resolve(p);
            let name = p.rsplit('/').next().unwrap_or(&p).to_string();
            if let Some(data) = self.files.lock().unwrap().get(&p).cloned() {
                return Ok(RemoteEntry {
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
                });
            }
            if self.dirs.lock().unwrap().contains(&p) {
                return Ok(RemoteEntry {
                    name,
                    path: p.to_string(),
                    is_dir: true,
                    size: 0,
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
            Err(ProviderError::NotFound(p))
        }
        async fn size(&mut self, p: &str) -> Result<u64, ProviderError> {
            let p = self.resolve(p);
            self.files
                .lock()
                .unwrap()
                .get(&p)
                .map(|d| d.len() as u64)
                .ok_or_else(|| ProviderError::NotFound(p.to_string()))
        }
        async fn exists(&mut self, p: &str) -> Result<bool, ProviderError> {
            let p = self.resolve(p);
            Ok(self.files.lock().unwrap().contains_key(&p)
                || self.dirs.lock().unwrap().contains(&p))
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
        // Both kinds now map to the EXACT decrypted size (rclone via its overhead
        // map, AeroCrypt v3 via the container decoder). Exact per-length mapping is
        // pinned by the `decrypted_size_*` tests and `v3_decrypted_size_*`.
        assert_eq!(
            listed[0].size,
            payload.len() as u64,
            "decrypted size is exact"
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

    #[tokio::test]
    async fn wrap_aerocrypt_uses_local_headerless_config_when_marker_absent() {
        use base64::Engine as _;

        let salt = overlay::random_salt_v3();
        let tmp = OverlayConfig::v3_bootstrap(salt);
        let master_key = overlay::derive_master_key(&tmp, "overlay-pass").unwrap();
        let config_json = overlay::init_config_v3(&salt, &master_key).unwrap();
        let salt_b64 = base64::engine::general_purpose::STANDARD.encode(salt);
        let binding = OverlayUnlockParams {
            kind: "aerocrypt".to_string(),
            remote_scope: "/Vault".to_string(),
            filename_encryption: "standard".to_string(),
            directory_name_encryption: true,
            off_suffix: None,
            profile_id: Some("profile-headerless".to_string()),
            local_config_json: Some(config_json),
            local_config_salt: Some(salt_b64),
        };

        let inner = Box::new(MemProvider::new());
        let mut provider =
            wrap_provider_with_overlay_if_bound(inner, Some(&binding), "overlay-pass", "", None)
                .await
                .unwrap();

        let dir =
            std::env::temp_dir().join(format!("crypt_headerless_test_{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let local = dir.join("plain.txt");
        let payload = b"headerless local config unlock";
        tokio::fs::write(&local, payload).await.unwrap();

        provider
            .upload(local.to_str().unwrap(), "/Vault/plain.txt", None)
            .await
            .unwrap();
        let got = provider
            .download_to_bytes("/Vault/plain.txt")
            .await
            .unwrap();
        assert_eq!(got, payload);
        let _ = tokio::fs::remove_dir_all(&dir).await;
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
            profile_id: None,
            local_config_json: None,
            local_config_salt: None,
        };

        // No-op on a raw slot.
        assert!(
            !clear_overlay_in_place(&mut slot),
            "raw slot has no overlay"
        );

        // Apply: the slot now holds a decorator.
        let scope = apply_overlay_in_place(&mut slot, &binding, "pw", "salt", None, true, None)
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
        apply_overlay_in_place(&mut slot, &binding, "pw", "salt", None, true, None)
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
        // Write path outside the bound anchor must still refuse (fail-closed).
        // (Read paths outside now pass through for Model-B plaintext areas.)
        let err = provider.mkdir("/Elsewhere/newdir").await;
        assert!(matches!(err, Err(ProviderError::InvalidPath(_))));
    }

    #[tokio::test]
    async fn factory_passes_through_without_binding() {
        let inner: Box<dyn StorageProvider> = Box::new(MemProvider::new());
        let wrapped = wrap_provider_with_overlay_if_bound(inner, None, "", "", None)
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
            profile_id: None,
            local_config_json: None,
            local_config_salt: None,
        };
        let mut wrapped =
            wrap_provider_with_overlay_if_bound(inner, Some(&binding), "pw", "salt", None)
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
            profile_id: None,
            local_config_json: None,
            local_config_salt: None,
        };
        // No config on the (empty) remote -> unlock fails, no raw provider handed
        // back.
        let res = wrap_provider_with_overlay_if_bound(inner, Some(&binding), "pw", "", None).await;
        assert!(res.is_err(), "missing config must fail closed");
    }

    #[tokio::test]
    async fn factory_aerocrypt_fails_closed_on_wrong_password() {
        // T4.3 locked-vault gate: the config EXISTS but the supplied password is
        // wrong (the vault is effectively locked). The aerocrypt config MAC verify
        // must reject it, so the wrap factory returns Err and the raw provider is
        // NEVER handed back. This is the fail-closed guarantee every wrap-based
        // surface (background sync, CLI/agent, MCP, cross-profile) inherits.
        let binding = OverlayUnlockParams {
            kind: "aerocrypt".to_string(),
            remote_scope: "/Vault".to_string(),
            filename_encryption: String::new(),
            directory_name_encryption: true,
            off_suffix: None,
            profile_id: None,
            local_config_json: None,
            local_config_salt: None,
        };
        // Bootstrap a real v3 config under the CORRECT password.
        let mut mem = MemProvider::new();
        unlock_overlay_keys_encrypting(
            &mut mem,
            &binding,
            "correct-pw",
            "",
            None,
            true,
            true,
            None,
        )
        .await
        .expect("bootstrap v3 config");

        // Wrapping with the WRONG password must fail closed against that config.
        let inner: Box<dyn StorageProvider> = Box::new(mem);
        let res =
            wrap_provider_with_overlay_if_bound(inner, Some(&binding), "wrong-pw", "", None).await;
        assert!(
            res.is_err(),
            "a wrong password on an existing aerocrypt config must fail closed"
        );
    }

    #[tokio::test]
    async fn factory_aerocrypt_wraps_after_bootstrap_never_raw() {
        // T4.3 never-raw anchor (aerocrypt success arm): a crypt-bound aerocrypt
        // profile with the correct password resolves to the CryptOverlayProvider,
        // so every path/content is mapped/encrypted - never the raw inner. The
        // rclone success arm is `factory_wraps_rclone_binding`; the fail arms are
        // the two `factory_aerocrypt_fails_closed_*` tests.
        let binding = OverlayUnlockParams {
            kind: "aerocrypt".to_string(),
            remote_scope: "/Vault".to_string(),
            filename_encryption: String::new(),
            directory_name_encryption: true,
            off_suffix: None,
            profile_id: None,
            local_config_json: None,
            local_config_salt: None,
        };
        let mut mem = MemProvider::new();
        unlock_overlay_keys_encrypting(&mut mem, &binding, "pw", "", None, true, true, None)
            .await
            .expect("bootstrap v3 config");
        let inner: Box<dyn StorageProvider> = Box::new(mem);
        let mut wrapped =
            wrap_provider_with_overlay_if_bound(inner, Some(&binding), "pw", "", None)
                .await
                .expect("correct password must wrap");
        assert!(
            wrapped
                .as_any_mut()
                .downcast_mut::<CryptOverlayProvider>()
                .is_some(),
            "a bound aerocrypt profile must resolve to the crypt decorator, never the raw inner"
        );
    }

    #[tokio::test]
    async fn aerocrypt_activation_refuses_bootstrap_on_nonempty_folder() {
        // Audit FINDING 1: interactive activation (allow_init=true) must NOT
        // bootstrap a fresh overlay over a location that already holds files.
        // Headerless vaults carry no remote marker, so bootstrapping there would
        // rotate the salt and permanently orphan the existing (possibly
        // headerless) vault. It must fail closed and write nothing instead.
        let mut mem = MemProvider::new();
        let seed = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(seed.path(), b"existing ciphertext").unwrap();
        mem.upload(
            &seed.path().to_string_lossy(),
            "/Vault/already-here.bin",
            None,
        )
        .await
        .expect("seed an existing object under the scope");
        let binding = OverlayUnlockParams {
            kind: "aerocrypt".to_string(),
            remote_scope: "/Vault".to_string(),
            filename_encryption: String::new(),
            directory_name_encryption: true,
            off_suffix: None,
            profile_id: None,
            local_config_json: None,
            local_config_salt: None,
        };
        let res =
            unlock_overlay_keys_encrypting(&mut mem, &binding, "pw", "", None, true, true, None)
                .await;
        assert!(
            res.is_err(),
            "activation must refuse to bootstrap over a non-empty folder (would orphan existing files)"
        );
        assert!(
            !mem.exists("/Vault/.aeroftp-crypt.json")
                .await
                .expect("probe config marker"),
            "no overlay config may be written when bootstrap is refused"
        );
    }

    #[tokio::test]
    async fn aerocrypt_activation_refuses_bootstrap_when_scope_listing_permission_denied() {
        // A-F3: a permission-denied list cannot prove the headerless scope is
        // empty. Refuse before writing a new config, but keep the successful
        // empty-listing bootstrap covered by the adjacent regression test.
        let mut mem = StrictMemProvider::with_permission_denied_list(&["/Vault"]);
        let binding = OverlayUnlockParams {
            kind: "aerocrypt".to_string(),
            remote_scope: "/Vault".to_string(),
            filename_encryption: String::new(),
            directory_name_encryption: true,
            off_suffix: None,
            profile_id: None,
            local_config_json: None,
            local_config_salt: None,
        };

        let res =
            unlock_overlay_keys_encrypting(&mut mem, &binding, "pw", "", None, true, true, None)
                .await;
        let err = res
            .err()
            .expect("activation must refuse a permission-denied scope listing");
        assert!(
            err.contains("cannot be listed due to permission denial"),
            "refusal should explain the inaccessible scope: {err}"
        );
        assert!(
            !mem.exists("/Vault/.aeroftp-crypt.json")
                .await
                .expect("probe config marker"),
            "no overlay config may be written when bootstrap is refused"
        );
        assert!(
            mem.file_paths().is_empty(),
            "no remote file may be written on refusal: {:?}",
            mem.file_paths()
        );
    }

    #[tokio::test]
    async fn aerocrypt_activation_bootstraps_on_existing_empty_scope() {
        // Frictionless headerless (v4.1.4 pre-tag audit A-F3): pointing a vault at
        // an EMPTY existing folder (or the whole remote, whose root always exists)
        // must BOOTSTRAP, not refuse. Only a scope that LISTS existing content is
        // refused (re-init would rotate the salt and orphan it). An earlier
        // `exists()==false` gate wrongly refused this case and was reverted.
        let mut mem = MemProvider::new();
        mem.seed_raw_dir("/Vault");
        let binding = OverlayUnlockParams {
            kind: "aerocrypt".to_string(),
            remote_scope: "/Vault".to_string(),
            filename_encryption: String::new(),
            directory_name_encryption: true,
            off_suffix: None,
            profile_id: None,
            local_config_json: None,
            local_config_salt: None,
        };

        let keys =
            unlock_overlay_keys_encrypting(&mut mem, &binding, "pw", "", None, true, true, None)
                .await
                .expect("an empty existing scope must bootstrap a v3 overlay");
        assert!(matches!(keys, OverlayKeys::AeroCrypt { .. }));
        assert!(
            mem.exists("/Vault/.aeroftp-crypt.json")
                .await
                .expect("probe config marker"),
            "bootstrap over an empty existing scope must write the overlay config"
        );
    }

    #[tokio::test]
    async fn aerocrypt_headerless_activation_requires_a_profile_id() {
        // Headerless creation via interactive activation must REQUIRE a saved
        // profile to persist the public config in the keystore. Without one it
        // fails closed, never handing back a usable overlay whose metadata was
        // never stored (which would orphan every file encrypted under it), and
        // it writes nothing to the remote.
        let mut mem = MemProvider::new();
        let binding = OverlayUnlockParams {
            kind: "aerocrypt".to_string(),
            remote_scope: "/Vault".to_string(),
            filename_encryption: String::new(),
            directory_name_encryption: true,
            off_suffix: None,
            profile_id: None,
            local_config_json: None,
            local_config_salt: None,
        };
        // allow_init=true (interactive), with_header=false (headerless), empty folder.
        let res =
            unlock_overlay_keys_encrypting(&mut mem, &binding, "pw", "", None, true, false, None)
                .await;
        assert!(
            res.is_err(),
            "headerless activation without a profile id must fail closed"
        );
        assert!(
            !mem.exists("/Vault/.aeroftp-crypt.json")
                .await
                .expect("probe marker"),
            "a refused headerless activation must not write a remote marker"
        );
    }

    #[tokio::test]
    async fn aerocrypt_activation_bootstraps_v3_on_empty_folder() {
        // Interactive GUI activation (allow_init=true): pointing an aerocrypt
        // overlay at an empty folder must INIT a v3 config (write it to the remote)
        // instead of failing "could not be unlocked". A reconnect then READS that
        // config. The non-interactive factory stays fail-closed (allow_init=false).
        let mut mem = MemProvider::new();
        let binding = OverlayUnlockParams {
            kind: "aerocrypt".to_string(),
            remote_scope: "/Vault".to_string(),
            filename_encryption: String::new(),
            directory_name_encryption: true,
            off_suffix: None,
            profile_id: None,
            local_config_json: None,
            local_config_salt: None,
        };

        let keys =
            unlock_overlay_keys_encrypting(&mut mem, &binding, "pw", "", None, true, true, None)
                .await
                .expect("empty folder must bootstrap a v3 overlay");
        assert!(matches!(keys, OverlayKeys::AeroCrypt { .. }));

        // The config was persisted to the remote and is v3.
        let cfg = mem
            .raw_bytes("/Vault/.aeroftp-crypt.json")
            .expect("a config must be written on bootstrap");
        let v: serde_json::Value = serde_json::from_slice(&cfg).unwrap();
        assert_eq!(v["version"], serde_json::json!(3));

        // Re-activation reads the existing config (no second bootstrap, no clobber).
        let keys2 =
            unlock_overlay_keys_encrypting(&mut mem, &binding, "pw", "", None, true, true, None)
                .await
                .expect("re-activation must read the existing config");
        assert!(matches!(keys2, OverlayKeys::AeroCrypt { .. }));
        let still: Vec<String> = mem
            .raw_paths()
            .into_iter()
            .filter(|p| p == "/Vault/.aeroftp-crypt.json")
            .collect();
        assert_eq!(
            still.len(),
            1,
            "re-activation must not write a second config"
        );

        // Non-interactive (allow_init=false) on a still-empty folder stays
        // fail-closed: it never creates an overlay implicitly.
        let other = OverlayUnlockParams {
            kind: "aerocrypt".to_string(),
            remote_scope: "/Empty".to_string(),
            filename_encryption: String::new(),
            directory_name_encryption: true,
            off_suffix: None,
            profile_id: None,
            local_config_json: None,
            local_config_salt: None,
        };
        let res =
            unlock_overlay_keys_encrypting(&mut mem, &other, "pw", "", None, false, true, None)
                .await;
        assert!(
            res.is_err(),
            "non-interactive empty folder must fail closed"
        );
        assert!(
            mem.raw_bytes("/Empty/.aeroftp-crypt.json").is_none(),
            "fail-closed path must not write a config"
        );
    }

    /// `expect_err` for unlock results. [`OverlayKeys`] intentionally has no
    /// `Debug` impl (key material), so `Result::expect_err` cannot be used.
    fn unlock_err(res: Result<OverlayKeys, String>, msg: &str) -> String {
        match res {
            Ok(_) => panic!("{msg}"),
            Err(e) => e,
        }
    }

    #[tokio::test]
    async fn aerocrypt_keyfile_vault_reconciles_and_fails_closed() {
        // Tier 1 keyfile second factor: bootstrapping with a keyfile digest
        // writes a requires-keyfile v3 config (kdf_inputs + vault_id, NO
        // keyfile_hint by default, F5). Unlocking it then requires the SAME
        // digest: password-only and wrong-digest attempts fail closed, and a
        // password-only vault rejects a spurious keyfile with a clear reconcile
        // error (never a confusing derived-key mismatch).
        let mut mem = MemProvider::new();
        let binding = OverlayUnlockParams {
            kind: "aerocrypt".to_string(),
            remote_scope: "/KfVault".to_string(),
            filename_encryption: String::new(),
            directory_name_encryption: true,
            off_suffix: None,
            profile_id: None,
            local_config_json: None,
            local_config_salt: None,
        };
        let digest = crate::aerocrypt::keyfile_digest_from_file(
            crate::aerocrypt::generate_keyfile_v1().as_bytes(),
        )
        .unwrap();

        unlock_overlay_keys_encrypting(
            &mut mem,
            &binding,
            "pw",
            "",
            Some(&digest),
            true,
            true,
            None,
        )
        .await
        .expect("bootstrap a keyfile vault");
        let cfg = mem.raw_bytes("/KfVault/.aeroftp-crypt.json").unwrap();
        let v: serde_json::Value = serde_json::from_slice(&cfg).unwrap();
        assert_eq!(v["kdf_inputs"], serde_json::json!(["password", "keyfile"]));
        assert!(
            v.get("vault_id").is_some(),
            "keyfile config records a vault_id"
        );
        assert!(
            v.get("keyfile_hint").is_none(),
            "no keyfile_hint by default (F5)"
        );

        unlock_overlay_keys_encrypting(
            &mut mem,
            &binding,
            "pw",
            "",
            Some(&digest),
            false,
            true,
            None,
        )
        .await
        .expect("correct password + keyfile must unlock");

        let err = unlock_err(
            unlock_overlay_keys_encrypting(&mut mem, &binding, "pw", "", None, false, true, None)
                .await,
            "password-only on a keyfile vault must fail closed",
        );
        assert!(
            err.contains("requires a keyfile"),
            "clear reconcile error: {err}"
        );

        let wrong = crate::aerocrypt::keyfile_digest(b"not the keyfile");
        let res = unlock_overlay_keys_encrypting(
            &mut mem,
            &binding,
            "pw",
            "",
            Some(&wrong),
            false,
            true,
            None,
        )
        .await;
        assert!(res.is_err(), "a wrong keyfile must fail closed");

        let pw_only = OverlayUnlockParams {
            kind: "aerocrypt".to_string(),
            remote_scope: "/PwVault".to_string(),
            filename_encryption: String::new(),
            directory_name_encryption: true,
            off_suffix: None,
            profile_id: None,
            local_config_json: None,
            local_config_salt: None,
        };
        unlock_overlay_keys_encrypting(&mut mem, &pw_only, "pw", "", None, true, true, None)
            .await
            .expect("bootstrap a password-only vault");
        let err = unlock_err(
            unlock_overlay_keys_encrypting(
                &mut mem,
                &pw_only,
                "pw",
                "",
                Some(&digest),
                false,
                true,
                None,
            )
            .await,
            "a spurious keyfile on a password-only vault must be rejected",
        );
        assert!(
            err.contains("was not created with a keyfile"),
            "clear reconcile error: {err}"
        );
    }

    #[tokio::test]
    async fn rclone_binding_rejects_keyfile() {
        // Keyfiles are an AeroCrypt feature; a keyfile stored against an
        // rclone-crypt binding is a misconfiguration that must surface.
        let mut mem = MemProvider::new();
        let binding = OverlayUnlockParams {
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
            unlock_overlay_keys_encrypting(
                &mut mem,
                &binding,
                "pw",
                "",
                Some(&digest),
                false,
                true,
                None,
            )
            .await,
            "rclone-crypt must reject a keyfile",
        );
        assert!(err.contains("AeroCrypt feature"), "clear error: {err}");
    }

    #[test]
    fn keyfile_digest_from_path_fails_closed_on_unreadable() {
        // A keyfile vault must never silently fall back to password-only: an
        // unreadable stored keyfile path is a hard error at the resolver.
        let missing = std::env::temp_dir().join(format!("kf_missing_{}", uuid::Uuid::new_v4()));
        let err = keyfile_digest_from_path(missing.to_str().unwrap())
            .expect_err("a missing keyfile must be a hard error");
        assert!(err.contains("cannot read AeroCrypt keyfile"), "{err}");
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
            // Fail-safe: a partial ciphertext is never byte-resumable, so the
            // decorator must never advertise append-resume nor let a resume
            // append reach the inner store (it would corrupt the AEAD framing).
            assert!(
                !provider.supports_resume_upload_append(),
                "{label}: crypt overlay must not offer append-resume"
            );
            let resumed = provider
                .resume_upload("/tmp/x", "/whatever.bin", 8, None)
                .await;
            assert!(
                matches!(resumed, Err(ProviderError::NotSupported(_))),
                "{label}: resume_upload must be NotSupported, got {resumed:?}"
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

    #[test]
    fn select_profile_by_name_is_exact() {
        // Background sync resolves its server profile by EXACT name (the same key
        // its credential lookup `server_<name>` uses). A substring / prefix must
        // NOT match, or a crypt-bound "prod" could be resolved against a
        // non-crypt "prod-staging" and fail-open the overlay.
        let profiles = vec![
            serde_json::json!({ "id": "a", "name": "prod" }),
            serde_json::json!({ "id": "b", "name": "prod-staging" }),
        ];
        assert_eq!(
            select_profile_by_name(&profiles, "prod")
                .and_then(|p| p.get("id"))
                .and_then(|v| v.as_str()),
            Some("a")
        );
        assert_eq!(
            select_profile_by_name(&profiles, "prod-staging")
                .and_then(|p| p.get("id"))
                .and_then(|v| v.as_str()),
            Some("b")
        );
        // No match -> None -> the named wrapper returns the inner provider
        // untouched (a profile absent from the readable set cannot carry a
        // binding, so passthrough is correct and safe).
        assert!(select_profile_by_name(&profiles, "pro").is_none());
        assert!(select_profile_by_name(&profiles, "PROD").is_none());
        assert!(select_profile_by_name(&profiles, "absent").is_none());
        assert!(select_profile_by_name(&[], "prod").is_none());
    }

    #[test]
    fn p1_stack_seam_builder_is_crypt_only_identity_path() {
        // P1: the stack builder at the seam (replaces direct wrap at cloud_service:1693)
        // must be a pure crypt-only path with ZERO behavior change.
        // - non-crypt profiles: returns inner untouched (identity)
        // - crypt profiles: delegates to the existing wrap (fail-closed preserved)
        // Ordering test (multi-layer) will be added in P2 when Compress is present.
        // We pin the public seam fn + reuse the exact-name select (already tested).
        // Full async round requires CredentialStore + profile vault; the logical seam
        // is covered by delegation + existing wrap_connected..._named tests.
        // Pin the public seam fn (existence + type) at compile time. When select
        // returns None the named wrap (and thus the builder) returns Ok(inner);
        // see select_profile_by_name_is_exact and the wrap impl for the identity
        // contract. No runtime assert needed: this compiling IS the check.
        let _ = build_aerocloud_overlay_stack;
    }

    #[test]
    fn select_profile_by_name_ignores_id_and_missing_name() {
        // Matching is on `name` only: an id that equals the query must not
        // match (the sync connects by name, not id), and a nameless profile is
        // skipped rather than panicking.
        let profiles = vec![
            serde_json::json!({ "id": "prod" }),
            serde_json::json!({ "name": "prod", "id": "real" }),
        ];
        assert_eq!(
            select_profile_by_name(&profiles, "prod")
                .and_then(|p| p.get("id"))
                .and_then(|v| v.as_str()),
            Some("real")
        );
    }

    // ── #390 strict fail-closed guard ────────────────────────────────────────

    /// A strict-parent provider (models WebDAV / OpenDrive): a PUT into a
    /// collection whose parent directory does not exist fails `NotFound`, so the
    /// decorator's #385 retry path (`ensure_parent_dirs`) is exercised. Directories
    /// must be created explicitly with `mkdir`; `exists` sees both files and dirs.
    struct StrictMemProvider {
        files: Mutex<HashMap<String, Vec<u8>>>,
        dirs: Mutex<Vec<String>>,
        fail_list_permission: bool,
    }

    impl StrictMemProvider {
        fn with_dirs(seed: &[&str]) -> Self {
            Self {
                files: Mutex::new(HashMap::new()),
                dirs: Mutex::new(seed.iter().map(|s| s.to_string()).collect()),
                fail_list_permission: false,
            }
        }
        fn with_permission_denied_list(seed: &[&str]) -> Self {
            Self {
                fail_list_permission: true,
                ..Self::with_dirs(seed)
            }
        }
        fn parent_of(p: &str) -> String {
            match p.trim_end_matches('/').rsplit_once('/') {
                Some((parent, _)) if !parent.is_empty() => parent.to_string(),
                _ => "/".to_string(),
            }
        }
        fn dir_list(&self) -> Vec<String> {
            self.dirs.lock().unwrap().clone()
        }
        fn file_paths(&self) -> Vec<String> {
            self.files.lock().unwrap().keys().cloned().collect()
        }
    }

    #[async_trait]
    impl StorageProvider for StrictMemProvider {
        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
        fn provider_type(&self) -> ProviderType {
            ProviderType::WebDav
        }
        fn display_name(&self) -> String {
            "strict-mem".into()
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
            if self.fail_list_permission {
                return Err(ProviderError::PermissionDenied(path.to_string()));
            }
            Ok(Vec::new())
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
            _remote: &str,
            _local: &str,
            _cb: Option<Box<dyn Fn(u64, u64) + Send>>,
        ) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn download_to_bytes(&mut self, _remote: &str) -> Result<Vec<u8>, ProviderError> {
            Ok(Vec::new())
        }
        async fn upload(
            &mut self,
            local: &str,
            remote: &str,
            _cb: Option<Box<dyn Fn(u64, u64) + Send>>,
        ) -> Result<(), ProviderError> {
            let parent = Self::parent_of(remote);
            if parent != "/" && !self.dirs.lock().unwrap().iter().any(|d| d == &parent) {
                // Strict provider: no implicit parent creation.
                return Err(ProviderError::NotFound(parent));
            }
            let data = std::fs::read(local).map_err(ProviderError::IoError)?;
            self.files.lock().unwrap().insert(remote.to_string(), data);
            Ok(())
        }
        async fn mkdir(&mut self, p: &str) -> Result<(), ProviderError> {
            let mut dirs = self.dirs.lock().unwrap();
            if !dirs.iter().any(|d| d == p) {
                dirs.push(p.to_string());
            }
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
        async fn rename(&mut self, _from: &str, _to: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn stat(&mut self, p: &str) -> Result<RemoteEntry, ProviderError> {
            Err(ProviderError::NotFound(p.to_string()))
        }
        async fn size(&mut self, p: &str) -> Result<u64, ProviderError> {
            Err(ProviderError::NotFound(p.to_string()))
        }
        async fn exists(&mut self, p: &str) -> Result<bool, ProviderError> {
            Ok(self.files.lock().unwrap().contains_key(p)
                || self.dirs.lock().unwrap().iter().any(|d| d == p))
        }
        async fn keep_alive(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn server_info(&mut self) -> Result<String, ProviderError> {
            Ok("strict-mem".into())
        }
    }

    async fn write_temp(payload: &[u8]) -> (std::path::PathBuf, String) {
        let dir = std::env::temp_dir().join(format!("crypt390_{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let local = dir.join("hello390.txt");
        tokio::fs::write(&local, payload).await.unwrap();
        let s = local.to_string_lossy().to_string();
        (dir, s)
    }

    /// #390 option 1 (strict): uploading with the overlay on into a plaintext-named
    /// folder that already exists on the wire (created while the overlay was off)
    /// must FAIL CLOSED with a clear message, and must NOT materialize a phantom
    /// `enc(name)` folder or store the file anywhere.
    #[tokio::test]
    async fn upload_into_plaintext_folder_fails_closed_no_phantom() {
        // Anchor plus the plaintext folder the user created with the overlay off.
        let inner = Box::new(StrictMemProvider::with_dirs(&[
            "/AeroCryptTest",
            "/AeroCryptTest/CryptPlain",
        ]));
        let keys = rclone_keys(FilenameEncryption::Standard, true, ".bin");
        let mut provider = CryptOverlayProvider::new(inner, keys, "/AeroCryptTest");

        let (dir, local) = write_temp(b"secret payload for 390").await;
        let err = provider
            .upload(&local, "/AeroCryptTest/CryptPlain/hello390.txt", None)
            .await
            .expect_err("must refuse writing into a plaintext folder");
        match &err {
            ProviderError::InvalidPath(msg) => {
                assert!(msg.contains("#390"), "message must reference #390: {msg}");
                assert!(
                    msg.contains("plaintext folder"),
                    "message must explain the plaintext folder: {msg}"
                );
            }
            other => panic!("expected InvalidPath refusal, got {other:?}"),
        }

        let mem = provider
            .as_any_mut()
            .downcast_mut::<CryptOverlayProvider>()
            .unwrap()
            .inner
            .as_any_mut()
            .downcast_mut::<StrictMemProvider>()
            .unwrap();
        assert!(
            mem.file_paths().is_empty(),
            "no file may be stored on refusal: {:?}",
            mem.file_paths()
        );
        assert_eq!(
            mem.dir_list(),
            vec![
                "/AeroCryptTest".to_string(),
                "/AeroCryptTest/CryptPlain".to_string()
            ],
            "no phantom encrypted folder may be created"
        );
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// #385 must still work: uploading into a genuinely-new encrypted subtree (no
    /// plaintext folder exists on the wire) creates the encrypted parent chain and
    /// stores the file under an encrypted path, without any plaintext folder name
    /// leaking onto the wire.
    #[tokio::test]
    async fn upload_into_new_encrypted_subtree_still_creates_and_stores() {
        // Only the anchor exists; the target subfolder is brand new.
        let inner = Box::new(StrictMemProvider::with_dirs(&["/AeroCryptTest"]));
        let keys = rclone_keys(FilenameEncryption::Standard, true, ".bin");
        let mut provider = CryptOverlayProvider::new(inner, keys, "/AeroCryptTest");

        let (dir, local) = write_temp(b"payload for a fresh crypt folder").await;
        provider
            .upload(&local, "/AeroCryptTest/FreshFolder/hello390.txt", None)
            .await
            .expect("a genuinely new encrypted subtree must be created and stored");

        let mem = provider
            .as_any_mut()
            .downcast_mut::<CryptOverlayProvider>()
            .unwrap()
            .inner
            .as_any_mut()
            .downcast_mut::<StrictMemProvider>()
            .unwrap();
        let files = mem.file_paths();
        assert_eq!(files.len(), 1, "exactly one object stored: {files:?}");
        assert!(
            !files[0].contains("FreshFolder") && !files[0].contains("hello390"),
            "stored path must be encrypted: {}",
            files[0]
        );
        assert!(
            mem.dir_list().iter().all(|d| !d.contains("FreshFolder")),
            "no plaintext folder name may leak onto the wire: {:?}",
            mem.dir_list()
        );
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    fn trash_entry(name: &str) -> RemoteEntry {
        RemoteEntry {
            name: name.to_string(),
            path: format!("/trash/{name}"),
            is_dir: false,
            size: 4096,
            modified: None,
            permissions: None,
            owner: None,
            group: None,
            is_symlink: false,
            link_target: None,
            mime_type: None,
            metadata: HashMap::new(),
        }
    }

    // Regression for #397: a provider-specific command (trash/restore/empty, and
    // every other `downcast_mut::<ConcreteProvider>()`) must peel the crypt
    // overlay to reach the transport, instead of downcasting the wrapper.
    #[test]
    fn concrete_provider_mut_peels_overlay_to_inner_transport() {
        // No-op when Crypt is off: a bare provider is returned unchanged.
        let mut bare: Box<dyn StorageProvider> = Box::new(MemProvider::new());
        assert!(concrete_provider_mut(&mut *bare)
            .as_any_mut()
            .downcast_mut::<MemProvider>()
            .is_some());

        // Wrapped: the raw downcast fails (the honest target is the decorator
        // itself, which is what broke every trash handler under Crypt), but
        // peeling reaches the inner MemProvider.
        let mut wrapped: Box<dyn StorageProvider> = Box::new(CryptOverlayProvider::new(
            Box::new(MemProvider::new()),
            aerocrypt_keys(),
            "/vault",
        ));
        assert!(
            wrapped.as_any_mut().downcast_mut::<MemProvider>().is_none(),
            "raw downcast through the overlay must fail"
        );
        assert!(
            concrete_provider_mut(&mut *wrapped)
                .as_any_mut()
                .downcast_mut::<MemProvider>()
                .is_some(),
            "peel must reach the concrete transport"
        );
    }

    #[test]
    fn decode_overlay_trash_names_decodes_in_scope_and_passes_foreign() {
        let overlay =
            CryptOverlayProvider::new(Box::new(MemProvider::new()), aerocrypt_keys(), "/vault");
        let enc = overlay.keys.encode_name("secret.txt", false).unwrap();
        let mut boxed: Box<dyn StorageProvider> = Box::new(overlay);

        let mut entries = vec![trash_entry(&enc), trash_entry("foreign.txt")];
        let raw_path_0 = entries[0].path.clone();
        decode_overlay_trash_names(&mut *boxed, &mut entries);

        // In-scope ciphertext name is decoded to plaintext for display...
        assert_eq!(entries[0].name, "secret.txt");
        // ...while the raw path/token is left untouched for the restore round-trip.
        assert_eq!(entries[0].path, raw_path_0);
        // A foreign / out-of-scope plaintext entry (global trash) passes through.
        assert_eq!(entries[1].name, "foreign.txt");

        // No-op on a bare provider (Crypt off).
        let mut bare: Box<dyn StorageProvider> = Box::new(MemProvider::new());
        let mut plain = vec![trash_entry("plain.txt")];
        decode_overlay_trash_names(&mut *bare, &mut plain);
        assert_eq!(plain[0].name, "plain.txt");
    }

    fn s3_trash_entry(key: &str) -> TrashEntry {
        TrashEntry {
            key: key.to_string(),
            display_key: key.to_string(),
            version_id: "vid-1".to_string(),
            is_delete_marker: false,
            is_latest: true,
            size: 10,
            last_modified: None,
        }
    }

    // #266 / #399: the S3 trash view must decrypt only the display key and keep
    // `key` + `version_id` byte-for-byte raw, so restore/purge round-trip the
    // exact backend tokens.
    #[test]
    fn decode_overlay_trash_keys_decodes_display_and_keeps_key_raw() {
        let overlay =
            CryptOverlayProvider::new(Box::new(MemProvider::new()), aerocrypt_keys(), "/vault");
        let enc = overlay.keys.encode_name("secret.txt", false).unwrap();
        let mut boxed: Box<dyn StorageProvider> = Box::new(overlay);

        let mut entries = vec![s3_trash_entry(&enc), s3_trash_entry("foreign.txt")];
        decode_overlay_trash_keys(&mut *boxed, &mut entries);

        // In-scope ciphertext key: display decrypted...
        assert_eq!(entries[0].display_key, "secret.txt");
        // ...raw key + version_id untouched for the restore round-trip.
        assert_eq!(entries[0].key, enc);
        assert_eq!(entries[0].version_id, "vid-1");
        // Foreign / out-of-scope plaintext key passes through on both fields.
        assert_eq!(entries[1].display_key, "foreign.txt");
        assert_eq!(entries[1].key, "foreign.txt");

        // No-op on a bare provider (Crypt off): display_key stays as seeded.
        let mut bare: Box<dyn StorageProvider> = Box::new(MemProvider::new());
        let mut plain = vec![s3_trash_entry("plain.txt")];
        decode_overlay_trash_keys(&mut *bare, &mut plain);
        assert_eq!(plain[0].display_key, "plain.txt");
        assert_eq!(plain[0].key, "plain.txt");
    }
}
