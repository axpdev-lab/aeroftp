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
use futures_util::StreamExt;
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
/// `RevisionState` of the revision a file reads as its content.
const REVISION_STATE_ACTIVE: i64 = 1;
/// Blocks of one file fetched at once (`fileDownloader.ts::MAX_DOWNLOAD_BLOCK_SIZE`).
/// They are still written in index order.
const BLOCKS_IN_FLIGHT: usize = 10;

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RevisionResponse {
    revision: RevisionPage,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RevisionPage {
    /// `RevisionState`: 0 draft, 1 active, 2 obsolete.
    #[serde(default)]
    state: Option<i64>,
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
    ///
    /// A revision the server no longer has as active is
    /// [`ProtonError::RevisionNotActive`], before anything is written. With
    /// `confirm_active` (a node held since a listing) the server must say it
    /// is active; without (a node just read), only a state other than active
    /// is refused.
    pub async fn download<W: AsyncWrite + Unpin>(
        &self,
        node: &Node<P>,
        out: &mut W,
        mut progress: impl FnMut(u64),
        confirm_active: bool,
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

        let mut thumbnails: Vec<(i64, Vec<u8>)> = Vec::new();
        let mut block_digests: Vec<Vec<u8>> = Vec::new();
        let mut sha1 = Sha1::new();
        let mut written: u64 = 0;

        let mut page = self
            .revision_page(node, &revision.revision_id, next_index, BLOCK_PAGE_SIZE)
            .await?;
        let active = match page.state {
            Some(state) => state == REVISION_STATE_ACTIVE,
            None => !confirm_active,
        };
        if !active {
            return Err(ProtonError::RevisionNotActive);
        }
        let manifest_signature = page.manifest_signature.take().filter(|s| !s.is_empty());
        for thumbnail in std::mem::take(&mut page.thumbnails) {
            let hash = BASE64
                .decode(thumbnail.hash.trim())
                .map_err(|_| ProtonError::Integrity("thumbnail hash is not base64".into()))?;
            thumbnails.push((thumbnail.thumbnail_type, hash));
        }
        // apiService.ts: pages are read until one comes back empty. The pages
        // feed one stream of blocks, so the window of BLOCKS_IN_FLIGHT blocks
        // runs on across page boundaries as the SDK's `fileDownloader.ts`
        // does: before, the first block of a page was asked only once the
        // last one of the page before had arrived, and every page boundary
        // left a few blocks in flight instead of ten (a 200 MB file has two).
        let first_index = next_index;
        let (blocks_tx, blocks_rx) =
            tokio::sync::mpsc::channel::<BlockRef>(2 * BLOCK_PAGE_SIZE as usize);
        let pages = async {
            let blocks_tx = blocks_tx;
            let mut following = first_index;
            while !page.blocks.is_empty() {
                let mut blocks = std::mem::take(&mut page.blocks);
                blocks.sort_by_key(|b| b.index);
                // RevisionReader.cs: indices must be consecutive from 1.
                for (offset, block) in blocks.iter().enumerate() {
                    let expected = following + offset as u64;
                    if block.index != expected {
                        return Err(ProtonError::Integrity(format!(
                            "expected block {expected}, got {}",
                            block.index
                        )));
                    }
                }
                // The page that would close the file is left out when this
                // page was short and its blocks complete the count the
                // attributes give: the server has nothing more to send. The
                // manifest signature covers every block either way, so a
                // block the server failed to list still fails the download.
                let short = blocks.len() < BLOCK_PAGE_SIZE as usize;
                following += blocks.len() as u64;
                let complete = short
                    && !node.xattr.block_sizes.is_empty()
                    && following - 1 == node.xattr.block_sizes.len() as u64;
                // Read while the blocks before it download: the channel holds
                // two pages, so the next page is asked as soon as this one is
                // handed over.
                for block in blocks {
                    if blocks_tx.send(block).await.is_err() {
                        // The blocks stopped being read: a block failed, and
                        // its error is the one reported.
                        return Ok(());
                    }
                }
                if complete {
                    break;
                }
                page = self
                    .revision_page(node, &revision.revision_id, following, BLOCK_PAGE_SIZE)
                    .await?;
            }
            Ok::<(), ProtonError>(())
        };
        let downloads = async {
            let incoming = futures_util::stream::unfold(blocks_rx, |mut blocks_rx| async move {
                blocks_rx.recv().await.map(|block| (block, blocks_rx))
            });
            // Fetched up to BLOCKS_IN_FLIGHT at a time, handed over in order.
            let mut fetched = std::pin::pin!(incoming
                .map(|block| self.fetch_block(node, &revision.revision_id, session_key, block))
                .buffered(BLOCKS_IN_FLIGHT));
            while let Some(result) = fetched.next().await {
                let (plain, digest) = result?;
                sha1.update(&plain);
                out.write_all(&plain).await.map_err(ProtonError::LocalIo)?;
                written += plain.len() as u64;
                progress(written);
                block_digests.push(digest);
                next_index += 1;
            }
            Ok::<(), ProtonError>(())
        };
        let (listed, done) = tokio::join!(pages, downloads);
        done?;
        listed?;
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

#[cfg(test)]
mod tests {
    use super::super::account::test_account;
    use super::super::provider::DriveOps;
    use super::super::test_drive::{TestDrive, ROOT};
    use super::*;

    /// `count` blocks of a few bytes each, all different.
    fn blocks(count: usize) -> Vec<Vec<u8>> {
        (0..count)
            .map(|i| format!("block {i:03};").into_bytes())
            .collect()
    }

    fn refs(blocks: &[Vec<u8>]) -> Vec<&[u8]> {
        blocks.iter().map(Vec::as_slice).collect()
    }

    async fn download(
        client: &(impl DriveOps + ?Sized),
        link_id: &str,
    ) -> Result<Vec<u8>, ProtonError> {
        let mut out = Vec::new();
        client.download(link_id, &mut out, &mut |_| {}).await?;
        Ok(out)
    }

    /// A file the listing just decrypted downloads from that node: the
    /// revision page and the block, without loading the node again (the
    /// official client reads from its synced cache the same way).
    #[tokio::test]
    async fn a_listed_file_downloads_without_reloading_its_node() {
        let account = test_account();
        let mut drive = TestDrive::start(&account).await;
        let content = blocks(1);
        let (file, _) = drive.add_file(account.provider(), ROOT, "a.txt", &refs(&content));
        let client = drive.client(account);
        DriveOps::children(&client, ROOT).await.unwrap();
        drive.reset_counts();

        assert_eq!(download(&client, &file).await.unwrap(), content.concat());
        assert_eq!(drive.count("links"), 0, "{:?}", drive.log());
        assert_eq!(drive.count("storage"), 1);
    }

    /// A revision written after the listing: the listed node names the old
    /// one, the server says it is no longer active, and the download takes
    /// the node again and reads the new content.
    #[tokio::test]
    async fn a_revision_replaced_after_the_listing_downloads_the_new_content() {
        let account = test_account();
        let mut drive = TestDrive::start(&account).await;
        let old = blocks(2);
        let docs = drive.add_folder(account.provider(), ROOT, "docs");
        let (file, _) = drive.add_file(account.provider(), &docs, "a.txt", &refs(&old));
        let new = vec![b"new content".to_vec()];
        let client = drive.client(account);
        DriveOps::children(&client, ROOT).await.unwrap();
        DriveOps::children(&client, &docs).await.unwrap();
        drive.new_revision(client.provider(), &file, &refs(&new));
        drive.reset_counts();

        assert_eq!(download(&client, &file).await.unwrap(), new.concat());
        assert_eq!(drive.count("links"), 1, "{:?}", drive.log());
        // The obsolete revision's blocks are never fetched.
        assert_eq!(drive.count("storage"), 1);
    }

    /// The page that ends a file is read only when the blocks so far do not
    /// account for it: a short page whose blocks match the attributes ends
    /// the file, a full page (exactly the page size) is followed by another.
    #[tokio::test]
    async fn the_closing_revision_page_is_read_only_when_needed() {
        let account = test_account();
        let mut drive = TestDrive::start(&account).await;
        let page = BLOCK_PAGE_SIZE as usize;
        let cases = [
            (1, 1),
            (page - 1, 1),
            (page, 2),
            (page + 1, 2),
            (2 * page, 3),
        ];
        let mut files = Vec::new();
        for (count, _) in cases {
            let content = blocks(count);
            let (file, _) = drive.add_file(
                account.provider(),
                ROOT,
                &format!("{count}.bin"),
                &refs(&content),
            );
            files.push((file, content));
        }
        let client = drive.client(account);
        DriveOps::children(&client, ROOT).await.unwrap();
        for ((count, pages), (file, content)) in cases.iter().zip(&files) {
            drive.reset_counts();
            assert_eq!(
                download(&client, file).await.unwrap(),
                content.concat(),
                "{count} blocks"
            );
            assert_eq!(
                drive.count("revision"),
                *pages,
                "{count} blocks: {:?}",
                drive.log()
            );
            assert_eq!(drive.count("storage"), *count);
        }
    }

    /// Blocks flow across revision pages: the first block of the next page is
    /// asked while the last block of this page is still on its way, as the
    /// SDK's downloader does, instead of after the whole page arrived.
    #[tokio::test]
    async fn the_next_page_starts_before_this_page_ends() {
        let account = test_account();
        let mut drive = TestDrive::start(&account).await;
        let page = BLOCK_PAGE_SIZE as usize;
        let content = blocks(page + 5);
        let (file, _) = drive.add_file(account.provider(), ROOT, "a.bin", &refs(&content));
        let client = drive.client(account);
        DriveOps::children(&client, ROOT).await.unwrap();
        drive.hold_block_until_asked(page, page + 1);

        assert_eq!(download(&client, &file).await.unwrap(), content.concat());
        assert_eq!(
            drive.hold_released_by_request(),
            Some(true),
            "block {} was asked only after block {page} arrived",
            page + 1
        );
    }

    /// A server that drops the end of a file: the manifest signature covers
    /// every block, so the download fails instead of returning less, whether
    /// the page looks complete or not.
    #[tokio::test]
    async fn a_file_missing_its_last_blocks_fails_the_manifest() {
        let account = test_account();
        let mut drive = TestDrive::start(&account).await;
        let page = BLOCK_PAGE_SIZE as usize;
        let mut files = Vec::new();
        for count in [2, page + 1] {
            let content = blocks(count);
            let (file, revision) = drive.add_file(
                account.provider(),
                ROOT,
                &format!("{count}.bin"),
                &refs(&content),
            );
            drive.truncate_blocks(&file, &revision, count - 1);
            files.push(file);
        }
        let client = drive.client(account);
        DriveOps::children(&client, ROOT).await.unwrap();
        for file in files {
            let error = download(&client, &file).await.unwrap_err();
            assert!(
                matches!(&error, ProtonError::Integrity(reason) if reason.contains("manifest")),
                "{error:?}"
            );
        }
    }
}
