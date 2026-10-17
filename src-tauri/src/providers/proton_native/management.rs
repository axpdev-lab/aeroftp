//! Folder creation, rename, move, trash, restore and permanent delete.
//!
//! Ported from the Proton Drive SDK at commit [`super::SDK_REFERENCE_COMMIT`]
//! (MIT, Copyright (c) 2025-2026 Proton AG, see `LICENSE-PROTON-DRIVE-SDK.md`):
//! `internal/nodes/{nodesManagement,cryptoService,apiService}.ts`. Spec
//! sections 7 to 10 of `APPENDIX-PROTON-DRIVE/05_port-spec.md`.
//!
//! Every write is signed by the primary key of the address that owns the My
//! Files share, with that address as the signature email. Moves re-encrypt
//! only the node passphrase key packet, the name and its lookup hash: the
//! node key, its content and all descendants stay as they are.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use proton_crypto_account::proton_crypto::crypto::PGPProviderSync;
use serde::Deserialize;
use serde_json::{json, Value};

use super::client::DriveClient;
use super::drive_crypto;
use super::error::ProtonError;
use super::nodes::{validate_node_name, Node};

/// Batch link operations accept at most 100 IDs (`nodes/apiService.ts`).
const LINK_BATCH_SIZE: usize = 100;

/// `errorCodes.ts` NOT_FOUND: on the trash step it means "not active", i.e.
/// already trashed (or gone).
const CODE_NOT_ACTIVE: i64 = 2501;

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct CreateFolderResponse {
    folder: CreatedFolder,
}

#[derive(Deserialize)]
struct CreatedFolder {
    #[serde(rename = "ID")]
    id: String,
}

