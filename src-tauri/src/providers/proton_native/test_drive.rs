//! A Drive API double for the tests of [`DriveClient`]: a local HTTP server
//! that answers the link, children, revision and storage requests from items
//! encrypted here with the same `proton-crypto` calls the client uses, so the
//! client under test decrypts and verifies them for real. Every request is
//! counted by route.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::{Path, RawQuery, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use proton_crypto_account::proton_crypto::crypto::PGPProviderSync;
use serde_json::{json, Value};
use sha1::{Digest, Sha1};
use zeroize::Zeroizing;

use super::account::ProtonAccount;
use super::api::ProtonApi;
use super::client::{DriveClient, MyFiles};
use super::drive_crypto;
use super::nodes::LINK_STATE_ACTIVE;

pub const VOLUME: &str = "vol";
pub const ROOT: &str = "root";
const ACTIVE: i64 = 1;
const OBSOLETE: i64 = 2;

struct Revision {
    state: i64,
    manifest_signature: String,
    /// Encrypted blocks, in index order from 1.
    blocks: Vec<Vec<u8>>,
}

/// How the test drive answers `GET drive/v2/shares/my-files`.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum MyFilesRoot {
    /// The root folder's details, as Proton sends them.
    #[default]
    Details,
    /// Only the root's link ID.
    IdOnly,
    /// The root's details with a node passphrase that is not the one locked
    /// to the share key.
    WrongPassphrase,
    /// The root's details with the passphrase signature of another node.
    WrongSignature,
}

#[derive(Default)]
struct Wire {
    base: String,
    /// `/core/v4/users` and `/core/v4/addresses`, with the delay before each
    /// answer.
    users: Value,
    addresses: Value,
    account_delay: std::time::Duration,
    my_files: Value,
    my_files_root: MyFilesRoot,
    /// My Files answers that the account has no Drive volume (code 2501).
    no_volume: bool,
    /// When each route was asked, from the server's start.
    arrivals: Vec<(&'static str, std::time::Duration)>,
    started: Option<std::time::Instant>,
    links: HashMap<String, Value>,
    children: HashMap<String, Vec<String>>,
    revisions: HashMap<(String, String), Revision>,
    counts: HashMap<&'static str, usize>,
    /// The order requests arrived in, as `route link`.
    log: Vec<String>,
}

type Shared = Arc<Mutex<Wire>>;

fn count(wire: &Shared, route: &'static str, what: &str) {
    let mut wire = wire.lock().unwrap();
    *wire.counts.entry(route).or_default() += 1;
    wire.log.push(format!("{route} {what}"));
    let at = wire
        .started
        .map(|started| started.elapsed())
        .unwrap_or_default();
    wire.arrivals.push((route, at));
}

async fn users(State(wire): State<Shared>) -> Json<Value> {
    count(&wire, "users", "");
    let delay = wire.lock().unwrap().account_delay;
    tokio::time::sleep(delay).await;
    Json(wire.lock().unwrap().users.clone())
}

async fn addresses(State(wire): State<Shared>) -> Json<Value> {
    count(&wire, "addresses", "");
    let delay = wire.lock().unwrap().account_delay;
    tokio::time::sleep(delay).await;
    Json(wire.lock().unwrap().addresses.clone())
}

async fn my_files(State(wire): State<Shared>) -> axum::response::Response {
    use axum::response::IntoResponse;
    count(&wire, "my-files", "");
    let wire = wire.lock().unwrap();
    if wire.no_volume {
        return (
            axum::http::StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({ "Code": 2501, "Error": "Volume not found" })),
        )
            .into_response();
    }
    let mut answer = wire.my_files.clone();
    let mut root = wire.links[ROOT].clone();
    match wire.my_files_root {
        MyFilesRoot::Details => {}
        MyFilesRoot::IdOnly => root = json!({ "Link": { "LinkID": ROOT } }),
        MyFilesRoot::WrongPassphrase => {
            // A passphrase locked to another key than the share key.
            let other = wire
                .links
                .iter()
                .find(|(id, _)| id.as_str() != ROOT)
                .map(|(_, details)| details["Link"]["NodePassphrase"].clone())
                .expect("a second node to borrow a passphrase from");
            root["Link"]["NodePassphrase"] = other;
        }
        MyFilesRoot::WrongSignature => {
            let other = wire
                .links
                .iter()
                .find(|(id, _)| id.as_str() != ROOT)
                .map(|(_, details)| details["Link"]["NodePassphraseSignature"].clone())
                .expect("a second node to borrow a signature from");
            root["Link"]["NodePassphraseSignature"] = other;
        }
    }
    answer["Link"] = root;
    Json(answer).into_response()
}

