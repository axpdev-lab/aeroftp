//! Backend-reviewed manifest contract and bounded managed STDIO installer.
//! No package manager, lifecycle script, model-supplied URL or launch-time download.
// SPDX-License-Identifier: GPL-3.0-or-later
use crate::mcp_client_config::{McpManagedInstall, McpSandboxConfig, McpServerConfig};
use crate::mcp_client_install_paths as paths;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use tauri::{AppHandle, Manager, Webview};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug, Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Runtime {
    Node,
    Python,
}
#[derive(Clone, Debug, Serialize)]
pub struct ManagedManifest {
    pub id: &'static str,
    pub version: &'static str,
    pub archive_url: &'static str,
    pub archive_sha256: &'static str,
    pub runtime: Runtime,
    /// Archive is a complete runnable tree, including pinned transitive dependencies.
    /// Paths are relative to its root; no package-root stripping or install scripts.
    pub entry: &'static str,
    pub args: &'static [&'static str],
    pub network: bool,
}
// MCLIENT-07 supplies reviewed artifacts and cards. Never accept a renderer manifest.
const REVIEWED_MANIFESTS: &[ManagedManifest] = &[];

fn manifest(id: &str, version: &str) -> Result<&'static ManagedManifest, &'static str> {
    REVIEWED_MANIFESTS
        .iter()
        .find(|m| m.id == id && m.version == version)
        .ok_or("MCP_INSTALL_UNKNOWN")
}

fn validate_manifest(m: &ManagedManifest) -> Result<(), &'static str> {
    if !crate::mcp_client_config::identifier(m.id, 64)
        || !crate::mcp_client_config::identifier(&m.version.replace('.', "-"), 64)
        || m.archive_sha256.len() != 64
        || !m
            .archive_sha256
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        || !paths::relative_path(Path::new(m.entry))
        || !crate::mcp_client_config::literal(m.entry)
        || m.args.len() > 31
        || m.args
            .iter()
            .any(|arg| !crate::mcp_client_config::literal(arg))
    {
        return Err("MCP_INSTALL_INVALID");
    }
    crate::mcp_client_http_config::parse_public_https(m.archive_url, 2048)
        .map_err(|_| "MCP_INSTALL_INVALID")?;
    Ok(())
}

fn runtime(runtime: Runtime) -> Result<String, &'static str> {
    runtime_in(
        runtime,
        &[
            Path::new("/usr/bin"),
            Path::new("/usr/local/bin"),
            Path::new("/bin"),
        ],
    )
}
fn runtime_in(runtime: Runtime, bases: &[&Path]) -> Result<String, &'static str> {
    let binary = match runtime {
        Runtime::Node => "node",
        Runtime::Python => "python3",
    };
    // Only system runtimes already exposed by the sandbox; no shell/PATH package runners.
    bases
        .iter()
        .map(|base| base.join(binary))
        .filter_map(|p| p.canonicalize().ok())
        .find(|p| p.is_file() && p.starts_with("/usr"))
        .and_then(|p| p.to_str().map(str::to_owned))
        .ok_or("MCP_INSTALL_RUNTIME")
}

/// Dedicated app-data tree. Check every existing component, including parent roots.
fn private_dir(path: &Path) -> Result<(), &'static str> {
    let mut cursor = PathBuf::new();
    for component in path.components() {
        cursor.push(component);
        match std::fs::symlink_metadata(&cursor) {
            Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {}
            Ok(_) => return Err("MCP_DIRECTORY_CHANGED"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(&cursor).map_err(|_| "MCP_INSTALL_IO")?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&cursor, std::fs::Permissions::from_mode(0o700))
                        .map_err(|_| "MCP_INSTALL_IO")?;
                }
            }
            Err(_) => return Err("MCP_INSTALL_IO"),
        }
    }
    Ok(())
}

fn base(app: &AppHandle, user: i64, id: &str) -> Result<PathBuf, &'static str> {
    if !crate::mcp_client_config::identifier(id, 64) || user <= 0 {
        return Err("MCP_INSTALL_INVALID");
    }
    Ok(app
        .path()
        .app_data_dir()
        .map_err(|_| "MCP_INSTALL_IO")?
        .join("mcp-servers")
        .join(format!("user-{user}"))
        .join(id))
}

