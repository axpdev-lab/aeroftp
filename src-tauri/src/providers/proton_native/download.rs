//! Download of a file's active revision.
//!
//! Ported from the Proton Drive SDK at commit [`super::SDK_REFERENCE_COMMIT`]
//! (MIT, Copyright (c) 2025-2026 Proton AG, see `LICENSE-PROTON-DRIVE-SDK.md`):
//! `internal/download/{apiService,cryptoService,fileDownloader}.ts`, with the
//! stricter block ordering of the C# `RevisionReader.cs`. Spec section 5 of
//! `APPENDIX-PROTON-DRIVE/05_port-spec.md`.
//!
//! Integrity, in order: every encrypted block must hash to the server's
//! `Hash`; every block must decrypt with the file's content key; the
//! manifest (thumbnail hashes by type, then block hashes by index) must carry
//! a valid signature from the uploader. Blocks are written in index order as
//! they arrive; a manifest failure is reported after the data is written, as
//! in the SDK, and the caller must then discard the output.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use proton_crypto_account::proton_crypto::crypto::PGPProviderSync;
use serde::Deserialize;
use sha1::{Digest, Sha1};
use tokio::io::{AsyncWrite, AsyncWriteExt};

use super::client::DriveClient;
use super::drive_crypto::{self, Verified};
use super::error::ProtonError;
use super::nodes::Node;

