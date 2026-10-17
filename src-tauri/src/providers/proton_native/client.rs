//! The Drive client: My Files, node loading and decryption, listing, path
//! resolution and quota.
//!
//! Ported from the Proton Drive SDK at commit [`super::SDK_REFERENCE_COMMIT`]
//! (MIT, Copyright (c) 2025-2026 Proton AG, see `LICENSE-PROTON-DRIVE-SDK.md`):
//! `internal/shares/{apiService,cryptoService,manager}.ts`,
//! `internal/nodes/{apiService,cryptoService,nodesAccess}.ts` and
//! `incubating/account/js/src/addresses.ts` for other users' public keys.
//! Section numbers in comments refer to `APPENDIX-PROTON-DRIVE/05_port-spec.md`.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use std::collections::HashMap;
use std::sync::Arc;

use proton_crypto_account::proton_crypto::crypto::{DataEncoding, PGPProviderSync};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::Mutex;
use zeroize::Zeroizing;

use super::account::ProtonAccount;
use super::api::ProtonApi;
use super::drive_crypto::{self, Verified};
use super::error::ProtonError;
use super::nodes::{
    parse_extended_attributes, ActiveRevision, ExtendedAttributes, LinkDetails, LinksResponse,
    Node, NodeKind, LEGACY_CONTENT_KEY_CUTOFF, LINK_STATE_ACTIVE, NODE_TYPE_FILE, NODE_TYPE_FOLDER,
    UNSIGNED_HASH_KEY_CUTOFF, UNSIGNED_NAME_CUTOFF,
};

/// `nodes/apiService.ts::API_NODES_BATCH_SIZE`: at most 100 IDs per details
/// call.
const NODES_BATCH_SIZE: usize = 100;

/// `GET drive/v2/shares/my-files`, the fields the SDK reads (spec 1).
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct MyFilesResponse {
    volume: MyFilesVolume,
    share: MyFilesShare,
    link: MyFilesLinkWrapper,
}

