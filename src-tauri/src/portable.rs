//! Portable-mode detection and data directory resolution.
//!
//! When AeroFTP is shipped as the Windows portable ZIP, a `portable.marker`
//! file lives next to `AeroFTP.exe`. Its presence is the single source of
//! truth for "this is a portable install". When detected:
//!
//!   - all per-app data (config, cache, logs, vault, AI databases) goes into
//!     `<exe-dir>/data/...` instead of `%APPDATA%`/`~/.config`. This is what
//!     "portable" means to users: copy the folder, your state comes with it.
//!   - the auto-updater swaps the `.exe` in place rather than launching the
//!     NSIS installer (handled in `windows_update_helper.rs`).
//!
//! Detection is cached on first call. The marker is read at most once per
//! process; if the user adds or removes it after launch, behaviour for the
//! current session is unchanged. This is intentional — we don't want a
//! mid-session jump between two data directories.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

const MARKER_FILENAME: &str = "portable.marker";
const PORTABLE_DATA_DIRNAME: &str = "data";
const AEROFTP_DATA_RELEASE_LEAF: &str = "aeroftp";
const AEROFTP_DATA_DEBUG_LEAF: &str = "aeroftp-dev";

/// Cached portable-mode flag. Computed on first access and reused.
static PORTABLE_ROOT: OnceLock<Option<PathBuf>> = OnceLock::new();
static LEGACY_APP_CONFIG_MIGRATED: OnceLock<()> = OnceLock::new();

/// Resolve the portable root directory (the folder containing AeroFTP.exe
/// and `portable.marker`). Returns `None` when not running as portable.
fn portable_root() -> Option<&'static Path> {
    PORTABLE_ROOT
        .get_or_init(|| {
            let exe = std::env::current_exe().ok()?;
            let dir = exe.parent()?.to_path_buf();
            let marker = dir.join(MARKER_FILENAME);
            if marker.is_file() {
                Some(dir)
            } else {
                None
            }
        })
        .as_deref()
}

/// True when the running binary is the portable build.
pub fn is_portable() -> bool {
    portable_root().is_some()
}

/// Portable data root: `<exe-dir>/data`. None when not portable.
fn portable_data_root() -> Option<PathBuf> {
    portable_root().map(|root| root.join(PORTABLE_DATA_DIRNAME))
}

/// Ensure a directory exists with secure permissions when portable.
/// Idempotent; safe to call repeatedly.
fn ensure_dir(path: &Path) -> Result<(), String> {
    if !path.exists() {
        std::fs::create_dir_all(path)
            .map_err(|e| format!("Failed to create {}: {e}", path.display()))?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("Failed to secure {}: {e}", path.display()))?;
    }
    Ok(())
}

pub fn aeroftp_data_leaf_for_debug(debug: bool) -> &'static str {
    if debug {
        AEROFTP_DATA_DEBUG_LEAF
    } else {
        AEROFTP_DATA_RELEASE_LEAF
    }
}

fn aeroftp_data_leaf() -> &'static str {
    aeroftp_data_leaf_for_debug(cfg!(debug_assertions))
}

/// Single source of truth for AeroFTP's file-backed app state.
///
/// Release builds preserve the historical `aeroftp` leaf byte-for-byte. Debug
/// builds use the sibling `aeroftp-dev` leaf so `tauri dev` / `cargo run`
/// cannot read or mutate an installed release vault, sync journal, settings, or
/// SQLite history. Portable builds keep the same self-contained `<exe>/data`
/// root and apply the same release/debug leaf underneath it.
pub fn aeroftp_data_root() -> Option<PathBuf> {
    let leaf = aeroftp_data_leaf();
    if let Some(data_root) = portable_data_root() {
        let dir = data_root.join(leaf);
        ensure_dir(&dir).ok()?;
        return Some(dir);
    }
    let dir = dirs::config_dir()
        .or_else(dirs::home_dir)
        .map(|base| base.join(leaf))?;
    ensure_dir(&dir).ok()?;
    Some(dir)
}

/// Resolve the legacy identifier-scoped config directory used before the
/// unified data-root migration. This is read only as a release-build migration
/// source; debug builds intentionally do not copy release state into
/// `aeroftp-dev`.
///
/// It is built from [`LEGACY_APP_IDENTIFIER`] and never from the identifier in
/// `tauri.conf.json`: Tauri's `app_config_dir()` answers with the CURRENT
/// identifier, which since #814 is a directory the pre-migration releases never
/// wrote, so asking Tauri turned the GUI migration into a silent no-op.
fn legacy_app_config_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|base| base.join(LEGACY_APP_IDENTIFIER))
}

/// The entries the import never carries, whatever the destination holds: a
/// SQLite sidecar and a symbolic link. Shared by [`copy_missing_tree`] and by
/// [`has_importable_file`], so the offer and the copy apply the same skip
/// rules.
fn never_copied(src: &Path) -> bool {
    // SQLite sidecars belong to one database generation, not to a directory.
    // In particular, after a keystore restore removed -wal/-shm, copying the
    // legacy sidecars on the next boot can replay OLD pages over the restored
    // database (#736). New databases are snapshotted below with their committed
    // WAL contents; sidecars must never travel independently.
    let name = src.file_name().and_then(|s| s.to_str()).unwrap_or_default();
    if [".db-wal", ".db-shm", ".db-journal"]
        .iter()
        .any(|suffix| name.ends_with(suffix))
    {
        return true;
    }
    // The tree we import here is a config tree the user consented to copy, but a
    // symlink inside it can point anywhere: outside the consented tree (dragging
    // in foreign secrets like ~/.ssh) or at an ancestor, forming a cycle that
    // would recurse until path-length exhaustion. So we never follow links, we
    // skip them; skipping also kills the cycle recursion. `symlink_metadata`
    // never follows the link; a metadata error means the path is gone, and the
    // callers' `is_dir`/`is_file` checks already no-op on a missing path.
    if let Ok(meta) = src.symlink_metadata() {
        if meta.file_type().is_symlink() {
            return true;
        }
    }
    false
}

/// True when `src` holds at least one file [`copy_missing_tree`] would carry,
/// at any depth. An empty host config, or one made only of sidecars, symbolic
/// links and empty folders, has nothing to import. A folder or entry this walk
/// cannot read counts as importable: the copy fails on it and reports the
/// error, where answering "nothing here" would silently hide a configuration
/// the offer could not look into.
fn has_importable_file(src: &Path) -> bool {
    if never_copied(src) {
        return false;
    }
    if src.is_dir() {
        match std::fs::read_dir(src) {
            Ok(entries) => entries.into_iter().any(|entry| match entry {
                Ok(entry) => has_importable_file(&entry.path()),
                Err(_) => true,
            }),
            Err(_) => true,
        }
    } else {
        src.is_file()
    }
}

/// A copy by [`copy_missing_tree`] that stopped on an error, with the number
/// of files it had copied before it: a report that dropped them left the user
/// with no idea that part of the configuration was already in place.
#[derive(Debug)]
struct CopyInterrupted {
    copied: usize,
    error: std::io::Error,
}

impl std::fmt::Display for CopyInterrupted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} ({} {} copied before the error)",
            self.error,
            self.copied,
            if self.copied == 1 { "file" } else { "files" }
        )
    }
}

impl From<CopyInterrupted> for std::io::Error {
    fn from(interrupted: CopyInterrupted) -> Self {
        std::io::Error::new(interrupted.error.kind(), interrupted.to_string())
    }
}

/// Copy into `dst` every file of `src` that `dst` does not have yet, never
/// replacing one, and return how many files were copied: zero when `dst`
/// already had all of them, so a caller can report only what happened.
///
/// The vault files at the top of the tree ([`VAULT_FILES`]) are one unit: they
/// come in only when `dst` has none of them, and a failure part way through
/// them removes the ones this call copied. The partition database is created
/// lazily, so an install can hold its own key without it, and a copy file by
/// file then put another install's partition database (or key) next to this
/// one's vault, where nothing can open it.
fn copy_missing_tree(src: &Path, dst: &Path) -> Result<usize, CopyInterrupted> {
    let mut copied = 0;
    copy_missing_root(src, dst, &mut copied)
        .map(|()| copied)
        .map_err(|error| CopyInterrupted { copied, error })
}

/// The top of [`copy_missing_tree`]: the vault unit first, then the rest.
fn copy_missing_root(src: &Path, dst: &Path, copied: &mut usize) -> std::io::Result<()> {
    if never_copied(src) || !src.is_dir() {
        return copy_missing_node(src, dst, copied);
    }
    prepare_copy_dir(dst)?;
    copy_vault_unit(src, dst, copied)?;
    for entry in entries_by_name(src)? {
        let name = entry.file_name();
        if VAULT_FILES.iter().any(|vault_file| name == *vault_file) {
            continue;
        }
        copy_missing_node(&entry.path(), &dst.join(name), copied)?;
    }
    Ok(())
}

/// Copy the vault files of `src` into `dst` together, or none of them.
fn copy_vault_unit(src: &Path, dst: &Path, copied: &mut usize) -> std::io::Result<()> {
    // A dangling link counts as present, as it does for the no-clobber rename.
    if VAULT_FILES
        .iter()
        .any(|name| dst.join(name).symlink_metadata().is_ok())
    {
        return Ok(());
    }
    let mut created = Vec::new();
    for name in VAULT_FILES {
        let before = *copied;
        let outcome = copy_missing_node(&src.join(name), &dst.join(name), copied);
        if *copied > before {
            created.push(dst.join(name));
        }
        if let Err(e) = outcome {
            for path in &created {
                let _ = std::fs::remove_file(path);
            }
            *copied -= created.len();
            return Err(e);
        }
    }
    Ok(())
}

