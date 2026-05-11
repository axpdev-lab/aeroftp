// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// AeroFTP Full Keystore Export/Import
// Exports ALL vault entries as encrypted .aeroftp-keystore file
// Uses Argon2id + AES-256-GCM (same as profile_export)

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

// File format version:
//   v1 -- legacy, vault entries only, payload uncompressed
//   v2 -- v3.7.8+, adds sqlite_dumps, files, local_storage so a single
//         .aeroftp-keystore file is a *complete* application snapshot,
//         and zstd-compresses the serialised payload before AES-GCM.
//         Compression is the difference between a 50 MB backup and a
//         5 MB backup for a power user with extensive AeroAgent chat
//         history: the SQLite dumps and JSON blobs of the new sections
//         compress 3-6x, the encrypted vault entries pass through at 1x.
//         The `compression` envelope field tells the reader which codec
//         was used; the only valid v2 value today is "zstd".
const FILE_VERSION: u32 = 2;
const FILE_VERSION_V1_LEGACY: u32 = 1;

/// zstd level applied to the serialised payload at export time.
/// Level 19 is the upper end of the "default" compression bracket
/// before the costly `--ultra` levels (20-22). It trades a fraction of
/// a second of CPU at export for a noticeably smaller output: empirical
/// tests on a typical SQLite chat-history dump (~30 MB) drop the v2
/// payload by an extra ~15% versus level 9, at ~3x the CPU. The price
/// is paid once, in the background, on a user-initiated export action,
/// so the trade is one-sided in favour of the smaller file.
const ZSTD_COMPRESSION_LEVEL: i32 = 19;

/// Whitelist of SQLite databases included in a full backup.
///
/// Each entry is the file name as it lives under `app_config_dir()`. The
/// list is hard-coded rather than glob-scanned so a future stray `*.db`
/// (e.g. a third-party dependency dropping its own file there) does not
/// leak into the backup or, worse, get clobbered on import.
const SQLITE_DBS: &[&str] = &[
    "ai_chat.db",
    "agent_memory.db",
    "file_tags.db",
    "vault_history.db",
    "speedtest_history.db",
];

/// Relative paths under `app_config_dir()` whose contents are copied
/// verbatim into the backup as binary blobs. Subtrees are walked
/// recursively. `plugins/` contains installed plugin manifests + shell
/// scripts the user assembled by hand; `sync_snapshots/` is rollback
/// state the user explicitly chose to keep.
const FILE_DIRS: &[&str] = &["plugins", "sync_snapshots"];

/// Two-tier export contract surfaced to the UI.
///
/// `VaultOnly` is the historical behaviour pre-v3.7.8: a slim, fast
/// export of the encrypted vault entries only -- ideal for "I just want
/// to move my saved servers to another machine". `Full` writes an
/// authoritative application snapshot: every SQLite DB, every plugin,
/// every rollback snapshot, plus the caller-supplied localStorage
/// whitelist. The file format is the same v2 envelope in both cases;
/// `VaultOnly` simply leaves the optional sections empty so the file
/// stays small and import resolution is unambiguous.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportMode {
    VaultOnly,
    Full,
}

impl ExportMode {
    /// Parse the camel-cased mode string from the Tauri command boundary.
    pub fn from_str(s: &str) -> Result<Self, KeystoreExportError> {
        match s {
            "vault_only" | "vaultOnly" | "vault" => Ok(Self::VaultOnly),
            "full" | "Full" | "complete" => Ok(Self::Full),
            other => Err(KeystoreExportError::Encryption(format!(
                "Unknown export mode: {other}"
            ))),
        }
    }
}

/// A2-01: fsync the parent directory of a freshly written file (Unix only).
/// On Windows this is a no-op: directory handles need FILE_FLAG_BACKUP_SEMANTICS
/// and FlushFileBuffers requires GENERIC_WRITE, neither of which `File::open`
/// provides. Windows guarantees rename durability via NTFS journaling instead.
#[cfg(unix)]
fn fsync_parent_dir(file_path: &std::path::Path) {
    if let Some(parent) = file_path.parent() {
        if let Ok(dir) = std::fs::File::open(parent) {
            let _ = dir.sync_all();
        }
    }
}

#[cfg(not(unix))]
fn fsync_parent_dir(_file_path: &std::path::Path) {}

fn normalize_merge_strategy(merge_strategy: &str) -> Result<&'static str, KeystoreExportError> {
    match merge_strategy {
        "skip" | "skip_existing" => Ok("skip_existing"),
        "overwrite" | "overwrite_all" => Ok("overwrite"),
        other => Err(KeystoreExportError::Encryption(format!(
            "Invalid merge strategy: {}",
            other
        ))),
    }
}

// ============ Error Types ============