pub(crate) fn validate_binding(
    app: &AppHandle,
    user: i64,
    config: &McpServerConfig,
) -> Result<(), &'static str> {
    if let Some(install) = &config.sandbox.managed {
        let m = manifest(&install.manifest_id, &install.version)?;
        validate_manifest(m)?;
        let expected = base(app, user, m.id)?.join(m.version);
        if config.id != m.id
            || install.archive_sha256 != m.archive_sha256
            || install.network_declared != m.network
            || config.command != runtime(m.runtime)?
            || config.args
                != std::iter::once(expected.join(m.entry).to_string_lossy().into_owned())
                    .chain(m.args.iter().map(|s| (*s).into()))
                    .collect::<Vec<_>>()
            || config
                .sandbox
                .directories
                .first()
                .is_none_or(|g| Path::new(&g.path) != expected)
        {
            return Err("MCP_INSTALL_INVALID");
        }
    } else {
        for grant in &config.sandbox.directories {
            paths::custom_path(Path::new(&grant.path))?;
        }
    }
    for grant in &config.sandbox.directories {
        if paths::directory_grant(Path::new(&grant.path))? != *grant {
            return Err("MCP_DIRECTORY_CHANGED");
        }
    }
    Ok(())
}

/// Move the artifact out of the launch path while its catalog transaction is
/// pending. Restore it on rollback; remove it only after the catalog commits.
pub(crate) struct Removal {
    original: Option<PathBuf>,
    quarantine: Option<tempfile::TempDir>,
}
impl Drop for Removal {
    fn drop(&mut self) {
        if let (Some(original), Some(quarantine)) = (&self.original, &self.quarantine) {
            let _ = std::fs::rename(quarantine.path().join("tree"), original);
        }
    }
}
impl Removal {
    pub(crate) fn commit(mut self) -> Result<(), &'static str> {
        self.original = None;
        if let Some(quarantine) = self.quarantine.take() {
            quarantine.close().map_err(|_| "MCP_INSTALL_IO")?;
        }
        Ok(())
    }
}
fn quarantine(expected: &Path) -> Result<Removal, &'static str> {
    match std::fs::symlink_metadata(expected) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Removal {
            original: None,
            quarantine: None,
        }),
        Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {
            let parent = expected.parent().ok_or("MCP_INSTALL_INVALID")?;
            private_dir(parent)?;
            let quarantine = tempfile::tempdir_in(parent).map_err(|_| "MCP_INSTALL_IO")?;
            std::fs::rename(expected, quarantine.path().join("tree"))
                .map_err(|_| "MCP_INSTALL_IO")?;
            Ok(Removal {
                original: Some(expected.into()),
                quarantine: Some(quarantine),
            })
        }
        _ => Err("MCP_DIRECTORY_CHANGED"),
    }
}
pub(crate) fn uninstall(
    app: &AppHandle,
    user: i64,
    config: &McpServerConfig,
) -> Result<Removal, &'static str> {
    let Some(install) = &config.sandbox.managed else {
        return Ok(Removal {
            original: None,
            quarantine: None,
        });
    };
    let expected = base(app, user, &config.id)?.join(&install.version);
    if config
        .sandbox
        .directories
        .first()
        .is_none_or(|g| Path::new(&g.path) != expected)
    {
        return Err("MCP_INSTALL_INVALID");
    }
    // Delete only the backend-derived artifact, never a renderer-selected path.
    quarantine(&expected)
}

pub(crate) fn invalidate_all() {
    INSTALL_GENERATION.fetch_add(1, Ordering::AcqRel);
    if let Ok(operations) = OPERATIONS.lock() {
        for operation in operations.values() {
            operation.cancel.cancel();
        }
    }
}