/// Create `dst` as a private folder for the copy.
fn prepare_copy_dir(dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dst, std::fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

/// The entries of `dir` in name order, so a copy that fails stops at the same
/// place on every run and says the same count.
fn entries_by_name(dir: &Path) -> std::io::Result<Vec<std::fs::DirEntry>> {
    let mut entries = std::fs::read_dir(dir)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    Ok(entries)
}

/// True for a file to copy through a SQLite snapshot: one that starts with the
/// SQLite header, or an empty one (an empty database whose data may still sit
/// in its WAL). `vault.db` is JSON under a `.db` name, and the snapshot failed
/// on it ("file is not a database"), so a host vault could never come in.
fn is_sqlite_database(path: &Path) -> std::io::Result<bool> {
    use std::io::Read;
    const HEADER: &[u8; 16] = b"SQLite format 3\0";
    let mut start = Vec::with_capacity(HEADER.len());
    std::fs::File::open(path)?
        .take(HEADER.len() as u64)
        .read_to_end(&mut start)?;
    Ok(start.is_empty() || start == HEADER)
}

/// One file or folder of [`copy_missing_tree`], counting into `copied`.
fn copy_missing_node(src: &Path, dst: &Path, copied: &mut usize) -> std::io::Result<()> {
    if never_copied(src) {
        return Ok(());
    }
    if src.is_dir() {
        prepare_copy_dir(dst)?;
        for entry in entries_by_name(src)? {
            copy_missing_node(&entry.path(), &dst.join(entry.file_name()), copied)?;
        }
        Ok(())
    } else if src.is_file() && !dst.exists() {
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if src.extension().and_then(|s| s.to_str()) == Some("db") && is_sqlite_database(src)? {
            let snapshot = tempfile::Builder::new()
                .prefix(".aeroftp-migration-")
                .tempfile_in(dst.parent().unwrap_or_else(|| Path::new(".")))?;
            let conn = rusqlite::Connection::open_with_flags(
                src,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .map_err(|e| std::io::Error::other(format!("Open legacy SQLite snapshot: {e}")))?;
            conn.busy_timeout(std::time::Duration::from_secs(5))
                .map_err(|e| std::io::Error::other(e.to_string()))?;
            conn.execute(
                "VACUUM main INTO ?1",
                rusqlite::params![snapshot.path().to_string_lossy().to_string()],
            )
            .map_err(|e| std::io::Error::other(format!("Snapshot legacy SQLite database: {e}")))?;
            // Do not overwrite a destination another startup created meanwhile.
            snapshot.as_file().sync_all()?;
            match snapshot.persist_noclobber(dst) {
                Ok(_) => {}
                Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(()),
                Err(e) => return Err(e.error),
            }
        } else if !copy_file_noclobber(src, dst)? {
            return Ok(());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(dst, std::fs::Permissions::from_mode(0o600));
        }
        *copied += 1;
        Ok(())
    } else {
        Ok(())
    }
}

/// Copy `src` to `dst` unless `dst` exists, without ever replacing it. The bytes
/// go to a temporary sibling that is linked into place with `persist_noclobber`,
/// so two first starts racing each other (both run before the single-instance
/// plugin exists) cannot overwrite what the other has just written, and an
/// interrupted copy leaves no half-written destination behind. Returns
/// `Ok(false)` when the destination was already there. The SQLite branch above
/// reaches the same guarantee through its own snapshot file.
fn copy_file_noclobber(src: &Path, dst: &Path) -> std::io::Result<bool> {
    let parent = dst.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let mut staged = tempfile::Builder::new()
        .prefix(".aeroftp-copy-")
        .tempfile_in(parent)?;
    std::io::copy(&mut std::fs::File::open(src)?, staged.as_file_mut())?;
    staged.as_file().sync_all()?;
    match staged.persist_noclobber(dst) {
        Ok(_) => Ok(true),
        Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(e.error),
    }
}

const LEGACY_CONFIG_MERGED_MARKER: &str = ".legacy-config-merged";

/// What one call of [`merge_legacy_config_once`] did.
#[derive(Debug, PartialEq, Eq)]
enum LegacyMerge {
    /// The marker was there: an earlier start merged, nothing was looked at.
    AlreadyMerged,
    /// The merge ran now and copied this many files, zero when the data root
    /// already had all of them. The marker is written either way.
    Merged { copied: usize },
}

/// Merge the legacy tree into `new_dir` once per data root. Without a durable
/// record every start copied again whatever was missing, so a file the user
/// deleted from the data root (a database, a plugin) came back from the legacy
/// tree on the next start.
fn merge_legacy_config_once(legacy_dir: &Path, new_dir: &Path) -> std::io::Result<LegacyMerge> {
    let marker = new_dir.join(LEGACY_CONFIG_MERGED_MARKER);
    if marker.exists() {
        return Ok(LegacyMerge::AlreadyMerged);
    }
    let copied = copy_missing_tree(legacy_dir, new_dir)?;
    std::fs::write(&marker, b"merged\n")?;
    Ok(LegacyMerge::Merged { copied })
}

fn migrate_legacy_app_config_dir(legacy_dir: Option<PathBuf>, new_dir: &Path) {
    if cfg!(debug_assertions) || is_portable() {
        return;
    }
    if LEGACY_APP_CONFIG_MIGRATED.get().is_some() {
        return;
    }
    let Some(legacy_dir) = legacy_dir else {
        return;
    };
    if !legacy_dir.is_dir() || legacy_dir == new_dir {
        return;
    }
    merge_legacy_config_and_log(&legacy_dir, new_dir);
    let _ = LEGACY_APP_CONFIG_MIGRATED.set(());
}

/// Run the one-time merge and report what it did. Every start after the first
/// finds the marker, and a first start can find every legacy file already in
/// the data root: in both cases nothing is copied, so only a merge that copied
/// files may say "Migrated". A support log that reports a migration that did
/// not happen misleads whoever reads it.
fn merge_legacy_config_and_log(legacy_dir: &Path, new_dir: &Path) {
    match merge_legacy_config_once(legacy_dir, new_dir) {
        Ok(LegacyMerge::Merged { copied }) if copied > 0 => tracing::info!(
            "Migrated legacy AeroFTP app config from {} to {} ({} {} copied)",
            legacy_dir.display(),
            new_dir.display(),
            copied,
            if copied == 1 { "file" } else { "files" }
        ),
        Ok(LegacyMerge::Merged { .. }) => tracing::debug!(
            "Legacy AeroFTP app config at {} had nothing missing from {}, nothing copied ({} written)",
            legacy_dir.display(),
            new_dir.display(),
            LEGACY_CONFIG_MERGED_MARKER
        ),
        Ok(LegacyMerge::AlreadyMerged) => tracing::debug!(
            "Legacy AeroFTP app config already merged into {} ({} present), nothing copied from {}",
            new_dir.display(),
            LEGACY_CONFIG_MERGED_MARKER,
            legacy_dir.display()
        ),
        Err(e) => tracing::warn!(
            "Failed to migrate legacy AeroFTP app config from {} to {}: {}",
            legacy_dir.display(),
            new_dir.display(),
            e
        ),
    }
}

/// Resolve the per-app config directory. In portable mode this is
/// `<exe-dir>/data/aeroftp` (or `aeroftp-dev` in debug); otherwise it is the
/// canonical AeroFTP data root.
///
/// This is the wrapper to use everywhere instead of calling
/// `app.path().app_config_dir()` directly. It keeps portable installs
/// self-contained and keeps debug builds isolated from release data.
///
/// The handle is not needed for the resolution any more; the parameter keeps
/// GUI code on the GUI entry point while [`cli_app_config_dir`] serves the
/// binaries that have no handle.
pub fn app_config_dir(_app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let dir = aeroftp_data_root().ok_or_else(|| "Cannot resolve AeroFTP data root".to_string())?;
    migrate_legacy_app_config_dir(legacy_app_config_dir(), &dir);
    Ok(dir)
}

/// Resolve the per-app data directory. In portable mode this is
/// `<exe-dir>/data`; otherwise `<data dir>/com.aeroftp.AeroFTP`, the location
/// Tauri's `app_data_dir()` gave every release before #814. It is pinned to
/// [`LEGACY_APP_IDENTIFIER`] for the reason given there: the speech model
/// downloaded into it is hundreds of megabytes that an update must not orphan.
pub fn app_data_dir() -> Result<PathBuf, String> {
    if let Some(data_root) = portable_data_root() {
        ensure_dir(&data_root)?;
        return Ok(data_root);
    }
    dirs::data_dir()
        .map(|base| base.join(LEGACY_APP_IDENTIFIER))
        .ok_or_else(|| "Cannot resolve app data dir".to_string())
}

/// Resolve the credential-store directory. Kept as a compatibility wrapper
/// because diagnostics and credential-store code already call this name.
pub fn credential_store_dir() -> Option<PathBuf> {
    aeroftp_data_root()
}

/// CLI-friendly resolution of the per-app config directory, mirroring
/// [`app_config_dir`] but without an `AppHandle`. Standalone binaries
/// (`aeroftp-cli`, `aerorsync_serve`) use this so a full keystore
/// export/import sees the same SQLite databases + plugin trees that
/// the GUI runtime would.
///
/// Returns `None` when no plausible config root exists (no `$HOME`,
/// no `%APPDATA%`). Callers should fall back to "vault only" mode in
/// that case rather than silently writing into the working directory.
pub fn cli_app_config_dir() -> Option<PathBuf> {
    let dir = aeroftp_data_root()?;
    migrate_legacy_app_config_dir(legacy_app_config_dir(), &dir);
    Some(dir)
}

// ===========================================================================
// Flatpak host-config import (B3)
// ===========================================================================
//
// A Flatpak install redirects XDG_CONFIG_HOME into ~/.var/app/<id>/config, so a
// user moving from the .deb gets a brand-new, empty data root: their saved
// servers and encrypted vault become invisible (not lost, not corrupted). For an
// encrypted-credential app a fresh data root reads as "I lost my passwords",
// which is the trap we must not spring.
//
// With --filesystem=home (granted by the Flatpak manifest for the local pane
// anyway) the sandbox can still read the real ~/.config/aeroftp. We offer a
// one-time, consent-gated import that copies only what is missing and never
// overwrites, rather than a silent copy: relocating an encrypted vault should be
// the user's explicit choice, and a user who deliberately wanted a clean
// sandboxed install should not be surprised. A marker records the decision so
// the offer is made exactly once.

const FLATPAK_IMPORT_DECIDED_MARKER: &str = ".flatpak-host-import-decided";

/// True when running inside a Flatpak sandbox. Mirrors the `FLATPAK_ID` signal
/// used by `detect_install_format()` in `lib.rs`; do not invent a second one.
pub fn is_flatpak() -> bool {
    std::env::var_os("FLATPAK_ID").is_some()
}

/// Testable core of [`host_config_dir_under_flatpak`]: no env, the home and the
/// data root come in resolved, so the branch logic is unit-tested on a temporary
/// home without a real sandbox.
fn host_config_dir_impl(
    is_flatpak: bool,
    home: Option<PathBuf>,
    leaf: &str,
    current: Option<PathBuf>,
) -> Option<PathBuf> {
    if !is_flatpak {
        return None;
    }
    let candidate = home?.join(".config").join(leaf);
    // A dotfiles manager (GNU Stow and the like) links the whole folder into
    // place. That link is the user's own config, so the root is resolved once
    // here, and the never-follow rule of [`never_copied`] applies to what is
    // inside it.
    let root_is_link = candidate
        .symlink_metadata()
        .is_ok_and(|meta| meta.file_type().is_symlink());
    let candidate = if root_is_link {
        std::fs::canonicalize(&candidate).unwrap_or(candidate)
    } else {
        candidate
    };
    // A no-op (candidate == data root, compared resolved so that a link to the
    // data root counts too) or a missing host config is nothing to import; bail
    // so the caller never runs a self-referential migration. Whether the offer
    // is shown also depends on what the folder holds, which [`import_offer`]
    // decides.
    let is_data_root = current.as_deref().is_some_and(|current| {
        current == candidate
            || matches!(
                (std::fs::canonicalize(current), std::fs::canonicalize(&candidate)),
                (Ok(current), Ok(candidate)) if current == candidate
            )
    });
    if is_data_root || !candidate.is_dir() {
        return None;
    }
    Some(candidate)
}

/// The real host `~/.config/<leaf>` as seen from inside a Flatpak sandbox
/// (visible thanks to `--filesystem=home`). `None` when not under Flatpak, when
/// that directory does not exist, or when it resolves to the current data root.
/// A directory with nothing the import would copy is returned: an explicit
/// import of it reports that nothing was copied, and only the offer skips it.
///
/// `$HOME` inside the sandbox is the real host home, while `dirs::config_dir()`
/// is redirected into the sandbox, so the host path is built from `$HOME`
/// directly rather than from the redirected XDG base.
pub fn host_config_dir_under_flatpak() -> Option<PathBuf> {
    host_config_dir_impl(
        is_flatpak(),
        dirs::home_dir(),
        aeroftp_data_leaf(),
        aeroftp_data_root(),
    )
}

/// Whether a first-run host-config import should be offered, and the paths.
#[derive(Debug, Clone)]
pub struct FlatpakImportStatus {
    /// True when the offer should be shown: under Flatpak, host config present,
    /// and the user has not already accepted or declined.
    pub available: bool,
    pub source: Option<PathBuf>,
    pub target: Option<PathBuf>,
}

/// The files that hold the vault and the saved servers encrypted under it: the
/// server list lives in `user_partitions.db`, and the key of each account there
/// is wrapped by the vault. The import never overwrites, so when this install
/// already has one of them the host's stay behind, and the report says so.
const VAULT_FILES: [&str; 3] = [
    crate::credential_store::VAULTKEY_FILENAME,
    crate::credential_store::VAULT_FILENAME,
    crate::user_partitions::DB_FILENAME,
];

/// What an accepted import did with the host vault and saved servers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostVault {
    /// Nothing to report: the host config holds no vault, or the import was
    /// declined.
    Absent,
    /// Copied: this install had no vault of its own.
    Imported,
    /// Left on the host: this install already has its own vault (the GUI creates
    /// one at the first start, before the offer), and the import never replaces
    /// a file.
    Skipped,
}

/// Whether the host vault and saved servers will come in with the copy. The
/// copy takes the vault files as one unit and never replaces a file, so any
/// vault file this install already has (the GUI creates them at the first
/// start, before the offer) keeps all of the host's out.
/// A name counts as present even when it is a dangling link, as it does for the
/// copy's no-clobber rename.
fn host_vault_outcome(src: &Path, dst: &Path) -> HostVault {
    let on_host: Vec<&str> = VAULT_FILES
        .into_iter()
        .filter(|name| {
            let file = src.join(name);
            !never_copied(&file) && file.is_file()
        })
        .collect();
    if on_host.is_empty() {
        HostVault::Absent
    } else if on_host
        .iter()
        .any(|name| dst.join(name).symlink_metadata().is_ok())
    {
        HostVault::Skipped
    } else {
        HostVault::Imported
    }
}

/// A host-config import that stopped on an error, with what it had done
/// before it. The files already copied stay in the sandbox and load at the
/// next start; a report that dropped their count left the user with no idea
/// that part of the configuration was already in place (4.2.1 review, L12).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlatpakImportError {
    /// Files copied into the sandbox before the error. Zero when the copy
    /// never started (no host configuration to import).
    pub copied: usize,
    /// The error, which names the count too, for the callers that show text.
    pub message: String,
}