#[derive(Debug, thiserror::Error)]
pub enum KeystoreExportError {
    #[error("Invalid password")]
    InvalidPassword,
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("Encryption error: {0}")]
    Encryption(String),
    #[error("Unsupported file version: {0}")]
    UnsupportedVersion(u32),
    #[error("Vault not ready")]
    VaultNotReady,
}

// ============ File Format ============

#[derive(Serialize, Deserialize)]
struct KeystoreExportFile {
    version: u32,
    salt: Vec<u8>,
    nonce: Vec<u8>,
    encrypted_payload: Vec<u8>,
    metadata: KeystoreMetadata,
    /// Codec applied to the serialised payload BEFORE AES-256-GCM.
    /// Recognised values:
    ///   - absent / `None` / `"none"` -- raw JSON, used by v1 and by
    ///     the v2 escape hatch for forensic dumps
    ///   - `"zstd"` -- the v2 default. zstd-1.5+ frame format.
    ///
    /// Stored outside the encrypted blob deliberately: the reader must
    /// know which codec to invoke BEFORE it has the plaintext, so the
    /// field would not be useful inside the encrypted payload.
    /// (Disclosing "this archive is compressed" is not a secret: every
    /// compressed file format advertises that fact in its magic bytes
    /// anyway.) Marked optional with serde so v1 files without the
    /// field deserialise as `None` and we route them through the
    /// uncompressed path automatically.
    #[serde(default)]
    compression: Option<String>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct KeystoreMetadata {
    pub export_date: String,
    pub aeroftp_version: String,
    pub entries_count: u32,
    pub categories: KeystoreCategories,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct KeystoreCategories {
    pub server_credentials: u32,
    pub server_profiles: u32,
    pub ai_keys: u32,
    pub oauth_tokens: u32,
    pub config_entries: u32,
    // v2 additions. Marked optional with serde defaults so v1 files
    // deserialise into v2 metadata without panicking.
    #[serde(default)]
    pub sqlite_dbs: u32,
    #[serde(default)]
    pub files: u32,
    #[serde(default)]
    pub local_storage_keys: u32,
}

/// Inner payload deserialised after AES-GCM decryption. v1 files (pre
/// v3.7.8) used a bare `HashMap<String, String>` for vault entries; the
/// v2 reader handles both shapes via `try_from_v1_or_v2` below.
#[derive(Serialize, Deserialize, Default)]
struct ExportPayload {
    #[serde(default)]
    vault_entries: HashMap<String, String>,
    /// Filename (e.g. "ai_chat.db") to verbatim binary contents of the
    /// SQLite file after a WAL-checkpoint flush.
    #[serde(default)]
    sqlite_dumps: HashMap<String, Vec<u8>>,
    /// Relative path under `app_config_dir()` to file bytes.
    #[serde(default)]
    files: HashMap<String, Vec<u8>>,
    /// WebView2 / WebKitGTK `localStorage` keys not backed by the vault.
    /// The frontend gathers these against a whitelist and hands them
    /// down to the backend right before export.
    #[serde(default)]
    local_storage: HashMap<String, String>,
}

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct KeystoreImportResult {
    pub imported: u32,
    pub skipped: u32,
    pub total: u32,
    // v2 additions. Defaulted so callers reading from a v1 file or
    // ignoring the optional sections compile unchanged.
    #[serde(default)]
    pub sqlite_dbs_restored: u32,
    #[serde(default)]
    pub files_restored: u32,
    /// Local-storage keys returned to the frontend so it can re-hydrate
    /// `window.localStorage` after the import dialog closes. The backend
    /// has no business writing to WebView2 storage directly, so the
    /// caller is responsible for applying these.
    #[serde(default)]
    pub local_storage: HashMap<String, String>,
}

/// User-facing selectivity for import. All flags default to `true` so
/// "Import everything" is a single zero-flag call; the import dialog
/// flips individual flags off when the user opts out of a section.
#[derive(Debug, Clone, Copy)]
pub struct ImportSections {
    pub vault: bool,
    pub sqlite_dbs: bool,
    pub files: bool,
    pub local_storage: bool,
}

impl Default for ImportSections {
    fn default() -> Self {
        Self {
            vault: true,
            sqlite_dbs: true,
            files: true,
            local_storage: true,
        }
    }
}

// ============ Categorization ============

/// Categorize a vault account name into its logical group
fn categorize_account(name: &str) -> &'static str {
    if name.starts_with("server_") && !name.starts_with("server_profile_") {
        "server_credentials"
    } else if name.starts_with("server_profile_") || name.starts_with("config_server") {
        "server_profiles"
    } else if name.starts_with("ai_apikey_") {
        "ai_keys"
    } else if name.starts_with("oauth_") {
        "oauth_tokens"
    } else {
        "config_entries"
    }
}

fn count_categories(accounts: &[String]) -> KeystoreCategories {
    let mut cats = KeystoreCategories {
        server_credentials: 0,
        server_profiles: 0,
        ai_keys: 0,
        oauth_tokens: 0,
        config_entries: 0,
        // v2 fields are filled by the export step that knows the actual
        // disk + localStorage tally; this categoriser only counts vault
        // accounts so it leaves them at zero.
        sqlite_dbs: 0,
        files: 0,
        local_storage_keys: 0,
    };
    for name in accounts {
        match categorize_account(name) {
            "server_credentials" => cats.server_credentials += 1,
            "server_profiles" => cats.server_profiles += 1,
            "ai_keys" => cats.ai_keys += 1,
            "oauth_tokens" => cats.oauth_tokens += 1,
            _ => cats.config_entries += 1,
        }
    }
    cats
}

// ============ SQLite + filesystem snapshot helpers ============

/// Capture a SQLite file as a self-contained binary blob.
///
/// We do NOT use the SQLite Online Backup API directly: it would require
/// opening the source DB which may be locked by the main app. Instead
/// we open a short-lived read-write connection, run
/// `PRAGMA wal_checkpoint(TRUNCATE)` to fold the -wal sidecar back into
/// the main file, then read the bytes off disk. If the DB is absent
/// (the user simply has not used AeroAgent yet, etc.) returns `Ok(None)`
/// so the caller silently skips it instead of failing the whole export.
fn snapshot_sqlite_db(path: &Path) -> Result<Option<Vec<u8>>, std::io::Error> {
    if !path.is_file() {
        return Ok(None);
    }
    // Best-effort WAL flush. If it fails (corrupt journal, locked, not
    // a SQLite file) we still try to read the bytes -- worst case the
    // import side sees a slightly out-of-date snapshot, which is still
    // strictly better than dropping the file entirely.
    if let Ok(conn) = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) {
        let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
    }
    Ok(Some(std::fs::read(path)?))
}