#[derive(Deserialize)]
struct MyFilesVolume {
    #[serde(rename = "VolumeID")]
    volume_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct MyFilesShare {
    #[serde(rename = "ShareID")]
    share_id: String,
    creator_email: String,
    key: String,
    passphrase: String,
    #[serde(default)]
    passphrase_signature: Option<String>,
    #[serde(rename = "AddressID")]
    address_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct MyFilesLinkWrapper {
    link: MyFilesLink,
}

#[derive(Deserialize)]
struct MyFilesLink {
    #[serde(rename = "LinkID")]
    link_id: String,
}

/// `GET drive/v2/volumes/{v}/folders/{l}/children` (spec 2.1).
#[derive(Deserialize)]
struct ChildrenPage {
    #[serde(rename = "LinkIDs", default)]
    link_ids: Vec<String>,
    #[serde(rename = "AnchorID", default)]
    anchor_id: Option<String>,
    #[serde(rename = "More", default)]
    more: bool,
}

/// `GET core/v4/keys/all`.
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct AllKeysResponse {
    #[serde(default)]
    address: Option<AllKeysAddress>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct AllKeysAddress {
    #[serde(default)]
    keys: Vec<AllKeysKey>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct AllKeysKey {
    public_key: String,
}

/// `GET core/v4/users`, the quota fields. `MaxDriveSpace` and
/// `UsedDriveSpace` are not in the SDK's types; they are read if the live
/// API sends them and ignored otherwise (spec 11).
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct UsersQuotaResponse {
    user: UserQuota,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct UserQuota {
    #[serde(default)]
    max_space: Option<u64>,
    #[serde(default)]
    used_space: Option<u64>,
    #[serde(default)]
    max_drive_space: Option<u64>,
    #[serde(default)]
    used_drive_space: Option<u64>,
    #[serde(default)]
    product_used_space: Option<ProductUsedSpace>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ProductUsedSpace {
    #[serde(default)]
    drive: Option<u64>,
}

/// Storage quota as AeroFTP reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quota {
    pub used: u64,
    pub total: u64,
}

impl UserQuota {
    /// Drive-specific figures when the API sends them, else the account-wide
    /// ones (the Drive part of the used space when broken down).
    fn quota(&self) -> Quota {
        let total = self.max_drive_space.or(self.max_space).unwrap_or(0);
        let used = self
            .used_drive_space
            .or_else(|| self.product_used_space.as_ref().and_then(|p| p.drive))
            .or(self.used_space)
            .unwrap_or(0);
        Quota { used, total }
    }
}

/// The decrypted My Files share.
pub struct MyFiles<P: PGPProviderSync> {
    pub volume_id: String,
    pub share_id: String,
    pub root_link_id: String,
    pub address_id: String,
    pub share_key: P::PrivateKey,
    pub share_verified: Verified,
}

/// A signed-in Drive client over one account's My Files.
pub struct DriveClient<P: PGPProviderSync> {
    pub(crate) api: Arc<ProtonApi>,
    pub(crate) account: ProtonAccount<P>,
    pub(crate) my_files: MyFiles<P>,
    nodes: Mutex<HashMap<String, Arc<Node<P>>>>,
    other_public_keys: Mutex<HashMap<String, Vec<P::PublicKey>>>,
    /// Identifies this client's drafts, so a stale draft of ours can be
    /// replaced (`ClientUID` in the SDK).
    pub(crate) client_uid: String,
}

impl<P: PGPProviderSync> std::fmt::Debug for DriveClient<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DriveClient")
            .field("volume_id", &self.my_files.volume_id)
            .field("share_id", &self.my_files.share_id)
            .finish_non_exhaustive()
    }
}

/// Imports armored public keys from `core/v4/keys/all`; keys that do not
/// parse are skipped.
fn import_public_keys<P: PGPProviderSync>(provider: &P, keys: &[AllKeysKey]) -> Vec<P::PublicKey> {
    keys.iter()
        .filter_map(|key| {
            provider
                .public_key_import(key.public_key.as_bytes(), DataEncoding::Armor)
                .ok()
        })
        .collect()
}

impl<P: PGPProviderSync> DriveClient<P> {
    /// Loads My Files (spec 1) and decrypts its share key with the own
    /// address that owns the share, verifying the passphrase signature with
    /// the share creator's keys. A missing volume is an error: AeroFTP does
    /// not create the default volume behind the user's back.
    pub async fn open(api: Arc<ProtonApi>, account: ProtonAccount<P>) -> Result<Self, ProtonError> {
        let response: MyFilesResponse = api
            .get("drive/v2/shares/my-files", &[])
            .await
            .map_err(|error| match error {
                ProtonError::Api { code: 2501, .. } => ProtonError::NotFound(
                    "this Proton account has no Drive volume yet; open Proton Drive once to create it"
                        .into(),
                ),
                other => other,
            })?;
        let share = response.share;

        let verification_keys = match account.address(&share.creator_email) {
            Ok(address) => address
                .keys
                .iter()
                .map(|key| key.public_key.clone())
                .collect::<Vec<_>>(),
            Err(_) => {
                Self::fetch_public_keys(&api, account.provider(), &share.creator_email).await?
            }
        };
        let address = account.address(&share.address_id)?;
        let decryption_keys: Vec<&P::PrivateKey> =
            address.keys.iter().map(|key| &key.private_key).collect();
        let verification_refs: Vec<&P::PublicKey> = verification_keys.iter().collect();
        let (share_key, share_verified) = drive_crypto::decrypt_key(
            account.provider(),
            &share.key,
            &share.passphrase,
            share.passphrase_signature.as_deref(),
            &decryption_keys,
            &verification_refs,
        )?;
        if !share_verified.is_valid() {
            log::warn!("Proton: the My Files share passphrase signature did not verify");
        }

        Ok(Self {
            api,
            my_files: MyFiles {
                volume_id: response.volume.volume_id,
                share_id: share.share_id,
                root_link_id: response.link.link.link_id,
                address_id: share.address_id,
                share_key,
                share_verified,
            },
            account,
            nodes: Mutex::new(HashMap::new()),
            other_public_keys: Mutex::new(HashMap::new()),
            client_uid: format!("aeroftp-{}", uuid::Uuid::new_v4()),
        })
    }

    pub fn provider(&self) -> &P {
        self.account.provider()
    }

    pub fn volume_id(&self) -> &str {
        &self.my_files.volume_id
    }

    async fn fetch_public_keys(
        api: &ProtonApi,
        provider: &P,
        email: &str,
    ) -> Result<Vec<P::PublicKey>, ProtonError> {
        match api
            .get::<AllKeysResponse>(
                "core/v4/keys/all",
                &[
                    ("Email", email.to_string()),
                    ("InternalOnly", "1".to_string()),
                ],
            )
            .await
        {
            Ok(response) => Ok(response
                .address
                .map(|address| import_public_keys(provider, &address.keys))
                .unwrap_or_default()),
            // `addresses.ts`: an unknown address has no keys, not an error.
            Err(error) if error.is_address_not_found() => Ok(Vec::new()),
            Err(error) => Err(error),
        }
    }

    /// `addresses.ts::getPublicKeys`: own addresses use their unlocked keys,
    /// other emails are looked up once and cached.
    pub async fn public_keys(&self, email: &str) -> Result<Vec<P::PublicKey>, ProtonError> {
        if let Ok(address) = self.account.address(email) {
            return Ok(address
                .keys
                .iter()
                .map(|key| key.public_key.clone())
                .collect());
        }
        if let Some(keys) = self.other_public_keys.lock().await.get(email) {
            return Ok(keys.clone());
        }
        let keys = Self::fetch_public_keys(&self.api, self.provider(), email).await?;
        self.other_public_keys
            .lock()
            .await
            .insert(email.to_string(), keys.clone());
        Ok(keys)
    }

    async fn optional_public_keys(&self, email: Option<&str>) -> Vec<P::PublicKey> {
        match email {
            Some(email) => self.public_keys(email).await.unwrap_or_else(|error| {
                log::warn!("Proton: public keys of a signer could not be loaded: {error}");
                Vec::new()
            }),
            None => Vec::new(),
        }
    }

    /// `POST drive/v2/volumes/{v}/links` (spec 2.2). Unknown IDs are simply
    /// absent from the answer.
    pub(crate) async fn load_details(
        &self,
        link_ids: &[String],
    ) -> Result<Vec<LinkDetails>, ProtonError> {
        let mut all = Vec::with_capacity(link_ids.len());
        for chunk in link_ids.chunks(NODES_BATCH_SIZE) {
            let response: LinksResponse = self
                .api
                .post(
                    &format!("drive/v2/volumes/{}/links", self.my_files.volume_id),
                    &json!({ "LinkIDs": chunk }),
                )
                .await?;
            all.extend(response.links);
        }
        Ok(all)
    }

    /// The node `link_id`, from cache or loaded with its ancestors (the
    /// root's parent key is the share key).
    pub async fn node(&self, link_id: &str) -> Result<Arc<Node<P>>, ProtonError> {
        if let Some(node) = self.nodes.lock().await.get(link_id) {
            return Ok(Arc::clone(node));
        }
        let details = self
            .load_details(&[link_id.to_string()])
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| ProtonError::NotFound(format!("node {link_id}")))?;
        let parent_key = self
            .parent_key(details.link.parent_link_id.as_deref())
            .await?;
        let node = Arc::new(self.decrypt_node(details, &parent_key).await);
        self.nodes
            .lock()
            .await
            .insert(link_id.to_string(), Arc::clone(&node));
        Ok(node)
    }

    async fn parent_key(&self, parent_link_id: Option<&str>) -> Result<P::PrivateKey, ProtonError> {
        match parent_link_id {
            None => Ok(self.my_files.share_key.clone()),
            Some(parent) => {
                let parent = Box::pin(self.node(parent)).await?;
                parent.node_key.clone().ok_or_else(|| {
                    ProtonError::Crypto("the parent folder key is not available".into())
                })
            }
        }
    }

    pub async fn root(&self) -> Result<Arc<Node<P>>, ProtonError> {
        let root = self.my_files.root_link_id.clone();
        self.node(&root).await
    }

    /// Drops cached nodes after a change, so the next read reloads them.
    pub async fn forget(&self, link_ids: &[&str]) {
        let mut nodes = self.nodes.lock().await;
        for id in link_ids {
            nodes.remove(*id);
        }
    }

    pub async fn forget_all(&self) {
        self.nodes.lock().await.clear();
    }

    /// Child IDs of a folder, every page (spec 2.1). Never cached, as in the
    /// SDK.
    async fn child_ids(&self, folder_link_id: &str) -> Result<Vec<String>, ProtonError> {
        let path = format!(
            "drive/v2/volumes/{}/folders/{}/children",
            self.my_files.volume_id, folder_link_id
        );
        let mut ids = Vec::new();
        let mut anchor: Option<String> = None;
        loop {
            let query: Vec<(&str, String)> = anchor
                .as_ref()
                .map(|anchor| vec![("AnchorID", anchor.clone())])
                .unwrap_or_default();
            let page: ChildrenPage = self.api.get(&path, &query).await?;
            ids.extend(page.link_ids);
            match (page.more, page.anchor_id) {
                (true, Some(next)) => anchor = Some(next),
                _ => break,
            }
        }
        Ok(ids)
    }

    /// The active children of a folder, decrypted. Drafts and trashed nodes
    /// are left out (spec 2.4: the SDK skips drafts; trashed ones are dropped
    /// defensively).
    pub async fn children(&self, folder: &Node<P>) -> Result<Vec<Arc<Node<P>>>, ProtonError> {
        if !folder.is_folder() {
            return Err(ProtonError::Decode("not a folder".into()));
        }
        let folder_key = folder
            .node_key
            .clone()
            .ok_or_else(|| ProtonError::Crypto("the folder key is not available".into()))?;
        let ids = self.child_ids(&folder.link_id).await?;
        let details = self.load_details(&ids).await?;
        let mut children = Vec::with_capacity(details.len());
        for detail in details {
            let link = &detail.link;
            let is_draft = link.node_type == NODE_TYPE_FILE
                && detail
                    .file
                    .as_ref()
                    .is_none_or(|f| f.active_revision.is_none());
            let inactive = link.state.is_some_and(|state| state != LINK_STATE_ACTIVE)
                || link.trash_time.is_some();
            if is_draft || inactive {
                continue;
            }
            let link_id = link.link_id.clone();
            let node = Arc::new(self.decrypt_node(detail, &folder_key).await);
            self.nodes.lock().await.insert(link_id, Arc::clone(&node));
            children.push(node);
        }
        Ok(children)
    }

    /// `cryptoService.ts::decryptNode` (spec 2.5): every field degrades on
    /// its own; nothing here fails the whole node.
    async fn decrypt_node(&self, details: LinkDetails, parent_key: &P::PrivateKey) -> Node<P> {
        let provider = self.provider();
        let link = details.link;
        let mut errors = Vec::new();

        let kind = match link.node_type {
            NODE_TYPE_FOLDER => NodeKind::Folder,
            NODE_TYPE_FILE => NodeKind::File,
            other => {
                errors.push(format!("unsupported node type {other}"));
                NodeKind::File
            }
        };

        let parent_public = provider.private_key_to_public_key(parent_key).ok();
        let signature_email_keys = self
            .optional_public_keys(link.signature_email.as_deref())
            .await;
        // The anonymous rule: without a signature email, the parent key
        // signs.
        let key_verification_keys: Vec<P::PublicKey> = if link.signature_email.is_some() {
            signature_email_keys.clone()
        } else {
            parent_public.iter().cloned().collect()
        };
        let name_verification_keys: Vec<P::PublicKey> =
            if link.name_signature_email == link.signature_email {
                key_verification_keys.clone()
            } else if link.name_signature_email.is_some() {
                self.optional_public_keys(link.name_signature_email.as_deref())
                    .await
            } else {
                parent_public.iter().cloned().collect()
            };

        // Node key.
        let key_refs: Vec<&P::PublicKey> = key_verification_keys.iter().collect();
        let (node_key, mut key_verified) = match drive_crypto::decrypt_key(
            provider,
            &link.node_key,
            &link.node_passphrase,
            link.node_passphrase_signature.as_deref(),
            &[parent_key],
            &key_refs,
        ) {
            Ok((key, verified)) => (Some(key), verified),
            Err(error) => {
                errors.push(format!("node key: {error}"));
                (None, Verified::Invalid("node key not decrypted".into()))
            }
        };
        let node_public = node_key
            .as_ref()
            .and_then(|key| provider.private_key_to_public_key(key).ok());

        // Name, with the parent key.
        let name_refs: Vec<&P::PublicKey> = name_verification_keys.iter().collect();
        let (name, name_verified) =
            match drive_crypto::decrypt_inline(provider, &link.name, parent_key, &name_refs) {
                Ok((bytes, verified)) => {
                    let verified = match verified {
                        Verified::NotSigned if link.create_time < UNSIGNED_NAME_CUTOFF => {
                            Verified::Valid
                        }
                        other => other,
                    };
                    match String::from_utf8(bytes) {
                        Ok(name) => (Ok(name), verified),
                        Err(_) => (Err("the name is not UTF-8".to_string()), verified),
                    }
                }
                Err(error) => (
                    Err(format!("name: {error}")),
                    Verified::Invalid("name not decrypted".into()),
                ),
            };

        let mut hash_key = None;
        let mut content_key = None;
        let mut active_revision = None;
        let mut xattr = ExtendedAttributes::default();
        let mut mime_type = None;
        let mut total_encrypted_size = None;

        if let (Some(node_key), Some(node_public)) = (node_key.as_ref(), node_public.as_ref()) {
            match kind {
                NodeKind::Folder => {
                    let folder = details.folder.unwrap_or(super::nodes::FolderDto {
                        node_hash_key: None,
                        xattr: None,
                    });
                    if let Some(armored) = folder.node_hash_key.as_deref() {
                        let mut refs: Vec<&P::PublicKey> = vec![node_public];
                        refs.extend(signature_email_keys.iter());
                        match drive_crypto::decrypt_inline(provider, armored, node_key, &refs) {
                            Ok((bytes, verified)) => {
                                let verified = match verified {
                                    Verified::NotSigned
                                        if link.create_time < UNSIGNED_HASH_KEY_CUTOFF =>
                                    {
                                        Verified::Valid
                                    }
                                    other => other,
                                };
                                if key_verified.is_valid() && !verified.is_valid() {
                                    key_verified = verified;
                                }
                                hash_key = Some(Zeroizing::new(bytes));
                            }
                            Err(error) => errors.push(format!("hash key: {error}")),
                        }
                    }
                    if let Some(armored) = folder.xattr.as_deref() {
                        let refs: Vec<&P::PublicKey> = if link.signature_email.is_some() {
                            signature_email_keys.iter().collect()
                        } else {
                            vec![node_public]
                        };
                        match drive_crypto::decrypt_inline(provider, armored, node_key, &refs) {
                            Ok((bytes, _)) => xattr = parse_extended_attributes(&bytes, 0),
                            Err(error) => errors.push(format!("folder attributes: {error}")),
                        }
                    }
                }
                NodeKind::File => {
                    if let Some(file) = details.file {
                        mime_type = file.media_type.clone();
                        total_encrypted_size = file.total_encrypted_size;
                        let mut refs: Vec<&P::PublicKey> = vec![node_public];
                        refs.extend(key_verification_keys.iter());
                        match drive_crypto::decrypt_content_key(
                            provider,
                            &file.content_key_packet,
                            file.content_key_packet_signature.as_deref(),
                            node_key,
                            &refs,
                        ) {
                            Ok((session_key, verified)) => {
                                let verified = match verified {
                                    Some(Verified::Invalid(reason))
                                        if link.create_time < LEGACY_CONTENT_KEY_CUTOFF =>
                                    {
                                        // Old clients signed the content key
                                        // with an address key: retry with all
                                        // of ours.
                                        let own: Vec<P::PublicKey> = self
                                            .account
                                            .addresses()
                                            .iter()
                                            .flat_map(|a| {
                                                a.keys.iter().map(|k| k.public_key.clone())
                                            })
                                            .collect();
                                        let own_refs: Vec<&P::PublicKey> = own.iter().collect();
                                        match file.content_key_packet_signature.as_deref() {
                                            Some(signature) => Some(drive_crypto::verify_detached(
                                                provider,
                                                proton_crypto_account::proton_crypto::crypto::SessionKey::export(&session_key).as_ref(),
                                                signature,
                                                &own_refs,
                                            )),
                                            None => Some(Verified::Invalid(reason)),
                                        }
                                    }
                                    other => other,
                                };
                                if let Some(verified) = verified {
                                    if key_verified.is_valid() && !verified.is_valid() {
                                        key_verified = verified;
                                    }
                                }
                                content_key = Some(session_key);
                            }
                            Err(error) => errors.push(format!("content key: {error}")),
                        }
                        if let Some(revision) = file.active_revision {
                            if let Some(armored) = revision.xattr.as_deref() {
                                let revision_keys = self
                                    .optional_public_keys(revision.signature_email.as_deref())
                                    .await;
                                let refs: Vec<&P::PublicKey> = if revision.signature_email.is_some()
                                {
                                    revision_keys.iter().collect()
                                } else {
                                    vec![node_public]
                                };
                                match drive_crypto::decrypt_inline(
                                    provider, armored, node_key, &refs,
                                ) {
                                    Ok((bytes, _)) => {
                                        xattr =
                                            parse_extended_attributes(&bytes, revision.create_time)
                                    }
                                    Err(error) => errors.push(format!("file attributes: {error}")),
                                }
                            }
                            active_revision = Some(ActiveRevision {
                                revision_id: revision.revision_id,
                                create_time: revision.create_time,
                                encrypted_size: revision.encrypted_size,
                                manifest_signature: revision
                                    .manifest_signature
                                    .filter(|s| !s.is_empty()),
                                signature_email: revision.signature_email,
                            });
                        }
                    } else {
                        errors.push("file without file details".into());
                    }
                }
            }
        }

        if !errors.is_empty() {
            log::warn!(
                "Proton: node {} decrypted with {} issue(s)",
                link.link_id,
                errors.len()
            );
        }

        Node {
            link_id: link.link_id,
            parent_link_id: link.parent_link_id,
            kind,
            name,
            name_verified,
            name_hash: link.name_hash,
            create_time: link.create_time,
            modify_time: link.modify_time,
            mime_type,
            node_key,
            key_verified,
            hash_key,
            content_key,
            active_revision,
            xattr,
            total_encrypted_size,
            signature_email: link.signature_email,
            name_signature_email: link.name_signature_email,
            armored_name: link.name,
            armored_node_passphrase: link.node_passphrase,
            errors,
        }
    }

    /// Resolves an absolute AeroFTP path (`/`, `/a/b.txt`) from the My Files
    /// root by listing each folder on the way. Names are compared exactly:
    /// Proton does not normalise them and allows no duplicates in a folder.
    pub async fn resolve_path(&self, path: &str) -> Result<Arc<Node<P>>, ProtonError> {
        let mut current = self.root().await?;
        for component in path.split('/').filter(|c| !c.is_empty() && *c != ".") {
            let children = self.children(&current).await?;
            current = children
                .into_iter()
                .find(|child| child.display_name() == Some(component))
                .ok_or_else(|| ProtonError::NotFound(path.to_string()))?;
        }
        Ok(current)
    }

    /// Quota from `core/v4/users` (spec 11).
    pub async fn quota(&self) -> Result<Quota, ProtonError> {
        let response: UsersQuotaResponse = self.api.get("core/v4/users", &[]).await?;
        Ok(response.user.quota())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quota_prefers_drive_figures_then_the_drive_share_of_used_space() {
        let drive: UsersQuotaResponse = serde_json::from_str(
            r#"{"User":{"MaxSpace":500,"UsedSpace":300,"MaxDriveSpace":200,"UsedDriveSpace":50}}"#,
        )
        .unwrap();
        assert_eq!(
            drive.user.quota(),
            Quota {
                used: 50,
                total: 200
            }
        );

        let product: UsersQuotaResponse = serde_json::from_str(
            r#"{"User":{"MaxSpace":500,"UsedSpace":300,"ProductUsedSpace":{"Drive":120,"Mail":180}}}"#,
        )
        .unwrap();
        assert_eq!(
            product.user.quota(),
            Quota {
                used: 120,
                total: 500
            }
        );

        let plain: UsersQuotaResponse =
            serde_json::from_str(r#"{"User":{"MaxSpace":500,"UsedSpace":300}}"#).unwrap();
        assert_eq!(
            plain.user.quota(),
            Quota {
                used: 300,
                total: 500
            }
        );
    }

    #[test]
    fn children_page_shapes_deserialize() {
        let page: ChildrenPage =
            serde_json::from_str(r#"{"Code":1000,"LinkIDs":["a","b"],"AnchorID":"b","More":true}"#)
                .unwrap();
        assert_eq!(page.link_ids, vec!["a", "b"]);
        assert!(page.more);
        let last: ChildrenPage =
            serde_json::from_str(r#"{"Code":1000,"LinkIDs":[],"AnchorID":null,"More":false}"#)
                .unwrap();
        assert!(!last.more && last.anchor_id.is_none());
    }
}