impl std::fmt::Display for FlatpakImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for FlatpakImportError {}

impl FlatpakImportError {
    /// The failure as `aeroftp-cli flatpak-import --json` prints it: the CLI
    /// error object plus `copied`, so a script can tell a copy that stopped
    /// half way from one that never started.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "status": "error",
            "error": self.message,
            "code": 1,
            "copied": self.copied,
        })
    }
}

/// Outcome of an import decision.
#[derive(Debug, Clone)]
pub struct FlatpakImportReport {
    /// Files copied into the sandbox: 0 on a decline, on an accept whose sandbox
    /// already had a file with the same name for each file the import would copy,
    /// and when the host config holds none (`nothing_importable`). Folders are
    /// recreated in the sandbox but not counted.
    pub copied: usize,
    pub vault: HostVault,
    /// The host config holds no file the import copies (only empty folders,
    /// SQLite sidecars or symbolic links), so an accept that copied nothing says
    /// "nothing to import" rather than "every file already has one here". The
    /// offer is never shown for such a config; the explicit CLI import is.
    pub nothing_importable: bool,
    pub source: Option<PathBuf>,
    pub target: Option<PathBuf>,
}

impl FlatpakImportReport {
    /// True only when the import brought files in, the one case in which a
    /// restart has something new to load.
    pub fn imported(&self) -> bool {
        self.copied > 0
    }

    /// The host vault and saved servers were copied into this install.
    pub fn vault_imported(&self) -> bool {
        self.vault == HostVault::Imported
    }

    /// The host has a vault, and this install already had its own, so the host
    /// vault and saved servers were not imported.
    pub fn vault_skipped(&self) -> bool {
        self.vault == HostVault::Skipped
    }

    /// The report as the GUI command `flatpak_config_import_apply` and
    /// `aeroftp-cli flatpak-import --json` return it. One serializer for both,
    /// so every caller can tell "nothing to import" from "every file the import
    /// copies is already here", and a field added to the report reaches both.
    pub fn to_json(&self) -> serde_json::Value {
        let path = |p: Option<&PathBuf>| p.map(|p| p.to_string_lossy().into_owned());
        serde_json::json!({
            "imported": self.imported(),
            "copied": self.copied,
            "vault_imported": self.vault_imported(),
            "vault_skipped": self.vault_skipped(),
            "nothing_importable": self.nothing_importable,
            "source": path(self.source.as_ref()),
            "target": path(self.target.as_ref()),
        })
    }
}

fn flatpak_import_marker_path() -> Option<PathBuf> {
    aeroftp_data_root().map(|d| d.join(FLATPAK_IMPORT_DECIDED_MARKER))
}

fn flatpak_import_decided() -> bool {
    flatpak_import_marker_path()
        .map(|m| m.exists())
        .unwrap_or(false)
}

fn write_flatpak_import_marker(data_root: &Path) {
    let _ = std::fs::create_dir_all(data_root);
    let _ = std::fs::write(data_root.join(FLATPAK_IMPORT_DECIDED_MARKER), b"decided\n");
}

/// Should the first-run host-config import prompt be shown, and from/to where.
pub fn flatpak_host_import_status() -> FlatpakImportStatus {
    import_offer(
        host_config_dir_under_flatpak(),
        aeroftp_data_root(),
        flatpak_import_decided(),
    )
}

/// Testable core of [`flatpak_host_import_status`]. The offer says an existing
/// configuration was found, so it is shown only when the host config holds a
/// file the copy would carry: accepting one made only of empty folders, SQLite
/// sidecars and symbolic links would end in "nothing to import".
fn import_offer(
    source: Option<PathBuf>,
    target: Option<PathBuf>,
    decided: bool,
) -> FlatpakImportStatus {
    let offerable = source.as_deref().is_some_and(has_importable_file);
    FlatpakImportStatus {
        available: offerable && !decided,
        source,
        target,
    }
}

/// Apply (`accept = true`) or decline (`accept = false`) the host-config import.
///
/// On accept, copy the host config into the sandbox data root with
/// `copy_missing_tree`, which copies only absent files and never overwrites, so
/// re-running it is safe and a partially set-up sandbox is preserved. Either way
/// the decision is recorded so the prompt is not shown again. The host vault
/// comes in only when this install has none of its own; the report says which
/// ([`HostVault`]), because the GUI creates this install's vault at the first
/// start, before the offer, and a vault the copy left behind must not be
/// announced as imported.
pub fn flatpak_host_import_apply(accept: bool) -> Result<FlatpakImportReport, FlatpakImportError> {
    apply_flatpak_host_import(accept, host_config_dir_under_flatpak(), aeroftp_data_root())
}

/// Testable core of [`flatpak_host_import_apply`]: the paths come in resolved,
/// and the decision marker goes into `target`, the data root where
/// [`flatpak_import_decided`] looks for it.
fn apply_flatpak_host_import(
    accept: bool,
    source: Option<PathBuf>,
    target: Option<PathBuf>,
) -> Result<FlatpakImportReport, FlatpakImportError> {
    let mut report = FlatpakImportReport {
        copied: 0,
        vault: HostVault::Absent,
        nothing_importable: false,
        source: source.clone(),
        target: target.clone(),
    };
    if accept {
        match (source.as_ref(), target.as_ref()) {
            (Some(src), Some(dst)) => {
                // Looked at before the copy: what this install already has is
                // exactly what the copy leaves in place.
                report.vault = host_vault_outcome(src, dst);
                report.copied = copy_missing_tree(src, dst).map_err(|e| FlatpakImportError {
                    copied: e.copied,
                    message: format!(
                        "Import host config from {} to {}: {e}",
                        src.display(),
                        dst.display()
                    ),
                })?;
                report.nothing_importable = report.copied == 0 && !has_importable_file(src);
            }
            _ => {
                return Err(FlatpakImportError {
                    copied: 0,
                    message: "No host configuration available to import".to_string(),
                })
            }
        }
    }
    if let Some(dst) = target.as_deref() {
        write_flatpak_import_marker(dst);
    }
    Ok(report)
}

/// The application identifier every release before #814 shipped with, and the
/// one the identifier-scoped state on users' machines is filed under.
///
/// #814 renamed the identifier in `tauri.conf.json` to [`APP_IDENTIFIER`]. Tauri
/// derives several directories from that value, and a rename moves each of
/// them to an empty sibling: the WebView data (all `localStorage`: language,
/// theme, AeroFile tabs, custom icons, AI, OAuth and terminal settings),
/// `app_data_dir()` (the downloaded speech model), the legacy config tree the
/// data-root migration reads, the window-state file and the log folder. None of
/// it is the vault, which lives under the name-scoped `aeroftp` root, but an
/// update that silently resets every preference and re-downloads a model is a
/// regression. So the locations below stay on this identifier, the same choice
/// #814 made for the keyring service name, and what cannot be pinned is carried
/// over once by [`carry_identifier_scoped_state`].
pub const LEGACY_APP_IDENTIFIER: &str = "com.aeroftp.AeroFTP";

/// The identifier in `tauri.conf.json`. Pinned against the file by a test, so a
/// future rename cannot leave the carry-over below pointing at the wrong place.
pub const APP_IDENTIFIER: &str = "app.aeroftp.AeroFTP";

