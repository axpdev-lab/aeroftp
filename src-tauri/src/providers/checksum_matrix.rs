//! Which digests each backend can hand back WITHOUT downloading the file.
//!
//! `supports_checksum()` answers "is there a server-side digest at all". That
//! is enough for the sync engine, which only needs to know whether to ask, but
//! it is not enough for a surface that has to decide, before the user clicks,
//! whether to offer MD5 on a backend that only ever returns SHA-1. This module
//! is that second answer, and it is the single source of truth for it: the GUI
//! Properties dialog reads it to replace a doomed "Calculate" button with a
//! statement, and `docs/PROTOCOL-FEATURES.md` is generated from it, so the
//! published table cannot drift from the code (a test pins the two together).
//!
//! The match on `ProviderType` is exhaustive on purpose. A new backend does
//! not silently inherit "no digests": it fails to compile until someone states
//! what it can do, which is the only way a matrix like this stays true.
//!
//! Two rules the entries follow:
//!
//! * **Never advertise what only a download could produce.** Every algorithm
//!   listed here comes from metadata the backend already holds (an ETag, a
//!   `file.hashes` field, an `oc:checksums` prop) or from work the *server*
//!   does on its own disk (`sha256sum` over an SSH exec channel). Nothing here
//!   streams file content to us.
//! * **A backend's own scheme keeps its own name.** Dropbox's `content_hash`
//!   is a SHA-256 of block SHA-256s and GitHub's is a git blob SHA-1; neither
//!   is comparable with the digest of the same bytes on disk, so they are
//!   reported under `dropbox` and `git-sha1` rather than aliased onto `sha256`
//!   and `sha1` where they would quietly fail every comparison.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use super::types::{ChecksumCapability, ProviderType};

/// Human label for a canonical algorithm key, for the generated docs table.
/// The GUI does not use this: it renders its own translated row labels.
pub fn algorithm_label(key: &str) -> &str {
    match key {
        "md5" => "MD5",
        "sha1" => "SHA-1",
        "sha256" => "SHA-256",
        "sha384" => "SHA-384",
        "sha512" => "SHA-512",
        "blake3" => "BLAKE3",
        "crc32" => "CRC32",
        "adler32" => "Adler-32",
        "quickxor" => "QuickXorHash",
        "dropbox" => "Dropbox content hash",
        "koofr" => "Koofr hash",
        "git-sha1" => "Git blob SHA-1",
        other => other,
    }
}

/// One line of English prose per caveat token, for the generated docs table.
/// The GUI shows its own translated wording for the same token.
pub fn caveat_text(token: &str) -> &str {
    match token {
        S3_ETAG => "The ETag is the object MD5 only for single-part objects that are not SSE-KMS or SSE-C encrypted. A multipart ETag carries a `-N` suffix and an SSE-KMS ETag is opaque, so neither is 32 hex characters and both are omitted rather than guessed. An SSE-C ETag can be 32 hex characters, which is indistinguishable from a real digest, so a reported ETag is advisory for such an object.",
        B2_UNVERIFIED => "Large files and uploads B2 recorded as `unverified:` carry no usable `contentSha1`; those are omitted.",
        SFTP_SHA256SUM => "Computed by the remote host with `sha256sum` over an SSH exec channel: the bytes are read on the server, never sent to us. Omitted if the host has no `sha256sum`.",
        FTP_NEGOTIATED => "Depends on what the server advertises in FEAT: `HASH` (algorithm chosen by the server), or the older `XMD5` / `XSHA1` / `XCRC`. A server advertising none offers no digest.",
        WEBDAV_OC => "Only ownCloud and Nextcloud publish the `oc:checksums` property, and only for files uploaded by a client that sent one. Every other WebDAV server omits it.",
        DRIVE_WORKSPACE => "Native Google Workspace documents have no byte stream and therefore no digest.",
        ONEDRIVE_HASHES => "Which of the three is present depends on the account: personal OneDrive publishes QuickXorHash, business and SharePoint typically SHA-1 and SHA-256.",
        DROPBOX_CONTENT_HASH => "Not a plain file digest: a SHA-256 over the SHA-256 of each 4 MiB block. Comparable only against another Dropbox content hash.",
        KOOFR_PROPRIETARY => "Koofr's own opaque hash. Comparable only against another Koofr hash.",
        GITHUB_BLOB => "The git blob SHA-1, computed over `blob <len>\\0` + content, so it does not match a plain SHA-1 of the same file.",
        IMMICH_ASSET => "The asset checksum Immich records at upload time.",
        CIPHERTEXT => "Inside the Overlays Path the digest describes the encrypted bytes stored on the server, not the plaintext you see. It will not match a local hash of the same file, by design.",
        other => other,
    }
}