/// `{Code: 1001, Responses: [{LinkID, Response: {Code, Error}}]}`.
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct BatchResponse {
    #[serde(default)]
    responses: Vec<BatchItem>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct BatchItem {
    #[serde(rename = "LinkID", default)]
    link_id: Option<String>,
    response: BatchItemResponse,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct BatchItemResponse {
    #[serde(default)]
    code: i64,
    #[serde(default)]
    error: Option<String>,
}

/// A link succeeded when its code is 1000 to 1002 without an error, or its
/// code is one the caller tolerates.
fn check_batch(response: BatchResponse, what: &str, tolerated: &[i64]) -> Result<(), ProtonError> {
    for item in response.responses {
        let ok = (matches!(item.response.code, 1000..=1002) && item.response.error.is_none())
            || tolerated.contains(&item.response.code);
        if !ok {
            return Err(ProtonError::Api {
                status: 200,
                code: item.response.code,
                message: format!(
                    "{what} failed for {}: {}",
                    item.link_id.unwrap_or_default(),
                    item.response.error.unwrap_or_default()
                ),
            });
        }
    }
    Ok(())
}

/// What writing into a folder needs: its key (to re-encrypt), its public
/// key (to encrypt children's names and passphrases) and its hash key (for
/// the children's lookup hashes).
struct FolderKeys<P: PGPProviderSync> {
    key: P::PrivateKey,
    public: P::PublicKey,
    hash_key: Vec<u8>,
}

impl<P: PGPProviderSync> DriveClient<P> {
    /// The address that signs writes, and its primary private key.
    pub(crate) fn signing_identity(&self) -> Result<(String, P::PrivateKey), ProtonError> {
        let address = self.account.address(&self.my_files.address_id)?;
        let key = address
            .keys
            .iter()
            .find(|key| key.primary)
            .or_else(|| address.keys.first())
            .ok_or_else(|| ProtonError::Crypto("the share address has no usable key".into()))?;
        Ok((address.email.clone(), key.private_key.clone()))
    }

    fn folder_keys(&self, folder: &Node<P>) -> Result<FolderKeys<P>, ProtonError> {
        if !folder.is_folder() {
            return Err(ProtonError::Decode(
                "creating items in non-folders is not allowed".into(),
            ));
        }
        let key = folder
            .node_key
            .clone()
            .ok_or_else(|| ProtonError::Crypto("the folder key is not available".into()))?;
        let public = self
            .provider()
            .private_key_to_public_key(&key)
            .map_err(|error| ProtonError::Crypto(error.to_string()))?;
        let hash_key = folder
            .hash_key
            .as_ref()
            .map(|hash_key| hash_key.to_vec())
            .ok_or_else(|| ProtonError::Crypto("the folder hash key is not available".into()))?;
        Ok(FolderKeys {
            key,
            public,
            hash_key,
        })
    }

    /// `POST drive/v2/volumes/{v}/folders` (spec 7). Returns the new link ID.
    /// A name already taken is API code 2500.
    pub async fn create_folder(&self, parent: &Node<P>, name: &str) -> Result<String, ProtonError> {
        validate_node_name(name).map_err(ProtonError::Decode)?;
        let FolderKeys {
            public: parent_public,
            hash_key: parent_hash_key,
            ..
        } = self.folder_keys(parent)?;
        let (email, address_key) = self.signing_identity()?;
        let provider = self.provider();

        let node = drive_crypto::generate_node_key(provider, &parent_public, &address_key)?;
        let armored_name =
            drive_crypto::encrypt_node_name(provider, name, None, &parent_public, &address_key)?;
        let hash = drive_crypto::lookup_hash(&parent_hash_key, name);
        let (_, armored_hash_key) = drive_crypto::generate_hash_key(provider, &node.key)?;

        let response: CreateFolderResponse = self
            .api
            .post(
                &format!("drive/v2/volumes/{}/folders", self.volume_id()),
                &json!({
                    "ParentLinkID": parent.link_id,
                    "NodeKey": node.armored_key,
                    "NodeHashKey": armored_hash_key,
                    "NodePassphrase": node.armored_passphrase,
                    "NodePassphraseSignature": node.armored_passphrase_signature,
                    "SignatureEmail": email,
                    "Name": armored_name,
                    "Hash": hash,
                }),
            )
            .await?;
        Ok(response.folder.id)
    }

    /// `PUT drive/v2/volumes/{v}/links/{l}/rename` (spec 8): the new name is
    /// encrypted with the existing name session key; `OriginalHash` guards
    /// against a concurrent change (API code 2000).
    pub async fn rename(&self, node: &Node<P>, new_name: &str) -> Result<(), ProtonError> {
        validate_node_name(new_name).map_err(ProtonError::Decode)?;
        let original_hash = node
            .name_hash
            .clone()
            .ok_or_else(|| ProtonError::Decode("the root folder cannot be renamed".into()))?;
        let parent_id = node
            .parent_link_id
            .clone()
            .ok_or_else(|| ProtonError::Decode("the root folder cannot be renamed".into()))?;
        let parent = self.node(&parent_id).await?;
        let FolderKeys {
            key: parent_key,
            public: parent_public,
            hash_key: parent_hash_key,
        } = self.folder_keys(&parent)?;
        let (email, address_key) = self.signing_identity()?;
        let provider = self.provider();

        let name_session_key =
            drive_crypto::message_session_key(provider, &node.armored_name, &parent_key)?;
        let armored_name = drive_crypto::encrypt_node_name(
            provider,
            new_name,
            Some(&name_session_key),
            &parent_public,
            &address_key,
        )?;
        let hash = drive_crypto::lookup_hash(&parent_hash_key, new_name);

        let _: Value = self
            .api
            .put(
                &format!(
                    "drive/v2/volumes/{}/links/{}/rename",
                    self.volume_id(),
                    node.link_id
                ),
                &json!({
                    "Name": armored_name,
                    "NameSignatureEmail": email,
                    "Hash": hash,
                    "OriginalHash": original_hash,
                }),
            )
            .await?;
        self.forget(&[&node.link_id]).await;
        Ok(())
    }

    /// `PUT drive/v2/volumes/{v}/links/{l}/move` (spec 9), optionally under a
    /// new name. The passphrase keeps its plaintext and session key and gets
    /// a key packet for the new parent; the name is re-encrypted with its
    /// session key; the hash uses the new parent's hash key.
    pub async fn move_node(
        &self,
        node: &Node<P>,
        new_parent: &Node<P>,
        new_name: Option<&str>,
    ) -> Result<(), ProtonError> {
        let original_hash = node
            .name_hash
            .clone()
            .ok_or_else(|| ProtonError::Decode("the root folder cannot be moved".into()))?;
        let old_parent_id = node
            .parent_link_id
            .clone()
            .ok_or_else(|| ProtonError::Decode("the root folder cannot be moved".into()))?;
        let name = match new_name {
            Some(name) => name.to_string(),
            None => node.display_name().map(str::to_string).ok_or_else(|| {
                ProtonError::Crypto("the node name could not be decrypted".into())
            })?,
        };
        validate_node_name(&name).map_err(ProtonError::Decode)?;
        let old_parent = self.node(&old_parent_id).await?;
        let old_parent_key = old_parent
            .node_key
            .clone()
            .ok_or_else(|| ProtonError::Crypto("the parent folder key is not available".into()))?;
        let FolderKeys {
            public: new_parent_public,
            hash_key: new_parent_hash_key,
            ..
        } = self.folder_keys(new_parent)?;
        let (email, address_key) = self.signing_identity()?;
        let provider = self.provider();

        let passphrase_session_key = drive_crypto::message_session_key(
            provider,
            &node.armored_node_passphrase,
            &old_parent_key,
        )?;
        let (passphrase, _) = drive_crypto::decrypt_inline(
            provider,
            &node.armored_node_passphrase,
            &old_parent_key,
            &[],
        )?;
        let passphrase = zeroize::Zeroizing::new(passphrase);
        let armored_passphrase = drive_crypto::reencrypt_with_session_key(
            provider,
            &passphrase,
            &passphrase_session_key,
            &new_parent_public,
        )?;
        let name_session_key =
            drive_crypto::message_session_key(provider, &node.armored_name, &old_parent_key)?;
        let armored_name = drive_crypto::encrypt_node_name(
            provider,
            &name,
            Some(&name_session_key),
            &new_parent_public,
            &address_key,
        )?;
        let hash = drive_crypto::lookup_hash(&new_parent_hash_key, &name);

        let mut body = json!({
            "ParentLinkID": new_parent.link_id,
            "NodePassphrase": armored_passphrase,
            "Name": armored_name,
            "NameSignatureEmail": email,
            "Hash": hash,
            "OriginalHash": original_hash,
            "ContentHash": Value::Null,
        });
        // An anonymous node's passphrase was signed by its old parent: it
        // gets a new signature by the address (nodesManagement.ts).
        if node.signature_email.is_none() {
            body["NodePassphraseSignature"] = Value::String(drive_crypto::sign_detached(
                provider,
                &passphrase,
                &address_key,
            )?);
            body["SignatureEmail"] = Value::String(email);
        }
        let _: Value = self
            .api
            .put(
                &format!(
                    "drive/v2/volumes/{}/links/{}/move",
                    self.volume_id(),
                    node.link_id
                ),
                &body,
            )
            .await?;
        self.forget(&[&node.link_id]).await;
        Ok(())
    }

    async fn link_batch(
        &self,
        method: reqwest::Method,
        path: &str,
        link_ids: &[&str],
        what: &str,
        tolerated: &[i64],
    ) -> Result<(), ProtonError> {
        for chunk in link_ids.chunks(LINK_BATCH_SIZE) {
            let body = json!({ "LinkIDs": chunk });
            let response: BatchResponse = if method == reqwest::Method::PUT {
                self.api.put(path, &body).await?
            } else {
                self.api.post(path, &body).await?
            };
            check_batch(response, what, tolerated)?;
        }
        self.forget(link_ids).await;
        Ok(())
    }

    /// `POST drive/v2/volumes/{v}/trash_multiple` (spec 10).
    pub async fn trash(&self, link_ids: &[&str]) -> Result<(), ProtonError> {
        let path = format!("drive/v2/volumes/{}/trash_multiple", self.volume_id());
        self.link_batch(reqwest::Method::POST, &path, link_ids, "trash", &[])
            .await
    }

    /// `PUT drive/v2/volumes/{v}/trash/restore_multiple`.
    pub async fn restore(&self, link_ids: &[&str]) -> Result<(), ProtonError> {
        let path = format!(
            "drive/v2/volumes/{}/trash/restore_multiple",
            self.volume_id()
        );
        self.link_batch(reqwest::Method::PUT, &path, link_ids, "restore", &[])
            .await
    }

    /// Permanent delete: Proton deletes only trashed items, so the nodes are
    /// trashed first, then `POST drive/v2/volumes/{v}/trash/delete_multiple`.
    /// A node already in the trash answers the trash step with 2501 ("Active
    /// file/folder not found"); that is tolerated there, and a node that does
    /// not exist at all still fails the delete step with the same code.
    pub async fn delete_permanently(&self, link_ids: &[&str]) -> Result<(), ProtonError> {
        let trash_path = format!("drive/v2/volumes/{}/trash_multiple", self.volume_id());
        self.link_batch(
            reqwest::Method::POST,
            &trash_path,
            link_ids,
            "trash",
            &[CODE_NOT_ACTIVE],
        )
        .await?;
        let path = format!(
            "drive/v2/volumes/{}/trash/delete_multiple",
            self.volume_id()
        );
        self.link_batch(reqwest::Method::POST, &path, link_ids, "delete", &[])
            .await
    }

    /// `DELETE drive/volumes/{v}/trash`: empties the whole trash.
    pub async fn empty_trash(&self) -> Result<(), ProtonError> {
        let _: Value = self
            .api
            .delete(&format!("drive/volumes/{}/trash", self.volume_id()))
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_batch_with_one_failed_link_is_an_error() {
        let ok: BatchResponse = serde_json::from_str(
            r#"{"Code":1001,"Responses":[{"LinkID":"a","Response":{"Code":1000}}]}"#,
        )
        .unwrap();
        assert!(check_batch(ok, "trash", &[]).is_ok());
        let failed: BatchResponse = serde_json::from_str(
            r#"{"Code":1001,"Responses":[{"LinkID":"a","Response":{"Code":1000}},{"LinkID":"b","Response":{"Code":2501,"Error":"File or folder not found"}}]}"#,
        )
        .unwrap();
        let error = check_batch(failed, "trash", &[]).unwrap_err();
        assert!(
            matches!(error, ProtonError::Api { code: 2501, .. }),
            "{error:?}"
        );
    }

    /// Live 2026-10-17: trashing a folder already in the trash answers 2501
    /// per link. The permanent delete tolerates exactly that code on its
    /// trash step and nothing else.
    #[test]
    fn the_trash_step_of_a_permanent_delete_tolerates_only_not_active() {
        let already: BatchResponse = serde_json::from_str(
            r#"{"Code":1001,"Responses":[{"LinkID":"w","Response":{"Code":2501,"Error":"Active file/folder not found"}}]}"#,
        )
        .unwrap();
        assert!(check_batch(already, "trash", &[CODE_NOT_ACTIVE]).is_ok());
        let other: BatchResponse = serde_json::from_str(
            r#"{"Code":1001,"Responses":[{"LinkID":"w","Response":{"Code":2011,"Error":"No permission"}}]}"#,
        )
        .unwrap();
        assert!(check_batch(other, "trash", &[CODE_NOT_ACTIVE]).is_err());
    }
}