/// Resolve the WebView2 / WebKitGTK data directory every window uses.
///
/// In portable mode this is `<exe-dir>/data/webview`. Two portable
/// installations of AeroFTP in different folders MUST NOT share WebView
/// state (localStorage, IndexedDB, cookies, cache) otherwise deleting a
/// saved server in one folder propagates to the other through the
/// identifier-scoped default folder Windows picks for WebView2.
///
/// Installed builds on Linux and Windows get `<local data dir>/com.aeroftp.AeroFTP`,
/// which is exactly where Tauri put the WebView data before #814 (it defaults
/// to `app_local_data_dir()`, which follows the configured identifier). Leaving
/// the default in place after the rename would have started every upgraded
/// installation on an empty `localStorage`. On macOS WebKit files the store by
/// bundle identifier and a directory cannot be chosen here, so this returns
/// `None` and [`carry_identifier_scoped_state`] copies the store instead.
pub fn webview_data_dir() -> Option<PathBuf> {
    if let Some(data_root) = portable_data_root() {
        let dir = data_root.join("webview");
        ensure_dir(&dir).ok()?;
        return Some(dir);
    }
    installed_webview_data_dir()
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
fn installed_webview_data_dir() -> Option<PathBuf> {
    dirs::data_local_dir().map(|base| base.join(LEGACY_APP_IDENTIFIER))
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
fn installed_webview_data_dir() -> Option<PathBuf> {
    None
}

/// The folder the file log target writes to: Tauri's `app_log_dir()` for the
/// legacy identifier, so the log a user or a bug report points at keeps its
/// place across the rename. `None` only when the platform base is unknown.
pub fn log_dir() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        dirs::home_dir().map(|home| home.join("Library/Logs").join(LEGACY_APP_IDENTIFIER))
    }
    #[cfg(not(target_os = "macos"))]
    {
        dirs::data_local_dir().map(|base| base.join(LEGACY_APP_IDENTIFIER).join("logs"))
    }
}

/// Carry over, once, the identifier-scoped state that cannot be pinned to
/// [`LEGACY_APP_IDENTIFIER`]. Must run before the Tauri builder starts, because
/// the window-state plugin reads its file while plugins initialise and the
/// WebView store is opened by the first window.
///
/// - The window-state file, which the plugin always resolves under the CURRENT
///   identifier's config directory.
/// - On macOS, the WebKit website data store (`~/Library/WebKit/<bundle id>`),
///   where `localStorage` lives and which WebKit files by bundle identifier.
///
/// Each item is copied only when the destination does not exist yet, so a user
/// who has already run the renamed build keeps what that build wrote, and a
/// second start does nothing. Portable builds keep everything under their own
/// folder and are skipped.
pub fn carry_identifier_scoped_state() {
    if is_portable() {
        return;
    }
    if let Some(config) = dirs::config_dir() {
        carry_file_if_absent(
            &config.join(LEGACY_APP_IDENTIFIER).join(WINDOW_STATE_FILE),
            &config.join(APP_IDENTIFIER).join(WINDOW_STATE_FILE),
        );
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = dirs::home_dir() {
            let webkit = home.join("Library/WebKit");
            carry_tree_if_absent(
                &webkit.join(LEGACY_APP_IDENTIFIER),
                &webkit.join(APP_IDENTIFIER),
            );
        }
    }
}

/// The window-state plugin's own file name, taken from the plugin rather than
/// restated: a copy of the literal would keep matching nothing, in silence, the
/// day the plugin changed its default.
const WINDOW_STATE_FILE: &str = tauri_plugin_window_state::DEFAULT_FILENAME;

fn carry_file_if_absent(src: &Path, dst: &Path) {
    if !src.is_file() || dst.exists() {
        return;
    }
    match copy_file_noclobber(src, dst) {
        Ok(true) => tracing::info!("Carried {} over to {}", src.display(), dst.display()),
        Ok(false) => {}
        Err(e) => tracing::warn!(
            "Could not carry {} over to {}: {}",
            src.display(),
            dst.display(),
            e
        ),
    }
}

/// Copy a whole tree to a destination that does not exist, through a sibling
/// staging directory renamed into place at the end: an interrupted copy leaves
/// no half-filled destination behind, and the next start tries again.
#[cfg(any(target_os = "macos", test))]
fn carry_tree_if_absent(src: &Path, dst: &Path) {
    if !src.is_dir() || dst.exists() {
        return;
    }
    let Some(parent) = dst.parent() else {
        return;
    };
    let prefix = format!(
        ".{}.carry-",
        dst.file_name().and_then(|n| n.to_str()).unwrap_or("state")
    );
    reclaim_dead_staging(parent, &prefix);
    let staging = parent.join(format!("{prefix}{}", std::process::id()));
    let result = std::fs::create_dir_all(parent)
        .and_then(|()| copy_tree_plain(src, &staging))
        .and_then(|()| std::fs::rename(&staging, dst));
    match result {
        Ok(()) => tracing::info!("Carried {} over to {}", src.display(), dst.display()),
        Err(e) => {
            let _ = std::fs::remove_dir_all(&staging);
            tracing::warn!(
                "Could not carry {} over to {}: {}",
                src.display(),
                dst.display(),
                e
            );
        }
    }
}

