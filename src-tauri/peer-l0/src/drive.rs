//! WI-3d: the L1 encrypted-drive engine, moved out of the peer-l0 binary into
//! the crate library so the app can run it (the bin and the app both call these
//! entry points). Byte-equivalent to the WI-4c `main.rs` engine; only the
//! in-crate paths were rewritten (`aeroftp_peer_l0::` -> `crate::`) and the
//! public entry points exposed. Both keying paths are preserved unchanged: the
//! `--secret` dev/pairing path (WI-1/WI-2 gates) and the WI-4c sealed-capability
//! path.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::{
    decrypt_blob, derive_drive_key, encrypt_blob, open_capability, seal_capability, Capability,
    Identity, IdentityPublic,
};

// --------------------------------------------------------------------------------------------
// WI-4c: capability-path helpers (per-drive RANDOM content key shared via a sealed Capability).
// These sit ALONGSIDE the existing --secret (dev/pairing) path; the two are mutually exclusive.
// --------------------------------------------------------------------------------------------

/// How a docs-publish run keys the drive (and, on the capability path, who to issue tokens to).
pub enum PublishKey {
    /// Legacy dev/pairing-secret path (WI-1/WI-2 gate harness): drive_key = HKDF(secret, ns).
    DevSecret(Vec<u8>),
    /// WI-4c capability path: a per-drive RANDOM 32-byte content key + the issuance plan.
    /// Boxed so the two variants stay close in size (CapIssue carries an Identity).
    Capability {
        content_key: [u8; 32],
        issue: Box<CapIssue>,
    },
}

/// How a docs-replicate run recovers the 32-byte drive content key.
pub enum ReplicateKey {
    /// Legacy dev/pairing-secret path: drive_key = HKDF(secret, ns).
    DevSecret(Vec<u8>),
    /// WI-4c capability path: open a sealed token with my identity, verifying the expected issuer.
    /// Boxed (Identity is large) so the variants stay close in size.
    Capability(Box<ReplicateCap>),
}

/// Replicator's capability inputs: my identity, the expected issuer, and the token to open.
pub struct ReplicateCap {
    pub me: Identity,
    pub expected_issuer: IdentityPublic,
    pub token: String,
}

/// Publisher-side capability issuance: after the ticket is shared, seal+sign one token per grant.
pub struct CapIssue {
    pub issuer: Identity,
    /// (printable AeroFTP-ID, parsed public identity) for each recipient.
    pub grants: Vec<(String, IdentityPublic)>,
    pub cap_out: Option<String>,
    pub drive_name: String,
}

/// Decode a base64url (no pad) 32-byte content key.
pub fn decode_content_key(s: &str) -> Result<[u8; 32]> {
    let bytes = base64::Engine::decode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, s.trim())
        .context("content-key is not valid base64url")?;
    let arr: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("content-key must decode to exactly 32 bytes"))?;
    Ok(arr)
}

/// Encode a 32-byte content key for display (base64url, no pad).
pub(crate) fn encode_content_key(k: &[u8; 32]) -> String {
    base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, k)
}

/// Fresh random 32-byte content key from OS randomness.
pub fn random_content_key() -> [u8; 32] {
    use rand::RngCore;
    let mut k = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut k);
    k
}

/// Load a 64-byte identity secret file (as written by `identity-new`).
pub fn load_identity(path: &str) -> Result<Identity> {
    let bytes = std::fs::read(path).with_context(|| format!("read identity file {path}"))?;
    let arr: [u8; 64] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("identity file must be exactly 64 bytes"))?;
    Ok(Identity::from_secret_bytes(&arr))
}

