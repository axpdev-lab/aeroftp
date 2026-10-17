//! Drive nodes: the wire shape of a link, the decrypted node, and its
//! extended attributes.
//!
//! Ported from the Proton Drive SDK at commit [`super::SDK_REFERENCE_COMMIT`]
//! (MIT, Copyright (c) 2025-2026 Proton AG, see `LICENSE-PROTON-DRIVE-SDK.md`):
//! `client/js/src/internal/nodes/{apiService,cryptoService,nodesAccess,
//! extendedAttributes,validations}.ts`, with the API types of
//! `internal/apiService/driveTypes.ts` (`LinkDto`, `FileDto`, `FolderDto`).
//! The rules and their sources are summarised in the appendix spec
//! `APPENDIX-PROTON-DRIVE/05_port-spec.md`, sections 2 to 4.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use chrono::{DateTime, Utc};
use proton_crypto_account::proton_crypto::crypto::PGPProviderSync;
use serde::Deserialize;
use zeroize::Zeroizing;

use super::drive_crypto::Verified;

/// `NodeType2`: 1 folder, 2 file, 3 album.
pub const NODE_TYPE_FOLDER: i64 = 1;
pub const NODE_TYPE_FILE: i64 = 2;

/// `LinkState2`: 0 draft, 1 active, 2 trashed.
pub const LINK_STATE_ACTIVE: i64 = 1;

/// The SDK's name limit, counted in UTF-16 code units like JS `.length`.
pub const MAX_NAME_UTF16_UNITS: usize = 255;

/// Before this instant an unsigned node name counts as valid
/// (`nodes/cryptoService.ts`, 2021-01-01).
pub const UNSIGNED_NAME_CUTOFF: i64 = 1_609_459_200;
/// Before this instant an unsigned folder hash key counts as valid
/// (2021-08-01).
pub const UNSIGNED_HASH_KEY_CUTOFF: i64 = 1_627_776_000;
/// Before this instant a content key signature that fails with the author's
/// keys is retried with every own address key (2023-01-01).
pub const LEGACY_CONTENT_KEY_CUTOFF: i64 = 1_672_531_200;
/// Revisions created before this instant store `BlockSizes` in descending
/// order (an old Windows client bug, 2025-01-01).
pub const DESCENDING_BLOCK_SIZES_CUTOFF: i64 = 1_735_689_600;

/// `LoadLinkDetailsResponseDto`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct LinksResponse {
    #[serde(default)]
    pub links: Vec<LinkDetails>,
}