/// Remove staging directories a killed start left behind. Each carries the pid
/// of the process that made it, and one whose process is gone can never be
/// renamed into place, so without this it would stay on disk for good (the next
/// start has another pid). A staging directory whose process is alive, another
/// start copying right now, is left alone. Every doubt resolves toward keeping:
/// a pid the system has since reused for an unrelated live process, or a
/// liveness answer the platform cannot give, leaves the directory on disk,
/// which costs space and never a copy in progress.
#[cfg(any(target_os = "macos", test))]
fn reclaim_dead_staging(parent: &Path, prefix: &str) {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name
            .to_str()
            .and_then(|n| n.strip_prefix(prefix))
            .and_then(|p| p.parse::<u32>().ok())
        else {
            continue;
        };
        // Our own pid cannot be a copy in progress: this process has not
        // started one yet, so a directory under our pid was left by a dead
        // process that had the same pid, and adopting it would carry its
        // stale files into place.
        if pid == std::process::id() || !crate::aerovault_v3::process_is_alive(pid) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Byte copy of a tree, every file including SQLite sidecars: the WebKit store
/// is copied while no WebView has it open, so a database and its WAL travel as
/// one consistent generation. Symlinks are skipped, never followed.
#[cfg(any(target_os = "macos", test))]
fn copy_tree_plain(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let target = dst.join(entry.file_name());
        if kind.is_dir() {
            copy_tree_plain(&entry.path(), &target)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// True when an `EBWebView` folder exists under the shared, identifier-scoped
/// `%LOCALAPPDATA%\com.aeroftp.AeroFTP` directory. Used by the portable
/// migration wizard to detect "there is legacy state from a previous
/// non-portable (or pre-isolation portable) install on this machine".
///
/// Only meaningful when running portable: an installed build expects that
/// directory to exist (it's its own state).
#[cfg(windows)]
pub fn shared_webview_data_present() -> bool {
    if !is_portable() {
        return false;
    }
    let Some(local_appdata) = dirs::data_local_dir() else {
        return false;
    };
    let candidate = local_appdata.join(LEGACY_APP_IDENTIFIER).join("EBWebView");
    candidate.is_dir()
}

#[cfg(not(windows))]
pub fn shared_webview_data_present() -> bool {
    false
}

// ===========================================================================
// Windows install-format detection
// ===========================================================================
//
// The auto-updater needs to know which artifact to download and which install
// path to follow. The three Windows formats — MSI, NSIS .exe, portable ZIP —
// require different update strategies:
//
//   - MSI: msiexec /i ... /qb /norestart (silent upgrade, in-place)
//   - NSIS: setup.exe /S (silent install, in-place)
//   - Portable: rename + swap of AeroFTP.exe (no installer)
//
// Misclassification is harmful: a portable user who gets pointed at the NSIS
// installer ends up with two copies on disk and a broken update story.
//
// Detection runs in three deterministic stages:
//
//   1. Portable marker (most reliable) — `portable.marker` next to the .exe.
//      Ships inside the ZIP and is the canonical signal.
//
//   2. Registry Uninstall scan (HKLM then HKCU) — walk
//      `Software\Microsoft\Windows\CurrentVersion\Uninstall\*` looking for
//      a sub-key whose `InstallLocation` matches the parent of the running
//      exe AND whose `DisplayName` contains "AeroFTP". The `WindowsInstaller`
//      DWORD distinguishes MSI (=1) from NSIS (=0 or absent).
//
//   3. Fallback path heuristic — if neither marker nor registry resolves,
//      classify by `%ProgramFiles%` containment. Logged as a warning so
//      the operator knows detection was inconclusive.

#[cfg(windows)]
const REGISTRY_UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall";

/// Windows-only install-format detection. Order: marker → registry → path.
#[cfg(windows)]
pub fn detect_windows_install_format() -> String {
    if is_portable() {
        return "portable".to_string();
    }

    if let Some(format) = detect_via_registry() {
        return format;
    }

    log::warn!(
        "Windows install-format detection: marker absent and registry scan inconclusive, \
         falling back to path heuristic"
    );
    detect_via_path_heuristic()
}

/// Cross-platform stub so the call site compiles everywhere. The non-Windows
/// path is never exercised in production (the `match` in `detect_install_format`
/// gates it on `os == "windows"`), but keeping the function signature uniform
/// avoids `#[cfg]` noise in the caller.
#[cfg(not(windows))]
pub fn detect_windows_install_format() -> String {
    "exe".to_string()
}

#[cfg(windows)]
fn detect_via_registry() -> Option<String> {
    use winreg::enums::*;
    use winreg::RegKey;

    let exe_parent = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let exe_parent_norm = normalize_windows_path(&exe_parent);

    // Try HKLM first (per-machine MSI installs land here), then HKCU
    // (Tauri NSIS per-user installs default to HKCU).
    for hive in [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER] {
        let root = RegKey::predef(hive);
        let uninstall = match root.open_subkey_with_flags(REGISTRY_UNINSTALL_KEY, KEY_READ) {
            Ok(k) => k,
            Err(_) => continue,
        };

        for sub_key_name in uninstall.enum_keys().flatten() {
            let sub = match uninstall.open_subkey_with_flags(&sub_key_name, KEY_READ) {
                Ok(s) => s,
                Err(_) => continue,
            };

            let display_name: String = sub.get_value("DisplayName").unwrap_or_default();
            if !display_name.contains("AeroFTP") {
                continue;
            }

            let install_location: String = sub.get_value("InstallLocation").unwrap_or_default();
            if install_location.is_empty() {
                continue;
            }

            let install_norm = normalize_windows_path(std::path::Path::new(&install_location));
            if install_norm != exe_parent_norm {
                continue;
            }

            // Match found. WindowsInstaller=1 ⇒ MSI; otherwise NSIS.
            let windows_installer: u32 = sub.get_value("WindowsInstaller").unwrap_or(0);
            let format = if windows_installer == 1 { "msi" } else { "exe" };
            log::info!(
                "Windows install-format detected via registry: {} (key: {}\\{}, DisplayName: {})",
                format,
                if hive == HKEY_LOCAL_MACHINE {
                    "HKLM"
                } else {
                    "HKCU"
                },
                sub_key_name,
                display_name
            );
            return Some(format.to_string());
        }
    }

    None
}

/// Last-resort heuristic: classify by Program Files containment. Used only
/// when both marker and registry fail (typically: corrupt registry, manual
/// install via xcopy, or a pre-marker portable that the user hasn't migrated).
#[cfg(windows)]
fn detect_via_path_heuristic() -> String {
    let exe_path = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return "exe".to_string(),
    };
    let path_str = exe_path.to_string_lossy().to_lowercase();
    let pf = std::env::var("ProgramFiles")
        .unwrap_or_default()
        .to_lowercase();
    let pf86 = std::env::var("ProgramFiles(x86)")
        .unwrap_or_default()
        .to_lowercase();

    if (!pf.is_empty() && path_str.starts_with(&pf))
        || (!pf86.is_empty() && path_str.starts_with(&pf86))
        || path_str.contains("program files")
    {
        "msi".to_string()
    } else {
        "exe".to_string()
    }
}

/// Lowercase + trailing-separator-strip normalization. Windows paths from
/// the registry can come in mixed case with or without a trailing backslash;
/// equality must be case-insensitive and separator-tolerant.
#[cfg(windows)]
fn normalize_windows_path(path: &std::path::Path) -> String {
    let s = path.to_string_lossy().to_lowercase();
    s.trim_end_matches(['\\', '/']).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_migration_never_replays_old_wal_over_restored_profiles() {
        let root = tempfile::tempdir().unwrap();
        let legacy = root.path().join("legacy");
        let current = root.path().join("current");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::create_dir_all(&current).unwrap();
        let source = rusqlite::Connection::open(legacy.join("user_partitions.db")).unwrap();
        source.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; CREATE TABLE profiles(name TEXT); INSERT INTO profiles VALUES('old profile');").unwrap();
        let dest = current.join("user_partitions.db");
        source
            .execute(
                "VACUUM main INTO ?1",
                rusqlite::params![dest.to_string_lossy().to_string()],
            )
            .unwrap();
        {
            let restored = rusqlite::Connection::open(&dest).unwrap();
            restored.execute_batch("UPDATE profiles SET name='renamed current profile'; PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
        }
        assert!(legacy.join("user_partitions.db-wal").exists());
        copy_missing_tree(&legacy, &current).unwrap();
        let restored = rusqlite::Connection::open(&dest).unwrap();
        let name: String = restored
            .query_row("SELECT name FROM profiles", [], |r| r.get(0))
            .unwrap();
        assert_eq!(name, "renamed current profile");
    }

    #[test]
    fn legacy_migration_includes_committed_wal_in_a_new_database() {
        let root = tempfile::tempdir().unwrap();
        let legacy = root.path().join("legacy");
        let current = root.path().join("current");
        std::fs::create_dir_all(&legacy).unwrap();
        let source = rusqlite::Connection::open(legacy.join("user_partitions.db")).unwrap();
        source.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; CREATE TABLE profiles(name TEXT); INSERT INTO profiles VALUES('latest committed profile');").unwrap();
        copy_missing_tree(&legacy, &current).unwrap();
        assert!(!current.join("user_partitions.db-wal").exists());
        let copied = rusqlite::Connection::open(current.join("user_partitions.db")).unwrap();
        let name: String = copied
            .query_row("SELECT name FROM profiles", [], |r| r.get(0))
            .unwrap();
        assert_eq!(name, "latest committed profile");
    }

    struct CaptureWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for CaptureWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Run one merge the way a release start does and return everything it
    /// wrote to the log, at every level.
    fn merge_log_of_one_start(legacy: &Path, current: &Path) -> String {
        let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = buf.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(move || CaptureWriter(sink.clone()))
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            merge_legacy_config_and_log(legacy, current)
        });
        let bytes = buf.lock().unwrap().clone();
        String::from_utf8(bytes).unwrap()
    }

    #[test]
    fn legacy_merge_reports_a_migration_only_when_it_copied() {
        let root = tempfile::tempdir().unwrap();
        let legacy = root.path().join("legacy");
        let current = root.path().join("current");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("settings.json"), b"{}").unwrap();

        // First start after the upgrade: the file is copied and the log says so.
        // This also proves the capture sees the line the next start must not print.
        let first = merge_log_of_one_start(&legacy, &current);
        assert!(current.join("settings.json").is_file());
        assert!(
            first.contains("Migrated legacy AeroFTP app config"),
            "the start that copied did not report it: {first:?}"
        );

        // Every later start finds the marker and copies nothing: the file removed
        // from the data root stays removed, and the log must not claim otherwise.
        std::fs::remove_file(current.join("settings.json")).unwrap();
        let later = merge_log_of_one_start(&legacy, &current);
        assert!(!current.join("settings.json").exists());
        assert!(
            !later.contains("Migrated"),
            "a start that copied nothing reported a migration: {later:?}"
        );
    }

    #[test]
    fn legacy_merge_with_nothing_missing_reports_no_migration() {
        let root = tempfile::tempdir().unwrap();
        let legacy = root.path().join("legacy");
        let current = root.path().join("current");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::create_dir_all(&current).unwrap();
        std::fs::write(legacy.join("settings.json"), b"{\"old\":true}").unwrap();
        std::fs::write(current.join("settings.json"), b"{\"new\":true}").unwrap();

        // No marker yet, so the merge runs, but the data root already has every
        // legacy file: nothing is copied and the log must not say otherwise.
        let log = merge_log_of_one_start(&legacy, &current);
        assert_eq!(
            std::fs::read(current.join("settings.json")).unwrap(),
            b"{\"new\":true}"
        );
        assert!(
            !log.contains("Migrated"),
            "a merge that copied nothing reported a migration: {log:?}"
        );
        // The merge still counts as done: the next start must skip it.
        assert!(current.join(LEGACY_CONFIG_MERGED_MARKER).is_file());
    }

    #[test]
    fn copy_missing_tree_counts_only_the_files_it_copied() {
        let root = tempfile::tempdir().unwrap();
        let src = root.path().join("legacy");
        let dst = root.path().join("current");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        std::fs::write(src.join("a.json"), b"a").unwrap();
        std::fs::write(src.join("b.json"), b"legacy b").unwrap();
        std::fs::write(src.join("sub").join("c.json"), b"c").unwrap();
        {
            let c = rusqlite::Connection::open(src.join("x.db")).unwrap();
            c.execute_batch("CREATE TABLE t(v TEXT); INSERT INTO t VALUES('x');")
                .unwrap();
        }
        // A sidecar never travels on its own, so it is not a copied file.
        std::fs::write(src.join("orphan.db-wal"), b"stale").unwrap();
        // Already in the data root: kept as it is, and not counted.
        std::fs::write(dst.join("b.json"), b"current b").unwrap();

        // Copied and skipped siblings in whatever order read_dir yields them, plus
        // a subfolder: the result is the sum, not the outcome of the last child.
        assert_eq!(copy_missing_tree(&src, &dst).unwrap(), 3);
        assert_eq!(std::fs::read(dst.join("b.json")).unwrap(), b"current b");
        assert!(dst.join("a.json").is_file());
        assert!(dst.join("sub").join("c.json").is_file());
        assert!(dst.join("x.db").is_file());
        assert!(!dst.join("orphan.db-wal").exists());

        assert_eq!(copy_missing_tree(&src, &dst).unwrap(), 0);
    }

    /// Marker absent ⇒ not portable, all helpers fall through to Tauri/dirs.
    /// We can't easily run `app_config_dir` here without an AppHandle, but we
    /// can sanity-check the detection contract.
    #[test]
    fn detection_is_marker_driven() {
        // In a non-installed test binary, std::env::current_exe() points at the
        // test runner, which has no marker next to it. So portable_root() must
        // return None unless someone manually drops portable.marker into
        // target/debug — which would be a bug in the test environment.
        // We just assert the cached function doesn't panic and is deterministic.
        let first = portable_root().is_some();
        let second = portable_root().is_some();
        assert_eq!(first, second);
    }

    #[test]
    fn portable_data_root_aligns_with_root() {
        match (portable_root(), portable_data_root()) {
            (None, None) => {}
            (Some(root), Some(data)) => {
                assert_eq!(data, root.join(PORTABLE_DATA_DIRNAME));
            }
            other => panic!("portable_root and portable_data_root disagree: {other:?}"),
        }
    }

    #[test]
    fn data_root_leaf_is_sibling_safe() {
        assert_eq!(aeroftp_data_leaf_for_debug(false), "aeroftp");
        assert_eq!(aeroftp_data_leaf_for_debug(true), "aeroftp-dev");
    }

    #[test]
    fn current_profile_data_root_uses_expected_leaf() {
        let Some(root) = aeroftp_data_root() else {
            return;
        };
        let expected = aeroftp_data_leaf_for_debug(cfg!(debug_assertions));
        assert_eq!(root.file_name().and_then(|s| s.to_str()), Some(expected));
    }

    // ---- Flatpak host-config import (B3) ----

    /// Outside a Flatpak sandbox there is nothing to import, no matter what the
    /// host looks like. This is the guard that keeps native, portable, Snap and
    /// AppImage installs untouched.
    #[test]
    fn host_config_absent_when_not_flatpak() {
        let tmp = tempfile::tempdir().unwrap();
        let (home, config) = host_config_home(&tmp);
        std::fs::write(config.join("servers.json"), b"{}").unwrap();
        let got = host_config_dir_impl(
            false,
            Some(home),
            "aeroftp",
            Some(PathBuf::from("/whatever")),
        );
        assert!(got.is_none());
    }

    /// Under Flatpak with a real host config present, resolve `$HOME/.config/<leaf>`.
    #[test]
    fn host_config_resolved_under_flatpak() {
        let tmp = tempfile::tempdir().unwrap();
        let (home, config) = host_config_home(&tmp);
        std::fs::write(config.join("servers.json"), b"{}").unwrap();
        let got = host_config_dir_impl(
            true,
            Some(home),
            "aeroftp",
            Some(tmp.path().join("sandbox").join("aeroftp")),
        );
        assert_eq!(got, Some(config));
    }

    /// A host config that does not exist on disk is not offered.
    #[test]
    fn host_config_skipped_when_dir_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let got = host_config_dir_impl(
            true,
            Some(tmp.path().join("home")),
            "aeroftp",
            Some(tmp.path().join("sandbox").join("aeroftp")),
        );
        assert!(got.is_none());
    }

    /// If the resolved host path equals the current data root, importing would be
    /// a self-referential no-op, so it must be refused (guards a misconfigured or
    /// non-redirected environment from copying a tree onto itself).
    #[test]
    fn host_config_skipped_when_equal_to_data_root() {
        let tmp = tempfile::tempdir().unwrap();
        let (home, config) = host_config_home(&tmp);
        std::fs::write(config.join("servers.json"), b"{}").unwrap();
        let got = host_config_dir_impl(true, Some(home), "aeroftp", Some(config));
        assert!(got.is_none());
    }

    /// The home layout the offer looks at: `<home>/.config/aeroftp`.
    fn host_config_home(tmp: &tempfile::TempDir) -> (PathBuf, PathBuf) {
        let home = tmp.path().join("home");
        let config = home.join(".config").join("aeroftp");
        std::fs::create_dir_all(&config).unwrap();
        (home, config)
    }

    /// A host config that exists but holds nothing the import copies is still
    /// the source of an explicit import (the CLI `flatpak-import`): it ends in
    /// "nothing to import", not in "no host configuration available".
    #[test]
    fn an_explicit_import_of_a_host_config_with_nothing_to_copy_copies_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let (home, config) = host_config_home(&tmp);
        std::fs::write(config.join("history.db-wal"), b"stale").unwrap();
        let sandbox = tmp.path().join("sandbox").join("aeroftp");

        let source = host_config_dir_impl(true, Some(home), "aeroftp", Some(sandbox.clone()));
        let report = apply_flatpak_host_import(true, source, Some(sandbox))
            .expect("an explicit import of a host config with nothing to copy failed");

        assert_eq!(report.copied, 0);
        assert!(!report.imported());
        assert!(
            report.nothing_importable,
            "a host config with nothing to copy was reported as if its files were already here"
        );
    }

    /// The GUI reads the report as JSON: without `nothing_importable` it tells a
    /// user whose host config holds only links and sidecars that this install
    /// already has a file with the same name for each of them.
    #[test]
    fn the_import_report_json_says_when_there_was_nothing_to_import() {
        let tmp = tempfile::tempdir().unwrap();
        let (home, config) = host_config_home(&tmp);
        std::fs::write(config.join("history.db-wal"), b"stale").unwrap();
        let sandbox = tmp.path().join("sandbox").join("aeroftp");
        let source = host_config_dir_impl(true, Some(home), "aeroftp", Some(sandbox.clone()));

        let json = apply_flatpak_host_import(true, source, Some(sandbox))
            .unwrap()
            .to_json();

        assert_eq!(json["copied"], 0);
        assert_eq!(json["imported"], false);
        assert_eq!(
            json["nothing_importable"], true,
            "the report JSON does not say the host config held nothing to import: {json}"
        );

        let (_tmp, host, sandbox) = flatpak_import_fixture();
        std::fs::create_dir_all(&sandbox).unwrap();
        std::fs::write(sandbox.join("servers.json"), b"sandbox servers").unwrap();
        let json = apply_flatpak_host_import(true, Some(host), Some(sandbox))
            .unwrap()
            .to_json();
        assert_eq!(json["copied"], 0);
        assert_eq!(json["nothing_importable"], false, "{json}");
    }

    /// What the GUI offer sees for the host config under `home`, through the
    /// same two steps [`flatpak_host_import_status`] runs.
    fn offer_for(home: PathBuf, tmp: &tempfile::TempDir) -> FlatpakImportStatus {
        let sandbox = tmp.path().join("sandbox").join("aeroftp");
        import_offer(
            host_config_dir_impl(true, Some(home), "aeroftp", Some(sandbox.clone())),
            Some(sandbox),
            false,
        )
    }

    #[test]
    fn host_config_with_nothing_to_copy_is_not_offered() {
        let tmp = tempfile::tempdir().unwrap();
        let (home, config) = host_config_home(&tmp);
        // Only what the import never copies: an empty folder, a SQLite sidecar
        // and a symbolic link.
        std::fs::create_dir_all(config.join("plugins")).unwrap();
        std::fs::write(config.join("history.db-wal"), b"stale").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(tmp.path(), config.join("elsewhere")).unwrap();

        let status = offer_for(home, &tmp);

        assert!(
            !status.available,
            "an import with nothing to copy was offered from {:?}",
            status.source
        );
    }

    #[test]
    fn host_config_with_a_file_to_copy_is_offered() {
        let tmp = tempfile::tempdir().unwrap();
        let (home, config) = host_config_home(&tmp);
        std::fs::create_dir_all(config.join("plugins").join("p")).unwrap();
        std::fs::write(config.join("plugins").join("p").join("plugin.json"), b"{}").unwrap();

        let status = offer_for(home, &tmp);

        assert!(status.available);
        assert_eq!(status.source, Some(config));
    }

    #[test]
    fn a_decided_import_is_not_offered_again() {
        let tmp = tempfile::tempdir().unwrap();
        let (_home, config) = host_config_home(&tmp);
        std::fs::write(config.join("servers.json"), b"{}").unwrap();

        let status = import_offer(Some(config), Some(tmp.path().join("sandbox")), true);

        assert!(!status.available);
    }

    /// A folder the offer cannot read is offered, so the copy reports the error
    /// instead of the offer silently hiding a configuration it could not look
    /// into.
    #[cfg(unix)]
    #[test]
    fn host_config_with_an_unreadable_folder_is_offered_and_the_copy_reports_it() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let (home, config) = host_config_home(&tmp);
        let locked = config.join("plugins");
        std::fs::create_dir_all(&locked).unwrap();
        std::fs::write(locked.join("plugin.json"), b"{}").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        // Root reads a 0o000 folder anyway, so the premise does not hold there.
        let premise_holds = std::fs::read_dir(&locked).is_err();

        let status = offer_for(home, &tmp);
        let result = apply_flatpak_host_import(true, status.source.clone(), status.target.clone());
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();

        if !premise_holds {
            eprintln!("skipped: a 0o000 folder is readable here (running as root)");
            return;
        }
        assert!(
            status.available,
            "a host config the offer could not read was not offered"
        );
        assert!(
            result.is_err(),
            "the copy did not report the unreadable folder: {:?}",
            result.map(|r| r.copied)
        );
    }

    /// A dotfiles manager (GNU Stow and the like) links the whole
    /// `~/.config/aeroftp` into place. That link is the user's own config, so the
    /// root is resolved once, and the never-follow rule applies to what is
    /// inside it.
    #[cfg(unix)]
    #[test]
    fn a_host_config_linked_in_by_a_dotfiles_manager_is_imported() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let dotfiles = home.join("dotfiles").join("aeroftp");
        std::fs::create_dir_all(&dotfiles).unwrap();
        std::fs::create_dir_all(home.join(".config")).unwrap();
        std::fs::write(dotfiles.join("servers.json"), b"host servers").unwrap();
        // A link inside the tree is still never followed.
        std::os::unix::fs::symlink(tmp.path(), dotfiles.join("elsewhere")).unwrap();
        std::os::unix::fs::symlink(&dotfiles, home.join(".config").join("aeroftp")).unwrap();
        let sandbox = tmp.path().join("sandbox").join("aeroftp");

        let status = offer_for(home, &tmp);
        assert!(status.available, "a linked host config was not offered");
        let report = apply_flatpak_host_import(true, status.source, Some(sandbox.clone())).unwrap();

        assert_eq!(report.copied, 1);
        assert_eq!(
            std::fs::read(sandbox.join("servers.json")).unwrap(),
            b"host servers"
        );
        assert!(sandbox.join("elsewhere").symlink_metadata().is_err());
    }

    /// Resolving a linked root must not defeat the self-reference guard: a host
    /// config that is a link to the data root is not an import source.
    #[cfg(unix)]
    #[test]
    fn a_host_config_linked_to_the_data_root_is_not_imported() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let sandbox = tmp.path().join("sandbox").join("aeroftp");
        std::fs::create_dir_all(&sandbox).unwrap();
        std::fs::write(sandbox.join("servers.json"), b"sandbox servers").unwrap();
        std::fs::create_dir_all(home.join(".config")).unwrap();
        std::os::unix::fs::symlink(&sandbox, home.join(".config").join("aeroftp")).unwrap();

        let got = host_config_dir_impl(true, Some(home), "aeroftp", Some(sandbox));

        assert!(
            got.is_none(),
            "the data root was offered as its own source: {got:?}"
        );
    }

    /// A host config and an empty sandbox data root, as the import finds them.
    fn flatpak_import_fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let host = tmp.path().join("host").join("aeroftp");
        let sandbox = tmp.path().join("sandbox").join("aeroftp");
        std::fs::create_dir_all(&host).unwrap();
        std::fs::write(host.join("servers.json"), b"host servers").unwrap();
        (tmp, host, sandbox)
    }

    #[test]
    fn flatpak_import_accepted_copies_what_is_missing_and_records_the_decision() {
        let (_tmp, host, sandbox) = flatpak_import_fixture();

        let report =
            apply_flatpak_host_import(true, Some(host.clone()), Some(sandbox.clone())).unwrap();

        assert!(report.imported());
        assert_eq!(report.copied, 1);
        assert_eq!(
            std::fs::read(sandbox.join("servers.json")).unwrap(),
            b"host servers"
        );
        assert!(sandbox.join(FLATPAK_IMPORT_DECIDED_MARKER).is_file());
    }

    #[test]
    fn flatpak_import_with_nothing_missing_is_not_reported_as_imported() {
        let (_tmp, host, sandbox) = flatpak_import_fixture();
        std::fs::create_dir_all(&sandbox).unwrap();
        std::fs::write(sandbox.join("servers.json"), b"sandbox servers").unwrap();

        let report =
            apply_flatpak_host_import(true, Some(host.clone()), Some(sandbox.clone())).unwrap();

        // Nothing was copied, so the GUI must not announce an import and ask for
        // a restart, and the CLI must not print "Imported".
        assert!(
            !report.imported(),
            "an import that copied nothing was reported as imported"
        );
        assert_eq!(report.copied, 0);
        assert!(!report.nothing_importable);
        assert_eq!(
            std::fs::read(sandbox.join("servers.json")).unwrap(),
            b"sandbox servers"
        );
        assert!(sandbox.join(FLATPAK_IMPORT_DECIDED_MARKER).is_file());
    }

    /// A host vault, marked by `vault.key` and a real SQLite
    /// `user_partitions.db`. The tests that need the JSON `vault.db` too add
    /// it themselves.
    fn write_host_vault(host: &Path) {
        std::fs::write(
            host.join(crate::credential_store::VAULTKEY_FILENAME),
            b"host key",
        )
        .unwrap();
        let db =
            rusqlite::Connection::open(host.join(crate::user_partitions::DB_FILENAME)).unwrap();
        db.execute_batch("CREATE TABLE users(name TEXT); INSERT INTO users VALUES('host');")
            .unwrap();
    }

    /// What the first start of a Flatpak install writes before the import is
    /// offered: `init_credential_store` creates `vault.key` and `vault.db`, and
    /// the account setup creates `user_partitions.db`.
    fn write_sandbox_vault(sandbox: &Path) {
        std::fs::create_dir_all(sandbox).unwrap();
        for name in VAULT_FILES {
            std::fs::write(sandbox.join(name), b"sandbox").unwrap();
        }
    }

    #[test]
    fn flatpak_import_says_the_host_vault_stayed_behind_when_this_install_has_one() {
        let (_tmp, host, sandbox) = flatpak_import_fixture();
        write_host_vault(&host);
        write_sandbox_vault(&sandbox);

        let report =
            apply_flatpak_host_import(true, Some(host.clone()), Some(sandbox.clone())).unwrap();

        // servers.json was copied, the vault was not: the report must not let
        // the GUI say "restart to load your servers and vault".
        assert_eq!(report.copied, 1);
        assert_eq!(
            report.vault,
            HostVault::Skipped,
            "a host vault this install already has was reported as {:?}",
            report.vault
        );
        assert!(report.vault_skipped() && !report.vault_imported());
        for name in VAULT_FILES {
            assert_eq!(std::fs::read(sandbox.join(name)).unwrap(), b"sandbox");
        }
    }

    #[test]
    fn flatpak_import_says_the_host_vault_stayed_behind_when_nothing_was_copied() {
        let (_tmp, host, sandbox) = flatpak_import_fixture();
        std::fs::remove_file(host.join("servers.json")).unwrap();
        write_host_vault(&host);
        write_sandbox_vault(&sandbox);

        let report =
            apply_flatpak_host_import(true, Some(host.clone()), Some(sandbox.clone())).unwrap();

        assert_eq!(report.copied, 0);
        assert_eq!(report.vault, HostVault::Skipped);
    }

    #[test]
    fn flatpak_import_reports_the_host_vault_it_copied() {
        let (_tmp, host, sandbox) = flatpak_import_fixture();
        write_host_vault(&host);

        let report =
            apply_flatpak_host_import(true, Some(host.clone()), Some(sandbox.clone())).unwrap();

        assert_eq!(report.copied, 3);
        assert_eq!(report.vault, HostVault::Imported);
        assert_eq!(
            std::fs::read(sandbox.join(crate::credential_store::VAULTKEY_FILENAME)).unwrap(),
            b"host key"
        );
    }

    #[test]
    fn flatpak_import_without_a_host_vault_reports_none() {
        let (_tmp, host, sandbox) = flatpak_import_fixture();
        write_sandbox_vault(&sandbox);

        let report =
            apply_flatpak_host_import(true, Some(host.clone()), Some(sandbox.clone())).unwrap();

        assert_eq!(report.copied, 1);
        assert_eq!(report.vault, HostVault::Absent);
    }

    /// Pre-release review of 4.2.1 (M5): the partition database is created
    /// lazily, so a sandbox can hold `vault.key` and `vault.db` without
    /// `user_partitions.db`. The copy then brought the host's partition
    /// database in next to this install's own vault, whose key cannot open it,
    /// while the report said the host vault stayed behind. The three files
    /// are one vault: with any of them here, none comes in.
    #[test]
    fn flatpak_import_never_mixes_a_host_vault_file_into_this_installs_vault() {
        let (_tmp, host, sandbox) = flatpak_import_fixture();
        write_host_vault(&host);
        std::fs::create_dir_all(&sandbox).unwrap();
        for name in [
            crate::credential_store::VAULTKEY_FILENAME,
            crate::credential_store::VAULT_FILENAME,
        ] {
            std::fs::write(sandbox.join(name), b"sandbox").unwrap();
        }

        let report =
            apply_flatpak_host_import(true, Some(host.clone()), Some(sandbox.clone())).unwrap();

        assert!(
            !sandbox.join(crate::user_partitions::DB_FILENAME).exists(),
            "the host partition database was copied next to this install's vault"
        );
        assert_eq!(report.copied, 1, "only servers.json comes in");
        assert_eq!(report.vault, HostVault::Skipped);
    }

    /// The real `vault.db` is JSON under a `.db` name. The SQLite snapshot
    /// taken for every `.db` failed on it, so a host vault could not come into
    /// an install without one: the import stopped half way through the vault.
    #[test]
    fn flatpak_import_copies_a_whole_host_vault_with_its_json_vault_db() {
        let (_tmp, host, sandbox) = flatpak_import_fixture();
        write_host_vault(&host);
        std::fs::write(
            host.join(crate::credential_store::VAULT_FILENAME),
            br#"{"version":2,"entries":{}}"#,
        )
        .unwrap();

        let report =
            apply_flatpak_host_import(true, Some(host.clone()), Some(sandbox.clone())).unwrap();

        assert_eq!(report.copied, 4);
        assert_eq!(report.vault, HostVault::Imported);
        assert_eq!(
            std::fs::read(sandbox.join(crate::credential_store::VAULT_FILENAME)).unwrap(),
            br#"{"version":2,"entries":{}}"#
        );
        let db =
            rusqlite::Connection::open(sandbox.join(crate::user_partitions::DB_FILENAME)).unwrap();
        let user: String = db
            .query_row("SELECT name FROM users", [], |r| r.get(0))
            .unwrap();
        assert_eq!(user, "host");
    }

    /// A vault file the copy cannot read leaves no part of the host vault
    /// behind: the ones already copied are removed again, so the next attempt
    /// does not find a partial vault and skip the rest.
    #[cfg(unix)]
    #[test]
    fn a_host_vault_that_cannot_be_copied_whole_leaves_none_of_it() {
        use std::os::unix::fs::PermissionsExt;
        // SAFETY: geteuid never fails and has no preconditions.
        if unsafe { libc::geteuid() } == 0 {
            eprintln!("skipped: root reads a file without read permission");
            return;
        }
        let (_tmp, host, sandbox) = flatpak_import_fixture();
        write_host_vault(&host);
        std::fs::write(host.join(crate::credential_store::VAULT_FILENAME), b"{}").unwrap();
        // The partition database goes last, after the key and vault.db.
        let partitions = host.join(crate::user_partitions::DB_FILENAME);
        std::fs::set_permissions(&partitions, std::fs::Permissions::from_mode(0o000)).unwrap();

        let result = apply_flatpak_host_import(true, Some(host.clone()), Some(sandbox.clone()));
        std::fs::set_permissions(&partitions, std::fs::Permissions::from_mode(0o600)).unwrap();

        assert!(result.is_err());
        for name in VAULT_FILES {
            assert!(
                !sandbox.join(name).exists(),
                "{name} of a partial host vault was left in the sandbox"
            );
        }
    }

    /// Pre-release review of 4.2.1 (L12): a copy that failed half way said
    /// nothing of the files it had already copied.
    #[test]
    fn a_failed_import_says_how_many_files_it_had_copied() {
        let (_tmp, host, sandbox) = flatpak_import_fixture();
        std::fs::create_dir_all(host.join("zz-plugins")).unwrap();
        std::fs::write(host.join("zz-plugins").join("p.json"), b"{}").unwrap();
        // A file where the copy needs a folder: creating it fails.
        std::fs::create_dir_all(&sandbox).unwrap();
        std::fs::write(sandbox.join("zz-plugins"), b"not a folder").unwrap();

        let error = apply_flatpak_host_import(true, Some(host.clone()), Some(sandbox.clone()))
            .expect_err("the copy fails on zz-plugins");

        assert!(sandbox.join("servers.json").is_file());
        assert!(
            error.message.contains("1 file copied before"),
            "the error lost the partial count: {error}"
        );
        // The count travels as a number too: the CLI's JSON error carries it,
        // so a script can tell a copy that stopped half way from one that
        // never started.
        assert_eq!(error.copied, 1);
        assert_eq!(error.to_json()["copied"], 1);
        assert_eq!(error.to_json()["code"], 1);
    }

    /// An import with nothing to copy from stopped before the copy: the
    /// error says it copied nothing, so a caller never reads a stale count.
    #[test]
    fn an_import_without_a_host_config_reports_zero_copied() {
        let (_tmp, _host, sandbox) = flatpak_import_fixture();
        let error = apply_flatpak_host_import(true, None, Some(sandbox))
            .expect_err("nothing to import from");
        assert_eq!(error.copied, 0);
        assert_eq!(error.to_json()["copied"], 0);
    }

    #[test]
    fn flatpak_import_declined_copies_nothing_and_records_the_decision() {
        let (_tmp, host, sandbox) = flatpak_import_fixture();

        let report =
            apply_flatpak_host_import(false, Some(host.clone()), Some(sandbox.clone())).unwrap();

        assert!(!report.imported());
        assert_eq!(report.copied, 0);
        assert!(!sandbox.join("servers.json").exists());
        assert!(sandbox.join(FLATPAK_IMPORT_DECIDED_MARKER).is_file());
    }

    #[test]
    fn flatpak_import_without_a_host_config_fails_and_keeps_the_offer_open() {
        let (_tmp, _host, sandbox) = flatpak_import_fixture();

        let result = apply_flatpak_host_import(true, None, Some(sandbox.clone()));

        assert!(result.is_err());
        // No marker: the next start offers the import again.
        assert!(!sandbox.join(FLATPAK_IMPORT_DECIDED_MARKER).exists());
    }

    /// The import copies only what is absent and never overwrites an existing
    /// file in the sandbox, so a partially set-up install is preserved.
    #[test]
    fn copy_missing_tree_never_overwrites() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        std::fs::write(src.join("vault.bin"), b"HOST-VAULT").unwrap();
        std::fs::write(src.join("sub/servers.json"), b"HOST-SERVERS").unwrap();
        // A file the sandbox already has must NOT be clobbered.
        std::fs::write(dst.join("vault.bin"), b"SANDBOX-VAULT").unwrap();

        copy_missing_tree(&src, &dst).unwrap();

        // Existing file preserved, missing file copied over.
        assert_eq!(
            std::fs::read(dst.join("vault.bin")).unwrap(),
            b"SANDBOX-VAULT"
        );
        assert_eq!(
            std::fs::read(dst.join("sub/servers.json")).unwrap(),
            b"HOST-SERVERS"
        );
    }

    /// Recursively check whether any regular file under `dir` contains `needle`.
    /// Uses `symlink_metadata` so the walk itself never chases a link into the
    /// tree it is verifying against.
    #[cfg(unix)]
    fn tree_contains_bytes(dir: &Path, needle: &[u8]) -> bool {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return false;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = path.symlink_metadata() else {
                continue;
            };
            if meta.file_type().is_symlink() {
                continue;
            }
            if meta.is_dir() {
                if tree_contains_bytes(&path, needle) {
                    return true;
                }
            } else if let Ok(bytes) = std::fs::read(&path) {
                if bytes.windows(needle.len()).any(|w| w == needle) {
                    return true;
                }
            }
        }
        false
    }

    /// A symlink inside the consented tree can point outside it (foreign secrets
    /// like ~/.ssh) or at an ancestor (a cycle). The import must never follow one:
    /// real files are still copied, but no symlink is materialised and no target
    /// content ever lands under the destination.
    #[cfg(unix)]
    #[test]
    fn copy_missing_tree_skips_symlinks() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let src = root.join("src");
        let dst = root.join("dst");

        // Foreign content that lives OUTSIDE the consented tree. A followed
        // symlink would drag it into the sandbox; a correct import must not.
        let secret = b"OUTSIDE-SECRET-MUST-NOT-BE-COPIED";
        let outside_dir = root.join("outside");
        std::fs::create_dir_all(&outside_dir).unwrap();
        std::fs::write(outside_dir.join("secret.txt"), secret).unwrap();
        let outside_file = root.join("outside-file.txt");
        std::fs::write(&outside_file, secret).unwrap();

        std::fs::create_dir_all(&src).unwrap();
        // (a) a real file we DO want copied.
        std::fs::write(src.join("real.txt"), b"REAL").unwrap();
        // (b) a symlink to a file outside the tree.
        symlink(&outside_file, src.join("link-to-file")).unwrap();
        // (c) a symlink to a directory outside the tree holding a secret.
        symlink(&outside_dir, src.join("link-to-dir")).unwrap();
        // (d) a self-referencing directory symlink: following it would recurse
        //     until path-length exhaustion.
        symlink(&src, src.join("loop")).unwrap();

        copy_missing_tree(&src, &dst).unwrap();

        // The real file is copied.
        assert_eq!(std::fs::read(dst.join("real.txt")).unwrap(), b"REAL");
        // The symlinks were skipped, not materialised in the destination.
        assert!(!dst.join("link-to-file").exists());
        assert!(!dst.join("link-to-dir").exists());
        assert!(!dst.join("loop").exists());
        // The foreign secret appears nowhere under the destination.
        assert!(!tree_contains_bytes(&dst, secret));
    }
}