/// Recursively collect every regular file under `root` and return them
/// keyed by their path relative to `root` (so the entry can be restored
/// to the exact same layout). Symlinks are skipped (`fs::metadata`
/// follows them so a broken link is silently dropped, and a link to
/// something outside the tree could otherwise smuggle arbitrary bytes
/// into the backup).
fn snapshot_directory_tree(root: &Path) -> Result<HashMap<String, Vec<u8>>, std::io::Error> {
    let mut out = HashMap::new();
    if !root.is_dir() {
        return Ok(out);
    }
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let ft = entry.file_type()?;
            let path = entry.path();
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                stack.push(path);
                continue;
            }
            if ft.is_file() {
                let rel = path
                    .strip_prefix(root)
                    .map_err(|e| std::io::Error::other(e.to_string()))?
                    .to_string_lossy()
                    .replace('\\', "/");
                let bytes = std::fs::read(&path)?;
                out.insert(rel, bytes);
            }
        }
    }
    Ok(out)
}

/// Restore a file tree previously captured by `snapshot_directory_tree`.
///
/// Refuses any entry whose normalised relative path contains a `..`
/// segment, is absolute, or contains a drive prefix (`C:\` style).
/// Those would let a maliciously hand-crafted backup write outside the
/// target directory at import time.
fn restore_directory_tree(
    root: &Path,
    files: &HashMap<String, Vec<u8>>,
) -> Result<u32, std::io::Error> {
    if files.is_empty() {
        return Ok(0);
    }
    std::fs::create_dir_all(root)?;
    let mut written = 0u32;
    for (rel, bytes) in files {
        // Path traversal guard
        if rel.is_empty()
            || rel.starts_with('/')
            || rel.starts_with('\\')
            || rel.contains("..")
            || rel.contains(':')
        {
            tracing::warn!("Skipping unsafe path during restore: {}", rel);
            continue;
        }
        let target = root.join(rel);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&target, bytes)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Plugin scripts need exec bit; everything else stays 0600.
            let mode = if rel.starts_with("plugins/")
                && (rel.ends_with(".sh") || rel.ends_with(".py") || rel.ends_with(".js"))
            {
                0o700
            } else {
                0o600
            };
            let _ = std::fs::set_permissions(&target, std::fs::Permissions::from_mode(mode));
        }
        written += 1;
    }
    Ok(written)
}

// ============ Export/Import ============

