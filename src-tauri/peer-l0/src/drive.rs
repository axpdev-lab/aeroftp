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
// Security: replica-path containment.
//
// The per-file `key` in a drive manifest is fully publisher-controlled. The receiver's
// integrity checks (BLAKE3 + plaintext_len) are computed by the same publisher, so they do
// NOT bound the key. A malicious or phished-into-accepting capability could ship a key such
// as `../../../../home/victim/.bashrc` (or an absolute path: Rust's `Path::join` REPLACES the
// base when the joined component is absolute) and escape the replica root chosen by the user,
// turning "replicate read-only into my chosen folder" into arbitrary file write/delete.
//
// `safe_replica_path` rejects any key that is empty, absolute, rooted, or contains a parent
// (`..`) component, and returns the joined path only when it provably stays under `out_path`.
// Lexical checking is not enough on its own: materialization creates only directories and
// regular files, but the folder the user picks may already hold links (or gain one between
// runs), and a write or delete through one lands outside the root. `write_replica_file` and
// `remove_replica_file` refuse a key whose path on disk goes through a link, and the write goes
// through a fresh temporary renamed over the name, which replaces a link instead of following it.
// --------------------------------------------------------------------------------------------

/// Join a publisher-controlled manifest key under `out_path`, or `None` if the key would escape
/// the replica root. See the module-level note above for the threat model.
pub(crate) fn safe_replica_path(out_path: &Path, rel_key: &str) -> Option<PathBuf> {
    use std::path::Component;
    if rel_key.is_empty() {
        return None;
    }
    let rel = Path::new(rel_key);
    for comp in rel.components() {
        match comp {
            // Plain path segments are the only thing a key may contain.
            Component::Normal(_) | Component::CurDir => {}
            // `..`, a leading `/`, or a Windows prefix (`C:`) can all escape the root.
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(out_path.join(rel))
}

/// The first existing entry from `out_path` down to `target` (included) that is a link, if
/// any. Entries not there yet stop the walk: they are created as real folders. Any other
/// error reading an entry (a folder that cannot be read, say) is returned: it is not proof
/// that no link is on the way.
fn link_on_the_way(out_path: &Path, target: &Path) -> std::io::Result<Option<PathBuf>> {
    let rel = target.strip_prefix(out_path).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{} is not under the replica folder", target.display()),
        )
    })?;
    let mut at = out_path.to_path_buf();
    for part in rel.components() {
        at.push(part);
        match std::fs::symlink_metadata(&at) {
            Ok(meta) if meta.file_type().is_symlink() => return Ok(Some(at)),
            Ok(meta) if meta.is_dir() => refuse_world_writable_folder(&at, &meta)?,
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        }
    }
    Ok(None)
}

/// A folder any user can write into (o+w, sticky or not) lets another user plant a link
/// in it between a check and the I/O that follows; the replica refuses to write through
/// one. The error names the folder, its mode and the fix.
fn refuse_world_writable_folder(folder: &Path, meta: &std::fs::Metadata) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = meta.permissions().mode() & 0o7777;
        if mode & 0o002 != 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "refusing the replica folder {}: it is writable by every user (mode {:o}), so another user could place a link in it; run `chmod o-w {}`",
                    folder.display(),
                    mode,
                    folder.display()
                ),
            ));
        }
    }
    #[cfg(not(unix))]
    let _ = (folder, meta);
    Ok(())
}

/// Check the replica root before anything is written: the root must not be writable by
/// every user, and no folder above it may be (a sticky one, like /tmp, is fine there: no
/// other user can rename or replace the entry it holds). A root writable by its group is
/// allowed, with a warning: its group members are trusted with it, as with any
/// group-writable folder of the user's.
///
/// Answers the canonical root, which every later operation must use instead of the path as
/// given: links on the given path are resolved once, here, where the folders above the
/// canonical root are the ones checked, and a link swapped later cannot redirect the replica.
pub(crate) fn check_replica_root(out_path: &Path) -> std::io::Result<PathBuf> {
    let root = std::fs::canonicalize(out_path)?;
    let meta = std::fs::metadata(&root)?;
    refuse_world_writable_folder(&root, &meta)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = meta.permissions().mode() & 0o7777;
        if mode & 0o020 != 0 {
            eprintln!(
                "warning: the replica folder {} is writable by its group (mode {:o}); its members are trusted with the replica",
                root.display(),
                mode
            );
        }
        // The path as given counts too: a root named through a link, or through folders that
        // are links, can be replaced in any of those folders even when its target is private.
        let given = std::path::absolute(out_path)?;
        // A root that is itself a link must belong to the owner of the folder it names: in
        // a sticky folder another user could otherwise replace a link of their own.
        {
            use std::os::unix::fs::MetadataExt;
            let link = std::fs::symlink_metadata(&given)?;
            if link.file_type().is_symlink() && link.uid() != meta.uid() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!(
                        "refusing the replica folder {}: it is a link owned by another user than the folder it names ({}); give the folder's own path",
                        given.display(),
                        root.display()
                    ),
                ));
            }
        }
        // A folder named on the way that is itself a link (`/tmp/link/root`) can be replaced
        // by the link's owner, even in a sticky folder: it must belong to root or the
        // replica folder's owner.
        for ancestor in given.ancestors().skip(1) {
            use std::os::unix::fs::MetadataExt;
            let entry = std::fs::symlink_metadata(ancestor)?;
            if !link_is_trusted(entry.file_type().is_symlink(), entry.uid(), meta.uid()) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!(
                        "refusing the replica folder {}: the folder {} on its path is a link owned by another user (uid {}), who could replace it; give the folder's own path",
                        given.display(),
                        ancestor.display(),
                        entry.uid()
                    ),
                ));
            }
        }
        for ancestor in root.ancestors().skip(1).chain(given.ancestors().skip(1)) {
            use std::os::unix::fs::MetadataExt;
            let above = std::fs::metadata(ancestor)?;
            let mode = above.permissions().mode() & 0o7777;
            if !ancestor_is_safe(mode, above.uid(), meta.uid()) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!(
                        "refusing the replica folder {}: the folder above it, {}, is writable by every user (mode {:o}) and not both sticky and owned by root or the replica folder's owner, so another user could replace the replica folder; run `chmod o-w {}`",
                        root.display(),
                        ancestor.display(),
                        mode,
                        ancestor.display()
                    ),
                ));
            }
        }
    }
    Ok(root)
}

