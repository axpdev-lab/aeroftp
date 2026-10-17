//! Upload of a new file and of a new revision of an existing file.
//!
//! Ported from the Proton Drive SDK at commit [`super::SDK_REFERENCE_COMMIT`]
//! (MIT, Copyright (c) 2025-2026 Proton AG, see `LICENSE-PROTON-DRIVE-SDK.md`):
//! `internal/upload/{apiService,cryptoService,blockVerifier,streamUploader,
//! manager,fileUploader}.ts`. Spec section 6 of
//! `APPENDIX-PROTON-DRIVE/05_port-spec.md`.
//!
//! The steps, in order: create the draft (new node key and content key, or
//! the existing ones for a revision); for every 4 MiB block, encrypt it with
//! the content key, prove to Proton it decrypts (the verification token),
//! sign it, request an upload URL and send it; sign the manifest of block
//! hashes; commit the revision with the extended attributes (size, block
//! sizes, modification time, SHA-1). Any failure after the draft exists
//! deletes the draft, so no half file is left behind.
//!
//! Format: content is written as SEIPDv1 AES-256 with a v3 key packet, the
//! legacy format every Proton client reads. A revision of a file whose content
//! key came from a v6 key packet (AEAD, written by the newer official
//! clients) is refused: Proton rejects revisions whose scheme does not match
//! the file, and `proton-crypto` cannot write AEAD yet.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use chrono::{DateTime, SecondsFormat, Utc};
use proton_crypto_account::proton_crypto::crypto::{
    PGPProviderSync, SessionKey, SessionKeyAlgorithm,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sha1::{Digest, Sha1};
use tokio::io::{AsyncRead, AsyncReadExt};

use super::client::DriveClient;
use super::drive_crypto;
use super::error::ProtonError;
use super::nodes::{validate_node_name, Node, LINK_STATE_ACTIVE};

/// `streamUploader.ts::FILE_CHUNK_SIZE`.
pub const BLOCK_SIZE: usize = 4 * 1024 * 1024;
/// Blocks prepared before their upload URLs are requested together. The SDK
/// buffers up to 15; fewer keeps memory at 20 MiB per upload.
const BLOCKS_PER_URL_REQUEST: usize = 5;
/// `streamUploader.ts::MAX_BLOCK_UPLOAD_RETRIES`.
const BLOCK_UPLOAD_RETRIES: u32 = 3;
/// The SDK refreshes an expired block token without a cap; the port caps it.
const TOKEN_REFRESH_LIMIT: u32 = 5;
/// Our own stale drafts are replaced at most this many times (C#: 3).
const DRAFT_CONFLICT_RETRIES: u32 = 3;

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct CreateFileResponse {
    file: CreatedFile,
}

#[derive(Deserialize)]
struct CreatedFile {
    #[serde(rename = "ID")]
    id: String,
    #[serde(rename = "RevisionID")]
    revision_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct CreateRevisionResponse {
    revision: CreatedRevision,
}

#[derive(Deserialize)]
struct CreatedRevision {
    #[serde(rename = "ID")]
    id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct VerificationData {
    verification_code: String,
    content_key_packet: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct BlockUploadResponse {
    #[serde(default)]
    upload_links: Vec<UploadLink>,
}

#[derive(Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
struct UploadLink {
    index: u64,
    #[serde(rename = "BareURL")]
    bare_url: String,
    token: String,
}

/// A block ready to send.
struct PreparedBlock {
    index: u64,
    encrypted: bytes::Bytes,
    enc_signature: String,
    token_b64: String,
}

/// What the block pipeline hands to the commit.
struct UploadedContent {
    manifest: Vec<u8>,
    block_sizes: Vec<u64>,
    sha1: String,
    total: u64,
}

/// The crypto context of the revision being written.
struct RevisionTarget<'a, P: PGPProviderSync> {
    link_id: &'a str,
    revision_id: &'a str,
    node_key: &'a P::PrivateKey,
    node_public: &'a P::PublicKey,
    content_key: &'a P::SessionKey,
}

/// `upload/apiService.ts`: `IntendedUploadSize` is sent with reduced
/// precision, `null` for small files, else floored to 100 kB.
fn intended_upload_size(size: u64) -> Value {
    if size <= 100_000 {
        Value::Null
    } else {
        json!(size / 100_000 * 100_000)
    }
}

/// `blockVerifier.ts`: the first bytes of the encrypted block XOR the
/// verification code, zero-padded when the block is shorter.
fn verification_token(code: &[u8], encrypted_block: &[u8]) -> Vec<u8> {
    code.iter()
        .enumerate()
        .map(|(i, byte)| byte ^ encrypted_block.get(i).copied().unwrap_or(0))
        .collect()
}

/// The `Common` extended attributes of a file (`extendedAttributes.ts`).
fn file_extended_attributes(
    size: u64,
    block_sizes: &[u64],
    sha1: &str,
    modification_time: Option<DateTime<Utc>>,
) -> String {
    let mut common = serde_json::Map::new();
    if let Some(time) = modification_time {
        common.insert(
            "ModificationTime".into(),
            json!(time.to_rfc3339_opts(SecondsFormat::Millis, true)),
        );
    }
    common.insert("Size".into(), json!(size));
    common.insert("BlockSizes".into(), json!(block_sizes));
    common.insert("Digests".into(), json!({ "SHA1": sha1 }));
    json!({ "Common": common }).to_string()
}

/// Reads up to one block; a short read only at end of stream.
async fn read_block<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Vec<u8>, ProtonError> {
    let mut block = vec![0u8; BLOCK_SIZE];
    let mut filled = 0;
    while filled < BLOCK_SIZE {
        let read = reader
            .read(&mut block[filled..])
            .await
            .map_err(ProtonError::LocalIo)?;
        if read == 0 {
            break;
        }
        filled += read;
    }
    block.truncate(filled);
    Ok(block)
}

impl<P: PGPProviderSync> DriveClient<P> {
    /// Uploads a new file named `name` into `parent`. Returns the new link
    /// ID. A name already taken is [`ProtonError::AlreadyExists`] with the
    /// existing link, on which [`Self::upload_revision`] can write instead.
    pub async fn upload_new_file<R: AsyncRead + Unpin>(
        &self,
        parent: &Node<P>,
        name: &str,
        reader: &mut R,
        size: u64,
        modification_time: Option<DateTime<Utc>>,
        mut progress: impl FnMut(u64),
    ) -> Result<String, ProtonError> {
        validate_node_name(name).map_err(ProtonError::InvalidName)?;
        let provider = self.provider();
        if !parent.is_folder() {
            return Err(ProtonError::Decode("the parent is not a folder".into()));
        }
        let parent_key = parent
            .node_key
            .clone()
            .ok_or_else(|| ProtonError::Crypto("the folder key is not available".into()))?;
        let parent_public = provider
            .private_key_to_public_key(&parent_key)
            .map_err(|error| ProtonError::Crypto(error.to_string()))?;
        let parent_hash_key = parent
            .hash_key
            .as_ref()
            .map(|key| key.to_vec())
            .ok_or_else(|| ProtonError::Crypto("the folder hash key is not available".into()))?;
        let (email, address_key) = self.signing_identity()?;

        let node = drive_crypto::generate_node_key(provider, &parent_public, &address_key)?;
        let node_public = provider
            .private_key_to_public_key(&node.key)
            .map_err(|error| ProtonError::Crypto(error.to_string()))?;
        let (content_key, content_key_packet, content_key_signature) =
            drive_crypto::generate_content_key(provider, &node_public, &node.key)?;
        let armored_name =
            drive_crypto::encrypt_node_name(provider, name, None, &parent_public, &address_key)?;
        let hash = drive_crypto::lookup_hash(&parent_hash_key, name);
        let mime_type = mime_guess::from_path(name)
            .first_or_octet_stream()
            .essence_str()
            .to_string();

        let body = json!({
            "ParentLinkID": parent.link_id,
            "Name": armored_name,
            "Hash": hash,
            "MIMEType": mime_type,
            "ClientUID": self.client_uid,
            "IntendedUploadSize": intended_upload_size(size),
            "NodeKey": node.armored_key,
            "NodePassphrase": node.armored_passphrase,
            "NodePassphraseSignature": node.armored_passphrase_signature,
            "ContentKeyPacket": content_key_packet,
            "ContentKeyPacketSignature": content_key_signature,
            "SignatureAddress": email,
        });
        let path = format!("drive/v2/volumes/{}/files", self.volume_id());
        let mut conflicts = 0;
        let created: CreateFileResponse = loop {
            match self.api.post(&path, &body).await {
                Ok(created) => break created,
                // Our own stale draft (same ClientUID) is deleted and the
                // draft created again (manager.ts).
                Err(ProtonError::AlreadyExists {
                    link_id: Some(link_id),
                    draft_client_uid: Some(owner),
                    is_draft: true,
                }) if owner == self.client_uid && conflicts < DRAFT_CONFLICT_RETRIES => {
                    conflicts += 1;
                    self.delete_draft_node(&link_id).await?;
                }
                Err(error) => return Err(error),
            }
        };

        let target = RevisionTarget {
            link_id: &created.file.id,
            revision_id: &created.file.revision_id,
            node_key: &node.key,
            node_public: &node_public,
            content_key: &content_key,
        };
        let result = self
            .write_and_commit(
                &target,
                &email,
                &address_key,
                reader,
                size,
                modification_time,
                &mut progress,
            )
            .await;
        if let Err(error) = result {
            if let Err(cleanup) = self.delete_draft_node(&created.file.id).await {
                log::warn!("Proton: failed upload draft could not be deleted: {cleanup}");
            }
            return Err(error);
        }
        Ok(created.file.id)
    }

    /// Uploads a new revision of the existing file `node`, reusing its node
    /// key and content key (`manager.ts`).
    pub async fn upload_revision<R: AsyncRead + Unpin>(
        &self,
        node: &Node<P>,
        reader: &mut R,
        size: u64,
        modification_time: Option<DateTime<Utc>>,
        mut progress: impl FnMut(u64),
    ) -> Result<(), ProtonError> {
        let provider = self.provider();
        let (Some(node_key), Some(content_key), Some(current)) = (
            node.node_key.as_ref(),
            node.content_key.as_ref(),
            node.active_revision.as_ref(),
        ) else {
            return Err(ProtonError::Crypto(
                "the file is missing the crypto material for a new revision".into(),
            ));
        };
        if content_key.algorithm() == SessionKeyAlgorithm::Unknown {
            return Err(ProtonError::RevisionUnsupported);
        }
        let node_public = provider
            .private_key_to_public_key(node_key)
            .map_err(|error| ProtonError::Crypto(error.to_string()))?;
        let (email, address_key) = self.signing_identity()?;

        let created: CreateRevisionResponse = self
            .api
            .post(
                &format!(
                    "drive/v2/volumes/{}/files/{}/revisions",
                    self.volume_id(),
                    node.link_id
                ),
                &json!({
                    "CurrentRevisionID": current.revision_id,
                    "ClientUID": self.client_uid,
                    "IntendedUploadSize": intended_upload_size(size),
                }),
            )
            .await?;

        let target = RevisionTarget {
            link_id: &node.link_id,
            revision_id: &created.revision.id,
            node_key,
            node_public: &node_public,
            content_key,
        };
        let result = self
            .write_and_commit(
                &target,
                &email,
                &address_key,
                reader,
                size,
                modification_time,
                &mut progress,
            )
            .await;
        if let Err(error) = result {
            let path = format!(
                "drive/v2/volumes/{}/files/{}/revisions/{}",
                self.volume_id(),
                node.link_id,
                created.revision.id
            );
            if let Err(cleanup) = self.api.delete::<Value>(&path).await {
                log::warn!("Proton: failed revision draft could not be deleted: {cleanup}");
            }
            return Err(error);
        }
        self.forget(&[&node.link_id]).await;
        Ok(())
    }

    /// `upload/apiService.ts::deleteDraft`.
    async fn delete_draft_node(&self, link_id: &str) -> Result<(), ProtonError> {
        let _: Value = self
            .api
            .post(
                &format!("drive/v2/volumes/{}/delete_multiple", self.volume_id()),
                &json!({ "LinkIDs": [link_id] }),
            )
            .await?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn write_and_commit<R: AsyncRead + Unpin>(
        &self,
        target: &RevisionTarget<'_, P>,
        email: &str,
        address_key: &P::PrivateKey,
        reader: &mut R,
        size: u64,
        modification_time: Option<DateTime<Utc>>,
        progress: &mut impl FnMut(u64),
    ) -> Result<(), ProtonError> {
        let content = self
            .upload_blocks(target, address_key, reader, size, progress)
            .await?;

        // streamUploader.ts: the uploaded content must be exactly what was
        // announced, or the file changed while it was read.
        let expected_blocks = size.div_ceil(BLOCK_SIZE as u64);
        if content.total != size || content.block_sizes.len() as u64 != expected_blocks {
            return Err(ProtonError::Integrity(format!(
                "uploaded {} bytes in {} blocks, expected {size} bytes in {expected_blocks}",
                content.total,
                content.block_sizes.len()
            )));
        }

        let provider = self.provider();
        let manifest_signature =
            drive_crypto::sign_detached(provider, &content.manifest, address_key)?;
        let xattr_json = file_extended_attributes(
            content.total,
            &content.block_sizes,
            &content.sha1,
            modification_time,
        );
        let xattr = drive_crypto::encrypt_extended_attributes(
            provider,
            &xattr_json,
            target.node_public,
            address_key,
        )?;
        let path = format!(
            "drive/v2/volumes/{}/files/{}/revisions/{}",
            self.volume_id(),
            target.link_id,
            target.revision_id
        );
        let commit = self
            .api
            .put::<Value>(
                &path,
                &json!({
                    "ManifestSignature": manifest_signature,
                    "SignatureAddress": email,
                    "XAttr": xattr,
                    "ChecksumVerified": false,
                    "Photo": Value::Null,
                }),
            )
            .await;
        match commit {
            Ok(_) => Ok(()),
            Err(error) => {
                // manager.ts: the commit may have landed with its answer
                // lost; the revision is active if so.
                let details = self.load_details(&[target.link_id.to_string()]).await;
                let landed = details
                    .ok()
                    .and_then(|d| d.into_iter().next())
                    .is_some_and(|d| {
                        d.link.state == Some(LINK_STATE_ACTIVE)
                            && d.file
                                .and_then(|f| f.active_revision)
                                .is_some_and(|r| r.revision_id == target.revision_id)
                    });
                if landed {
                    Ok(())
                } else {
                    Err(error)
                }
            }
        }
    }

    /// Reads, encrypts, verifies and sends every block; returns the manifest
    /// and the attributes the commit needs.
    async fn upload_blocks<R: AsyncRead + Unpin>(
        &self,
        target: &RevisionTarget<'_, P>,
        address_key: &P::PrivateKey,
        reader: &mut R,
        size: u64,
        progress: &mut impl FnMut(u64),
    ) -> Result<UploadedContent, ProtonError> {
        let provider = self.provider();
        let verification: VerificationData = self
            .api
            .get(
                &format!(
                    "drive/v2/volumes/{}/links/{}/revisions/{}/verification",
                    self.volume_id(),
                    target.link_id,
                    target.revision_id
                ),
                &[],
            )
            .await?;
        let verification_code = BASE64
            .decode(verification.verification_code.trim())
            .map_err(|_| ProtonError::Decode("verification code is not base64".into()))?;
        let verification_key = drive_crypto::decrypt_session_key_b64(
            provider,
            &verification.content_key_packet,
            target.node_key,
        )?;

        let mut manifest = Vec::new();
        let mut block_sizes = Vec::new();
        let mut sha1 = Sha1::new();
        let mut total: u64 = 0;
        let mut index: u64 = 0;
        let mut pending: Vec<PreparedBlock> = Vec::new();
        let expected_blocks = size.div_ceil(BLOCK_SIZE as u64);

        loop {
            let plain = if index < expected_blocks {
                read_block(reader).await?
            } else {
                Vec::new()
            };
            let at_end = plain.is_empty();
            if !at_end {
                index += 1;
                sha1.update(&plain);
                total += plain.len() as u64;
                block_sizes.push(plain.len() as u64);

                // Encrypt, then prove to Proton the block decrypts with the
                // key it knows; a failed check re-encrypts once.
                let mut encrypted =
                    drive_crypto::encrypt_block(provider, &plain, target.content_key)?;
                if drive_crypto::decrypt_block(provider, &encrypted, &verification_key).is_err() {
                    encrypted = drive_crypto::encrypt_block(provider, &plain, target.content_key)?;
                    drive_crypto::decrypt_block(provider, &encrypted, &verification_key).map_err(
                        |_| {
                            ProtonError::Integrity(format!("block {index} failed its verification"))
                        },
                    )?;
                }
                let token = verification_token(&verification_code, &encrypted);
                let enc_signature = drive_crypto::encrypt_block_signature(
                    provider,
                    &plain,
                    address_key,
                    target.content_key,
                    target.node_public,
                )?;
                let (digest, _) = drive_crypto::encrypted_block_hash(&encrypted);
                manifest.extend_from_slice(&digest);
                pending.push(PreparedBlock {
                    index,
                    encrypted: bytes::Bytes::from(encrypted),
                    enc_signature,
                    token_b64: BASE64.encode(token),
                });
            }
            if pending.len() >= BLOCKS_PER_URL_REQUEST || (at_end && !pending.is_empty()) {
                let batch = std::mem::take(&mut pending);
                self.send_blocks(target, &batch).await?;
                progress(total);
            }
            if at_end {
                break;
            }
        }

        Ok(UploadedContent {
            manifest,
            block_sizes,
            sha1: hex::encode(sha1.finalize()),
            total,
        })
    }

    async fn request_upload_links(
        &self,
        target: &RevisionTarget<'_, P>,
        blocks: &[&PreparedBlock],
    ) -> Result<Vec<UploadLink>, ProtonError> {
        let list: Vec<Value> = blocks
            .iter()
            .map(|block| {
                json!({
                    "Index": block.index,
                    "EncSignature": block.enc_signature,
                    "Verifier": { "Token": block.token_b64 },
                })
            })
            .collect();
        let response: BlockUploadResponse = self
            .api
            .post(
                "drive/blocks",
                &json!({
                    "AddressID": self.my_files.address_id,
                    "VolumeID": self.volume_id(),
                    "LinkID": target.link_id,
                    "RevisionID": target.revision_id,
                    "BlockList": list,
                    "ThumbnailList": [],
                }),
            )
            .await?;
        Ok(response.upload_links)
    }

    /// Sends a batch: one URL request, then each block in order. An expired
    /// token gets a fresh URL for that block; other failures are retried up
    /// to the SDK's limit.
    async fn send_blocks(
        &self,
        target: &RevisionTarget<'_, P>,
        batch: &[PreparedBlock],
    ) -> Result<(), ProtonError> {
        let refs: Vec<&PreparedBlock> = batch.iter().collect();
        let links = self.request_upload_links(target, &refs).await?;
        for block in batch {
            let mut link = links
                .iter()
                .find(|link| link.index == block.index)
                .cloned()
                .ok_or_else(|| {
                    ProtonError::Decode(format!("no upload URL for block {}", block.index))
                })?;
            let mut refreshes = 0;
            let mut failures = 0;
            loop {
                match self
                    .api
                    .storage_post_block(&link.bare_url, &link.token, block.encrypted.clone())
                    .await
                {
                    Ok(()) => break,
                    Err(ProtonError::StorageTokenExpired) if refreshes < TOKEN_REFRESH_LIMIT => {
                        refreshes += 1;
                        link = self
                            .request_upload_links(target, &[block])
                            .await?
                            .into_iter()
                            .next()
                            .ok_or_else(|| {
                                ProtonError::Decode(format!(
                                    "no upload URL for block {}",
                                    block.index
                                ))
                            })?;
                    }
                    Err(error @ (ProtonError::Network(_) | ProtonError::Timeout))
                        if failures < BLOCK_UPLOAD_RETRIES =>
                    {
                        failures += 1;
                        log::warn!(
                            "Proton: block {} upload failed ({error}), retrying",
                            block.index
                        );
                    }
                    Err(error) => return Err(error),
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intended_size_is_rounded_like_the_sdk() {
        assert_eq!(intended_upload_size(0), Value::Null);
        assert_eq!(intended_upload_size(100_000), Value::Null);
        assert_eq!(intended_upload_size(123_456), json!(100_000));
        assert_eq!(intended_upload_size(5_999_999), json!(5_900_000));
    }

    #[test]
    fn verification_token_is_xor_with_zero_padding() {
        let code = [0xffu8; 4];
        assert_eq!(
            verification_token(&code, &[0x0f, 0xf0]),
            vec![0xf0, 0x0f, 0xff, 0xff]
        );
        assert_eq!(
            verification_token(&code, &[1, 2, 3, 4, 5]),
            vec![0xfe, 0xfd, 0xfc, 0xfb]
        );
    }

    #[test]
    fn extended_attributes_have_the_sdk_shape() {
        let time = DateTime::from_timestamp_millis(1_792_185_300_123).unwrap();
        let json = file_extended_attributes(5, &[5], "abc", Some(time));
        let value: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["Common"]["Size"], json!(5));
        assert_eq!(value["Common"]["BlockSizes"], json!([5]));
        assert_eq!(value["Common"]["Digests"]["SHA1"], json!("abc"));
        assert_eq!(
            value["Common"]["ModificationTime"],
            json!("2026-10-16T21:15:00.123Z")
        );
        let without_time = file_extended_attributes(0, &[], "x", None);
        assert!(!without_time.contains("ModificationTime"));
    }

    #[tokio::test]
    async fn blocks_are_read_whole_until_the_end() {
        let data = vec![7u8; BLOCK_SIZE + 10];
        let mut reader = &data[..];
        assert_eq!(read_block(&mut reader).await.unwrap().len(), BLOCK_SIZE);
        assert_eq!(read_block(&mut reader).await.unwrap().len(), 10);
        assert!(read_block(&mut reader).await.unwrap().is_empty());
    }
}