/// Export the full application state to an encrypted file.
///
/// "Full" means: every vault entry, every SQLite database under
/// `config_dir`, every plugin and rollback snapshot on disk, and the
/// caller-supplied `local_storage` blob (the frontend whitelist of
/// non-vault preference keys). The output is a single AES-256-GCM
/// blob keyed via Argon2id -- one password unlocks the whole snapshot.
///
/// `config_dir` is resolved by the caller (the Tauri command handler)
/// via `portable::app_config_dir(app)` so this function stays
/// dependency-free of `AppHandle`. Pass `None` if the caller does not
/// want filesystem-level state included (e.g. legacy callers that only
/// want vault entries).
pub fn export_keystore(
    password: &str,
    file_path: &Path,
    mode: ExportMode,
    config_dir: Option<&Path>,
    local_storage_in: Option<HashMap<String, String>>,
) -> Result<KeystoreMetadata, KeystoreExportError> {
    // A2-05: Backend password minimum length check
    if password.len() < 8 {
        return Err(KeystoreExportError::Encryption(
            "Password must be at least 8 characters".into(),
        ));
    }
    let store = crate::credential_store::CredentialStore::from_cache()
        .ok_or(KeystoreExportError::VaultNotReady)?;

    // List all accounts and read their values
    let accounts = store
        .list_accounts()
        .map_err(|e| KeystoreExportError::Encryption(e.to_string()))?;

    let mut entries: HashMap<String, String> = HashMap::new();
    for account in &accounts {
        if let Ok(value) = store.get(account) {
            entries.insert(account.clone(), value);
        }
    }

    // Optional sections, populated only when the caller asks for a full
    // snapshot. VaultOnly explicitly skips disk and localStorage so the
    // resulting file has the same on-disk footprint as a pre-v3.7.8 v1
    // export and migrating between machines stays fast and predictable.
    let mut sqlite_dumps: HashMap<String, Vec<u8>> = HashMap::new();
    let mut files_blob: HashMap<String, Vec<u8>> = HashMap::new();
    let mut local_storage = HashMap::new();

    if mode == ExportMode::Full {
        if let Some(cfg) = config_dir {
            for db in SQLITE_DBS {
                let path = cfg.join(db);
                match snapshot_sqlite_db(&path) {
                    Ok(Some(bytes)) => {
                        sqlite_dumps.insert((*db).to_string(), bytes);
                    }
                    Ok(None) => {}
                    Err(e) => tracing::warn!("Could not snapshot {}: {}", db, e),
                }
            }
            for dir in FILE_DIRS {
                let path = cfg.join(dir);
                match snapshot_directory_tree(&path) {
                    Ok(map) => {
                        for (rel, bytes) in map {
                            files_blob.insert(format!("{}/{}", dir, rel), bytes);
                        }
                    }
                    Err(e) => tracing::warn!("Could not walk {}: {}", dir, e),
                }
            }
        }
        local_storage = local_storage_in.unwrap_or_default();
    } else {
        // VaultOnly: discard any caller-supplied disk paths or
        // localStorage blob -- the contract is explicit.
        let _ = (config_dir, local_storage_in);
    }

    let entries_count = entries.len() as u32;
    let mut categories = count_categories(&accounts);
    categories.sqlite_dbs = sqlite_dumps.len() as u32;
    categories.files = files_blob.len() as u32;
    categories.local_storage_keys = local_storage.len() as u32;

    let metadata = KeystoreMetadata {
        export_date: chrono::Utc::now().to_rfc3339(),
        aeroftp_version: env!("CARGO_PKG_VERSION").to_string(),
        entries_count,
        categories,
    };

    // Serialize the full v2 payload to JSON
    let payload = ExportPayload {
        vault_entries: entries,
        sqlite_dumps,
        files: files_blob,
        local_storage,
    };
    let payload_json = serde_json::to_vec(&payload)?;
    let raw_len = payload_json.len();

    // Compress before encryption (compress-then-encrypt is the standard
    // order: encrypted output is high-entropy and would not compress
    // afterwards anyway). CRIME / BREACH style attacks rely on an
    // attacker injecting plaintext and measuring the ciphertext length
    // -- the keystore export has no such channel, the user encrypts
    // their own state for themselves, so the optimisation is free.
    let compressed_payload = zstd::stream::encode_all(&payload_json[..], ZSTD_COMPRESSION_LEVEL)
        .map_err(|e| KeystoreExportError::Encryption(format!("zstd compress failed: {e}")))?;
    let compressed_len = compressed_payload.len();
    tracing::debug!(
        "Keystore payload compressed: {} -> {} bytes ({:.1}%)",
        raw_len,
        compressed_len,
        (compressed_len as f64) * 100.0 / raw_len.max(1) as f64,
    );

    // A2-06: Encrypt with Argon2id (128 MiB, same strength as vault) + AES-256-GCM
    let salt = crate::crypto::random_bytes(32);
    let key = crate::crypto::derive_key_strong(password, &salt)
        .map_err(KeystoreExportError::Encryption)?;
    let nonce = crate::crypto::random_bytes(12);
    let encrypted = crate::crypto::encrypt_aes_gcm(&key, &nonce, &compressed_payload)
        .map_err(KeystoreExportError::Encryption)?;

    let export_file = KeystoreExportFile {
        version: FILE_VERSION,
        salt,
        nonce,
        encrypted_payload: encrypted,
        metadata: metadata.clone(),
        compression: Some("zstd".to_string()),
    };

    let file_data = serde_json::to_vec_pretty(&export_file)?;
    // A2-08: Atomic write (temp+rename) + secure permissions
    let tmp_path = file_path.with_extension("tmp");
    // A2-01: write+fsync via a write-mode handle. On Windows `File::open` returns
    // a read-only handle and `sync_all` (FlushFileBuffers) needs GENERIC_WRITE,
    // which would fail with ERROR_ACCESS_DENIED (os error 5) and leave the .tmp
    // behind without ever renaming: see issue #124.
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp_path)?;
        f.write_all(&file_data)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp_path, file_path)?;
    fsync_parent_dir(file_path);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(file_path, std::fs::Permissions::from_mode(0o600));
    }

    tracing::info!(
        "Keystore exported: mode={:?} vault_entries={} sqlite_dbs={} files={} local_storage_keys={} to {:?}",
        mode,
        entries_count,
        metadata.categories.sqlite_dbs,
        metadata.categories.files,
        metadata.categories.local_storage_keys,
        file_path
    );
    Ok(metadata)
}