/// `FileDetailsDto | FolderDetailsDto | AlbumDetailsDto`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct LinkDetails {
    pub link: LinkDto,
    #[serde(default)]
    pub file: Option<FileDto>,
    #[serde(default)]
    pub folder: Option<FolderDto>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct LinkDto {
    #[serde(rename = "LinkID")]
    pub link_id: String,
    #[serde(rename = "ParentLinkID", default)]
    pub parent_link_id: Option<String>,
    #[serde(rename = "Type")]
    pub node_type: i64,
    #[serde(default)]
    pub state: Option<i64>,
    #[serde(default)]
    pub create_time: i64,
    #[serde(default)]
    pub modify_time: i64,
    #[serde(default)]
    pub trash_time: Option<i64>,
    pub name: String,
    #[serde(default)]
    pub name_hash: Option<String>,
    pub node_key: String,
    pub node_passphrase: String,
    #[serde(default)]
    pub node_passphrase_signature: Option<String>,
    #[serde(default)]
    pub signature_email: Option<String>,
    #[serde(default)]
    pub name_signature_email: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct FileDto {
    #[serde(default)]
    pub active_revision: Option<ActiveRevisionDto>,
    #[serde(default)]
    pub total_encrypted_size: Option<u64>,
    pub content_key_packet: String,
    #[serde(default)]
    pub media_type: Option<String>,
    #[serde(default)]
    pub content_key_packet_signature: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ActiveRevisionDto {
    #[serde(rename = "RevisionID")]
    pub revision_id: String,
    #[serde(default)]
    pub create_time: i64,
    #[serde(default)]
    pub encrypted_size: Option<u64>,
    #[serde(default)]
    pub manifest_signature: Option<String>,
    #[serde(rename = "XAttr", default)]
    pub xattr: Option<String>,
    #[serde(default)]
    pub signature_email: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct FolderDto {
    #[serde(default)]
    pub node_hash_key: Option<String>,
    #[serde(rename = "XAttr", default)]
    pub xattr: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    Folder,
    File,
}

/// What the extended attributes claim. Absent fields stay `None`: the SDK
/// derives no fallback, so any fallback is the caller's choice.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExtendedAttributes {
    pub size: Option<u64>,
    pub modification_time: Option<DateTime<Utc>>,
    pub block_sizes: Vec<u64>,
    pub sha1: Option<String>,
}

/// `extendedAttributes.ts` parsing: invalid JSON or wrongly typed fields are
/// dropped, never fatal. Revisions created before 2025-01-01 carry block
/// sizes in descending order; they are put back in index order.
pub fn parse_extended_attributes(json: &[u8], revision_create_time: i64) -> ExtendedAttributes {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(json) else {
        log::warn!("Proton: extended attributes are not valid JSON");
        return ExtendedAttributes::default();
    };
    let common = value.get("Common");
    let size = common
        .and_then(|c| c.get("Size"))
        .and_then(serde_json::Value::as_u64);
    let modification_time = common
        .and_then(|c| c.get("ModificationTime"))
        .and_then(serde_json::Value::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc));
    let mut block_sizes: Vec<u64> = common
        .and_then(|c| c.get("BlockSizes"))
        .and_then(serde_json::Value::as_array)
        .map(|sizes| sizes.iter().filter_map(serde_json::Value::as_u64).collect())
        .unwrap_or_default();
    if block_sizes.contains(&0) {
        block_sizes.clear();
    }
    if revision_create_time > 0 && revision_create_time < DESCENDING_BLOCK_SIZES_CUTOFF {
        // Descending means every block but the last was full: the last one
        // is the smallest. Index order puts it at the end.
        block_sizes.sort_unstable_by(|a, b| b.cmp(a));
    }
    let sha1 = common
        .and_then(|c| c.get("Digests"))
        .and_then(|d| d.get("SHA1"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_ascii_lowercase);
    ExtendedAttributes {
        size,
        modification_time,
        block_sizes,
        sha1,
    }
}

/// `validations.ts`: not empty and at most 255 UTF-16 code units. The SDK
/// checks nothing else on the client; the port refuses path separators too,
/// since AeroFTP addresses nodes by path.
pub fn validate_node_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("the name is empty".into());
    }
    if name.encode_utf16().count() > MAX_NAME_UTF16_UNITS {
        return Err(format!(
            "the name is longer than {MAX_NAME_UTF16_UNITS} characters"
        ));
    }
    if name.contains('/') || name == "." || name == ".." {
        return Err("the name cannot contain '/' or be '.' or '..'".into());
    }
    Ok(())
}

/// The active revision of a file, as far as reading it needs.
#[derive(Debug, Clone)]
pub struct ActiveRevision {
    pub revision_id: String,
    pub create_time: i64,
    pub encrypted_size: Option<u64>,
    pub manifest_signature: Option<String>,
    pub signature_email: Option<String>,
}

/// A decrypted node. Fields that could not be decrypted are `None` (or an
/// `Err` name) and the reason is in `errors`; the node stays listable, as in
/// the SDK, which returns "degraded" nodes instead of failing the listing.
pub struct Node<P: PGPProviderSync> {
    pub link_id: String,
    pub parent_link_id: Option<String>,
    pub kind: NodeKind,
    pub name: Result<String, String>,
    pub name_verified: Verified,
    pub name_hash: Option<String>,
    pub create_time: i64,
    /// Server-side modification (rename, move, new revision).
    pub modify_time: i64,
    pub mime_type: Option<String>,
    pub node_key: Option<P::PrivateKey>,
    pub key_verified: Verified,
    /// Folders only: the key of the lookup hashes of the children.
    pub hash_key: Option<Zeroizing<Vec<u8>>>,
    /// Files only.
    pub content_key: Option<P::SessionKey>,
    pub active_revision: Option<ActiveRevision>,
    pub xattr: ExtendedAttributes,
    pub total_encrypted_size: Option<u64>,
    pub signature_email: Option<String>,
    pub name_signature_email: Option<String>,
    /// Kept armored for rename and move, which reuse their session keys.
    pub armored_name: String,
    pub armored_node_passphrase: String,
    pub errors: Vec<String>,
}