/// `download/apiService.ts`: blocks per revision page.
const BLOCK_PAGE_SIZE: u32 = 20;
/// A hash or decryption failure is retried once (`fileDownloader.ts`).
const BLOCK_INTEGRITY_RETRIES: u32 = 1;
/// The SDK refreshes expired storage tokens without a cap; the port caps it.
const TOKEN_REFRESH_LIMIT: u32 = 5;

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RevisionResponse {
    revision: RevisionPage,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RevisionPage {
    #[serde(default)]
    manifest_signature: Option<String>,
    #[serde(default)]
    thumbnails: Vec<ThumbnailRef>,
    #[serde(default)]
    blocks: Vec<BlockRef>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ThumbnailRef {
    #[serde(rename = "Type")]
    thumbnail_type: i64,
    hash: String,
}

#[derive(Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
struct BlockRef {
    index: u64,
    #[serde(rename = "BareURL")]
    bare_url: String,
    token: String,
    hash: String,
}

/// What a finished download reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadOutcome {
    pub bytes: u64,
    pub blocks: u64,
    /// SHA-1 of the plaintext, lowercase hex.
    pub sha1: String,
}

impl<P: PGPProviderSync> DriveClient<P> {
    async fn revision_page(
        &self,
        node: &Node<P>,
        revision_id: &str,
        from_index: u64,
        page_size: u32,
    ) -> Result<RevisionPage, ProtonError> {
        let path = format!(
            "drive/v2/volumes/{}/files/{}/revisions/{}",
            self.volume_id(),
            node.link_id,
            revision_id
        );
        let response: RevisionResponse = self
            .api
            .get(
                &path,
                &[
                    ("PageSize", page_size.to_string()),
                    ("FromBlockIndex", from_index.to_string()),
                ],
            )
            .await?;
        Ok(response.revision)
    }

    /// Fetches one block, refreshing its token when the storage URL expired
    /// and retrying a hash or decryption failure once. Returns the plaintext
    /// and the raw SHA-256 of the encrypted bytes for the manifest.
    async fn fetch_block(
        &self,
        node: &Node<P>,
        revision_id: &str,
        session_key: &P::SessionKey,
        mut block: BlockRef,
    ) -> Result<(Vec<u8>, Vec<u8>), ProtonError> {
        let mut refreshes = 0;
        let mut integrity_failures = 0;
        loop {
            let encrypted = match self.api.storage_get(&block.bare_url, &block.token).await {
                Ok(bytes) => bytes,
                Err(ProtonError::StorageTokenExpired) if refreshes < TOKEN_REFRESH_LIMIT => {
                    refreshes += 1;
                    let page = self
                        .revision_page(node, revision_id, block.index, 1)
                        .await?;
                    block = page
                        .blocks
                        .into_iter()
                        .find(|b| b.index == block.index)
                        .ok_or_else(|| {
                            ProtonError::Integrity(format!("block {} disappeared", block.index))
                        })?;
                    continue;
                }
                Err(error) => return Err(error),
            };
            let (digest, encoded) = drive_crypto::encrypted_block_hash(&encrypted);
            let outcome = if encoded != block.hash {
                Err(ProtonError::Integrity(format!(
                    "block {} does not match its hash",
                    block.index
                )))
            } else {
                drive_crypto::decrypt_block(self.provider(), &encrypted, session_key)
                    .map(|plain| (plain, digest))
            };
            match outcome {
                Ok(result) => return Ok(result),
                Err(error) if integrity_failures < BLOCK_INTEGRITY_RETRIES => {
                    integrity_failures += 1;
                    log::warn!("Proton: block {} failed ({error}), retrying", block.index);
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// Downloads the active revision of `node` into `out`. `progress` gets
    /// the plaintext bytes written so far.
    pub async fn download<W: AsyncWrite + Unpin>(
        &self,
        node: &Node<P>,
        out: &mut W,
        mut progress: impl FnMut(u64),
    ) -> Result<DownloadOutcome, ProtonError> {
        if node.is_folder() {
            return Err(ProtonError::Decode("cannot download a folder".into()));
        }
        let (Some(session_key), Some(revision), Some(node_key)) = (
            node.content_key.as_ref(),
            node.active_revision.as_ref(),
            node.node_key.as_ref(),
        ) else {
            // `download/index.ts`: "This file has missing crypto material".
            return Err(ProtonError::Crypto(
                "this file has missing crypto material".into(),
            ));
        };

        let mut next_index: u64 = 1;
        let mut manifest_signature: Option<String> = None;
        let mut thumbnails: Vec<(i64, Vec<u8>)> = Vec::new();
        let mut block_digests: Vec<Vec<u8>> = Vec::new();
        let mut sha1 = Sha1::new();
        let mut written: u64 = 0;
        let mut first_page = true;

        loop {
            let page = self
                .revision_page(node, &revision.revision_id, next_index, BLOCK_PAGE_SIZE)
                .await?;
            if first_page {
                first_page = false;
                manifest_signature = page.manifest_signature.filter(|s| !s.is_empty());
                for thumbnail in page.thumbnails {
                    let hash = BASE64.decode(thumbnail.hash.trim()).map_err(|_| {
                        ProtonError::Integrity("thumbnail hash is not base64".into())
                    })?;
                    thumbnails.push((thumbnail.thumbnail_type, hash));
                }
            }
            if page.blocks.is_empty() {
                break;
            }
            let mut blocks = page.blocks;
            blocks.sort_by_key(|b| b.index);
            for block in blocks {
                // RevisionReader.cs: indices must be consecutive from 1.
                if block.index != next_index {
                    return Err(ProtonError::Integrity(format!(
                        "expected block {next_index}, got {}",
                        block.index
                    )));
                }
                let (plain, digest) = self
                    .fetch_block(node, &revision.revision_id, session_key, block)
                    .await?;
                sha1.update(&plain);
                out.write_all(&plain).await.map_err(ProtonError::LocalIo)?;
                written += plain.len() as u64;
                progress(written);
                block_digests.push(digest);
                next_index += 1;
            }
        }
        out.flush().await.map_err(ProtonError::LocalIo)?;

        // Manifest: thumbnails sorted by type, then blocks by index.
        thumbnails.sort_by_key(|(thumbnail_type, _)| *thumbnail_type);
        let mut manifest = Vec::with_capacity((thumbnails.len() + block_digests.len()) * 32);
        for (_, hash) in &thumbnails {
            manifest.extend_from_slice(hash);
        }
        for digest in &block_digests {
            manifest.extend_from_slice(digest);
        }
        let Some(signature) = manifest_signature else {
            return Err(ProtonError::Integrity(
                "cannot verify who uploaded this file: the manifest is not signed".into(),
            ));
        };
        let uploader_keys = match revision.signature_email.as_deref() {
            Some(email) => self.public_keys(email).await?,
            None => self
                .provider()
                .private_key_to_public_key(node_key)
                .map(|key| vec![key])
                .map_err(|error| ProtonError::Crypto(error.to_string()))?,
        };
        let refs: Vec<&P::PublicKey> = uploader_keys.iter().collect();
        match drive_crypto::verify_detached(self.provider(), &manifest, &signature, &refs) {
            Verified::Valid => {}
            other => {
                return Err(ProtonError::Integrity(format!(
                    "the manifest signature did not verify ({other:?})"
                )))
            }
        }

        let sha1 = hex::encode(sha1.finalize());
        if let Some(claimed) = node.xattr.sha1.as_deref() {
            if claimed != sha1 {
                // The SDK does not check it; a mismatch with a valid manifest
                // means the uploader's attributes are wrong, not the data.
                log::warn!("Proton: downloaded content does not match the claimed SHA-1");
            }
        }
        if let Some(claimed) = node.xattr.size {
            if claimed != written {
                log::warn!("Proton: downloaded size differs from the claimed size");
            }
        }
        Ok(DownloadOutcome {
            bytes: written,
            blocks: block_digests.len() as u64,
            sha1,
        })
    }
}