/// Decode the inner JSON payload tolerating both shapes:
///   - v1 (pre-v3.7.8): bare `HashMap<String, String>` of vault entries
///   - v2 (v3.7.8+):    `ExportPayload { vault_entries, sqlite_dumps, files, local_storage }`
///
/// We attempt v2 first because new exports are the common case; a v1
/// payload deserialises as the empty `ExportPayload` default (every
/// field is `#[serde(default)]`), which is not what we want, so we
/// disambiguate by checking the file `version` field captured upstream.
fn parse_export_payload(version: u32, payload_json: &[u8]) -> Result<ExportPayload, KeystoreExportError> {
    if version <= FILE_VERSION_V1_LEGACY {
        let entries: HashMap<String, String> = serde_json::from_slice(payload_json)?;
        Ok(ExportPayload {
            vault_entries: entries,
            ..Default::default()
        })
    } else {
        serde_json::from_slice::<ExportPayload>(payload_json).map_err(KeystoreExportError::from)
    }
}

/// Import an encrypted backup into the running app.
///
/// Behaviour by file format:
///   - v1 files: vault entries are imported with the configured merge
///     strategy. `sqlite_dbs_restored`, `files_restored`, and
///     `local_storage` in the result are zero / empty.
///   - v2 files: vault entries get the same treatment, then the SQLite
///     databases listed in [`SQLITE_DBS`] are restored to disk under
///     `config_dir` and the file trees under [`FILE_DIRS`] are restored
///     verbatim. The localStorage map is returned to the caller so the
///     frontend can rehydrate WebView storage from the same operation.
///
/// `sections` lets the import dialog opt out of individual areas (e.g.
/// "import my servers but not AI chats"); pass `ImportSections::default()`
/// for "everything".
///
/// `on_progress` callback receives (phase, current, total): phase is
/// `"decrypting"`, `"importing"`, `"sqlite"`, `"files"`.
#[allow(clippy::type_complexity)]
pub fn import_keystore(
    password: &str,
    file_path: &Path,
    merge_strategy: &str,
    sections: ImportSections,
    config_dir: Option<&Path>,
    on_progress: Option<&dyn Fn(&str, u32, u32)>,
) -> Result<KeystoreImportResult, KeystoreExportError> {
    let store = crate::credential_store::CredentialStore::from_cache()
        .ok_or(KeystoreExportError::VaultNotReady)?;

    // Read and parse file
    let file_data = std::fs::read(file_path)?;
    let export_file: KeystoreExportFile = serde_json::from_slice(&file_data)?;

    if export_file.version > FILE_VERSION {
        return Err(KeystoreExportError::UnsupportedVersion(export_file.version));
    }

    // Emit decrypting phase (Argon2id KDF is slow)
    let metadata_count = export_file.metadata.entries_count;
    if let Some(cb) = &on_progress {
        cb("decrypting", 0, metadata_count);
    }

    // A2-06: Try strong KDF first (128 MiB, new exports), fall back to legacy (64 MiB) for old files
    let key_strong = crate::crypto::derive_key_strong(password, &export_file.salt)
        .map_err(KeystoreExportError::Encryption)?;
    let raw_payload = match crate::crypto::decrypt_aes_gcm(
        &key_strong,
        &export_file.nonce,
        &export_file.encrypted_payload,
    ) {
        Ok(data) => data,
        Err(_) => {
            // Legacy fallback: file was exported with derive_key (64 MiB)
            let key_legacy = crate::crypto::derive_key(password, &export_file.salt)
                .map_err(KeystoreExportError::Encryption)?;
            crate::crypto::decrypt_aes_gcm(
                &key_legacy,
                &export_file.nonce,
                &export_file.encrypted_payload,
            )
            .map_err(|_| KeystoreExportError::InvalidPassword)?
        }
    };

    // Decompress if the envelope declares a codec. Missing field /
    // "none" leave the bytes as-is, which is the v1 contract.
    let payload_json = match export_file.compression.as_deref() {
        None | Some("none") | Some("") => raw_payload,
        Some("zstd") => zstd::stream::decode_all(&raw_payload[..]).map_err(|e| {
            KeystoreExportError::Encryption(format!("zstd decompress failed: {e}"))
        })?,
        Some(other) => {
            return Err(KeystoreExportError::Encryption(format!(
                "Unknown compression codec: {other}"
            )))
        }
    };

    let payload = parse_export_payload(export_file.version, &payload_json)?;
    let entries = if sections.vault {
        payload.vault_entries
    } else {
        HashMap::new()
    };
    let merge_strategy = normalize_merge_strategy(merge_strategy)?;

    // Get existing accounts for merge strategy
    let existing = if merge_strategy == "skip_existing" {
        store
            .list_accounts()
            .map_err(|e| KeystoreExportError::Encryption(e.to_string()))?
            .into_iter()
            .collect::<HashSet<_>>()
    } else {
        HashSet::new()
    };

    // GPT-A2-02: Stage entries first: collect what to import, then commit all-or-nothing
    // Uses owned values to support profile list merging
    let mut staged: Vec<(String, String)> = Vec::new();
    let mut originals: HashMap<String, Option<String>> = HashMap::new();
    let mut skipped = 0u32;
    let total = entries.len() as u32;

    for (account, value) in &entries {
        if merge_strategy == "skip_existing" && existing.contains(account) {
            // Special case: config_server_profiles is an aggregate list: merge by ID
            if account == "config_server_profiles" {
                if let Ok(existing_json) = store.get(account) {
                    let merged = merge_profile_lists(&existing_json, value);
                    if merged != existing_json {
                        originals.insert(account.clone(), Some(existing_json));
                        staged.push((account.clone(), merged));
                        continue;
                    }
                }
            }
            skipped += 1;
            continue;
        }
        let original = match store.get(account) {
            Ok(existing_value) => Some(existing_value),
            Err(crate::credential_store::CredentialError::NotFound(_)) => None,
            Err(e) => return Err(KeystoreExportError::Encryption(e.to_string())),
        };
        originals.insert(account.clone(), original);
        staged.push((account.clone(), value.clone()));
    }

    // Emit importing phase start
    let staged_total = staged.len() as u32;
    if let Some(cb) = &on_progress {
        cb("importing", 0, staged_total);
    }

    // Commit phase: write all staged entries, rollback on first failure
    let mut committed: Vec<String> = Vec::new();
    for (account, value) in &staged {
        match store.store(account, value) {
            Ok(_) => {
                committed.push(account.clone());
                if let Some(cb) = &on_progress {
                    cb("importing", committed.len() as u32, staged_total);
                }
            }
            Err(e) => {
                tracing::error!(
                    "Failed to import keystore entry '{}': {}: rolling back {} committed entries",
                    account,
                    e,
                    committed.len()
                );
                // Rollback: restore prior values for overwrites, delete only newly inserted entries
                for rollback_account in committed.iter().rev() {
                    let rollback_result = match originals.get(rollback_account) {
                        Some(Some(previous_value)) => store.store(rollback_account, previous_value),
                        Some(None) => store.delete(rollback_account),
                        None => Ok(()),
                    };
                    if let Err(re) = rollback_result {
                        tracing::warn!("Rollback failed for '{}': {}", rollback_account, re);
                    }
                }
                return Err(KeystoreExportError::Encryption(format!(
                    "Import failed at '{}': {}. {} entries rolled back.",
                    account,
                    e,
                    committed.len()
                )));
            }
        }
    }

    let imported = committed.len() as u32;

    // ====== v2 section restore ======
    // The vault commit above succeeded (or was empty when vault was
    // opted out); now apply SQLite + files atomically on disk. Failures
    // here are logged but do NOT roll back the vault import: a partial
    // restore where the user has their servers but their AI chat DB
    // failed to write is strictly more useful than a "nothing happened
    // because one file was locked" outcome.
    let mut sqlite_dbs_restored = 0u32;
    let mut files_restored = 0u32;

    if sections.sqlite_dbs && !payload.sqlite_dumps.is_empty() {
        if let Some(cfg) = config_dir {
            if let Err(e) = std::fs::create_dir_all(cfg) {
                tracing::warn!("Cannot create config_dir for SQLite restore: {}", e);
            } else {
                let total_dbs = payload.sqlite_dumps.len() as u32;
                if let Some(cb) = &on_progress {
                    cb("sqlite", 0, total_dbs);
                }
                for (idx, (name, bytes)) in payload.sqlite_dumps.iter().enumerate() {
                    // Whitelist enforcement: refuse names the import file
                    // makes up to write outside the SQLITE_DBS set.
                    if !SQLITE_DBS.contains(&name.as_str()) {
                        tracing::warn!("Refusing to restore unknown SQLite name: {}", name);
                        continue;
                    }
                    let target = cfg.join(name);
                    // Atomic: write to .tmp then rename. Sidecar -wal /
                    // -shm files from the previous DB are stale relative
                    // to the new DB content -- we explicitly remove them
                    // so SQLite does not try to replay a journal that
                    // refers to a different page layout.
                    let tmp = target.with_extension("db.tmp");
                    match std::fs::write(&tmp, bytes)
                        .and_then(|_| std::fs::rename(&tmp, &target))
                    {
                        Ok(_) => {
                            let _ = std::fs::remove_file(cfg.join(format!("{name}-wal")));
                            let _ = std::fs::remove_file(cfg.join(format!("{name}-shm")));
                            sqlite_dbs_restored += 1;
                            if let Some(cb) = &on_progress {
                                cb("sqlite", idx as u32 + 1, total_dbs);
                            }
                        }
                        Err(e) => tracing::warn!("SQLite restore failed for {}: {}", name, e),
                    }
                }
            }
        } else {
            tracing::warn!(
                "Import has {} SQLite dumps but config_dir is None; skipping",
                payload.sqlite_dumps.len()
            );
        }
    }

    if sections.files && !payload.files.is_empty() {
        if let Some(cfg) = config_dir {
            // Group payload entries by top-level directory so we can
            // refuse anything outside the FILE_DIRS whitelist before
            // touching disk.
            let mut grouped: HashMap<&str, HashMap<String, Vec<u8>>> = HashMap::new();
            for (rel, bytes) in &payload.files {
                let Some(slash) = rel.find('/') else { continue };
                let (head, tail) = rel.split_at(slash);
                let tail = tail.trim_start_matches('/');
                if !FILE_DIRS.contains(&head) {
                    tracing::warn!("Refusing to restore unknown subtree: {}", head);
                    continue;
                }
                grouped
                    .entry(FILE_DIRS.iter().find(|d| **d == head).copied().unwrap_or(""))
                    .or_default()
                    .insert(tail.to_string(), bytes.clone());
            }
            let total_files: u32 = grouped.values().map(|m| m.len() as u32).sum();
            if let Some(cb) = &on_progress {
                cb("files", 0, total_files);
            }
            let mut done = 0u32;
            for (dir, map) in grouped {
                if dir.is_empty() {
                    continue;
                }
                let root = cfg.join(dir);
                match restore_directory_tree(&root, &map) {
                    Ok(n) => {
                        files_restored += n;
                        done += n;
                        if let Some(cb) = &on_progress {
                            cb("files", done, total_files);
                        }
                    }
                    Err(e) => tracing::warn!("File restore failed for {}: {}", dir, e),
                }
            }
        } else {
            tracing::warn!(
                "Import has {} files but config_dir is None; skipping",
                payload.files.len()
            );
        }
    }

    let local_storage = if sections.local_storage {
        payload.local_storage
    } else {
        HashMap::new()
    };

    tracing::info!(
        "Keystore imported: vault={}({} skipped) sqlite_dbs={} files={} local_storage_keys={} from {:?}",
        imported,
        skipped,
        sqlite_dbs_restored,
        files_restored,
        local_storage.len(),
        file_path
    );
    Ok(KeystoreImportResult {
        imported,
        skipped,
        total,
        sqlite_dbs_restored,
        files_restored,
        local_storage,
    })
}