impl<P: PGPProviderSync> std::fmt::Debug for Node<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Node")
            .field("link_id", &self.link_id)
            .field("kind", &self.kind)
            .field("name_ok", &self.name.is_ok())
            .field("has_key", &self.node_key.is_some())
            .field("errors", &self.errors.len())
            .finish_non_exhaustive()
    }
}

impl<P: PGPProviderSync> Node<P> {
    pub fn is_folder(&self) -> bool {
        self.kind == NodeKind::Folder
    }

    /// Plaintext size: the extended attributes' claim. Without it, AeroFTP
    /// falls back to the encrypted size of the active revision, which is a
    /// little larger than the content (our choice; the SDK has none).
    pub fn size(&self) -> u64 {
        self.xattr
            .size
            .or_else(|| self.active_revision.as_ref().and_then(|r| r.encrypted_size))
            .unwrap_or(0)
    }

    /// Content modification time: the extended attributes' claim, else the
    /// server's modification time.
    pub fn modification_time(&self) -> Option<DateTime<Utc>> {
        self.xattr
            .modification_time
            .or_else(|| DateTime::from_timestamp(self.modify_time, 0))
    }

    pub fn display_name(&self) -> Option<&str> {
        self.name.as_deref().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_extended_attributes_parse_like_the_sdk() {
        let json = br#"{"Common":{"ModificationTime":"2026-10-16T21:15:00.123Z","Size":5242880,"BlockSizes":[4194304,1048576],"Digests":{"SHA1":"A9993E364706816ABA3E25717850C26C9CD0D89D"}}}"#;
        let xattr = parse_extended_attributes(json, 1_760_000_000);
        assert_eq!(xattr.size, Some(5_242_880));
        assert_eq!(xattr.block_sizes, vec![4_194_304, 1_048_576]);
        assert_eq!(
            xattr.sha1.as_deref(),
            Some("a9993e364706816aba3e25717850c26c9cd0d89d")
        );
        assert_eq!(
            xattr.modification_time.map(|t| t.timestamp_millis()),
            Some(1_792_185_300_123)
        );
    }

    #[test]
    fn invalid_or_mistyped_attributes_are_dropped_not_fatal() {
        assert_eq!(
            parse_extended_attributes(b"not json", 0),
            ExtendedAttributes::default()
        );
        let xattr = parse_extended_attributes(
            br#"{"Common":{"Size":"12","ModificationTime":"yesterday","BlockSizes":[0]}}"#,
            0,
        );
        assert_eq!(xattr, ExtendedAttributes::default());
    }

    #[test]
    fn legacy_descending_block_sizes_are_kept_largest_first() {
        let xattr = parse_extended_attributes(
            br#"{"Common":{"BlockSizes":[1048576,4194304]}}"#,
            1_700_000_000,
        );
        assert_eq!(xattr.block_sizes, vec![4_194_304, 1_048_576]);
    }

    #[test]
    fn names_follow_the_sdk_limit_and_refuse_separators() {
        assert!(validate_node_name("report.pdf").is_ok());
        assert!(validate_node_name("").is_err());
        assert!(validate_node_name(&"a".repeat(255)).is_ok());
        assert!(validate_node_name(&"a".repeat(256)).is_err());
        // 128 emoji are 256 UTF-16 code units.
        assert!(validate_node_name(&"😀".repeat(128)).is_err());
        assert!(validate_node_name("a/b").is_err());
        assert!(validate_node_name("..").is_err());
    }
}