/// Whether a folder above the replica root, with `mode` and owned by `owner`, cannot have
/// the root's entry replaced by another user: it is not writable by every user, or it is
/// sticky AND owned by root or by `trusted` (the replica folder's owner). The sticky bit
/// keeps other users from replacing an entry, but not the folder's own owner.
#[cfg(unix)]
pub(crate) fn ancestor_is_safe(mode: u32, owner: u32, trusted: u32) -> bool {
    if mode & 0o002 == 0 {
        return true;
    }
    mode & 0o1000 != 0 && (owner == 0 || owner == trusted)
}

/// Whether a component of the replica root's path as given, owned by `owner`, can be relied
/// on: a real folder can (its mode is checked separately), and a link only when it belongs to
/// root or to `trusted` (the replica folder's owner), since a link's owner can replace it.
#[cfg(unix)]
pub(crate) fn link_is_trusted(is_link: bool, owner: u32, trusted: u32) -> bool {
    !is_link || owner == 0 || owner == trusted
}

/// Make the folders from `out_path` down to `folder` that do not exist yet, private to the
/// user (0700 on unix): under a umask of 002 they would otherwise come out 0775, writable by
/// the group.
fn create_replica_folders(out_path: &Path, folder: &Path) -> std::io::Result<()> {
    let rel = folder.strip_prefix(out_path).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{} is not under the replica folder", folder.display()),
        )
    })?;
    let mut at = out_path.to_path_buf();
    for part in rel.components() {
        at.push(part);
        let builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        let builder = {
            use std::os::unix::fs::DirBuilderExt;
            let mut builder = builder;
            builder.mode(0o700);
            builder
        };
        match builder.create(&at) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

fn refuse_link_on_the_way(out_path: &Path, target: &Path) -> std::io::Result<()> {
    match link_on_the_way(out_path, target)? {
        Some(link) => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "refusing a replica path through the link {}",
                link.display()
            ),
        )),
        None => Ok(()),
    }
}

/// Write one replica file at `target` (already through `safe_replica_path`) under `out_path`:
/// no link on the way before or after its folders are made, and the bytes go to a new
/// temporary renamed over the name (deep-scan 14).
pub(crate) fn write_replica_file(
    out_path: &Path,
    target: &Path,
    data: &[u8],
) -> std::io::Result<()> {
    use std::io::Write;
    refuse_link_on_the_way(out_path, target)?;
    if let Some(parent) = target.parent() {
        create_replica_folders(out_path, parent)?;
    }
    refuse_link_on_the_way(out_path, target)?;
    if target.file_name().is_none() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "replica path names no file",
        ));
    }
    // A short name of its own, not derived from the key's: a valid name near the
    // filesystem's 255-byte limit must not fail for the length of its temporary.
    // A rewrite keeps the access the replica had; a new replica, which holds decrypted
    // plaintext, is private to the user (0600). Reading what stands at the name may fail
    // only because nothing is there yet.
    let keep = existing_replica_permissions(target)?;
    let temp = target.with_file_name(format!(".replica-{:016x}.tmp", rand::random::<u64>()));
    let written = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        // Created private, so no other user can open it while the plaintext is written.
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp)?;
        #[cfg(test)]
        BEFORE_REPLICA_WRITE.with(|hook| {
            if let Some(hook) = hook.borrow().as_ref() {
                hook(&temp)
            }
        });
        file.write_all(data)?;
        if let Some(permissions) = &keep {
            file.set_permissions(permissions.clone())?;
        }
        file.sync_all()?;
        std::fs::rename(&temp, target)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    written
}