/// Merge two server profile JSON arrays by "id" field.
/// Returns union: existing profiles + any backup profiles not already present.
fn merge_profile_lists(existing_json: &str, backup_json: &str) -> String {
    let mut existing: Vec<serde_json::Value> =
        serde_json::from_str(existing_json).unwrap_or_default();
    let backup: Vec<serde_json::Value> = serde_json::from_str(backup_json).unwrap_or_default();

    let existing_ids: HashSet<String> = existing
        .iter()
        .filter_map(|p| p.get("id").and_then(|v| v.as_str()).map(String::from))
        .collect();

    let mut added = 0usize;
    for profile in backup {
        if let Some(id) = profile.get("id").and_then(|v| v.as_str()) {
            if !existing_ids.contains(id) {
                existing.push(profile);
                added += 1;
            }
        }
    }

    if added > 0 {
        tracing::info!(
            "Merged {} server profiles from backup into existing list",
            added
        );
    }

    serde_json::to_string(&existing).unwrap_or_else(|_| existing_json.to_string())
}

/// Count vault entries by category from account name list
pub fn categorize_accounts(accounts: &[String]) -> KeystoreCategories {
    count_categories(accounts)
}

/// Read export file metadata without decrypting
pub fn read_keystore_metadata(file_path: &Path) -> Result<KeystoreMetadata, KeystoreExportError> {
    let file_data = std::fs::read(file_path)?;
    let export_file: KeystoreExportFile = serde_json::from_slice(&file_data)?;
    Ok(export_file.metadata)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// zstd round-trip on the shape of payload we actually produce:
    /// a `Vec<u8>` of serialised JSON containing structured text and
    /// arbitrary binary blobs. The level we ship (19) must round-trip
    /// the bytes exactly, and the test catches any future regression
    /// where someone accidentally swaps the codec or the level for
    /// something lossy.
    #[test]
    fn zstd_round_trip_preserves_bytes() {
        let raw = serde_json::to_vec(&serde_json::json!({
            "vault_entries": {
                "server_acme": "encrypted_blob_0",
                "ai_apikey_openai": "encrypted_blob_1",
            },
            "sqlite_dumps": {
                "ai_chat.db": vec![0x53, 0x51, 0x4c, 0x69, 0x74, 0x65, 0x20, 0x66],
            },
            "local_storage": {
                "aeroftp_ai_agent_mode": "auto",
            },
        }))
        .expect("serialise test payload");
        let compressed = zstd::stream::encode_all(&raw[..], ZSTD_COMPRESSION_LEVEL)
            .expect("zstd compress");
        let decompressed = zstd::stream::decode_all(&compressed[..]).expect("zstd decompress");
        assert_eq!(raw, decompressed);
    }

    /// Highly-repetitive plaintext must actually shrink under zstd.
    /// Asserts a conservative 50% reduction so the test does not flake
    /// across zstd library updates; in practice we see ~99%.
    #[test]
    fn zstd_compresses_repetitive_payload() {
        let raw = vec![b'A'; 64 * 1024];
        let compressed = zstd::stream::encode_all(&raw[..], ZSTD_COMPRESSION_LEVEL).unwrap();
        assert!(
            compressed.len() < raw.len() / 2,
            "expected at least 2x compression on AAAA payload, got {} -> {}",
            raw.len(),
            compressed.len()
        );
    }

    /// High-entropy plaintext (encrypted blobs already in vault) must
    /// at worst pass through with a small framing overhead, never blow
    /// up. Confirms the "vault entries compress at ~1x" claim in the
    /// design doc and catches a hypothetical future regression where
    /// someone enables a codec with pathological behaviour on random.
    #[test]
    fn zstd_does_not_explode_on_random_payload() {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::Hasher;
        let mut raw = Vec::with_capacity(64 * 1024);
        let mut hasher = DefaultHasher::new();
        for i in 0..(64 * 1024u64) {
            hasher.write_u64(i ^ 0xCAFEBABEu64);
            raw.push((hasher.finish() & 0xFF) as u8);
        }
        let compressed = zstd::stream::encode_all(&raw[..], ZSTD_COMPRESSION_LEVEL).unwrap();
        // Allow 5% framing overhead vs raw; anything beyond is suspicious.
        assert!(
            compressed.len() < (raw.len() * 105) / 100,
            "compressed random payload grew more than 5%: {} -> {}",
            raw.len(),
            compressed.len()
        );
    }

    /// Backward compatibility: v1 envelopes have no `compression`
    /// field. They must deserialise into `compression: None` so the
    /// import path routes them through the no-codec branch unchanged.
    #[test]
    fn v1_envelope_deserialises_without_compression_field() {
        let v1_envelope = serde_json::json!({
            "version": 1,
            "salt": [1, 2, 3],
            "nonce": [4, 5, 6],
            "encrypted_payload": [7, 8, 9],
            "metadata": {
                "exportDate": "2024-01-01T00:00:00Z",
                "aeroftpVersion": "3.7.0",
                "entriesCount": 1,
                "categories": {
                    "serverCredentials": 1,
                    "serverProfiles": 0,
                    "aiKeys": 0,
                    "oauthTokens": 0,
                    "configEntries": 0,
                },
            },
        });
        let bytes = serde_json::to_vec(&v1_envelope).unwrap();
        let parsed: KeystoreExportFile = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed.version, 1);
        assert!(parsed.compression.is_none());
    }
}