static INSTALL_GENERATION: AtomicU64 = AtomicU64::new(0);
// A Cancel may arrive while the install is still resolving its user context.
// Bound and expire these pre-start cancellations rather than acknowledging a no-op.
type PendingCancels = HashMap<(i64, String), std::time::Instant>;
static PENDING_CANCELS: LazyLock<Mutex<PendingCancels>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
fn prune_cancels(pending: &mut PendingCancels) {
    pending.retain(|_, at| at.elapsed() < std::time::Duration::from_secs(120));
}
fn cancel_operation(user: i64, id: &str) -> Result<(), &'static str> {
    if uuid::Uuid::parse_str(id).is_err() {
        return Err("MCP_INSTALL_INVALID");
    }
    let operations = OPERATIONS.lock().map_err(|_| "MCP_INSTALL_BUSY")?;
    if let Some(op) = operations.get(&user).filter(|op| op.id == id) {
        op.cancel.cancel();
        return Ok(());
    }
    let mut pending = PENDING_CANCELS.lock().map_err(|_| "MCP_INSTALL_BUSY")?;
    prune_cancels(&mut pending);
    if pending.len() >= 64 {
        return Err("MCP_INSTALL_BUSY");
    }
    pending.insert((user, id.into()), std::time::Instant::now());
    Ok(())
}

struct Operation {
    id: String,
    cancel: CancellationToken,
}
static OPERATIONS: LazyLock<Mutex<HashMap<i64, Operation>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
struct OperationGuard {
    user: i64,
    cancel: CancellationToken,
}
impl Drop for OperationGuard {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Ok(mut operations) = OPERATIONS.lock() {
            operations.remove(&self.user);
        }
    }
}
fn register(user: i64, id: &str) -> Result<OperationGuard, &'static str> {
    if uuid::Uuid::parse_str(id).is_err() {
        return Err("MCP_INSTALL_INVALID");
    }
    let mut operations = OPERATIONS.lock().map_err(|_| "MCP_INSTALL_BUSY")?;
    let mut pending = PENDING_CANCELS.lock().map_err(|_| "MCP_INSTALL_BUSY")?;
    prune_cancels(&mut pending);
    if pending.remove(&(user, id.into())).is_some() {
        return Err("MCP_INSTALL_CANCELLED");
    }
    if operations.contains_key(&user) {
        return Err("MCP_INSTALL_BUSY");
    }
    let cancel = CancellationToken::new();
    operations.insert(
        user,
        Operation {
            id: id.into(),
            cancel: cancel.clone(),
        },
    );
    Ok(OperationGuard { user, cancel })
}

async fn download(
    m: &ManagedManifest,
    cancel: &CancellationToken,
) -> Result<Vec<u8>, &'static str> {
    validate_manifest(m)?;
    let url = crate::mcp_client_http_config::parse_public_https(m.archive_url, 2048)
        .map_err(|_| "MCP_INSTALL_INVALID")?;
    let client = crate::mcp_client_http_transport::pinned_client(&url, cancel)
        .await
        .map_err(|error| match error {
            crate::mcp_client_http_transport::HttpError::Cancelled => "MCP_INSTALL_CANCELLED",
            _ => "MCP_INSTALL_DOWNLOAD",
        })?;
    let mut response = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err("MCP_INSTALL_CANCELLED"),
        response = client.get(url).send() => response.map_err(|_| "MCP_INSTALL_DOWNLOAD")?,
    };
    if !response.status().is_success() {
        return Err("MCP_INSTALL_DOWNLOAD");
    }
    if response
        .content_length()
        .is_some_and(|n| n > paths::MAX_ARCHIVE_BYTES as u64)
    {
        return Err("MCP_INSTALL_LIMIT");
    }
    let mut bytes = Vec::new();
    loop {
        let chunk = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err("MCP_INSTALL_CANCELLED"),
            chunk = response.chunk() => chunk.map_err(|_| "MCP_INSTALL_DOWNLOAD")?,
        };
        let Some(chunk) = chunk else {
            break;
        };
        if bytes.len().saturating_add(chunk.len()) > paths::MAX_ARCHIVE_BYTES {
            return Err("MCP_INSTALL_LIMIT");
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn stage(
    m: &ManagedManifest,
    bytes: &[u8],
    parent: &Path,
    cancel: &CancellationToken,
) -> Result<(tempfile::TempDir, String), &'static str> {
    paths::check_cancel(cancel)?;
    if bytes.len() > paths::MAX_ARCHIVE_BYTES {
        return Err("MCP_INSTALL_LIMIT");
    }
    if hex::encode(Sha256::digest(bytes)) != m.archive_sha256 {
        return Err("MCP_INSTALL_INTEGRITY");
    }
    private_dir(parent)?;
    let staging = tempfile::tempdir_in(parent).map_err(|_| "MCP_INSTALL_IO")?;
    paths::extract_archive(bytes, staging.path(), cancel)?;
    if !staging.path().join(m.entry).is_file() {
        return Err("MCP_INSTALL_ARCHIVE");
    }
    let (_, hash) = paths::snapshot(&paths::directory_grant(staging.path())?, cancel)?;
    Ok((staging, hash))
}