/// The permissions of the replica file at `target`, to keep across a rewrite: none when
/// nothing is there yet. Any other error reading it (a folder that cannot be searched) is
/// returned, not read as "no file".
fn existing_replica_permissions(target: &Path) -> std::io::Result<Option<std::fs::Permissions>> {
    match std::fs::symlink_metadata(target) {
        Ok(existing) if existing.is_file() => Ok(Some(existing.permissions())),
        Ok(_) => Ok(None),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// What the replica test seam runs on the new temporary, right before the plaintext is
/// written into it.
#[cfg(test)]
type ReplicaWriteHook = Box<dyn Fn(&Path)>;

#[cfg(test)]
thread_local! {
    static BEFORE_REPLICA_WRITE: std::cell::RefCell<Option<ReplicaWriteHook>> =
        const { std::cell::RefCell::new(None) };
}

/// Write one replica file, or report why it was refused and answer `false`: the caller skips
/// that key and goes on, so one refused key (a link on its way, an unwritable folder) cannot
/// stop the replication of the others.
pub(crate) fn write_replica_or_skip(
    out_path: &Path,
    target: &Path,
    data: &[u8],
    report: impl FnOnce(&std::io::Error),
) -> bool {
    match write_replica_file(out_path, target, data) {
        Ok(()) => true,
        Err(e) => {
            report(&e);
            false
        }
    }
}

/// Remove one replica file, never through a link on the way (the file itself, if a link, is
/// removed as a link).
pub(crate) fn remove_replica_file(out_path: &Path, target: &Path) -> std::io::Result<()> {
    if let Some(link) = link_on_the_way(out_path, target)? {
        if link != target {
            return refuse_link_on_the_way(out_path, target);
        }
    }
    match std::fs::remove_file(target) {
        // Already gone (never written, or deleted locally; its folder too): the deletion
        // is applied. The link checks above ran first, so this never skips a refusal.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        // On Windows a link to a folder is removed as a folder entry.
        Err(e)
            if cfg!(windows)
                && std::fs::symlink_metadata(target)
                    .is_ok_and(|meta| meta.file_type().is_symlink()) =>
        {
            std::fs::remove_dir(target).map_err(|_| e)
        }
        other => other,
    }
}

/// How long the watch waits before trying the current version again while one of its keys
/// is unapplied.
const UNAPPLIED_RETRY: std::time::Duration = std::time::Duration::from_secs(30);

/// When the watch next retries the current version: never while every key is applied; else
/// at the time already set, which events arriving meanwhile do not move, or one interval from
/// `now` when none is set (a retry that starts sets the next one itself).
pub(crate) fn next_retry_at(
    unapplied: bool,
    retry_at: Option<std::time::Instant>,
    now: std::time::Instant,
) -> Option<std::time::Instant> {
    if !unapplied {
        return None;
    }
    Some(retry_at.unwrap_or(now + UNAPPLIED_RETRY))
}

/// The keys of the first pass that it could not apply: pending, not applied. They are
/// retried and hold back "live", but they never enter the applied state, so a later version
/// that drops one never deletes a file the replica did not write.
pub(crate) fn first_pass_pending(
    applied_state: &std::collections::HashMap<String, String>,
    keys: &[String],
) -> std::collections::HashSet<String> {
    keys.iter()
        .filter(|key| !applied_state.contains_key(*key))
        .cloned()
        .collect()
}

/// Whether the file of `key` must be (re)written: unless the last applied state holds this
/// very hash for it and the file is on disk. A key a previous version failed to apply holds
/// no hash, so it is written again even when the hash has not changed.
pub(crate) fn needs_write(
    applied_state: &std::collections::HashMap<String, String>,
    key: &str,
    hash: &str,
    on_disk: bool,
) -> bool {
    !(on_disk && applied_state.get(key).is_some_and(|h| h == hash))
}

/// Whether a key is still to apply (written or deleted): one the replica applied before but
/// not now (no hash), or a pending one it never applied. The watch does not report "live"
/// until there is none, and retries the current version meanwhile.
pub(crate) fn still_unapplied(
    applied_state: &std::collections::HashMap<String, String>,
    pending: &std::collections::HashSet<String>,
) -> bool {
    !pending.is_empty() || applied_state.values().any(|hash| hash.is_empty())
}

/// Settle a version into the state the next one compares against. `state` holds the
/// version's keys (plus those whose deletion was refused), `prev` what the replica had
/// applied before, `applied` what this version wrote or found unchanged.
/// - Applied: kept with its hash, and no longer pending.
/// - Not applied, but applied before (its file on disk is the replica's own): kept with no
///   hash, so it is retried and stays deletable.
/// - Never applied: dropped from the state and kept pending, so it is retried but never
///   deleted: the file at that name, if any, is not the replica's.
///
/// A pending key the version no longer lists is dropped: nothing to retry, nothing to delete.
pub(crate) fn settle_version(
    state: &mut std::collections::HashMap<String, String>,
    prev: &std::collections::HashMap<String, String>,
    applied: &std::collections::HashSet<String>,
    pending: &mut std::collections::HashSet<String>,
) {
    pending.retain(|key| state.contains_key(key));
    state.retain(|key, hash| {
        if applied.contains(key) {
            pending.remove(key);
            true
        } else if prev.contains_key(key) {
            hash.clear();
            true
        } else {
            pending.insert(key.clone());
            false
        }
    });
}

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
    /// WI-4d: the per-drive 32-byte content key directly (the replicator already custodies it in its
    /// vault after a `peer import`). Use the ticket's namespace for dialing; no token to re-open.
    Content([u8; 32]),
}

/// WI-4d: what `run_docs_publish` reports back to an in-app caller once the drive's namespace exists
/// and its ticket is shared, BEFORE it blocks serving. Lets the CLI custody `(namespace_id,
/// content_key)` in the vault and print the ticket while the publisher keeps running.
pub struct DrivePublished {
    pub namespace_id: String,
    pub ticket: String,
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

/// WI-4d: extract the drive namespace id from a DocTicket string, WITHOUT importing/syncing it, so the
/// CLI can look up the vault-custodied content key for `peer replicate`.
pub fn namespace_from_ticket(ticket_str: &str) -> Result<String> {
    let ticket: iroh_docs::DocTicket = ticket_str.parse().context("failed to parse DocTicket")?;
    Ok(ticket.capability.id().to_string())
}

/// Issue one sealed capability token per grant, after the drive ticket is known.
/// WI-5b independence test lever: how much addressing the published DocTicket carries.
/// `AEROFTP_PEER_TICKET_ADDRS=id` -> `AddrInfoOptions::Id` (NodeID only), forcing the replicator to
/// resolve the publisher through discovery (Mainline DHT when `AEROFTP_PEER_DISCOVERY=dht`) rather than
/// dialing ticket-embedded addresses. Any other value keeps `RelayAndAddresses` (the robust default that
/// makes first contact discovery-independent). This is the lever that turns a 2-machine `dht` run into a
/// real proof that decentralized discovery RESOLVES, not just that the transfer happens to succeed.
fn ticket_addr_options_from_env() -> iroh_docs::api::protocol::AddrInfoOptions {
    use iroh_docs::api::protocol::AddrInfoOptions;
    match std::env::var("AEROFTP_PEER_TICKET_ADDRS")
        .ok()
        .as_deref()
        .map(str::trim)
    {
        Some("id") => AddrInfoOptions::Id,
        _ => AddrInfoOptions::RelayAndAddresses,
    }
}

pub(crate) fn issue_capabilities(
    cap: &CapIssue,
    ns: &iroh_docs::NamespaceId,
    content_key: &[u8; 32],
    ticket: &iroh_docs::DocTicket,
) -> Result<()> {
    let node_addrs: Vec<String> = ticket.nodes.iter().map(|n| n.id.to_string()).collect();
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
            p.get(rel_key).is_some_and(|old| old == &pt_blake3)
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
        let action = if prev.is_none_or(|p| !p.contains_key(rel_key)) {
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
#[allow(clippy::too_many_arguments)]
pub async fn run_docs_publish(
    key: String,
    dir: Option<String>,
    republish_after: u64,
    republish_count: u64,
    store: Option<String>,
    cfg: crate::endpoint::PeerEndpointConfig,
    publish_key: PublishKey,
    ready: Option<tokio::sync::oneshot::Sender<DrivePublished>>,
) -> Result<()> {
    use bytes::Bytes;
    use iroh::protocol::Router;
    use iroh_blobs::BlobsProtocol;
    use iroh_docs::api::protocol::ShareMode;
    use iroh_docs::protocol::Docs;
    use iroh_gossip::net::Gossip;

    let endpoint = crate::endpoint::build_base_endpoint(cfg).await?;
    let node_id = endpoint.id();

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
            .accept(iroh_blobs::ALPN, BlobsProtocol::new(&blobs, None))
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
            .accept(iroh_blobs::ALPN, BlobsProtocol::new(&blobs, None))
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
    // WI-5b independence lever: `AEROFTP_PEER_TICKET_ADDRS=id` emits a NodeID-ONLY ticket so the
    // replicator MUST resolve us through discovery (the Mainline DHT under DISCOVERY=dht) instead of
    // dialing addresses baked into the ticket — this is what actually EXERCISES decentralized discovery
    // (without it the ticket carries our relay+addrs and the dialer bypasses discovery entirely).
    // Any other value keeps the robust `RelayAndAddresses` default.
    let addr_opts = ticket_addr_options_from_env();
    println!("TICKET ADDRESSING: {addr_opts:?} (WI-5b; `Id` forces discovery resolution on the replicator)");
    let ticket = doc.share(ShareMode::Read, addr_opts).await?;
    println!("\n=== DOC TICKET (copy/paste to docs-replicate side) ===");
    println!("{}", ticket);
    println!("==================================================\n");

    // WI-4d: report the namespace + ticket to an in-app caller (the CLI) now that both exist, so it can
    // custody the drive key in the vault and surface the ticket while we keep serving below. Best-effort:
    // a dropped receiver (caller gone) just means nobody is listening.
    if let Some(tx) = ready {
        let _ = tx.send(DrivePublished {
            namespace_id: ns.to_string(),
            ticket: ticket.to_string(),
        });
    }

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

/// Optional drive-state reporter. The replicator calls it at phase boundaries
/// ("syncing" while a pass is pulling, "live" once converged and just watching)
/// so a host UI can reflect status without polling. `None` = silent (CLI). The
/// crate stays UI-agnostic: it only hands back a short state string.
pub type StatusReporter = std::sync::Arc<dyn Fn(&str) + Send + Sync>;

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
    on_status: Option<StatusReporter>,
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
            .accept(iroh_blobs::ALPN, BlobsProtocol::new(&blobs_fs, None))
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
            .accept(iroh_blobs::ALPN, BlobsProtocol::new(&blobs, None))
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
        ReplicateKey::Content(content_key) => {
            // WI-4d: the CLI already opened the capability at `peer import` time and stored the content
            // key in the vault; here it passes that key directly. Dial/namespace come from the ticket.
            println!("CONTENT KEY: provided by caller (vault-custodied); replicating ns={ns}");
            content_key
        }
    };
    let n = 12; // AES-GCM nonce

    if let Some(out_dir) = out {
        // === DRIVE MODE ===
        std::fs::create_dir_all(Path::new(&out_dir))?;
        // Every operation below uses the checked canonical root, never the path as given.
        let out_root = check_replica_root(Path::new(&out_dir))?;
        let out_path = out_root.as_path();

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
        // Every safe key of the manifest: one the pass fails to apply is recorded as such.
        let mut first_pass_keys: Vec<String> = Vec::new();

        for f in files {
            let rel_key = f["key"].as_str().unwrap_or("").to_string();
            let expected_pt_len = f["plaintext_len"].as_u64().unwrap_or(0);
            let expected_blake3 = f["plaintext_blake3"].as_str().unwrap_or("").to_string();

            // Security: refuse any publisher-controlled key that would escape the replica root
            // BEFORE touching the store or the disk. Skip (do not abort) so one hostile entry
            // cannot deny replication of the legitimate ones.
            let target = match safe_replica_path(out_path, &rel_key) {
                Some(p) => p,
                None => {
                    println!(
                        "FAIL {} (unsafe key rejected: escapes replica root)",
                        rel_key
                    );
                    continue;
                }
            };
            first_pass_keys.push(rel_key.clone());

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

            // Write to out/<rel_key> (target was containment-checked above).
            if !write_replica_or_skip(out_path, &target, &pt, |e| {
                println!("FAIL {} (write refused: {})", rel_key, e)
            }) {
                continue;
            }
            println!("OK {} (pt_len={}, blake3 verified)", rel_key, pt.len());
            prev_files.insert(rel_key.clone(), expected_blake3.clone());
            ok += 1;
            total_bytes += pt.len() as u64;
        }
        let mut pending = first_pass_pending(&prev_files, &first_pass_keys);

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
            // Drive-state reporting (event-driven, no polling): announce "live"
            // on entering the watch and again after any converge settles, and
            // "syncing" when a delta lands. `live_announced` de-dups the emit.
            let mut live_announced = false;
            // A key the first pass could not apply holds back "live" and is retried.
            let mut unapplied = still_unapplied(&prev_files, &pending);
            let mut retry_at = next_retry_at(unapplied, None, Instant::now());
            loop {
                if !live_announced {
                    // Not "live" while a key of the current version is unapplied.
                    if let Some(f) = &on_status {
                        f(if unapplied { "syncing" } else { "live" });
                    }
                    live_announced = true;
                }
                let rem = deadline.saturating_duration_since(Instant::now());
                if rem.is_zero() {
                    break;
                }
                // While a key of the current version is unapplied, the current version is
                // tried again at a fixed time, which events arriving meanwhile do not move.
                let now = Instant::now();
                let wait = match retry_at {
                    Some(at) => rem.min(at.saturating_duration_since(now)),
                    None => rem,
                };
                let mut retrying = retry_at.is_some_and(|at| now >= at);
                let trigger = if retrying {
                    // Due already: retried before any event that is waiting, so a steady
                    // stream of events cannot hold it back.
                    retry_at = Some(now + UNAPPLIED_RETRY);
                    true
                } else {
                    match tokio::time::timeout(wait, events.next()).await {
                        Err(_) if retry_at.is_some_and(|at| Instant::now() >= at) => {
                            retrying = true;
                            retry_at = Some(Instant::now() + UNAPPLIED_RETRY);
                            true
                        }
                        Err(_) => break,
                        Ok(None) => break,
                        Ok(Some(Ok(ev))) => match &ev {
                            LiveEvent::InsertRemote { entry, .. } => {
                                entry.key() == b"__drive_manifest__.json"
                            }
                            LiveEvent::PendingContentReady => true,
                            _ => false,
                        },
                        Ok(Some(Err(_))) => false,
                    }
                };
                if trigger {
                    // A new version is converging: flip the host UI back
                    // to "syncing"; the next loop-top re-announces "live".
                    if let Some(f) = &on_status {
                        f("syncing");
                    }
                    live_announced = false;
                    // Re-read the manifest (with retry helpers); if version advanced, apply diff:
                    // - skip unchanged (same blake3 in prev_files AND file exists on disk)
                    // - pull/decrypt/verify/write only for changed or added
                    // - delete local files for keys present in prev but absent from new manifest
                    let manifest_hash = match wait_entry_hash(&doc, manifest_key, 20).await {
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
                    let new_ver = new_m["drive_version"].as_u64().unwrap_or(current_version);
                    if new_ver < current_version || (new_ver == current_version && !retrying) {
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
                        let eblake = f["plaintext_blake3"].as_str().unwrap_or("").to_string();
                        new_state.insert(rkey, eblake);
                    }

                    let old_file_count = prev_files.len();
                    let mut written = 0usize;
                    let mut deleted = 0usize;
                    let mut applied = std::collections::HashSet::new();
                    let mut undeleted: Vec<String> = Vec::new();

                    // Process new/current files: skip if unchanged+exists, else pull+write (changed/added/new)
                    for f in &new_files {
                        let rkey = f["key"].as_str().unwrap_or("").to_string();
                        let ept_len = f["plaintext_len"].as_u64().unwrap_or(0);
                        let eblake = f["plaintext_blake3"].as_str().unwrap_or("").to_string();

                        // Security: same containment check as the initial pass; reject
                        // keys that would escape the replica root before any I/O.
                        let target = match safe_replica_path(out_path, &rkey) {
                            Some(p) => p,
                            None => {
                                eprintln!("converge: unsafe key rejected {rkey}");
                                // Never written, so never retried: it is settled.
                                applied.insert(rkey.clone());
                                continue;
                            }
                        };

                        if !needs_write(&prev_files, &rkey, &eblake, target.exists()) {
                            println!("skip unchanged {}", rkey);
                            applied.insert(rkey.clone());
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
                        // target was containment-checked above.
                        if !write_replica_or_skip(out_path, &target, &ptt, |e| {
                            eprintln!("converge: write {rkey} refused: {e}")
                        }) {
                            continue;
                        }
                        applied.insert(rkey.clone());
                        written += 1;
                    }

                    // Deletions: any key in prev_files that is absent from the new manifest -> rm local file
                    for old_key in prev_files.keys() {
                        if !new_state.contains_key(old_key) {
                            // Security: never let a publisher-controlled key drive a
                            // remove_file outside the replica root. Keys reach prev_files
                            // only after passing the guard, but re-check defensively.
                            if let Some(target) = safe_replica_path(out_path, old_key) {
                                match remove_replica_file(out_path, &target) {
                                    Ok(()) => deleted += 1,
                                    Err(e) => {
                                        eprintln!("converge: delete {old_key} refused: {e}");
                                        undeleted.push(old_key.clone());
                                    }
                                }
                            }
                        }
                    }

                    // A file whose deletion was refused stays known (with no hash), so
                    // the next version tries to delete it again.
                    for key in undeleted {
                        new_state.insert(key, String::new());
                    }
                    settle_version(&mut new_state, &prev_files, &applied, &mut pending);
                    unapplied = still_unapplied(&new_state, &pending);
                    retry_at = next_retry_at(unapplied, retry_at, Instant::now());
                    prev_files = new_state;
                    current_version = new_ver;
                    println!(
                        "DRIVE UPDATED: v{} -> v{}: {} written, {} deleted (file_count {} -> {})",
                        old_ver, new_ver, written, deleted, old_file_count, new_k
                    );
                    // Stage 10: flush after each live-converged version so a restart of the
                    // persistent replicator can resume at the latest version from disk (not
                    // an earlier one). The initial flush after DRIVE REPLICATED (S9) is kept.
                    blobs.flush().await;
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

#[cfg(test)]
mod replica_path_tests {
    use super::safe_replica_path;
    use std::path::Path;

    #[test]
    fn accepts_plain_relative_keys() {
        let root = Path::new("/home/u/AeroShare/drive1");
        assert_eq!(
            safe_replica_path(root, "a/b/c.txt"),
            Some(root.join("a/b/c.txt"))
        );
        assert_eq!(
            safe_replica_path(root, "file.bin"),
            Some(root.join("file.bin"))
        );
        // A leading `./` is harmless and stays inside the root.
        assert_eq!(safe_replica_path(root, "./x.txt"), Some(root.join("x.txt")));
    }

    #[test]
    fn rejects_traversal_and_absolute_keys() {
        let root = Path::new("/home/u/AeroShare/drive1");
        // Empty key.
        assert_eq!(safe_replica_path(root, ""), None);
        // Parent-dir escape.
        assert_eq!(safe_replica_path(root, "../../../../home/u/.bashrc"), None);
        assert_eq!(safe_replica_path(root, "a/../../etc/passwd"), None);
        // Absolute key would replace the base in Path::join.
        assert_eq!(safe_replica_path(root, "/etc/passwd"), None);
        assert_eq!(safe_replica_path(root, "/home/u/.bashrc"), None);
    }

    /// Deep-scan 14: a replica file is never written or removed through a link in the folder.
    #[cfg(unix)]
    #[test]
    fn replica_files_never_go_through_a_link() {
        use crate::drive::{
            link_on_the_way, remove_replica_file, write_replica_file, write_replica_or_skip,
        };
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&out).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("victim"), b"precious").unwrap();
        std::os::unix::fs::symlink(&outside, out.join("docs")).unwrap();
        std::os::unix::fs::symlink(outside.join("victim"), out.join("top.txt")).unwrap();

        let through_folder = safe_replica_path(&out, "docs/victim").unwrap();
        assert!(write_replica_file(&out, &through_folder, b"evil").is_err());
        assert!(remove_replica_file(&out, &through_folder).is_err());
        assert_eq!(std::fs::read(outside.join("victim")).unwrap(), b"precious");

        // A link at the name itself is refused too (the rename covers one that appears later).
        let at_name = safe_replica_path(&out, "top.txt").unwrap();
        assert!(link_on_the_way(&out, &at_name).unwrap().is_some());
        assert!(write_replica_file(&out, &at_name, b"new").is_err());
        assert_eq!(std::fs::read(outside.join("victim")).unwrap(), b"precious");
        // The link at the name is removed as itself; its target stays.
        remove_replica_file(&out, &at_name).unwrap();
        assert_eq!(
            std::fs::symlink_metadata(&at_name).unwrap_err().kind(),
            std::io::ErrorKind::NotFound
        );
        assert_eq!(std::fs::read(outside.join("victim")).unwrap(), b"precious");

        let fresh = safe_replica_path(&out, "a/b/c.txt").unwrap();
        write_replica_file(&out, &fresh, b"ok").unwrap();
        assert_eq!(std::fs::read(&fresh).unwrap(), b"ok");
        remove_replica_file(&out, &fresh).unwrap();
        assert!(!fresh.exists());

        // A refused key is reported and skipped; the next key is still written.
        let mut reported = Vec::new();
        assert!(!write_replica_or_skip(
            &out,
            &through_folder,
            b"evil",
            |e| { reported.push(e.kind()) }
        ));
        assert_eq!(reported, vec![std::io::ErrorKind::InvalidInput]);
        let next = safe_replica_path(&out, "next.txt").unwrap();
        assert!(write_replica_or_skip(&out, &next, b"next", |_| panic!(
            "not refused"
        )));
        assert_eq!(std::fs::read(&next).unwrap(), b"next");
        assert_eq!(std::fs::read(outside.join("victim")).unwrap(), b"precious");

        // A folder that cannot be read is not proof that no link is on the way.
        use std::os::unix::fs::PermissionsExt;
        let locked = out.join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let behind = safe_replica_path(&out, "locked/x.txt").unwrap();
        // A runner that bypasses permissions (root, CAP_DAC_OVERRIDE) reads the locked
        // folder anyway; there the case cannot be built.
        let bypassed = std::fs::read_dir(&locked).is_ok();
        let walked = link_on_the_way(&out, &behind);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
        if !bypassed {
            assert!(
                walked.is_err(),
                "a permission error is reported, not read as no link"
            );
        }
    }

    /// A rewrite keeps a long valid name and the replica's access; a key not applied
    /// in a version keeps no hash, so the next version retries it.
    #[cfg(unix)]
    #[test]
    fn replica_rewrites_keep_long_names_and_access_and_retry_what_failed() {
        use crate::drive::write_replica_file;
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        std::fs::create_dir_all(&out).unwrap();

        let long = "n".repeat(240);
        let target = safe_replica_path(&out, &long).unwrap();
        write_replica_file(&out, &target, b"long").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"long");

        let private = safe_replica_path(&out, "private.txt").unwrap();
        std::fs::write(&private, b"old").unwrap();
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o600)).unwrap();
        write_replica_file(&out, &private, b"new").unwrap();
        assert_eq!(std::fs::read(&private).unwrap(), b"new");
        let mode = std::fs::metadata(&private).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    /// A new replica file and the folders the replica makes are private to the user; a
    /// rewrite keeps the mode an existing replica had.
    #[cfg(unix)]
    #[test]
    fn replica_files_and_folders_are_created_private() {
        use crate::drive::write_replica_file;
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        std::fs::create_dir_all(&out).unwrap();

        let fresh = safe_replica_path(&out, "a/b/new.txt").unwrap();
        write_replica_file(&out, &fresh, b"plain").unwrap();
        assert_eq!(mode(&fresh), 0o600);
        assert_eq!(mode(&out.join("a")), 0o700);
        assert_eq!(mode(&out.join("a/b")), 0o700);

        let shared = safe_replica_path(&out, "shared.txt").unwrap();
        std::fs::write(&shared, b"old").unwrap();
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_replica_file(&out, &shared, b"new").unwrap();
        assert_eq!(mode(&shared), 0o644);
    }

    /// The replica refuses a folder every user can write into: the root, a folder above it
    /// that is not sticky, and a folder on a key's path. A group-writable root is allowed.
    #[cfg(unix)]
    #[test]
    fn world_writable_replica_folders_are_refused() {
        use crate::drive::{check_replica_root, write_replica_file};
        use std::os::unix::fs::PermissionsExt;
        let set = |p: &Path, m: u32| {
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(m)).unwrap()
        };
        let dir = tempfile::tempdir().unwrap();

        let open_root = dir.path().join("open_root");
        std::fs::create_dir(&open_root).unwrap();
        set(&open_root, 0o777);
        assert!(check_replica_root(&open_root).is_err());

        let open_parent = dir.path().join("open_parent");
        std::fs::create_dir_all(open_parent.join("root")).unwrap();
        set(&open_parent, 0o777);
        set(&open_parent.join("root"), 0o755);
        assert!(check_replica_root(&open_parent.join("root")).is_err());
        set(&open_parent, 0o1777);
        assert!(
            check_replica_root(&open_parent.join("root")).is_ok(),
            "a sticky parent is fine"
        );
        set(&open_parent, 0o755);

        let group_root = dir.path().join("group_root");
        std::fs::create_dir(&group_root).unwrap();
        set(&group_root, 0o775);
        assert!(
            check_replica_root(&group_root).is_ok(),
            "group-writable warns only"
        );

        let plain_root = dir.path().join("plain_root");
        std::fs::create_dir(&plain_root).unwrap();
        set(&plain_root, 0o755);
        assert!(check_replica_root(&plain_root).is_ok());

        std::fs::create_dir(plain_root.join("open")).unwrap();
        set(&plain_root.join("open"), 0o777);
        let through = safe_replica_path(&plain_root, "open/x.txt").unwrap();
        assert!(write_replica_file(&plain_root, &through, b"x").is_err());
        assert!(!through.exists());
    }

    /// A key a version failed to apply is written again by a later version with the same
    /// hash, and the watch is not "live" while any key is unapplied.
    #[test]
    fn a_failed_write_is_retried_and_holds_back_live() {
        use crate::drive::{needs_write, settle_version, still_unapplied};
        // Both keys were applied before; this version writes only one of them.
        let prev = std::collections::HashMap::from([
            ("ok".to_string(), "h0".to_string()),
            ("failed".to_string(), "h0".to_string()),
        ]);
        let mut state = std::collections::HashMap::from([
            ("ok".to_string(), "h1".to_string()),
            ("failed".to_string(), "h2".to_string()),
        ]);
        let applied = std::collections::HashSet::from(["ok".to_string()]);
        let mut pending = std::collections::HashSet::new();
        settle_version(&mut state, &prev, &applied, &mut pending);
        assert_eq!(state["failed"], "", "kept, deletable, with no hash");
        let none = std::collections::HashSet::new();
        assert!(
            still_unapplied(&state, &none),
            "not live while a key is unapplied"
        );
        // The next version carries the same hashes: the failed key is written again.
        assert!(needs_write(&state, "failed", "h2", true));
        assert!(!needs_write(&state, "ok", "h1", true));
        assert!(
            needs_write(&state, "ok", "h1", false),
            "a missing file is rewritten"
        );
        state.insert("failed".to_string(), "h2".to_string());
        assert!(!still_unapplied(&state, &none));
    }

    /// A replica root named through a link counts by the path as given: a link in a folder
    /// every user can write (not sticky) is refused even when it names a private folder.
    #[cfg(unix)]
    #[test]
    fn a_root_named_through_a_link_in_an_open_folder_is_refused() {
        use crate::drive::check_replica_root;
        use std::os::unix::fs::PermissionsExt;
        let set = |p: &Path, m: u32| {
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(m)).unwrap()
        };
        let dir = tempfile::tempdir().unwrap();
        let private = dir.path().join("private");
        std::fs::create_dir(&private).unwrap();
        set(&private, 0o700);
        let open = dir.path().join("open");
        std::fs::create_dir(&open).unwrap();
        std::os::unix::fs::symlink(&private, open.join("root")).unwrap();
        set(&open, 0o777);
        assert!(check_replica_root(&open.join("root")).is_err());
        assert!(
            check_replica_root(&private).is_ok(),
            "the folder's own path is fine"
        );
        set(&open, 0o1777);
        assert!(
            check_replica_root(&open.join("root")).is_ok(),
            "a link of the folder's owner in a sticky folder cannot be replaced"
        );
        set(&open, 0o755);
    }

    /// The temporary is already private when the plaintext goes in, and an error reading
    /// what stands at the name (a folder that cannot be searched) is returned.
    #[cfg(unix)]
    #[test]
    fn the_temporary_is_private_before_any_plaintext_is_written() {
        use crate::drive::{
            existing_replica_permissions, write_replica_file, BEFORE_REPLICA_WRITE,
        };
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        let seen = std::rc::Rc::new(std::cell::Cell::new(None));
        let record = seen.clone();
        BEFORE_REPLICA_WRITE.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move |temp: &Path| {
                let meta = std::fs::metadata(temp).unwrap();
                record.set(Some((meta.permissions().mode() & 0o777, meta.len())));
            }))
        });
        let target = safe_replica_path(&out, "secret.txt").unwrap();
        let written = write_replica_file(&out, &target, b"plaintext");
        BEFORE_REPLICA_WRITE.with(|hook| *hook.borrow_mut() = None);
        written.unwrap();
        assert_eq!(seen.get(), Some((0o600, 0)), "private and still empty");

        let locked = out.join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let bypassed = std::fs::read_dir(&locked).is_ok();
        let read = existing_replica_permissions(&locked.join("x.txt"));
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
        if !bypassed {
            assert!(
                read.is_err(),
                "an unreadable folder is reported, not read as no file"
            );
        }
        assert!(existing_replica_permissions(&out.join("missing.txt"))
            .unwrap()
            .is_none());
    }

    /// The retry time is not moved by events arriving meanwhile, and a key the first pass
    /// could not apply holds back "live" from the start.
    #[test]
    fn the_retry_time_holds_and_a_failed_first_write_is_not_live() {
        use crate::drive::{first_pass_pending, needs_write, next_retry_at, still_unapplied};
        let t0 = std::time::Instant::now();
        let first = next_retry_at(true, None, t0).unwrap();
        // Events every 10 s: the retry stays at its first time instead of moving away.
        for step in 1..=5u64 {
            let at_event = t0 + std::time::Duration::from_secs(10 * step);
            assert_eq!(next_retry_at(true, Some(first), at_event), Some(first));
        }
        assert_eq!(next_retry_at(false, Some(first), t0), None);

        let first_pass = std::collections::HashMap::from([("ok".to_string(), "h1".to_string())]);
        let pending = first_pass_pending(&first_pass, &["ok".to_string(), "failed".to_string()]);
        assert!(
            still_unapplied(&first_pass, &pending),
            "a failed first write is not live"
        );
        assert!(needs_write(&first_pass, "failed", "h2", false));
        assert!(!first_pass.contains_key("failed"), "pending, never applied");
    }

    /// A world-writable folder above the root is accepted only when it is sticky and owned
    /// by root or the replica folder's owner: its owner could otherwise swap the root.
    #[cfg(unix)]
    #[test]
    fn a_sticky_ancestor_must_belong_to_root_or_the_replica_owner() {
        use crate::drive::ancestor_is_safe;
        let me = 1000;
        assert!(ancestor_is_safe(0o755, 4242, me), "not world-writable");
        assert!(
            ancestor_is_safe(0o1777, 0, me),
            "sticky, owned by root (/tmp)"
        );
        assert!(
            ancestor_is_safe(0o1777, me, me),
            "sticky, owned by the replica's owner"
        );
        assert!(
            !ancestor_is_safe(0o1777, 4242, me),
            "sticky, owned by another user"
        );
        assert!(
            !ancestor_is_safe(0o777, me, me),
            "world-writable, not sticky"
        );
    }

    /// A link on the root's path as given counts only when it belongs to root or the replica
    /// folder's owner: another user's link, even in sticky /tmp, can be replaced by them.
    #[cfg(unix)]
    #[test]
    fn a_link_on_the_roots_path_must_belong_to_root_or_the_owner() {
        use crate::drive::{check_replica_root, link_is_trusted};
        let me = 1000;
        assert!(link_is_trusted(false, 4242, me), "a real folder");
        assert!(link_is_trusted(true, 0, me), "a link of root");
        assert!(
            link_is_trusted(true, me, me),
            "a link of the replica's owner"
        );
        assert!(!link_is_trusted(true, 4242, me), "a link of another user");

        // A link of my own on the way is accepted.
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir_all(real.join("root")).unwrap();
        std::os::unix::fs::symlink(&real, dir.path().join("link")).unwrap();
        assert!(check_replica_root(&dir.path().join("link").join("root")).is_ok());
    }

    /// A deletion whose file, or whose folder, is already gone is applied: the watch does not
    /// keep retrying it, while a link on the way is still refused first.
    #[cfg(unix)]
    #[test]
    fn deleting_a_replica_file_that_is_already_gone_is_applied() {
        use crate::drive::remove_replica_file;
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        std::fs::create_dir_all(out.join("kept")).unwrap();
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, out.join("linked")).unwrap();

        let missing = safe_replica_path(&out, "kept/missing.txt").unwrap();
        remove_replica_file(&out, &missing).unwrap();
        let no_folder = safe_replica_path(&out, "gone/deeper/x.txt").unwrap();
        remove_replica_file(&out, &no_folder).unwrap();
        let through_link = safe_replica_path(&out, "linked/x.txt").unwrap();
        assert!(
            remove_replica_file(&out, &through_link).is_err(),
            "still refused"
        );
    }

    /// A key the replica never applied is retried but never deleted: when a later version
    /// drops it, the file at that name (not the replica's) is left alone (Greptile on #1529).
    #[test]
    fn a_key_never_applied_is_never_deleted() {
        use crate::drive::{settle_version, still_unapplied};
        let prev = std::collections::HashMap::new();
        let mut state = std::collections::HashMap::from([
            ("a".to_string(), "h1".to_string()),
            ("report.pdf".to_string(), "h2".to_string()),
        ]);
        let applied = std::collections::HashSet::from(["a".to_string()]);
        let mut pending = std::collections::HashSet::new();
        settle_version(&mut state, &prev, &applied, &mut pending);
        assert!(
            !state.contains_key("report.pdf"),
            "not in the applied state: never deleted"
        );
        assert!(pending.contains("report.pdf"), "but still retried");
        assert!(still_unapplied(&state, &pending));

        // The next version drops it: the deletion loop walks the applied state only, and the
        // pending key is let go without a deletion.
        let prev = state.clone();
        let mut next = std::collections::HashMap::from([("a".to_string(), "h1".to_string())]);
        let deletable: Vec<_> = prev.keys().filter(|k| !next.contains_key(*k)).collect();
        assert!(deletable.is_empty());
        settle_version(&mut next, &prev, &applied, &mut pending);
        assert!(pending.is_empty());
        assert!(!still_unapplied(&next, &pending));
    }

    /// A root named through a chain of links (`link -> hop -> private`) is answered as its
    /// canonical folder, which the replica then uses: a later swap of `hop` cannot redirect
    /// it (Greptile on #1529).
    #[cfg(unix)]
    #[test]
    fn a_root_through_chained_links_is_used_by_its_canonical_path() {
        use crate::drive::check_replica_root;
        let dir = tempfile::tempdir().unwrap();
        let private = dir.path().join("private");
        std::fs::create_dir(&private).unwrap();
        std::os::unix::fs::symlink(&private, dir.path().join("hop")).unwrap();
        std::os::unix::fs::symlink(dir.path().join("hop"), dir.path().join("link")).unwrap();
        let root = check_replica_root(&dir.path().join("link")).unwrap();
        assert_eq!(root, private.canonicalize().unwrap());
        assert!(!std::fs::symlink_metadata(&root)
            .unwrap()
            .file_type()
            .is_symlink());
    }
}