#[cfg(test)]
mod identifier_scoped_state_tests {
    use super::*;

    #[test]
    fn app_identifier_matches_the_bundle_configuration() {
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).expect("tauri.conf.json");
        assert_eq!(conf["identifier"].as_str(), Some(APP_IDENTIFIER));
        assert_ne!(APP_IDENTIFIER, LEGACY_APP_IDENTIFIER);
    }

    #[test]
    fn the_legacy_identifier_is_the_one_every_earlier_release_used() {
        // Changing this string moves every pinned location to an empty folder,
        // which is the regression the pin exists to prevent.
        assert_eq!(LEGACY_APP_IDENTIFIER, "com.aeroftp.AeroFTP");
    }

    #[cfg(any(target_os = "linux", target_os = "windows"))]
    #[test]
    fn installed_webview_data_stays_on_the_legacy_identifier() {
        assert!(!is_portable(), "the test binary is not a portable install");
        let dir = webview_data_dir().expect("installed builds name a WebView folder");
        assert_eq!(
            dir,
            dirs::data_local_dir().unwrap().join(LEGACY_APP_IDENTIFIER)
        );
    }

    #[test]
    fn app_data_and_logs_stay_on_the_legacy_identifier() {
        assert!(!is_portable(), "the test binary is not a portable install");
        assert_eq!(
            app_data_dir().unwrap(),
            dirs::data_dir().unwrap().join(LEGACY_APP_IDENTIFIER)
        );
        let logs = log_dir().expect("log folder");
        assert!(
            logs.components()
                .any(|c| c.as_os_str() == LEGACY_APP_IDENTIFIER),
            "{}",
            logs.display()
        );
        assert_eq!(
            legacy_app_config_dir().unwrap(),
            dirs::config_dir().unwrap().join(LEGACY_APP_IDENTIFIER)
        );
    }

    #[test]
    fn a_tree_is_carried_whole_when_the_destination_is_absent() {
        let root = tempfile::tempdir().unwrap();
        let src = root.path().join(LEGACY_APP_IDENTIFIER);
        let dst = root.path().join(APP_IDENTIFIER);
        std::fs::create_dir_all(src.join("WebsiteData/LocalStorage")).unwrap();
        std::fs::write(
            src.join("WebsiteData/LocalStorage/localstorage.sqlite3"),
            b"db",
        )
        .unwrap();
        std::fs::write(
            src.join("WebsiteData/LocalStorage/localstorage.sqlite3-wal"),
            b"wal",
        )
        .unwrap();

        carry_tree_if_absent(&src, &dst);

        let ls = dst.join("WebsiteData/LocalStorage");
        assert_eq!(
            std::fs::read(ls.join("localstorage.sqlite3")).unwrap(),
            b"db"
        );
        // The WAL travels with its database: it may hold committed rows.
        assert_eq!(
            std::fs::read(ls.join("localstorage.sqlite3-wal")).unwrap(),
            b"wal"
        );
        assert!(src
            .join("WebsiteData/LocalStorage/localstorage.sqlite3")
            .is_file());
        let leftovers: Vec<_> = std::fs::read_dir(root.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .filter(|n| n.to_string_lossy().contains(".carry-"))
            .collect();
        assert!(leftovers.is_empty(), "staging left behind: {leftovers:?}");
    }

    #[test]
    fn staging_left_by_a_dead_start_is_reclaimed_and_a_live_one_is_not() {
        let root = tempfile::tempdir().unwrap();
        let src = root.path().join(LEGACY_APP_IDENTIFIER);
        let dst = root.path().join(APP_IDENTIFIER);
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("state"), b"old").unwrap();
        // A pid that is certainly gone: a child that has already been reaped.
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--list")
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let dead = child.id();
        child.wait().unwrap();
        let prefix = format!(".{APP_IDENTIFIER}.carry-");
        // A directory under our own pid, left by a dead process that had it.
        let reused = root.path().join(format!("{prefix}{}", std::process::id()));
        std::fs::create_dir_all(&reused).unwrap();
        std::fs::write(reused.join("stale"), b"from a dead process").unwrap();
        let stale = root.path().join(format!("{prefix}{dead}"));
        std::fs::create_dir_all(stale.join("half-copied")).unwrap();
        // On Unix pid 1 is always alive, so its staging must survive.
        #[cfg(unix)]
        let live = {
            let live = root.path().join(format!("{prefix}1"));
            std::fs::create_dir_all(&live).unwrap();
            live
        };

        carry_tree_if_absent(&src, &dst);

        assert!(!stale.exists(), "staging of a dead start left behind");
        assert!(
            !dst.join("stale").exists(),
            "a staging directory under our reused pid was adopted"
        );
        #[cfg(unix)]
        assert!(live.exists(), "staging of a live start removed");
        assert_eq!(std::fs::read(dst.join("state")).unwrap(), b"old");
    }

    #[test]
    fn nothing_is_carried_over_a_destination_that_exists() {
        let root = tempfile::tempdir().unwrap();
        let src = root.path().join(LEGACY_APP_IDENTIFIER);
        let dst = root.path().join(APP_IDENTIFIER);
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        std::fs::write(src.join("state"), b"old").unwrap();
        std::fs::write(dst.join("state"), b"new").unwrap();

        carry_tree_if_absent(&src, &dst);
        carry_file_if_absent(&src.join("state"), &dst.join("state"));

        assert_eq!(std::fs::read(dst.join("state")).unwrap(), b"new");
    }

    #[test]
    fn a_noclobber_copy_never_replaces_an_existing_destination() {
        let root = tempfile::tempdir().unwrap();
        let src = root.path().join("src");
        let dst = root.path().join("dst");
        std::fs::write(&src, b"old").unwrap();
        std::fs::write(&dst, b"written by the other start").unwrap();

        // The existence checks of the callers can both pass before either
        // copies; what must hold is that the copy itself does not replace.
        assert!(!copy_file_noclobber(&src, &dst).unwrap());

        assert_eq!(std::fs::read(&dst).unwrap(), b"written by the other start");
        let staged: Vec<_> = std::fs::read_dir(root.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .filter(|n| n.to_string_lossy().starts_with(".aeroftp-copy-"))
            .collect();
        assert!(staged.is_empty(), "temporary copy left behind: {staged:?}");
    }

    #[test]
    fn a_file_is_carried_into_a_missing_folder() {
        let root = tempfile::tempdir().unwrap();
        let src = root
            .path()
            .join(LEGACY_APP_IDENTIFIER)
            .join(WINDOW_STATE_FILE);
        let dst = root.path().join(APP_IDENTIFIER).join(WINDOW_STATE_FILE);
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        std::fs::write(&src, b"{\"main\":{}}").unwrap();

        carry_file_if_absent(&src, &dst);

        assert_eq!(std::fs::read(&dst).unwrap(), b"{\"main\":{}}");
    }
}