#[cfg(target_os = "linux")]
fn publish(staging: &Path, destination: &Path) -> Result<(), &'static str> {
    use std::os::unix::ffi::OsStrExt;
    let source = std::ffi::CString::new(staging.as_os_str().as_bytes())
        .map_err(|_| "MCP_INSTALL_INVALID")?;
    let target = std::ffi::CString::new(destination.as_os_str().as_bytes())
        .map_err(|_| "MCP_INSTALL_INVALID")?;
    // SAFETY: both NUL-terminated paths live for the syscall. Never replace an
    // existing entry, even if a directory appeared after the absence check.
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            target.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else if std::io::Error::last_os_error().kind() == std::io::ErrorKind::AlreadyExists {
        Err("MCP_INSTALL_EXISTS")
    } else {
        Err("MCP_INSTALL_IO")
    }
}
#[cfg(not(target_os = "linux"))]
fn publish(_: &Path, _: &Path) -> Result<(), &'static str> {
    Err("MCP_STDIO_SANDBOX_UNAVAILABLE")
}

#[tauri::command]
pub async fn mcp_client_install_manifests(
    webview: Webview,
) -> Result<Vec<ManagedManifest>, &'static str> {
    crate::only_main_window(webview.label(), "mcp_client_install_manifests")
        .map_err(|_| "MCP_MAIN_WINDOW_REQUIRED")?;
    Ok(REVIEWED_MANIFESTS.to_vec())
}