/// Write an identity's 64 secret bytes to `path` (0600 on unix).
pub fn write_identity(path: &str, id: &Identity) -> Result<()> {
    std::fs::write(path, id.to_secret_bytes())
        .with_context(|| format!("write identity file {path}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).ok();
    }
    Ok(())
}

/// Resolve a `--capability` argument: a literal `aeroftp-drive://` token OR a path to a file holding one.
pub fn load_capability_token(s: &str) -> Result<String> {
    if std::path::Path::new(s).is_file() {
        Ok(std::fs::read_to_string(s)
            .with_context(|| format!("read capability token file {s}"))?
            .trim()
            .to_string())
    } else {
        Ok(s.trim().to_string())
    }
}

/// Issue one sealed capability token per grant, after the drive ticket is known.
pub(crate) fn issue_capabilities(
    cap: &CapIssue,
    ns: &iroh_docs::NamespaceId,
    content_key: &[u8; 32],
    ticket: &iroh_docs::DocTicket,
) -> Result<()> {
    let node_addrs: Vec<String> = ticket.nodes.iter().map(|n| n.node_id.to_string()).collect();
    if let Some(dir) = &cap.cap_out {
        std::fs::create_dir_all(dir).ok();
    }
    println!("\n=== CAPABILITY TOKENS (WI-4c) ===");
    for (afid, recipient) in &cap.grants {
        let capability = Capability {
            namespace_id: ns.to_string(),
            content_key: *content_key,
            node_addrs: node_addrs.clone(),
            drive_name: cap.drive_name.clone(),
            version: 1,
            granted_to_ed: recipient.ed_bytes(),
            issued_at: chrono::Utc::now().timestamp(),
        };
        let token = seal_capability(&cap.issuer, recipient, &capability)?;
        println!("CAP TOKEN for {afid}: {token}");
        if let Some(dir) = &cap.cap_out {
            let short: String = afid
                .chars()
                .take(16)
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
                .collect();
            let path = std::path::Path::new(dir).join(format!("{short}.token"));
            std::fs::write(&path, &token)
                .with_context(|| format!("write cap token to {}", path.display()))?;
            println!("  (written to {})", path.display());
        }
    }
    println!("=================================\n");
    Ok(())
}

pub(crate) type DriveState = std::collections::HashMap<String, String>;

pub(crate) struct DriveStats {
    updated: usize,
    added: usize,
    deleted: usize,
    unchanged: usize,
    file_count: usize,
    total_pt: u64,
}

/// Publish the current on-disk state of `src` as a specific drive_version: encrypt each regular file
/// (fresh nonce) under `drive_key`, write it under its relative '/'-key, then write the signed +
/// encrypted manifest under `__drive_manifest__.json`.
/// When `prev` is Some, implements differential: skip unchanged (by plaintext_blake3), count added/updated/deleted/unchanged,
/// issue doc.del for removed keys. Always materializes full current manifest for the version.
/// Returns (stats, new_state) for chaining to next republish.
pub(crate) async fn publish_drive_version(
    doc: &iroh_docs::api::Doc,
    author: iroh_docs::AuthorId,
    drive_key: &[u8; 32],
    src: &Path,
    version: u64,
    prev: Option<&DriveState>,
) -> Result<(DriveStats, DriveState)> {
    use bytes::Bytes;
    if !src.is_dir() {
        anyhow::bail!("--dir {} is not a directory", src.display());
    }

    let mut files: Vec<(String, PathBuf)> = vec![];
    fn collect_files(dir: &Path, base: &Path, out: &mut Vec<(String, PathBuf)>) {
        if let Ok(read_dir) = std::fs::read_dir(dir) {
            for entry in read_dir.flatten() {
                let p = entry.path();
                if p.is_symlink() {
                    continue;
                }
                if p.is_dir() {
                    collect_files(&p, base, out);
                } else if p.is_file() {
                    let rel = p
                        .strip_prefix(base)
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .replace('\\', "/");
                    out.push((rel, p));
                }
            }
        }
    }
    collect_files(src, src, &mut files);
    files.sort_by(|a, b| a.0.cmp(&b.0));

    let mut manifest_files: Vec<serde_json::Value> = vec![];
    let mut total_pt: u64 = 0;

    let mut stats = DriveStats {
        updated: 0,
        added: 0,
        deleted: 0,
        unchanged: 0,
        file_count: files.len(),
        total_pt: 0,
    };
    let mut new_state: DriveState = std::collections::HashMap::new();

    for (rel_key, path) in &files {
        let pt = std::fs::read(path)?;
        let pt_blake3 = blake3::hash(&pt).to_hex().to_string();
        new_state.insert(rel_key.clone(), pt_blake3.clone());

        let is_unchanged = if let Some(p) = prev {
            p.get(rel_key).map_or(false, |old| old == &pt_blake3)
        } else {
            false
        };

        if is_unchanged {
            println!("skip unchanged key={}", rel_key);
            stats.unchanged += 1;
            manifest_files.push(serde_json::json!({
                "key": rel_key,
                "plaintext_len": pt.len(),
                "plaintext_blake3": pt_blake3
            }));
            total_pt += pt.len() as u64;
            continue;
        }

        // new or changed: fresh encrypt + set_bytes
        let (nonce, ct) = encrypt_blob(drive_key, &pt)?;
        let mut blob = nonce.clone();
        blob.extend_from_slice(&ct);
        let content_hash = doc
            .set_bytes(author, Bytes::from(rel_key.clone()), Bytes::from(blob))
            .await?;
        let action = if prev.map_or(true, |p| !p.contains_key(rel_key)) {
            "added"
        } else {
            "updated"
        };
        if action == "added" {
            stats.added += 1;
        } else {
            stats.updated += 1;
        }
        println!(
            "wrote key={} pt_len={} ct_len={} content_hash={}",
            rel_key,
            pt.len(),
            ct.len(),
            content_hash
        );
        manifest_files.push(serde_json::json!({
            "key": rel_key,
            "plaintext_len": pt.len(),
            "plaintext_blake3": pt_blake3
        }));
        total_pt += pt.len() as u64;
    }

    // Deletions (only when we have a prev snapshot): keys that existed before but are gone on disk now
    if let Some(p) = prev {
        for old_key in p.keys() {
            if !new_state.contains_key(old_key) {
                let _ = doc.del(author, Bytes::from(old_key.clone())).await;
                println!("deleted key={}", old_key);
                stats.deleted += 1;
            }
        }
    }

    stats.total_pt = total_pt;

    let manifest = serde_json::json!({
        "drive_version": version,
        "created": chrono::Utc::now().to_rfc3339(),
        "file_count": files.len(),
        "files": manifest_files
    });
    let manifest_bytes = serde_json::to_vec(&manifest)?;
    let (m_nonce, m_ct) = encrypt_blob(drive_key, &manifest_bytes)?;
    let mut m_blob = m_nonce.clone();
    m_blob.extend_from_slice(&m_ct);
    doc.set_bytes(
        author,
        Bytes::from("__drive_manifest__.json"),
        Bytes::from(m_blob),
    )
    .await?;

    Ok((stats, new_state))
}

/// L1 Stage 5/6/7/8: docs-publish with optional drive mode + republish-after + republish-count + store.
/// If --store absent: EXACT current in-memory behavior (no regression for single-entry / Stage 4-7 drive).
/// If --store <dir> present: use FsStore + Docs::persistent + author_default + list/open-or-create.
/// Drive content (entries + blobs) survives publisher restart. Replicate side remains in-memory.
pub async fn run_docs_publish(
    key: String,
    dir: Option<String>,
    republish_after: u64,
    republish_count: u64,
    store: Option<String>,
    cfg: crate::endpoint::PeerEndpointConfig,
    publish_key: PublishKey,
) -> Result<()> {
    use bytes::Bytes;
    use iroh::protocol::Router;
    use iroh_blobs::BlobsProtocol;
    use iroh_docs::api::protocol::{AddrInfoOptions, ShareMode};
    use iroh_docs::protocol::Docs;
    use iroh_gossip::net::Gossip;

    let endpoint = crate::endpoint::build_base_endpoint(cfg).await?;
    let node_id = endpoint.node_id();

    println!("=== AERO FTP PEER L1 DOCS PUBLISH (bare, no E2EE) ===");
    println!("NodeID: {}", node_id);
    println!("(this is the listener side; share the ticket below with the replicate side)");

    // WI-4c: the drive key is either HKDF(dev secret, ns) or a per-drive random content key. The ns is
    // only known after the doc is created, so resolve per branch via this closure (borrows the source).
    let resolve_drive_key = |ns: &iroh_docs::NamespaceId| -> [u8; 32] {
        match &publish_key {
            PublishKey::DevSecret(secret) => derive_drive_key(secret, &ns.to_string()),
            PublishKey::Capability { content_key, .. } => *content_key,
        }
    };

    let mut v1_file_count: usize = 0;
    let mut drive_state: Option<DriveState> = None;

    let doc: iroh_docs::api::Doc;
    let author: iroh_docs::AuthorId;
    let ns: iroh_docs::NamespaceId;
    let drive_key: [u8; 32];
    let router;

    if let Some(store_dir) = store.as_deref() {
        // === PERSISTENT MODE (Stage 8) ===
        use std::path::Path;
        let store_path = Path::new(store_dir);
        std::fs::create_dir_all(store_path)?;
        let blobs_path = store_path.join("blobs");
        let docs_path = store_path.join("docs");
        std::fs::create_dir_all(&blobs_path)?;
        std::fs::create_dir_all(&docs_path)?;

        let blobs = iroh_blobs::store::fs::FsStore::load(&blobs_path).await?;
        let gossip = Gossip::builder().spawn(endpoint.clone());
        let docs = Docs::persistent(docs_path)
            .spawn(endpoint.clone(), (*blobs).clone(), gossip.clone())
            .await?;

        router = Router::builder(endpoint.clone())
            .accept(
                iroh_blobs::ALPN,
                BlobsProtocol::new(&blobs, endpoint.clone(), None),
            )
            .accept(iroh_gossip::ALPN, gossip)
            .accept(iroh_docs::ALPN, docs.clone())
            .spawn();

        let api = docs.api();
        author = api.author_default().await?;

        // Re-open existing namespace if present (survives restart), else create new.
        // Remember the ns id in a "current-ns.txt" file in the store dir; reopen via api.open().
        // Verified locally: the capability persists in the docs redb, so api.open(ns) reattaches
        // the SAME namespace after a publisher restart (reopened=true).
        let ns_file = store_path.join("current-ns.txt");
        let mut reopened = false;
        let mut the_doc = None;
        if ns_file.exists() {
            if let Ok(ns_str) = std::fs::read_to_string(&ns_file) {
                if let Ok(ns_id) = ns_str.trim().parse::<iroh_docs::NamespaceId>() {
                    if let Ok(Some(d)) = api.open(ns_id).await {
                        the_doc = Some(d);
                        reopened = true;
                    }
                }
            }
        }
        if the_doc.is_none() {
            let d = api.create().await?;
            std::fs::write(&ns_file, d.id().to_string())?;
            the_doc = Some(d);
        }
        let the_doc = the_doc.unwrap();
        doc = the_doc;
        ns = doc.id();
        drive_key = resolve_drive_key(&ns);

        println!(
            "PERSISTENT DRIVE: store={} ns={} (reopened={}) author={}",
            store_dir, ns, reopened, author
        );

        if let Some(dir_path) = dir.as_deref() {
            // publish (or diff on reopen) the dir into the persistent doc
            let src = Path::new(dir_path);
            let (s1, state1) =
                publish_drive_version(&doc, author, &drive_key, src, 1, None).await?;
            println!(
                "DRIVE PUBLISHED: {} files + manifest, ns={}, total_plaintext_bytes={}",
                s1.file_count, ns, s1.total_pt
            );
            v1_file_count = s1.file_count;
            drive_state = Some(state1);
        }
        // else: pure reopen, no --dir -> serve the existing drive straight from disk.
        // Verified locally: a reopened ns serves the full drive (entries + blobs) to a fresh
        // replicate after a publisher restart (DRIVE REPLICATED: N/N, BLAKE3 verified).
    } else {
        // === ORIGINAL IN-MEMORY PATH (unchanged for --store absent) ===
        let blobs = iroh_blobs::store::mem::MemStore::default();
        let gossip = Gossip::builder().spawn(endpoint.clone());
        let docs = Docs::memory()
            .spawn(endpoint.clone(), (*blobs).clone(), gossip.clone())
            .await?;

        router = Router::builder(endpoint.clone())
            .accept(
                iroh_blobs::ALPN,
                BlobsProtocol::new(&blobs, endpoint.clone(), None),
            )
            .accept(iroh_gossip::ALPN, gossip)
            .accept(iroh_docs::ALPN, docs.clone())
            .spawn();

        let api = docs.api();

        // Create a fresh author (the signer) and a new document (the "drive" = namespace).
        author = api.author_create().await?;
        let the_doc = api.create().await?;
        doc = the_doc;
        ns = doc.id();
        drive_key = resolve_drive_key(&ns);

        println!("AuthorId: {}", author);
        println!("NamespaceId: {}", ns);

        if let Some(dir_path) = dir.as_deref() {
            // === DRIVE MODE (Stage 6 differential): publish v1 (prev=None -> all added); share ticket immediately
            // so watcher can join during v1. v2 republish (after sleep) passes prev state for diff + del.
            let src = Path::new(dir_path);
            let (s1, state1) =
                publish_drive_version(&doc, author, &drive_key, src, 1, None).await?;
            println!(
                "DRIVE PUBLISHED: {} files + manifest, ns={}, total_plaintext_bytes={}",
                s1.file_count, ns, s1.total_pt
            );
            v1_file_count = s1.file_count;
            drive_state = Some(state1);
        } else {
            // === SINGLE ENTRY MODE (exact backward compat with gate at 9f5a5c18) ===
            let content_str = format!("hi from L1 {}", chrono::Utc::now().to_rfc3339());
            let content: Vec<u8> = content_str.into_bytes();

            let (nonce, ct) = encrypt_blob(&drive_key, &content)?;
            let mut blob = nonce.clone();
            blob.extend_from_slice(&ct);

            let written_hash = doc
                .set_bytes(author, Bytes::from(key.clone()), Bytes::from(blob.clone()))
                .await?;

            println!(
                "Wrote entry: key={} content_hash={} size={}",
                key,
                written_hash,
                blob.len()
            );
            println!("stored ciphertext blob len={} (nonce {}B + ct) (E2EE; plaintext never leaves this process; entry signed by author)", blob.len(), nonce.len());
        }
    }

    // Produce a ticket that tells the other side the NamespaceId + where to find us. Read mode is
    // sufficient (the other side only pulls). Shared NOW (right after v1) so a watcher can join during
    // v1 and observe the live v1->v2 update below.
    let ticket = doc
        .share(ShareMode::Read, AddrInfoOptions::RelayAndAddresses)
        .await?;
    println!("\n=== DOC TICKET (copy/paste to docs-replicate side) ===");
    println!("{}", ticket);
    println!("==================================================\n");

    // WI-4c: on the capability path, seal+sign one token per --grant now that the ns + ticket exist.
    if let PublishKey::Capability { issue, .. } = &publish_key {
        println!("CONTENT KEY (b64url): {}", encode_content_key(&drive_key));
        issue_capabilities(issue, &ns, &drive_key, &ticket)?;
    }

    println!("Publish side ready. Waiting for replicators (Ctrl-C to stop)...");

    // Drive versioning (Stage 7 generalized): after ticket share, if republish_after > 0 then
    // loop republish_count times (v2, v3, ...). Each iteration sleeps, re-walks src on disk (test
    // harness mutates between), chains prev state for diff+del, prints per-version stats.
    if republish_after > 0 {
        if let Some(dir_path) = dir.as_deref() {
            let src = Path::new(dir_path);
            let mut prev_file_count = v1_file_count;
            for v in 2..=(1 + republish_count) {
                println!(
                    "(republish-after {}s: sleeping before v{})",
                    republish_after, v
                );
                tokio::time::sleep(std::time::Duration::from_secs(republish_after)).await;
                let prev = drive_state.as_ref();
                let (s, new_state) =
                    publish_drive_version(&doc, author, &drive_key, src, v, prev).await?;
                println!("DRIVE REPUBLISHED: v{}: {} updated, {} added, {} deleted, {} unchanged (file_count {} -> {})",
                    v, s.updated, s.added, s.deleted, s.unchanged, prev_file_count, s.file_count);
                prev_file_count = s.file_count;
                drive_state = Some(new_state);
            }
        }
    }

    // Keep the router (and thus the docs/gossip/blobs handlers) alive.
    tokio::signal::ctrl_c().await.ok();
    println!("Shutting down publish side.");
    // router drops will shutdown
    drop(router);
    Ok(())
}

/// Poll for an entry by exact key and return its content hash once it appears.
/// (RBSR entry sync is async; on a slow/relay link the entry lands a little after the event.)
pub(crate) async fn wait_entry_hash(
    doc: &iroh_docs::api::Doc,
    key: &[u8],
    attempts: u32,
) -> Option<iroh_blobs::Hash> {
    use futures_lite::stream::StreamExt;
    for _ in 0..attempts {
        if let Ok(stream) = doc.get_many(iroh_docs::store::Query::key_exact(key)).await {
            let mut pinned = Box::pin(stream);
            if let Some(Ok(entry)) = pinned.next().await {
                return Some(entry.content_hash());
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
    }
    None
}

/// L1 Stage 5: docs-replicate with optional drive + live watch (--out + --watch-secs).
/// If --out absent: single-entry compat.
/// If --out present + no watch: Stage-4 one-shot (initial reconstruct + exit).
/// If --watch-secs > 0: after initial, use import_and_subscribe + LiveEvent loop; on manifest version
/// increase, re-converge (re-pull manifest + all files in it).
/// Stage 9: --store <dir> makes replicate side persistent (FsStore + Docs::persistent + current-ns.txt +
/// api.open for resume). --store requires --out. Stage 10: --store + --watch-secs resumes from disk AND
/// re-arms live sync (doc.start_sync with the ticket's nodes), so a restarted replicator keeps converging.
pub async fn run_docs_replicate(
    ticket_str: String,
    out: Option<String>,
    watch_secs: u64,
    store: Option<String>,
    cfg: crate::endpoint::PeerEndpointConfig,
    key_src: ReplicateKey,
) -> Result<()> {
    use bytes::Bytes;
    use futures_lite::stream::StreamExt;
    use iroh::protocol::Router;
    use iroh_blobs::BlobsProtocol;
    use iroh_docs::protocol::Docs;
    use iroh_gossip::net::Gossip;
    use std::pin::Pin;

    /// Unified stream type so first-sync (import_and_subscribe) and resume (doc.subscribe after open)
    /// can both be stored in the same Option for the watch loop. Boxing erases the distinct `impl Trait`
    /// opaques returned by the two iroh APIs.
    type EventStream = Pin<
        Box<
            dyn futures_lite::stream::Stream<
                    Item = Result<iroh_docs::engine::LiveEvent, anyhow::Error>,
                > + Send
                + Unpin
                + 'static,
        >,
    >;

    let ticket: iroh_docs::DocTicket = ticket_str.parse().context("failed to parse DocTicket")?;

    let endpoint = crate::endpoint::build_base_endpoint(cfg).await?;

    println!("=== AERO FTP PEER L1 DOCS REPLICATE (bare, no E2EE) ===");
    println!("Importing ticket and syncing...");

    // Stage 9: support --store for persistent replicate (resume from disk, publisher may be dead).
    // When --store absent: EXACT original mem behavior and prints (no regression for single-entry,
    // Stage-4 one-shot, or Stage-5 live watch). Guards + persistent setup modeled on publish --store.
    if store.is_some() && out.is_none() {
        anyhow::bail!("--store requires --out (persistent resume is drive mode)");
    }

    enum ContentStore {
        Mem(iroh_blobs::store::mem::MemStore),
        Fs(iroh_blobs::store::fs::FsStore),
    }
    impl ContentStore {
        async fn get_bytes(&self, h: iroh_blobs::Hash) -> anyhow::Result<bytes::Bytes> {
            match self {
                ContentStore::Mem(m) => m
                    .blobs()
                    .get_bytes(h)
                    .await
                    .map_err(|e| anyhow::anyhow!("{e}")),
                ContentStore::Fs(f) => f
                    .blobs()
                    .get_bytes(h)
                    .await
                    .map_err(|e| anyhow::anyhow!("{e}")),
            }
        }
        async fn fetch_retry(&self, h: iroh_blobs::Hash, attempts: u32) -> Option<bytes::Bytes> {
            for _ in 0..attempts {
                if let Ok(b) = self.get_bytes(h).await {
                    return Some(b);
                }
                tokio::time::sleep(std::time::Duration::from_millis(180)).await;
            }
            None
        }
        /// Persist downloaded blobs durably before the process exits. Blobs synced over the network
        /// (manifest + file ciphertexts, incl. the big content blob) land on disk asynchronously; the
        /// redb bookkeeping that marks them complete is buffered. Without flushing it, a later reopen
        /// (offline resume) sees the data files but get_bytes returns Io(NotFound). sync_db + wait_idle
        /// (the latter documented for exactly this: "the store has written all data to disk") make the
        /// persistent replicate store durable + queryable across a restart. No-op for the mem path.
        async fn flush(&self) {
            if let ContentStore::Fs(f) = self {
                let _ = f.sync_db().await;
                let _ = f.wait_idle().await;
            }
        }
    }

    #[allow(unused_assignments)]
    let mut events: Option<EventStream> = None;
    let (doc, blobs, _router, _reopened) = if let Some(store_dir) = &store {
        std::fs::create_dir_all(store_dir).context("create_dir_all for rep store")?;
        let store_path = std::path::Path::new(store_dir);
        // Docs::persistent spawn failed with ENOENT on "docs" subdir in initial test runs even though
        // FsStore::load created its "blobs" sibling. Explicitly ensure the docs subdir (pub side
        // apparently creates it internally or via different timing; replicate path needs it upfront).
        std::fs::create_dir_all(store_path.join("docs"))
            .context("ensure docs subdir for Docs::persistent")?;
        let blobs_fs = iroh_blobs::store::fs::FsStore::load(store_path.join("blobs"))
            .await
            .context("FsStore::load (replicate --store)")?;
        let gossip = Gossip::builder().spawn(endpoint.clone());
        let docs_p = Docs::persistent(store_path.join("docs"))
            .spawn(endpoint.clone(), (*blobs_fs).clone(), gossip.clone())
            .await
            .context("Docs::persistent().spawn for rep --store")?;
        let router = Router::builder(endpoint.clone())
            .accept(
                iroh_blobs::ALPN,
                BlobsProtocol::new(&blobs_fs, endpoint.clone(), None),
            )
            .accept(iroh_gossip::ALPN, gossip)
            .accept(iroh_docs::ALPN, docs_p.clone())
            .spawn();
        let api = docs_p.api();
        let _ = api
            .author_default()
            .await
            .context("author_default on persistent rep docs")?;
        let ns_file = store_path.join("current-ns.txt");
        let mut reopened = false;
        let the_doc = if ns_file.exists() {
            if let Ok(ns_str) = std::fs::read_to_string(&ns_file) {
                if let Ok(ns_id) = ns_str.trim().parse::<iroh_docs::NamespaceId>() {
                    if let Ok(Some(d)) = api.open(ns_id).await {
                        reopened = true;
                        // Stage 10: after api.open (resume from disk), explicitly subscribe to get a
                        // LiveEvent stream so the existing watch loop can catch future versions live.
                        let sub = d
                            .subscribe()
                            .await
                            .context("subscribe on resumed persistent doc")?;
                        events = Some(Box::pin(sub));
                        // api.open opens only the LOCAL replica; unlike import_and_subscribe it does NOT
                        // start syncing with any peer, so subscribe() alone never delivers remote updates
                        // (verified: a resumed watcher missed a live v2 the publisher republished). For
                        // the live-watch case, (re)establish sync with the ticket's node addrs so the
                        // resumed replicator actually receives later versions. Skipped when watch_secs==0
                        // (the Stage-9 offline one-shot resume must not require the publisher).
                        if watch_secs > 0 {
                            d.start_sync(ticket.nodes.clone())
                                .await
                                .context("start_sync on resumed persistent doc (live watch)")?;
                        }
                        d
                    } else {
                        // Stale ns file or store inconsistency; fall back (will rewrite ns file below if import succeeds)
                        let (d, evs) = api.import_and_subscribe(ticket.clone()).await.context(
                            "import_and_subscribe fallback (ns file present but open failed)",
                        )?;
                        events = Some(Box::pin(evs));
                        std::fs::write(&ns_file, d.id().to_string())
                            .context("write ns_file fallback")?;
                        d
                    }
                } else {
                    anyhow::bail!("invalid NamespaceId in {}/current-ns.txt", store_dir);
                }
            } else {
                anyhow::bail!("failed to read {}/current-ns.txt", store_dir);
            }
        } else {
            let (d, evs) = api
                .import_and_subscribe(ticket.clone())
                .await
                .context("import_and_subscribe (first sync into persistent rep store)")?;
            events = Some(Box::pin(evs));
            std::fs::write(&ns_file, d.id().to_string())
                .context("write current-ns.txt after first import")?;
            d
        };
        let ns = the_doc.id();
        println!(
            "PERSISTENT REPLICATE: store={} ns={} (reopened={})",
            store_dir, ns, reopened
        );
        (the_doc, ContentStore::Fs(blobs_fs), router, reopened)
    } else {
        // === ORIGINAL IN-MEMORY PATH (unchanged for --store absent) ===
        let blobs = iroh_blobs::store::mem::MemStore::default();
        let gossip = Gossip::builder().spawn(endpoint.clone());
        let docs = Docs::memory()
            .spawn(endpoint.clone(), (*blobs).clone(), gossip.clone())
            .await?;

        let _router = Router::builder(endpoint.clone())
            .accept(
                iroh_blobs::ALPN,
                BlobsProtocol::new(&blobs, endpoint.clone(), None),
            )
            .accept(iroh_gossip::ALPN, gossip)
            .accept(iroh_docs::ALPN, docs.clone())
            .spawn();

        // Stage 5: use import_and_subscribe to get the LiveEvent stream for live convergence.
        let (doc, evs) = docs.api().import_and_subscribe(ticket).await?;
        let ns = doc.id();
        println!("Imported + opened doc: NamespaceId={}", ns);
        events = Some(Box::pin(evs));
        (doc, ContentStore::Mem(blobs), _router, false)
    };
    let ns = doc.id();

    // WI-4c: recover the drive key either by HKDF(dev secret, ns) or by opening a sealed capability
    // token (verifying the expected issuer) and cross-checking its namespace against the ticket's.
    let drive_key: [u8; 32] = match key_src {
        ReplicateKey::DevSecret(secret) => derive_drive_key(&secret, &ns.to_string()),
        ReplicateKey::Capability(cap_in) => {
            let ReplicateCap {
                me,
                expected_issuer,
                token,
            } = *cap_in;
            let cap =
                open_capability(&me, &expected_issuer, &token).context("open capability token")?;
            if cap.namespace_id != ns.to_string() {
                anyhow::bail!(
                    "capability namespace {} does not match the ticket drive namespace {}",
                    cap.namespace_id,
                    ns
                );
            }
            println!(
                "CAPABILITY OK: drive='{}' issuer verified, ns matches ticket; content key recovered",
                cap.drive_name
            );
            cap.content_key
        }
    };
    let n = 12; // AES-GCM nonce

    if let Some(out_dir) = out {
        // === DRIVE MODE ===
        let out_path = Path::new(&out_dir);
        std::fs::create_dir_all(out_path)?;

        // 1. Wait for + fetch + decrypt manifest (reuse the async-blob retry pattern)
        let manifest_key = b"__drive_manifest__.json";
        let mut manifest_entry = None;
        for _attempt in 0..30 {
            let q = iroh_docs::store::Query::key_exact(manifest_key);
            let mut stream = Box::pin(doc.get_many(q).await?);
            if let Some(res) = stream.next().await {
                manifest_entry = Some(res.context("manifest entry error")?);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        }
        let manifest_entry = manifest_entry.context("drive manifest entry not found (timeout)")?;
        let manifest_hash = manifest_entry.content_hash();

        let mut m_fetched: Option<Bytes> = None;
        let mut last_err: Option<anyhow::Error> = None;
        for attempt in 0..100 {
            match blobs.get_bytes(manifest_hash).await {
                Ok(b) => {
                    m_fetched = Some(b);
                    break;
                }
                Err(e) => {
                    last_err = Some(e);
                    if attempt % 10 == 0 {
                        println!("(waiting for manifest content blob, attempt {})", attempt);
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                }
            }
        }
        let m_fetched: Bytes = m_fetched.with_context(|| {
            format!(
                "manifest blob not available (timeout; last err: {:?})",
                last_err
            )
        })?;

        if m_fetched.len() < n {
            anyhow::bail!("manifest blob too short");
        }
        let (m_nonce, m_ct) = m_fetched.split_at(n);
        let manifest_pt = decrypt_blob(&drive_key, m_nonce, m_ct)
            .context("failed to decrypt manifest (wrong secret?)")?;
        let manifest: serde_json::Value =
            serde_json::from_slice(&manifest_pt).context("invalid manifest JSON")?;

        let files = manifest["files"].as_array().cloned().unwrap_or_default();
        let k = files.len();
        // Track the version from the first manifest we actually read (NOT hardcoded): a watcher that
        // joins after a republish must start at the real current version, else it would re-converge
        // needlessly and mislabel the transition. `mut` because the live loop advances it.
        let mut current_version: u64 = manifest["drive_version"].as_u64().unwrap_or(1);
        println!("Manifest: {} files, drive_version={}", k, current_version);

        let mut ok = 0usize;
        let mut total_bytes: u64 = 0;

        // Stage 6: track previous drive state (key -> plaintext_blake3) from the initial reconstruct.
        // Used for skip-unchanged decisions and deletion detection on version bumps.
        let mut prev_files: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();

        for f in files {
            let rel_key = f["key"].as_str().unwrap_or("").to_string();
            let expected_pt_len = f["plaintext_len"].as_u64().unwrap_or(0);
            let expected_blake3 = f["plaintext_blake3"].as_str().unwrap_or("").to_string();

            // Wait for this file's entry
            let mut file_entry = None;
            for _attempt in 0..30 {
                let q = iroh_docs::store::Query::key_exact(rel_key.as_bytes());
                let mut stream = Box::pin(doc.get_many(q).await?);
                if let Some(res) = stream.next().await {
                    file_entry = Some(res.context("file entry error")?);
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            }
            let file_entry = match file_entry {
                Some(e) => e,
                None => {
                    println!("FAIL {} (entry not found)", rel_key);
                    continue;
                }
            };
            let ch = file_entry.content_hash();

            // Fetch blob with retry (async download race)
            let mut f_fetched: Option<Bytes> = None;
            for attempt in 0..100 {
                match blobs.get_bytes(ch).await {
                    Ok(b) => {
                        f_fetched = Some(b);
                        break;
                    }
                    Err(_) => {
                        if attempt % 10 == 0 {
                            println!(
                                "(waiting for content blob of {}, attempt {})",
                                rel_key, attempt
                            );
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    }
                }
            }
            let f_fetched = match f_fetched {
                Some(b) => b,
                None => {
                    println!("FAIL {} (blob not available)", rel_key);
                    continue;
                }
            };

            if f_fetched.len() < n {
                println!("FAIL {} (blob too short)", rel_key);
                continue;
            }
            let (f_nonce, f_ct) = f_fetched.split_at(n);
            let pt = match decrypt_blob(&drive_key, f_nonce, f_ct) {
                Ok(p) => p,
                Err(e) => {
                    println!("FAIL {} (decrypt error: {})", rel_key, e);
                    continue;
                }
            };

            let got_blake3 = blake3::hash(&pt).to_hex().to_string();
            if got_blake3 != expected_blake3 {
                println!(
                    "FAIL {} (BLAKE3 mismatch: got {} expected {})",
                    rel_key, got_blake3, expected_blake3
                );
                continue;
            }
            if pt.len() as u64 != expected_pt_len {
                println!("FAIL {} (len mismatch)", rel_key);
                continue;
            }

            // Write to out/<rel_key>
            let target = out_path.join(&rel_key);
            if let Some(parent) = target.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            std::fs::write(&target, &pt)?;
            println!("OK {} (pt_len={}, blake3 verified)", rel_key, pt.len());
            prev_files.insert(rel_key.clone(), expected_blake3.clone());
            ok += 1;
            total_bytes += pt.len() as u64;
        }

        println!(
            "DRIVE REPLICATED: {}/{} files, all plaintext BLAKE3 verified, total_bytes={}",
            ok, k, total_bytes
        );

        // Stage 9: with --store, flush the downloaded blobs to disk so a later reopen (offline resume)
        // can serve them. No-op for the in-memory path. (Stage 10 allows watch + --store: this initial
        // flush still runs before the watch loop, which re-flushes after each converged version.)
        blobs.flush().await;

        // --- LIVE CONVERGENCE (Stage 6 differential + deletions) ---
        if watch_secs > 0 {
            use iroh_docs::engine::LiveEvent;
            use std::time::Instant;
            let deadline = Instant::now() + std::time::Duration::from_secs(watch_secs);
            println!("(watching {}s for live updates...)", watch_secs);

            // events comes from either first-sync (import_and_subscribe) or resume (doc.subscribe after api.open).
            // Store + watch is now supported (Stage 10); mem path is unchanged.
            // This keeps the original live watch behavior byte-identical when --store is absent.
            let mut events = events.context(
                "LiveEvent stream missing (every setup path populates events; unreachable)",
            )?;
            loop {
                let rem = deadline.saturating_duration_since(Instant::now());
                if rem.is_zero() {
                    break;
                }
                match tokio::time::timeout(rem, events.next()).await {
                    Err(_) => break,
                    Ok(None) => break,
                    Ok(Some(Ok(ev))) => {
                        let trigger = match &ev {
                            LiveEvent::InsertRemote { entry, .. } => {
                                entry.key() == b"__drive_manifest__.json"
                            }
                            LiveEvent::PendingContentReady => true,
                            _ => false,
                        };
                        if trigger {
                            // Re-read the manifest (with retry helpers); if version advanced, apply diff:
                            // - skip unchanged (same blake3 in prev_files AND file exists on disk)
                            // - pull/decrypt/verify/write only for changed or added
                            // - delete local files for keys present in prev but absent from new manifest
                            let manifest_hash = match wait_entry_hash(&doc, manifest_key, 20).await
                            {
                                Some(h) => h,
                                None => continue,
                            };
                            let mblob = match blobs.fetch_retry(manifest_hash, 60).await {
                                Some(b) => b,
                                None => {
                                    eprintln!("converge: manifest blob not available yet");
                                    continue;
                                }
                            };
                            if mblob.len() < n {
                                continue;
                            }
                            let (mn, mc) = mblob.split_at(n);
                            let mpt = match decrypt_blob(&drive_key, mn, mc) {
                                Ok(p) => p,
                                Err(e) => {
                                    eprintln!("converge: manifest decrypt failed: {e}");
                                    continue;
                                }
                            };
                            let new_m: serde_json::Value = match serde_json::from_slice(&mpt) {
                                Ok(v) => v,
                                Err(e) => {
                                    eprintln!("converge: manifest JSON parse failed: {e}");
                                    continue;
                                }
                            };
                            let new_ver =
                                new_m["drive_version"].as_u64().unwrap_or(current_version);
                            if new_ver <= current_version {
                                continue;
                            }

                            let old_ver = current_version;
                            let new_files = new_m["files"].as_array().cloned().unwrap_or_default();
                            let new_k = new_files.len();

                            // Build new_state (key -> blake3) from the incoming manifest for fast lookup
                            let mut new_state: std::collections::HashMap<String, String> =
                                std::collections::HashMap::new();
                            for f in &new_files {
                                let rkey = f["key"].as_str().unwrap_or("").to_string();
                                let eblake =
                                    f["plaintext_blake3"].as_str().unwrap_or("").to_string();
                                new_state.insert(rkey, eblake);
                            }

                            let old_file_count = prev_files.len();
                            let mut written = 0usize;
                            let mut deleted = 0usize;

                            // Process new/current files: skip if unchanged+exists, else pull+write (changed/added/new)
                            for f in &new_files {
                                let rkey = f["key"].as_str().unwrap_or("").to_string();
                                let ept_len = f["plaintext_len"].as_u64().unwrap_or(0);
                                let eblake =
                                    f["plaintext_blake3"].as_str().unwrap_or("").to_string();

                                let is_unchanged =
                                    prev_files.get(&rkey).map_or(false, |h| h == &eblake);
                                let target = out_path.join(&rkey);
                                if is_unchanged && target.exists() {
                                    println!("skip unchanged {}", rkey);
                                    continue;
                                }

                                let fch = match wait_entry_hash(&doc, rkey.as_bytes(), 20).await {
                                    Some(h) => h,
                                    None => {
                                        eprintln!("converge: entry {rkey} not found");
                                        continue;
                                    }
                                };
                                let fblob = match blobs.fetch_retry(fch, 60).await {
                                    Some(b) => b,
                                    None => {
                                        eprintln!("converge: blob for {rkey} not available");
                                        continue;
                                    }
                                };
                                if fblob.len() < n {
                                    eprintln!("converge: blob for {rkey} too short");
                                    continue;
                                }
                                let (fnc, fcc) = fblob.split_at(n);
                                let ptt = match decrypt_blob(&drive_key, fnc, fcc) {
                                    Ok(p) => p,
                                    Err(e) => {
                                        eprintln!("converge: decrypt {rkey} failed: {e}");
                                        continue;
                                    }
                                };
                                if blake3::hash(&ptt).to_hex().to_string() != eblake
                                    || ptt.len() as u64 != ept_len
                                {
                                    eprintln!("converge: integrity check failed for {rkey}");
                                    continue;
                                }
                                let target = out_path.join(&rkey);
                                if let Some(parent) = target.parent() {
                                    std::fs::create_dir_all(parent)?;
                                }
                                std::fs::write(&target, &ptt)?;
                                written += 1;
                            }

                            // Deletions: any key in prev_files that is absent from the new manifest -> rm local file
                            for old_key in prev_files.keys() {
                                if !new_state.contains_key(old_key) {
                                    let target = out_path.join(old_key);
                                    let _ = std::fs::remove_file(&target);
                                    deleted += 1;
                                }
                            }

                            prev_files = new_state;
                            current_version = new_ver;
                            println!("DRIVE UPDATED: v{} -> v{}: {} written, {} deleted (file_count {} -> {})", old_ver, new_ver, written, deleted, old_file_count, new_k);
                            // Stage 10: flush after each live-converged version so a restart of the
                            // persistent replicator can resume at the latest version from disk (not
                            // an earlier one). The initial flush after DRIVE REPLICATED (S9) is kept.
                            blobs.flush().await;
                        }
                    }
                    Ok(Some(Err(_))) => {}
                }
            }
        }
    } else {
        // === SINGLE ENTRY MODE (exact backward compat) ===
        let key = "hello.txt";
        let mut entry = None;
        for attempt in 0..20 {
            let q = iroh_docs::store::Query::key_exact(key.as_bytes());
            let mut stream = Box::pin(doc.get_many(q).await?);
            if let Some(res) = stream.next().await {
                entry = Some(res.context("entry stream item error")?);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            if attempt % 5 == 0 {
                println!("(waiting for replicated entry, attempt {})", attempt);
            }
        }
        let entry = entry.context("no entries returned after sync (timeout)")?;

        let entry_author = entry.author();
        let entry_key = String::from_utf8_lossy(entry.key()).to_string();
        let content_hash = entry.content_hash();
        let content_size = entry.content_len();

        println!(
            "R1: received entry key={} author={} hash={} size={}",
            entry_key, entry_author, content_hash, content_size
        );

        // Fetch with the async blob retry (from 9f5a5c18 gate fix)
        let mut fetched: Option<Bytes> = None;
        let mut last_err: Option<anyhow::Error> = None;
        for attempt in 0..100 {
            match blobs.get_bytes(content_hash).await {
                Ok(b) => {
                    fetched = Some(b);
                    break;
                }
                Err(e) => {
                    last_err = Some(e);
                    if attempt % 10 == 0 {
                        println!(
                            "(waiting for content blob download to complete, attempt {})",
                            attempt
                        );
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                }
            }
        }
        let fetched: Bytes = fetched.with_context(|| {
            format!(
                "blob content not available after docs replication (timeout; last error: {:?})",
                last_err
            )
        })?;

        let raw_preview: String = fetched
            .iter()
            .take(16)
            .map(|b| format!("{:02x}", b))
            .collect();
        println!("E3: raw fetched blob (first 16 bytes hex): {} (should not start with '68 69 20 66 72 6f 6d' = 'hi from')", raw_preview);

        // drive_key already computed above for both modes

        if fetched.len() < n {
            anyhow::bail!("fetched blob too short to contain nonce");
        }
        let (nonce, ct) = fetched.split_at(n);
        let plaintext = decrypt_blob(&drive_key, nonce, ct)?;

        println!(
            "R2 (decrypted): plaintext len={} : {:?}",
            plaintext.len(),
            String::from_utf8_lossy(&plaintext)
        );

        let local_hash = iroh_blobs::Hash::new(&fetched);
        println!("     local blake3 (of ct): {}", local_hash);
        println!("     entry content_hash  : {}", content_hash);
        if local_hash != content_hash {
            anyhow::bail!("BLAKE3 mismatch after fetch (ct)");
        }
        println!("     BLAKE3 (ct) match: PASS");

        println!("R3: entry author = {} (docs sync verifies the author signature; no error above means sig OK)", entry_author);

        println!("\nL1 E2EE replication SUCCESS (entry + ciphertext blob replicated over the network; decrypted with drive key).");
    }
    Ok(())
}