// SECVAL-B (2026-09-19), lead 7: the legacy merge has no durable marker, so
// the second start runs `copy_missing_tree` again. Modelled here as two calls
// (the wrapper returns early under debug_assertions, so a test build cannot
// call it; in a release build each process start calls it once).
#[cfg(test)]
mod secval_b_tests {
    use super::*;

    #[test]
    fn lead7_a_file_deleted_from_the_data_root_stays_deleted() {
        let root = tempfile::tempdir().unwrap();
        let legacy = root.path().join("com.aeroftp.AeroFTP");
        let current = root.path().join("aeroftp");
        std::fs::create_dir_all(legacy.join("plugins").join("oldplugin")).unwrap();
        std::fs::write(
            legacy.join("plugins").join("oldplugin").join("plugin.json"),
            b"{\"id\":\"oldplugin\"}",
        )
        .unwrap();
        {
            let c = rusqlite::Connection::open(legacy.join("agent_memory.db")).unwrap();
            c.execute_batch(
                "CREATE TABLE m(t TEXT); INSERT INTO m VALUES('pre-migration memory');",
            )
            .unwrap();
        }
        // Start 1 (first run after the upgrade): copies everything.
        merge_legacy_config_once(&legacy, &current).unwrap();
        assert!(current.join("agent_memory.db").is_file());
        assert!(current.join("plugins/oldplugin/plugin.json").is_file());

        // The user wipes the agent memory and uninstalls the plugin
        // (remove_plugin is remove_dir_all on the data-root copy).
        std::fs::remove_file(current.join("agent_memory.db")).unwrap();
        std::fs::remove_dir_all(current.join("plugins/oldplugin")).unwrap();

        // Start 2: same call, nothing on disk records that start 1 happened.
        merge_legacy_config_once(&legacy, &current).unwrap();
        let memory_back = current.join("agent_memory.db").exists();
        let plugin_back = current.join("plugins/oldplugin/plugin.json").exists();
        eprintln!(
            "after start 2: agent_memory.db back = {memory_back}, plugin back = {plugin_back}"
        );
        assert!(
            !memory_back && !plugin_back,
            "files deleted from the data root came back from the legacy tree"
        );
    }
}