#[tauri::command]
pub async fn mcp_client_install_server(
    webview: Webview,
    app: AppHandle,
    manifest_id: String,
    version: String,
    expected_revision: u64,
    network_consent: bool,
    operation_id: String,
) -> Result<(), &'static str> {
    crate::only_main_window(webview.label(), "mcp_client_install_server")
        .map_err(|_| "MCP_MAIN_WINDOW_REQUIRED")?;
    if !cfg!(target_os = "linux") {
        return Err("MCP_STDIO_SANDBOX_UNAVAILABLE");
    }
    let m = manifest(&manifest_id, &version)?;
    validate_manifest(m)?;
    let executable = runtime(m.runtime)?;
    if network_consent && !m.network {
        return Err("MCP_NETWORK_UNDECLARED");
    }
    let generation = INSTALL_GENERATION.load(Ordering::Acquire);
    let app_context = app.clone();
    let (_, initial_key, user) =
        tokio::task::spawn_blocking(move || crate::mcp_client_commands::context(&app_context))
            .await
            .map_err(|_| "MCP_STORE_UNAVAILABLE")??;
    let operation = register(user, &operation_id)?;
    let cancel = operation.cancel.clone();
    if INSTALL_GENERATION.load(Ordering::Acquire) != generation {
        cancel.cancel();
    }
    paths::check_cancel(&cancel)?;
    let bytes = tokio::time::timeout(std::time::Duration::from_secs(120), download(m, &cancel))
        .await
        .map_err(|_| "MCP_INSTALL_DOWNLOAD")??;
    tokio::task::spawn_blocking(move || {
        let parent = base(&app, user, m.id)?;
        let (staging, tree_hash) = stage(m, &bytes, &parent, &cancel)?;
        paths::check_cancel(&cancel)?;
        let (mut conn, key, current_user) = crate::mcp_client_commands::context(&app)?;
        if current_user != user || *key != *initial_key {
            return Err("MCP_USER_UNAVAILABLE");
        }
        let (transaction, mut configs) =
            crate::mcp_client_commands::begin_catalog_write(&mut conn, &key, user)?;
        let existing = configs.iter().find(|c| c.id == m.id);
        if existing.map_or(0, |c| c.revision) != expected_revision {
            return Err("MCP_CONFIG_STALE_REVISION");
        }
        if existing.is_some() {
            return Err("MCP_INSTALL_EXISTS");
        } // Remove/reinstall; upgrades are reviewed in MCLIENT-07.
        if crate::mcp_client_http_commands::load(&transaction, &key, user)?
            .iter()
            .any(|c| c.id == m.id)
        {
            return Err("MCP_CONFIG_DUPLICATE_ID");
        }
        let destination = parent.join(m.version);
        if destination.exists() || std::fs::symlink_metadata(&destination).is_ok() {
            return Err("MCP_INSTALL_EXISTS");
        }
        paths::check_cancel(&cancel)?;
        publish(staging.path(), &destination)?;
        let result = (|| {
            let grant = paths::directory_grant(&destination)?;
            let config = McpServerConfig {
                id: m.id.into(),
                command: executable,
                args: std::iter::once(destination.join(m.entry).to_string_lossy().into_owned())
                    .chain(m.args.iter().map(|s| (*s).into()))
                    .collect(),
                env: Default::default(),
                enabled: false,
                revision: 1,
                sandbox: McpSandboxConfig {
                    directories: vec![grant],
                    network_consent,
                    managed: Some(McpManagedInstall {
                        manifest_id: m.id.into(),
                        version: m.version.into(),
                        archive_sha256: m.archive_sha256.into(),
                        tree_sha256: tree_hash,
                        network_declared: m.network,
                    }),
                },
            };
            crate::mcp_client_commands::upsert_catalog(&mut configs, config)?;
            paths::check_cancel(&cancel)?;
            crate::mcp_client_commands::save(&transaction, &key, user, &configs)?;
            transaction.commit().map_err(|_| "MCP_STORE_UNAVAILABLE")
        })();
        if result.is_err() {
            let _ = std::fs::remove_dir_all(&destination);
        }
        result
    })
    .await
    .map_err(|_| "MCP_INSTALL_IO")?
}