async fn links(State(wire): State<Shared>, Json(body): Json<Value>) -> Json<Value> {
    let ids: Vec<String> = body["LinkIDs"]
        .as_array()
        .map(|ids| {
            ids.iter()
                .filter_map(|id| id.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    count(&wire, "links", &ids.join(","));
    let wire = wire.lock().unwrap();
    let found: Vec<Value> = ids
        .iter()
        .filter_map(|id| wire.links.get(id).cloned())
        .collect();
    Json(json!({ "Code": 1000, "Links": found }))
}

async fn children(
    State(wire): State<Shared>,
    Path((_volume, folder)): Path<(String, String)>,
) -> Json<Value> {
    count(&wire, "children", &folder);
    let wire = wire.lock().unwrap();
    let ids = wire.children.get(&folder).cloned().unwrap_or_default();
    Json(json!({ "Code": 1000, "LinkIDs": ids, "More": false }))
}

async fn revision(
    State(wire): State<Shared>,
    Path((_volume, link, revision)): Path<(String, String, String)>,
    RawQuery(query): RawQuery,
) -> Json<Value> {
    let query: HashMap<String, String> = query
        .unwrap_or_default()
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect();
    count(&wire, "revision", &link);
    let from: usize = query
        .get("FromBlockIndex")
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    let size: usize = query
        .get("PageSize")
        .and_then(|v| v.parse().ok())
        .unwrap_or(20);
    let wire = wire.lock().unwrap();
    let Some(found) = wire.revisions.get(&(link.clone(), revision.clone())) else {
        return Json(json!({ "Code": 2501, "Error": "not found" }));
    };
    let blocks: Vec<Value> = found
        .blocks
        .iter()
        .enumerate()
        .map(|(i, block)| (i + 1, block))
        .skip(from.saturating_sub(1))
        .take(size)
        .map(|(index, block)| {
            json!({
                "Index": index,
                "BareURL": format!("{}/storage/{link}/{revision}/{index}", wire.base),
                "Token": "token",
                "Hash": drive_crypto::encrypted_block_hash(block).1,
            })
        })
        .collect();
    Json(json!({
        "Code": 1000,
        "Revision": {
            "ID": revision,
            "State": found.state,
            "ManifestSignature": found.manifest_signature,
            "Thumbnails": [],
            "Blocks": blocks,
        }
    }))
}

async fn storage(
    State(wire): State<Shared>,
    Path((link, revision, index)): Path<(String, String, usize)>,
) -> Vec<u8> {
    count(&wire, "storage", &link);
    let wire = wire.lock().unwrap();
    wire.revisions
        .get(&(link, revision))
        .and_then(|found| found.blocks.get(index - 1).cloned())
        .unwrap_or_default()
}

/// Keys of the items made so far, to make their children.
struct FolderKeys<P: PGPProviderSync> {
    key: P::PrivateKey,
    hash_key: Zeroizing<Vec<u8>>,
}

struct FileKeys<P: PGPProviderSync> {
    key: P::PrivateKey,
    content_key: P::SessionKey,
}

/// The test drive: its server, and the keys to add items to it.
pub struct TestDrive<P: PGPProviderSync> {
    wire: Shared,
    pub api: Arc<ProtonApi>,
    email: String,
    address_key: P::PrivateKey,
    share_key: P::PrivateKey,
    folders: HashMap<String, FolderKeys<P>>,
    files: HashMap<String, FileKeys<P>>,
    next_id: usize,
}

impl<P: PGPProviderSync> TestDrive<P> {
    /// A drive with an empty root folder, owned by the first address of
    /// `account`, served on a local port.
    pub async fn start(account: &ProtonAccount<P>) -> Self {
        let wire: Shared = Arc::default();
        let app = Router::new()
            .route("/drive/v2/volumes/{volume}/links", post(links))
            .route(
                "/drive/v2/volumes/{volume}/folders/{folder}/children",
                get(children),
            )
            .route(
                "/drive/v2/volumes/{volume}/files/{link}/revisions/{revision}",
                get(revision),
            )
            .route("/storage/{link}/{revision}/{index}", get(storage))
            .route("/core/v4/users", get(users))
            .route("/core/v4/addresses", get(addresses))
            .route("/drive/v2/shares/my-files", get(my_files))
            .with_state(Arc::clone(&wire));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.ok() });
        {
            let mut wire = wire.lock().unwrap();
            wire.base = base.clone();
            wire.started = Some(std::time::Instant::now());
        }

        let api = ProtonApi::with_base_url(base, "test@1.0.0-alpha".into());
        api.set_tokens(Some(super::session::SessionTokens {
            uid: "uid".into(),
            access_token: secrecy::SecretString::from("access".to_string()),
            refresh_token: None,
        }))
        .await;

        let provider = account.provider();
        let address = &account.addresses()[0];
        let address_key = address.keys[0].private_key.clone();
        let address_public = address.keys[0].public_key.clone();
        let share =
            drive_crypto::generate_node_key(provider, &address_public, &address_key).unwrap();
        wire.lock().unwrap().my_files = json!({
            "Code": 1000,
            "Volume": { "VolumeID": VOLUME },
            "Share": {
                "ShareID": "share",
                "CreatorEmail": address.email,
                "Key": share.armored_key,
                "Passphrase": share.armored_passphrase,
                "PassphraseSignature": share.armored_passphrase_signature,
                "AddressID": address.id,
            },
        });
        let share_key = share.key;
        let mut drive = Self {
            wire,
            api: Arc::new(api),
            email: address.email.clone(),
            address_key,
            share_key,
            folders: HashMap::new(),
            files: HashMap::new(),
            next_id: 0,
        };
        drive.add_folder_with_id(provider, None, ROOT, "root");
        drive
    }

    /// A client of this drive for `account`, as `DriveClient::open` makes it.
    pub fn client(&self, account: ProtonAccount<P>) -> DriveClient<P> {
        let address_id = account.addresses()[0].id.clone();
        DriveClient::for_tests(
            Arc::clone(&self.api),
            account,
            MyFiles {
                volume_id: VOLUME.into(),
                share_id: "share".into(),
                root_link_id: ROOT.into(),
                address_id,
                share_key: self.share_key.clone(),
                share_verified: drive_crypto::Verified::Valid,
            },
        )
    }

    fn id(&mut self, prefix: &str) -> String {
        self.next_id += 1;
        format!("{prefix}{}", self.next_id)
    }

    /// The node part every link has: a new node key locked to the parent
    /// key, and the name encrypted to it.
    fn link(
        &self,
        provider: &P,
        parent: Option<&str>,
        link_id: &str,
        name: &str,
        node_type: i64,
    ) -> (drive_crypto::GeneratedNodeKey<P>, Value) {
        let parent_key = match parent {
            Some(parent) => &self.folders[parent].key,
            None => &self.share_key,
        };
        let parent_public = provider.private_key_to_public_key(parent_key).unwrap();
        let node =
            drive_crypto::generate_node_key(provider, &parent_public, &self.address_key).unwrap();
        let name_hash =
            parent.map(|parent| drive_crypto::lookup_hash(&self.folders[parent].hash_key, name));
        let armored_name = drive_crypto::encrypt_node_name(
            provider,
            name,
            None,
            &parent_public,
            &self.address_key,
        )
        .unwrap();
        let link = json!({
            "LinkID": link_id,
            "ParentLinkID": parent,
            "Type": node_type,
            "State": LINK_STATE_ACTIVE,
            "CreateTime": 1_760_000_000,
            "ModifyTime": 1_760_000_000,
            "Name": armored_name,
            "NameHash": name_hash,
            "NodeKey": node.armored_key,
            "NodePassphrase": node.armored_passphrase,
            "NodePassphraseSignature": node.armored_passphrase_signature,
            "SignatureEmail": self.email,
            "NameSignatureEmail": self.email,
        });
        (node, link)
    }

    fn add_folder_with_id(&mut self, provider: &P, parent: Option<&str>, id: &str, name: &str) {
        let (node, link) = self.link(provider, parent, id, name, 1);
        let (hash_key, armored_hash_key) =
            drive_crypto::generate_hash_key(provider, &node.key).unwrap();
        let details = json!({ "Link": link, "Folder": { "NodeHashKey": armored_hash_key } });
        let mut wire = self.wire.lock().unwrap();
        wire.links.insert(id.to_string(), details);
        wire.children.entry(id.to_string()).or_default();
        if let Some(parent) = parent {
            wire.children
                .entry(parent.to_string())
                .or_default()
                .push(id.to_string());
        }
        drop(wire);
        self.folders.insert(
            id.to_string(),
            FolderKeys {
                key: node.key,
                hash_key,
            },
        );
    }

    /// A folder `name` in `parent`; returns its link ID.
    pub fn add_folder(&mut self, provider: &P, parent: &str, name: &str) -> String {
        let id = self.id("folder-");
        self.add_folder_with_id(provider, Some(parent), &id, name);
        id
    }

    /// A file `name` in `parent` whose content is `blocks`, one block each;
    /// returns its link ID and the ID of its active revision.
    pub fn add_file(
        &mut self,
        provider: &P,
        parent: &str,
        name: &str,
        blocks: &[&[u8]],
    ) -> (String, String) {
        let id = self.id("file-");
        let (node, link) = self.link(provider, Some(parent), &id, name, 2);
        let node_public = provider.private_key_to_public_key(&node.key).unwrap();
        let (content_key, packet, packet_signature) =
            drive_crypto::generate_content_key(provider, &node_public, &node.key).unwrap();
        self.files.insert(
            id.clone(),
            FileKeys {
                key: node.key,
                content_key,
            },
        );
        let details = json!({
            "Link": link,
            "File": {
                "ContentKeyPacket": packet,
                "ContentKeyPacketSignature": packet_signature,
                "MediaType": "application/octet-stream",
            },
        });
        {
            let mut wire = self.wire.lock().unwrap();
            wire.links.insert(id.clone(), details);
            wire.children
                .entry(parent.to_string())
                .or_default()
                .push(id.clone());
        }
        let revision = self.new_revision(provider, &id, blocks);
        (id, revision)
    }

    /// A new active revision of `file` with content `blocks`; the one it
    /// replaces becomes obsolete. Returns its ID.
    pub fn new_revision(&mut self, provider: &P, file: &str, blocks: &[&[u8]]) -> String {
        let revision_id = self.id("rev-");
        let keys = &self.files[file];
        let node_public = provider.private_key_to_public_key(&keys.key).unwrap();
        let mut encrypted = Vec::new();
        let mut manifest = Vec::new();
        let mut sha1 = Sha1::new();
        for block in blocks {
            sha1.update(block);
            let sealed = drive_crypto::encrypt_block(provider, block, &keys.content_key).unwrap();
            manifest.extend_from_slice(&drive_crypto::encrypted_block_hash(&sealed).0);
            encrypted.push(sealed);
        }
        let manifest_signature =
            drive_crypto::sign_detached(provider, &manifest, &self.address_key).unwrap();
        let size: usize = blocks.iter().map(|block| block.len()).sum();
        let block_sizes: Vec<usize> = blocks.iter().map(|block| block.len()).collect();
        let xattr = drive_crypto::encrypt_extended_attributes(
            provider,
            &json!({ "Common": {
                "Size": size,
                "BlockSizes": block_sizes,
                "Digests": { "SHA1": hex::encode(sha1.finalize()) },
            }})
            .to_string(),
            &node_public,
            &self.address_key,
        )
        .unwrap();
        let mut wire = self.wire.lock().unwrap();
        for ((link, _), revision) in wire.revisions.iter_mut() {
            if link == file {
                revision.state = OBSOLETE;
            }
        }
        wire.revisions.insert(
            (file.to_string(), revision_id.clone()),
            Revision {
                state: ACTIVE,
                manifest_signature: manifest_signature.clone(),
                blocks: encrypted,
            },
        );
        let details = wire.links.get_mut(file).unwrap();
        details["File"]["ActiveRevision"] = json!({
            "RevisionID": revision_id,
            "CreateTime": 1_760_000_000,
            "ManifestSignature": manifest_signature,
            "XAttr": xattr,
            "SignatureEmail": self.email,
        });
        revision_id
    }

    /// Drops the blocks of `revision` after the first `keep`, leaving its
    /// manifest signature and attributes as they were: a server that loses
    /// the end of a file.
    pub fn truncate_blocks(&self, file: &str, revision: &str, keep: usize) {
        let mut wire = self.wire.lock().unwrap();
        let found = wire
            .revisions
            .get_mut(&(file.to_string(), revision.to_string()))
            .unwrap();
        found.blocks.truncate(keep);
    }

    /// Answers the account's keys from `users` and `addresses` (the API
    /// JSON), each after `delay`.
    pub fn serve_account(&self, users: Value, addresses: Value, delay: std::time::Duration) {
        let mut wire = self.wire.lock().unwrap();
        wire.users = users;
        wire.addresses = addresses;
        wire.account_delay = delay;
    }

    pub fn set_my_files_root(&self, root: MyFilesRoot) {
        self.wire.lock().unwrap().my_files_root = root;
    }

    /// My Files answers that the account has no Drive volume.
    pub fn without_volume(&self) {
        self.wire.lock().unwrap().no_volume = true;
    }

    /// When `route` was first asked, from the server's start.
    pub fn first_arrival(&self, route: &str) -> Option<std::time::Duration> {
        self.wire
            .lock()
            .unwrap()
            .arrivals
            .iter()
            .find(|(asked, _)| *asked == route)
            .map(|(_, at)| *at)
    }

    /// Requests made so far on `route` (`links`, `children`, `revision`,
    /// `storage`, `users`, `addresses`, `my-files`).
    pub fn count(&self, route: &str) -> usize {
        self.wire
            .lock()
            .unwrap()
            .counts
            .get(route)
            .copied()
            .unwrap_or(0)
    }

    /// Forgets the counts and the request log.
    pub fn reset_counts(&self) {
        let mut wire = self.wire.lock().unwrap();
        wire.counts.clear();
        wire.log.clear();
        wire.arrivals.clear();
    }

    /// The requests made so far, in order, as `route link`.
    pub fn log(&self) -> Vec<String> {
        self.wire.lock().unwrap().log.clone()
    }
}