pub const S3_ETAG: &str = "s3-etag";
pub const B2_UNVERIFIED: &str = "b2-unverified";
pub const SFTP_SHA256SUM: &str = "sftp-sha256sum";
pub const FTP_NEGOTIATED: &str = "ftp-negotiated";
pub const WEBDAV_OC: &str = "webdav-oc-checksums";
pub const DRIVE_WORKSPACE: &str = "drive-workspace-docs";
pub const ONEDRIVE_HASHES: &str = "onedrive-hashes";
pub const DROPBOX_CONTENT_HASH: &str = "dropbox-content-hash";
pub const KOOFR_PROPRIETARY: &str = "koofr-proprietary";
pub const GITHUB_BLOB: &str = "github-blob-sha";
pub const IMMICH_ASSET: &str = "immich-asset-checksum";
pub const CIPHERTEXT: &str = "crypt-overlay-ciphertext";

fn cap(algorithms: &[&str], caveat: Option<&str>) -> ChecksumCapability {
    ChecksumCapability {
        algorithms: algorithms.iter().map(|s| s.to_string()).collect(),
        negotiated: false,
        ciphertext: false,
        caveat: caveat.map(|s| s.to_string()),
    }
}

fn negotiated(algorithms: &[&str], caveat: &str) -> ChecksumCapability {
    ChecksumCapability {
        negotiated: true,
        ..cap(algorithms, Some(caveat))
    }
}

/// The static stance of a backend, before a live connection narrows it.
///
/// Two backends refine this at runtime because the answer is genuinely
/// per-connection: FTP reads it from FEAT (`FtpProvider::checksum_capability`)
/// and SFTP needs a live SSH session. Everything else is fixed by the API.
pub fn capability(kind: ProviderType) -> ChecksumCapability {
    match kind {
        // ── Transport protocols ──────────────────────────────────────────
        ProviderType::Ftp | ProviderType::Ftps => {
            negotiated(&["md5", "sha1", "sha256", "crc32"], FTP_NEGOTIATED)
        }
        ProviderType::Sftp => cap(&["sha256"], Some(SFTP_SHA256SUM)),
        ProviderType::WebDav => negotiated(&["sha1", "md5", "adler32"], WEBDAV_OC),
        ProviderType::S3 => cap(&["md5"], Some(S3_ETAG)),

        // ── Native provider APIs that publish a digest ───────────────────
        ProviderType::GoogleDrive => cap(&["md5", "sha1", "sha256"], Some(DRIVE_WORKSPACE)),
        ProviderType::OneDrive => cap(&["sha1", "sha256", "quickxor"], Some(ONEDRIVE_HASHES)),
        ProviderType::Dropbox => cap(&["dropbox"], Some(DROPBOX_CONTENT_HASH)),
        ProviderType::Box => cap(&["sha1"], None),
        ProviderType::PCloud => cap(&["md5", "sha1", "sha256"], None),
        ProviderType::YandexDisk => cap(&["md5"], None),
        ProviderType::Koofr => cap(&["koofr"], Some(KOOFR_PROPRIETARY)),
        ProviderType::Backblaze => cap(&["sha1"], Some(B2_UNVERIFIED)),
        ProviderType::GitHub => cap(&["git-sha1"], Some(GITHUB_BLOB)),
        ProviderType::Immich => cap(&["sha1"], Some(IMMICH_ASSET)),

        // ── Backends with no server-side digest ──────────────────────────
        // Not "not implemented yet" in every case: Azure publishes
        // Content-MD5 only for blobs whose uploader sent one, MEGA and Filen
        // and Internxt hash ciphertext client-side under their own scheme,
        // and the media CDNs address assets by id rather than by content.
        // Whatever the reason, none of them can answer today, and saying so
        // up front is the point of this table.
        ProviderType::Azure
        | ProviderType::AeroCloud
        | ProviderType::Mega
        | ProviderType::Filen
        | ProviderType::FourShared
        | ProviderType::ZohoWorkdrive
        | ProviderType::Internxt
        | ProviderType::KDrive
        | ProviderType::Jottacloud
        | ProviderType::DrimeCloud
        | ProviderType::FileLu
        | ProviderType::OpenDrive
        | ProviderType::GitLab
        | ProviderType::Swift
        | ProviderType::GooglePhotos
        | ProviderType::ImageKit
        | ProviderType::Uploadcare
        | ProviderType::Cloudinary
        | ProviderType::AeroVaultMount
        | ProviderType::Peer
        | ProviderType::Mtp => ChecksumCapability::default(),
    }
}