#[tauri::command]
pub async fn mcp_client_install_cancel(
    webview: Webview,
    app: AppHandle,
    operation_id: String,
) -> Result<(), &'static str> {
    crate::only_main_window(webview.label(), "mcp_client_install_cancel")
        .map_err(|_| "MCP_MAIN_WINDOW_REQUIRED")?;
    tokio::task::spawn_blocking(move || {
        let (_, _, user) = crate::mcp_client_commands::context(&app)?;
        cancel_operation(user, &operation_id)
    })
    .await
    .map_err(|_| "MCP_INSTALL_IO")?
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(hash: &'static str) -> ManagedManifest {
        ManagedManifest {
            id: "fixture",
            version: "1.0.0",
            archive_url: "https://example.com/pinned.tar.gz",
            archive_sha256: hash,
            runtime: Runtime::Node,
            entry: "main.js",
            args: &[],
            network: false,
        }
    }
    #[test]
    fn only_backend_reviewed_manifests_and_public_pinned_sources_are_accepted() {
        assert!(manifest("renderer-selected", "latest").is_err());
        let valid = fixture("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        assert!(validate_manifest(&valid).is_ok());
        for changed in [
            ManagedManifest {
                version: "../../escape",
                ..valid.clone()
            },
            ManagedManifest {
                archive_sha256: "bad",
                ..valid.clone()
            },
            ManagedManifest {
                entry: "../main.js",
                ..valid.clone()
            },
            ManagedManifest {
                archive_url: "http://localhost/archive",
                ..valid.clone()
            },
        ] {
            assert!(validate_manifest(&changed).is_err());
        }
        assert!(matches!(
            serde_json::from_str::<Runtime>("\"python\"").unwrap(),
            Runtime::Python
        ));
    }
    #[test]
    fn installer_operation_is_exclusive_cancellable_and_cleans_registry() {
        let id = uuid::Uuid::new_v4().to_string();
        let operation = register(887766, &id).unwrap();
        assert!(register(887766, &uuid::Uuid::new_v4().to_string()).is_err());
        assert!(!operation.cancel.is_cancelled());
        OPERATIONS
            .lock()
            .unwrap()
            .get(&887766)
            .unwrap()
            .cancel
            .cancel();
        assert!(operation.cancel.is_cancelled());
        drop(operation);
        assert!(register(887766, &id).is_ok());
        assert!(register(887766, "invalid-operation").is_err());
    }
    #[test]
    fn cancellation_before_registration_is_scoped_and_prevents_start() {
        let id = uuid::Uuid::new_v4().to_string();
        cancel_operation(887767, &id).unwrap();
        let other_user = register(887768, &id).unwrap();
        assert!(!other_user.cancel.is_cancelled());
        assert!(matches!(
            register(887767, &id),
            Err("MCP_INSTALL_CANCELLED")
        ));
        drop(other_user);
        let id = uuid::Uuid::new_v4().to_string();
        let running = register(887767, &id).unwrap();
        cancel_operation(887767, &id).unwrap();
        assert!(running.cancel.is_cancelled());
        assert!(cancel_operation(887767, "invalid").is_err());
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn integrity_staging_publication_uninstall_reinstall_and_no_overwrite() {
        let gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut builder = tar::Builder::new(gzip);
        let script = b"require('./dependency.js')";
        for (name, bytes) in [
            ("main.js", &script[..]),
            ("dependency.js", &b"module.exports=42"[..]),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, name, bytes).unwrap();
        }
        let bytes = builder.into_inner().unwrap().finish().unwrap();
        let m = fixture(Box::leak(
            hex::encode(Sha256::digest(&bytes)).into_boxed_str(),
        ));
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancellationToken::new();
        assert_eq!(
            stage(
                &fixture("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
                &bytes,
                dir.path(),
                &cancel
            )
            .unwrap_err(),
            "MCP_INSTALL_INTEGRITY"
        );
        let (staged, hash) = stage(&m, &bytes, dir.path(), &cancel).unwrap();
        let destination = dir.path().join("1.0.0");
        publish(staged.path(), &destination).unwrap();
        assert_eq!(
            paths::snapshot(&paths::directory_grant(&destination).unwrap(), &cancel)
                .unwrap()
                .1,
            hash
        );
        let (second, _) = stage(&m, &bytes, dir.path(), &cancel).unwrap();
        assert_eq!(
            publish(second.path(), &destination),
            Err("MCP_INSTALL_EXISTS")
        );
        {
            let removal = quarantine(&destination).unwrap();
            assert!(!destination.exists());
            drop(removal);
            assert!(destination.exists());
        }
        quarantine(&destination).unwrap().commit().unwrap();
        publish(second.path(), &destination).unwrap();
        let grant = paths::directory_grant(&destination).unwrap();
        assert_eq!(
            std::fs::read(destination.join("dependency.js")).unwrap(),
            b"module.exports=42"
        );
        std::fs::remove_dir_all(&destination).unwrap();
        assert_eq!(
            paths::snapshot(&grant, &cancel).unwrap_err(),
            "MCP_DIRECTORY_CHANGED"
        );
        cancel.cancel();
        assert_eq!(
            stage(&m, &bytes, dir.path(), &cancel).unwrap_err(),
            "MCP_INSTALL_CANCELLED"
        );
        assert!(!destination.exists());
        assert!(runtime_in(Runtime::Node, &[dir.path()]).is_err());
        assert!(quarantine(&destination).unwrap().commit().is_ok());
    }
}