/// Every backend, in the order the generated docs table lists them.
const DOC_ORDER: &[ProviderType] = &[
    ProviderType::Ftp,
    ProviderType::Ftps,
    ProviderType::Sftp,
    ProviderType::WebDav,
    ProviderType::S3,
    ProviderType::Azure,
    ProviderType::Swift,
    ProviderType::GoogleDrive,
    ProviderType::OneDrive,
    ProviderType::Dropbox,
    ProviderType::Box,
    ProviderType::PCloud,
    ProviderType::YandexDisk,
    ProviderType::Koofr,
    ProviderType::Backblaze,
    ProviderType::GitHub,
    ProviderType::GitLab,
    ProviderType::Immich,
    ProviderType::Mega,
    ProviderType::Filen,
    ProviderType::Internxt,
    ProviderType::ZohoWorkdrive,
    ProviderType::KDrive,
    ProviderType::Jottacloud,
    ProviderType::DrimeCloud,
    ProviderType::FileLu,
    ProviderType::FourShared,
    ProviderType::OpenDrive,
    ProviderType::GooglePhotos,
    ProviderType::ImageKit,
    ProviderType::Uploadcare,
    ProviderType::Cloudinary,
    ProviderType::Mtp,
];

/// Render the published table. `docs/PROTOCOL-FEATURES.md` carries the output
/// between its `CHECKSUM-MATRIX` markers and a test fails if the two differ,
/// so the documentation is generated rather than maintained.
pub fn markdown_table() -> String {
    let mut out = String::new();
    out.push_str("| Backend | Server-side digests | Notes |\n");
    out.push_str("| --- | --- | --- |\n");
    for kind in DOC_ORDER {
        let c = capability(*kind);
        let algos = if c.algorithms.is_empty() {
            "-".to_string()
        } else {
            let joined = c
                .algorithms
                .iter()
                .map(|a| algorithm_label(a))
                .collect::<Vec<_>>()
                .join(", ");
            if c.negotiated {
                format!("{} (negotiated)", joined)
            } else {
                joined
            }
        };
        let note = c.caveat.as_deref().map(caveat_text).unwrap_or("");
        out.push_str(&format!("| {} | {} | {} |\n", kind, algos, note));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The keys the GUI and the sync engine agree on are lowercase and free of
    /// whitespace; a stray "SHA-256" here would render a row the frontend
    /// cannot match against a returned digest.
    #[test]
    fn declared_algorithm_keys_are_canonical() {
        for kind in DOC_ORDER {
            for algo in capability(*kind).algorithms {
                assert!(
                    algo.chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                    "{kind} declares a non-canonical algorithm key {algo:?}"
                );
            }
        }
    }

    /// A caveat is a token the GUI translates and the docs expand, never a
    /// sentence to be shown raw. `caveat_text` returning the token unchanged
    /// means the token has no prose and the docs table would print the slug.
    #[test]
    fn every_declared_caveat_has_prose() {
        for kind in DOC_ORDER {
            if let Some(token) = capability(*kind).caveat {
                assert_ne!(
                    caveat_text(&token),
                    token,
                    "{kind} declares caveat {token:?} with no prose in caveat_text()"
                );
            }
        }
        assert_ne!(caveat_text(CIPHERTEXT), CIPHERTEXT);
    }

    /// A backend that declares no algorithm has nothing to caveat, and one
    /// that declares algorithms is not silently marked as ciphertext here:
    /// only the overlay sets that, at runtime, on top of the inner backend.
    #[test]
    fn empty_capabilities_are_plainly_empty() {
        for kind in DOC_ORDER {
            let c = capability(*kind);
            if c.algorithms.is_empty() {
                assert!(c.caveat.is_none(), "{kind} caveats an empty capability");
                assert!(!c.negotiated, "{kind} negotiates an empty capability");
            }
            assert!(!c.ciphertext, "{kind} is not an overlay");
        }
    }

    #[test]
    fn doc_order_covers_every_backend_with_a_digest() {
        // AeroVaultMount and Peer are in-process synthetic providers with no
        // remote to ask, and are deliberately absent from the published table.
        for kind in [
            ProviderType::AeroVaultMount,
            ProviderType::Peer,
            ProviderType::AeroCloud,
        ] {
            assert!(capability(kind).algorithms.is_empty());
            assert!(!DOC_ORDER.contains(&kind));
        }
    }

    const DOC_PATH: &str = "../docs/PROTOCOL-FEATURES.md";
    const BEGIN: &str = "<!-- BEGIN CHECKSUM-MATRIX -->";
    const END: &str = "<!-- END CHECKSUM-MATRIX -->";

    /// The published table is generated, not maintained: this fails the moment
    /// a backend's stance changes in code without the documentation following.
    /// Run with `AEROFTP_UPDATE_DOCS=1` to rewrite the block instead.
    #[test]
    fn published_table_matches_the_matrix() {
        let doc = std::fs::read_to_string(DOC_PATH).expect("docs/PROTOCOL-FEATURES.md");
        let head_end = doc.find(BEGIN).expect("BEGIN CHECKSUM-MATRIX marker");
        let tail_start = doc.find(END).expect("END CHECKSUM-MATRIX marker");
        let head = &doc[..head_end + BEGIN.len()];
        let block = &doc[head_end + BEGIN.len()..tail_start];
        let tail = &doc[tail_start..];

        // The generator comment sits inside the markers and is preserved.
        let comment_end = block.rfind("-->").map(|i| i + 3).unwrap_or(0);
        let generator_comment = &block[..comment_end];
        let expected = format!("{generator_comment}\n\n{}\n", markdown_table());

        if block == expected {
            return;
        }
        if std::env::var("AEROFTP_UPDATE_DOCS").is_ok() {
            std::fs::write(DOC_PATH, format!("{head}{expected}{tail}")).expect("rewrite docs");
            return;
        }
        panic!(
            "docs/PROTOCOL-FEATURES.md checksum table is stale.\n\
             Re-run with AEROFTP_UPDATE_DOCS=1 to regenerate.\n\
             --- expected ---\n{expected}\n--- found ---\n{block}"
        );
    }

    #[test]
    fn table_renders_one_row_per_backend() {
        let table = markdown_table();
        // Two header lines plus one row per backend.
        assert_eq!(table.lines().count(), DOC_ORDER.len() + 2);
        assert!(table.contains("| SFTP | SHA-256 |"));
        assert!(table.contains("(negotiated)"));
        assert!(table.contains("| Azure Blob | - |  |"));
    }
}
