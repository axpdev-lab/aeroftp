//! Filesystem utilities for AeroFile Places Sidebar
//!
//! Provides 13 Tauri commands for local filesystem navigation and management:
//! - `get_user_directories`: Standard XDG user directories (cross-platform via `dirs` crate)
//! - `list_mounted_volumes`: Mounted filesystem volumes with space info
//! - `list_subdirectories`: Subdirectories of a given path (for tree/breadcrumb)
//! - `eject_volume`: Unmount/eject a removable volume
//! - `get_file_properties`: Detailed metadata (permissions, ownership, symlinks, timestamps)
//! - `calculate_folder_size`: Recursive folder size, file count, and directory count
//! - `delete_to_trash`: Move file/directory to system trash (via `trash` crate)
//! - `list_trash_items`: List items in system trash (FreeDesktop spec / macOS)
//! - `restore_trash_item`: Restore a trash item to its original location
//! - `empty_trash`: Permanently delete all items in system trash
//! - `find_duplicate_files`: Scan directory for duplicate files (exact BLAKE3 or non-identical via shared engine)
//! - `scan_disk_usage`: Disk usage tree for treemap visualization
//! - `volumes_changed`: Fast change detection for mounted volumes (hash-based)

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use serde::Serialize;
use std::path::{Component, Path, PathBuf};
#[cfg(target_os = "linux")]
use std::sync::{LazyLock, Mutex};
// Unconditional since find_duplicate_files reports its progress on every
// platform; the volume-watcher emitters below are the linux/windows ones.
use tauri::Emitter;
#[cfg(target_os = "linux")]
use tracing::{error, info, warn};
#[cfg(not(target_os = "linux"))]
use tracing::{info, warn};

// ─── Path validation ────────────────────────────────────────────────────────

/// Validate a filesystem path. Rejects null bytes, `..` traversal, and excessive length.
pub fn validate_path(path: &str) -> Result<(), String> {
    let p = Path::new(path);
    if !p.is_absolute() {
        return Err("Path must be absolute".into());
    }
    if path.len() > 4096 {
        return Err("Path exceeds 4096 character limit".to_string());
    }
    if path.contains('\0') {
        return Err("Path contains null bytes".to_string());
    }
    for component in Path::new(path).components() {
        if matches!(component, Component::ParentDir) {
            return Err("Path traversal ('..') not allowed".to_string());
        }
    }
    Ok(())
}

// ─── Structs ────────────────────────────────────────────────────────────────

/// A standard user directory (Home, Desktop, Documents, etc.)
#[derive(Serialize, Clone)]
pub struct UserDirectory {
    pub key: String,
    pub path: String,
    pub icon: String,
}

/// Information about a mounted filesystem volume.
#[derive(Serialize, Clone)]
pub struct VolumeInfo {
    pub name: String,
    pub mount_point: String,
    pub volume_type: String,
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub fs_type: String,
    pub is_ejectable: bool,
}

/// An unmounted partition detected via lsblk (mountable by user).
#[derive(Serialize, Clone)]
pub struct UnmountedPartition {
    pub name: String,
    pub device: String,
    pub fs_type: String,
    pub size_bytes: u64,
}

/// A subdirectory entry for tree/breadcrumb navigation.
// Debug + PartialEq so the pin in `main_thread_tests` can assert that the async
// wrapper returns exactly what the blocking body computed.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct SubDirectory {
    pub name: String,
    pub path: String,
    pub has_children: bool,
}

// ─── Well-known hidden directories ──────────────────────────────────────────

/// Hidden directories that should still be shown in listings.
const WELL_KNOWN_HIDDEN: &[&str] = &[
    ".config", ".local", ".ssh", ".gnupg", ".cargo", ".rustup", ".npm", ".nvm", ".vscode", ".git",
];

// ─── Command 1: get_user_directories ────────────────────────────────────────

/// Returns standard user directories detected via the `dirs` crate.
/// Only directories that exist on disk are included.
///
/// `async` on purpose: every mapping ends in a `path.exists()`, which is a
/// `stat(2)`, and a home directory can live on an NFS/SMB/SSHFS mount whose
/// server has gone away. A sync command runs on the main thread (the GTK
/// thread on Linux), so one unreachable mount here freezes the whole window
/// for as long as the mount's own timeout, with no spinner and nothing the
/// user can tell apart from a crash. `spawn_blocking` keeps the syscalls on
/// the blocking pool.
#[tauri::command]
pub async fn get_user_directories() -> Vec<UserDirectory> {
    tokio::task::spawn_blocking(get_user_directories_blocking)
        .await
        .unwrap_or_else(|err| {
            warn!("get_user_directories blocking task failed: {err}");
            Vec::new()
        })
}

fn get_user_directories_blocking() -> Vec<UserDirectory> {
    let mappings: Vec<(Option<PathBuf>, &str, &str)> = vec![
        (dirs::home_dir(), "home", "Home"),
        (dirs::desktop_dir(), "desktop", "Monitor"),
        (dirs::document_dir(), "documents", "FileText"),
        (dirs::picture_dir(), "pictures", "Image"),
        (dirs::audio_dir(), "music", "Music"),
        (dirs::download_dir(), "downloads", "Download"),
        (dirs::video_dir(), "videos", "Video"),
    ];

    let mut dirs = Vec::new();
    for (maybe_path, key, icon) in mappings {
        if let Some(path) = maybe_path {
            if path.exists() {
                dirs.push(UserDirectory {
                    key: key.to_string(),
                    path: path.to_string_lossy().to_string(),
                    icon: icon.to_string(),
                });
            }
        }
    }
    info!("Detected {} user directories", dirs.len());
    dirs
}

// ─── Command 2: list_mounted_volumes ────────────────────────────────────────

/// Returns all mounted filesystem volumes, excluding pseudo-filesystems.
/// Uses platform-specific detection.
#[tauri::command]
pub async fn list_mounted_volumes() -> Result<Vec<VolumeInfo>, String> {
    #[cfg(target_os = "linux")]
    {
        list_mounted_volumes_linux().await
    }
    #[cfg(target_os = "macos")]
    {
        list_mounted_volumes_macos().await
    }
    #[cfg(target_os = "windows")]
    {
        list_mounted_volumes_windows().await
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        Ok(Vec::new())
    }
}

/// Pseudo-filesystem types to exclude from volume listings.
/// NOTE: `fuse.gvfsd-fuse` is here intentionally: the GVFS FUSE root mount at
/// `/run/user/<uid>/gvfs` is filtered out, and individual GVFS network shares are
/// enumerated separately by the GVFS scanning block in `list_mounted_volumes_linux()`.
/// Do NOT remove `fuse.gvfsd-fuse` without also removing the GVFS block (audit fix FS-004).
#[cfg(target_os = "linux")]
const PSEUDO_FS_TYPES: &[&str] = &[
    "proc",
    "sysfs",
    "devtmpfs",
    "tmpfs",
    "cgroup",
    "cgroup2",
    "overlay",
    "squashfs",
    "devpts",
    "securityfs",
    "pstore",
    "efivarfs",
    "bpf",
    "autofs",
    "mqueue",
    "hugetlbfs",
    "debugfs",
    "tracefs",
    "fusectl",
    "configfs",
    "ramfs",
    "rpc_pipefs",
    "nfsd",
    "fuse.portal",
    "fuse.gvfsd-fuse",
];

/// Network filesystem types.
#[cfg(target_os = "linux")]
const NETWORK_FS_TYPES: &[&str] = &["nfs", "nfs4", "cifs", "smbfs", "fuse.sshfs"];

#[cfg(target_os = "linux")]
async fn list_mounted_volumes_linux() -> Result<Vec<VolumeInfo>, String> {
    let content = std::fs::read_to_string("/proc/mounts")
        .map_err(|e| format!("Failed to read /proc/mounts: {}", e))?;

    // Pre-load volume labels from /dev/disk/by-label/
    let label_map = build_label_map();

    let mut volumes = Vec::new();

    for line in content.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 4 {
            continue;
        }

        let device = parts[0];
        let mount_point_raw = parts[1];
        let mount_point = &unescape_octal(mount_point_raw);
        let fs_type = parts[2];

        // Skip pseudo-filesystems
        if PSEUDO_FS_TYPES.contains(&fs_type) {
            continue;
        }

        // Skip system mount points
        if should_skip_mount_point(mount_point) {
            continue;
        }

        // Get disk space via statvfs(2)
        let (total_bytes, free_bytes) = get_disk_space(mount_point);

        // Determine volume type
        let volume_type = classify_volume(mount_point, fs_type);
        let is_ejectable = volume_type == "removable" || volume_type == "network";

        // Determine volume name
        let name = resolve_volume_name(device, mount_point, &label_map);

        volumes.push(VolumeInfo {
            name,
            mount_point: mount_point.to_string(),
            volume_type,
            total_bytes,
            free_bytes,
            fs_type: fs_type.to_string(),
            is_ejectable,
        });
    }

    // ── GVFS network shares (mounted via Nautilus/gio) ──
    // GVFS shares appear as subdirectories of /run/user/<uid>/gvfs/,
    // not as separate entries in /proc/mounts.
    if let Some(uid) = get_current_uid() {
        let gvfs_dir = format!("/run/user/{}/gvfs", uid);
        if let Ok(entries) = std::fs::read_dir(&gvfs_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                let mount_point = path.to_string_lossy().to_string();
                let dir_name = entry.file_name().to_string_lossy().to_string();
                let name = parse_gvfs_share_name(&dir_name);
                let (total_bytes, free_bytes) = get_disk_space(&mount_point);

                volumes.push(VolumeInfo {
                    name,
                    mount_point,
                    volume_type: "network".to_string(),
                    total_bytes,
                    free_bytes,
                    fs_type: gvfs_fs_type(&dir_name),
                    is_ejectable: true, // Can be unmounted
                });
            }
        }
    }

    info!("Detected {} mounted volumes", volumes.len());
    Ok(volumes)
}

/// Get the current user's UID (for GVFS path).
#[cfg(target_os = "linux")]
fn get_current_uid() -> Option<u32> {
    // SAFETY: getuid() is always safe and has no failure mode
    Some(unsafe { libc::getuid() })
}

/// Parse a GVFS directory name into a friendly display name.
///
/// Examples:
/// - `smb-share:server=mycloudex2ultra.local,share=ale` → `ale su mycloudex2ultra.local`
/// - `sftp:host=192.168.1.1,user=root` → `root su 192.168.1.1`
/// - `dav:host=nextcloud.example.com,ssl=true` → `nextcloud.example.com`
/// - `ftp:host=files.example.com` → `files.example.com`
#[cfg(target_os = "linux")]
fn parse_gvfs_share_name(dir_name: &str) -> String {
    // Parse key=value pairs from the part after the colon
    let params_str = dir_name.split(':').skip(1).collect::<Vec<_>>().join(":");
    let mut params = std::collections::HashMap::new();
    for pair in params_str.split(',') {
        if let Some((k, v)) = pair.split_once('=') {
            params.insert(k, v);
        }
    }

    let server = params
        .get("server")
        .or(params.get("host"))
        .copied()
        .unwrap_or("");
    let share = params.get("share").copied();
    let user = params.get("user").copied();

    // Build friendly name: "share su server" or "user su server" or just "server"
    if let Some(share) = share {
        if server.is_empty() {
            share.to_string()
        } else {
            format!("{} su {}", share, server)
        }
    } else if let Some(user) = user {
        if server.is_empty() {
            user.to_string()
        } else {
            format!("{} su {}", user, server)
        }
    } else if !server.is_empty() {
        server.to_string()
    } else {
        // Fallback: use the raw directory name
        dir_name.to_string()
    }
}

/// Determine filesystem type from GVFS directory name prefix.
#[cfg(target_os = "linux")]
fn gvfs_fs_type(dir_name: &str) -> String {
    if dir_name.starts_with("smb-share:") {
        "cifs".to_string()
    } else if dir_name.starts_with("sftp:") {
        "sftp".to_string()
    } else if dir_name.starts_with("ftp:") {
        "ftp".to_string()
    } else if dir_name.starts_with("dav:") || dir_name.starts_with("davs:") {
        "webdav".to_string()
    } else if dir_name.starts_with("nfs:") {
        "nfs".to_string()
    } else if dir_name.starts_with("afp:") {
        "afp".to_string()
    } else if dir_name.starts_with("mtp:") || dir_name.starts_with("gphoto2:") {
        // Phone / camera already mounted via gvfs: not a network share.
        // Product browsing still prefers the MTP provider session; this only
        // fixes volume classification (bonus, APPENDIX-MTP/04).
        "mtp".to_string()
    } else {
        "network".to_string()
    }
}

/// Check if a mount point should be skipped (system pseudo-paths).
#[cfg(target_os = "linux")]
fn should_skip_mount_point(mount_point: &str) -> bool {
    // Always skip /proc, /sys, /dev (except /dev/shm)
    if mount_point.starts_with("/proc") || mount_point.starts_with("/sys") {
        return true;
    }

    // Skip /dev/* except /dev/shm
    if mount_point.starts_with("/dev") && mount_point != "/dev/shm" {
        return true;
    }

    // Skip /run/* except /run/media
    if mount_point.starts_with("/run/") && !mount_point.starts_with("/run/media") {
        return true;
    }
    // Skip /run itself (exact match)
    if mount_point == "/run" {
        return true;
    }

    // Skip boot/EFI partitions (system-only, like Nautilus)
    // Covers /boot/efi (GRUB), /boot (kernel), /efi (systemd-boot)
    if mount_point == "/boot/efi" || mount_point == "/boot" || mount_point == "/efi" {
        return true;
    }

    false
}

/// Classify a volume as internal, removable, or network.
#[cfg(target_os = "linux")]
fn classify_volume(mount_point: &str, fs_type: &str) -> String {
    // Network filesystems
    if NETWORK_FS_TYPES.contains(&fs_type) {
        return "network".to_string();
    }

    // Removable media paths
    if mount_point.starts_with("/media/") || mount_point.starts_with("/run/media/") {
        return "removable".to_string();
    }

    // /mnt/ can be network or removable
    if mount_point.starts_with("/mnt/") {
        if NETWORK_FS_TYPES.contains(&fs_type) {
            return "network".to_string();
        }
        return "removable".to_string();
    }

    "internal".to_string()
}

/// Get disk space (total, free) in bytes for a mount point using `statvfs(2)`.
/// Uses the `libc` crate directly: no subprocess spawning.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn get_disk_space(mount_point: &str) -> (u64, u64) {
    // Unescape octal sequences in mount point (e.g. \040 for space)
    let unescaped = unescape_octal(mount_point);
    let c_path = match std::ffi::CString::new(unescaped.as_bytes()) {
        Ok(p) => p,
        Err(_) => return (0, 0),
    };
    // SAFETY: c_path is a valid NUL-terminated CString, stat is zero-initialized
    // and passed by mutable pointer. statvfs only writes to the provided buffer.
    unsafe {
        let mut stat: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c_path.as_ptr(), &mut stat) == 0 {
            #[allow(clippy::unnecessary_cast)]
            let total = stat.f_blocks as u64 * stat.f_frsize as u64;
            #[allow(clippy::unnecessary_cast)]
            let free = stat.f_bavail as u64 * stat.f_frsize as u64;
            (total, free)
        } else {
            (0, 0)
        }
    }
}

/// Build a map of device paths to volume labels from /dev/disk/by-label/.
#[cfg(target_os = "linux")]
fn build_label_map() -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    let label_dir = Path::new("/dev/disk/by-label");
    if let Ok(entries) = std::fs::read_dir(label_dir) {
        for entry in entries.flatten() {
            let label = entry.file_name().to_string_lossy().to_string();
            // Resolve the symlink to get the actual device path
            if let Ok(target) = std::fs::canonicalize(entry.path()) {
                let device_path = target.to_string_lossy().to_string();
                map.insert(device_path, label);
            }
        }
    }
    map
}

/// Resolve a human-readable volume name.
/// Priority: label from /dev/disk/by-label/ > mount point basename > device basename.
#[cfg(target_os = "linux")]
fn resolve_volume_name(
    device: &str,
    mount_point: &str,
    label_map: &std::collections::HashMap<String, String>,
) -> String {
    // Try to resolve device to canonical path for label lookup
    if let Ok(canonical) = std::fs::canonicalize(device) {
        let canonical_str = canonical.to_string_lossy().to_string();
        if let Some(label) = label_map.get(&canonical_str) {
            // Unescape octal sequences common in /dev/disk/by-label (e.g. \x20 for space)
            return unescape_label(label);
        }
    }

    // Fallback to mount point basename
    let basename = Path::new(mount_point)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    if basename.is_empty() || basename == "/" {
        // Root filesystem
        "System".to_string()
    } else {
        basename
    }
}

/// Unescape octal-encoded characters (e.g. `\040` → space, `\011` → tab).
/// Used for both /proc/mounts mount points and /dev/disk/by-label/ entries.
///
/// Multi-byte UTF-8 characters (e.g. "é" = `\303\251`) are handled correctly
/// by accumulating consecutive escaped bytes and decoding them as UTF-8.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn unescape_octal(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    // Buffer for accumulating consecutive octal-escaped bytes (UTF-8 sequences)
    let mut byte_buf: Vec<u8> = Vec::new();

    while i < len {
        if bytes[i] == b'\\'
            && i + 3 < len
            && bytes[i + 1].is_ascii_digit()
            && bytes[i + 1] < b'8'
            && bytes[i + 2].is_ascii_digit()
            && bytes[i + 2] < b'8'
            && bytes[i + 3].is_ascii_digit()
            && bytes[i + 3] < b'8'
        {
            // Parse 3 octal digits
            let o1 = bytes[i + 1] - b'0';
            let o2 = bytes[i + 2] - b'0';
            let o3 = bytes[i + 3] - b'0';
            let byte_val = (o1 << 6) | (o2 << 3) | o3;
            byte_buf.push(byte_val);
            i += 4;
        } else {
            // Flush accumulated byte buffer as UTF-8
            if !byte_buf.is_empty() {
                result.push_str(&String::from_utf8_lossy(&byte_buf));
                byte_buf.clear();
            }
            result.push(bytes[i] as char);
            i += 1;
        }
    }
    // Flush any remaining bytes
    if !byte_buf.is_empty() {
        result.push_str(&String::from_utf8_lossy(&byte_buf));
    }
    result
}

/// Unescape octal-encoded characters in volume labels.
/// Delegates to `unescape_octal` for backward compatibility.
#[cfg(target_os = "linux")]
fn unescape_label(label: &str) -> String {
    unescape_octal(label)
}

// ─── macOS implementation ───────────────────────────────────────────────────

#[cfg(target_os = "macos")]
async fn list_mounted_volumes_macos() -> Result<Vec<VolumeInfo>, String> {
    let mut volumes = Vec::new();

    // List /Volumes/ entries
    let volumes_dir = Path::new("/Volumes");
    if let Ok(entries) = std::fs::read_dir(volumes_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let mount_point = path.to_string_lossy().to_string();
            let name = entry.file_name().to_string_lossy().to_string();
            let (total_bytes, free_bytes) = get_disk_space(&mount_point);

            // Determine if removable (non-root volumes under /Volumes are typically removable)
            let volume_type = if name == "Macintosh HD" || mount_point == "/" {
                "internal"
            } else {
                "removable"
            };

            volumes.push(VolumeInfo {
                name,
                mount_point,
                volume_type: volume_type.to_string(),
                total_bytes,
                free_bytes,
                fs_type: "apfs".to_string(),
                is_ejectable: volume_type == "removable",
            });
        }
    }

    // Always include root if not already
    let has_root = volumes.iter().any(|v| v.mount_point == "/");
    if !has_root {
        let (total, free) = get_disk_space("/");
        volumes.insert(
            0,
            VolumeInfo {
                name: "Macintosh HD".to_string(),
                mount_point: "/".to_string(),
                volume_type: "internal".to_string(),
                total_bytes: total,
                free_bytes: free,
                fs_type: "apfs".to_string(),
                is_ejectable: false,
            },
        );
    }

    info!("Detected {} mounted volumes (macOS)", volumes.len());
    Ok(volumes)
}

// ─── Windows implementation (GAP-C02: real volume enumeration) ──────────────

#[cfg(target_os = "windows")]
async fn list_mounted_volumes_windows() -> Result<Vec<VolumeInfo>, String> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{
        GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
    };

    // Win32 drive type constants (windows-rs 0.58 returns u32, not enum)
    const DRIVE_REMOVABLE: u32 = 2;
    const DRIVE_FIXED: u32 = 3;
    const DRIVE_REMOTE: u32 = 4;
    const DRIVE_CDROM: u32 = 5;

    let drive_mask = unsafe { GetLogicalDrives() };
    if drive_mask == 0 {
        return Err("GetLogicalDrives failed".to_string());
    }

    let mut volumes = Vec::new();

    for i in 0u32..26 {
        if drive_mask & (1 << i) == 0 {
            continue;
        }

        let letter = (b'A' + i as u8) as char;
        let root: Vec<u16> = format!("{}:\\", letter)
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let root_pcwstr = PCWSTR(root.as_ptr());

        // Drive type classification
        let drive_type = unsafe { GetDriveTypeW(root_pcwstr) };
        let (volume_type, is_ejectable) = match drive_type {
            DRIVE_REMOVABLE => ("removable", true),
            DRIVE_FIXED => ("internal", false),
            DRIVE_REMOTE => ("network", false),
            DRIVE_CDROM => ("optical", true),
            _ => ("unknown", false),
        };

        // Volume name and filesystem type
        let mut vol_name_buf = [0u16; 256];
        let mut fs_name_buf = [0u16; 64];
        let vol_ok = unsafe {
            GetVolumeInformationW(
                root_pcwstr,
                Some(&mut vol_name_buf),
                None,
                None,
                None,
                Some(&mut fs_name_buf),
            )
        };

        let vol_name = if vol_ok.is_ok() {
            let len = vol_name_buf
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(vol_name_buf.len());
            OsString::from_wide(&vol_name_buf[..len])
                .to_string_lossy()
                .to_string()
        } else {
            String::new()
        };

        let fs_type = if vol_ok.is_ok() {
            let len = fs_name_buf
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(fs_name_buf.len());
            OsString::from_wide(&fs_name_buf[..len])
                .to_string_lossy()
                .to_lowercase()
        } else {
            "unknown".to_string()
        };

        // Disk space
        let mut free_bytes_available = 0u64;
        let mut total_bytes = 0u64;
        let mut _total_free = 0u64;
        let _ = unsafe {
            GetDiskFreeSpaceExW(
                root_pcwstr,
                Some(&mut free_bytes_available as *mut u64),
                Some(&mut total_bytes as *mut u64),
                Some(&mut _total_free as *mut u64),
            )
        };

        // Skip ejected or empty removable/optical slots. After a safe-remove (or an
        // always-empty card-reader / optical bay) the drive letter lingers in
        // GetLogicalDrives but has no media, so GetVolumeInformationW fails and the
        // reported size is 0. Without this, PLACES kept a ghost "Removable Disk (X:)
        // 0 B / 0 B" row after a successful eject (#351). The frontend already
        // re-enumerates after eject, so dropping the no-media slot here makes the row
        // disappear, matching Explorer's behaviour for a removed volume.
        if matches!(volume_type, "removable" | "optical") && vol_ok.is_err() && total_bytes == 0 {
            continue;
        }

        // Display name: "Volume Name (X:)" or "Local Disk (X:)"
        let display_name = if vol_name.is_empty() {
            match volume_type {
                "removable" => format!("Removable Disk ({}:)", letter),
                "network" => format!("Network Drive ({}:)", letter),
                "optical" => format!("CD/DVD Drive ({}:)", letter),
                _ => format!("Local Disk ({}:)", letter),
            }
        } else {
            format!("{} ({}:)", vol_name, letter)
        };

        volumes.push(VolumeInfo {
            name: display_name,
            mount_point: format!("{}:\\", letter),
            volume_type: volume_type.to_string(),
            total_bytes,
            free_bytes: free_bytes_available,
            fs_type,
            is_ejectable,
        });
    }

    info!("Detected {} mounted volumes (Windows)", volumes.len());
    Ok(volumes)
}

// ─── Command 3: list_subdirectories ─────────────────────────────────────────

/// Returns subdirectories of the given path, sorted alphabetically (case-insensitive).
/// Used for breadcrumb dropdowns and tree expansion in the Places sidebar.
/// Skips hidden directories unless they are well-known (e.g. `.config`, `.ssh`).
///
/// `async` is not decoration here, it is the fix for the worst instance of the
/// main-thread problem in this crate. A synchronous `#[tauri::command]` runs on
/// the main thread, which on Linux is the GTK thread, and this one:
///
///   * takes its path **straight from the frontend**, so the caller chooses
///     which mount we walk into;
///   * stats it twice (`exists`, `is_dir`), then `read_dir`s it, then stats
///     every entry, then `read_dir`s each subdirectory again for the expand
///     chevron. That is O(entries) syscalls, not one;
///   * has no timeout of its own anywhere, so on a dead NFS/SMB/SSHFS mount it
///     blocks in the kernel for however long that mount was configured to wait.
///
/// Sync, that is a frozen window with no spinner. On the blocking pool it is a
/// tree node that stays on "Loading...", and the rest of the app keeps working.
#[tauri::command]
pub async fn list_subdirectories(path: String) -> Result<Vec<SubDirectory>, String> {
    tokio::task::spawn_blocking(move || list_subdirectories_blocking(path))
        .await
        .unwrap_or_else(|err| {
            warn!("list_subdirectories blocking task failed: {err}");
            Err("Failed to read directory".to_string())
        })
}

fn list_subdirectories_blocking(path: String) -> Result<Vec<SubDirectory>, String> {
    validate_path(&path)?;

    let dir_path = Path::new(&path);
    if !dir_path.exists() {
        return Err("Path does not exist".to_string());
    }
    if !dir_path.is_dir() {
        return Err("Path is not a directory".to_string());
    }

    let entries =
        std::fs::read_dir(dir_path).map_err(|_| "Failed to read directory".to_string())?;

    let mut subdirs: Vec<SubDirectory> = Vec::new();

    for entry in entries.flatten() {
        let entry_path = entry.path();

        // Only include directories (follow symlinks)
        if !entry_path.is_dir() {
            continue;
        }

        let name = entry.file_name().to_string_lossy().to_string();

        // Skip hidden directories unless well-known
        if name.starts_with('.') && !WELL_KNOWN_HIDDEN.contains(&name.as_str()) {
            continue;
        }

        // Check if this directory has any subdirectory children (for tree expand chevron)
        let has_children = dir_has_subdirectory(&entry_path);

        subdirs.push(SubDirectory {
            name,
            path: entry_path.to_string_lossy().to_string(),
            has_children,
        });
    }

    // Sort alphabetically, case-insensitive
    subdirs.sort_by_key(|a| a.name.to_lowercase());

    Ok(subdirs)
}

/// Efficiently check if a directory contains at least one subdirectory.
/// Reads only a limited number of entries to avoid performance issues on large directories.
fn dir_has_subdirectory(path: &Path) -> bool {
    let entries = match std::fs::read_dir(path) {
        Ok(e) => e,
        Err(_) => return false, // Permission denied or other error
    };

    // Check up to 100 entries for a subdirectory
    for entry in entries.take(100).flatten() {
        if entry.path().is_dir() {
            return true;
        }
    }
    false
}

// ─── Command 4: eject_volume ────────────────────────────────────────────────

/// Ejects/unmounts a removable volume.
/// On Linux, tries `udisksctl unmount` first, then falls back to `umount`.
/// On macOS, uses `diskutil eject`.
#[tauri::command]
pub async fn eject_volume(mount_point: String) -> Result<String, String> {
    validate_path(&mount_point)?;

    let mp = Path::new(&mount_point);
    if !mp.exists() {
        return Err(format!("Mount point does not exist: {}", mount_point));
    }

    info!("Attempting to eject volume: {}", mount_point);

    #[cfg(target_os = "linux")]
    {
        eject_volume_linux(&mount_point).await
    }
    #[cfg(target_os = "macos")]
    {
        eject_volume_macos(&mount_point).await
    }
    #[cfg(target_os = "windows")]
    {
        eject_volume_windows(&mount_point).await
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        Err("Volume ejection is not supported on this platform".to_string())
    }
}

#[cfg(target_os = "linux")]
fn validate_device_path(path: &str) -> Result<(), String> {
    if !path.starts_with("/dev/") {
        return Err("Device path must start with /dev/".to_string());
    }
    // Reject shell metacharacters
    const FORBIDDEN: &[char] = &[';', '|', '&', '$', '`', '(', ')', '\n', '\r'];
    if path.contains(FORBIDDEN) {
        return Err("Device path contains forbidden characters".to_string());
    }
    // Must match /dev/[a-zA-Z0-9/_-]+
    let suffix = &path[5..]; // after "/dev/"
    if suffix.is_empty()
        || !suffix
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '/' || c == '_' || c == '-')
    {
        return Err("Device path contains invalid characters".to_string());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
async fn eject_volume_linux(mount_point: &str) -> Result<String, String> {
    // GVFS mounts (Nautilus network shares) need `gio mount -u`: not udisksctl/umount
    if mount_point.contains("/gvfs/") {
        let result = std::process::Command::new("gio")
            .args(["mount", "-u", mount_point])
            .output()
            .map_err(|e| format!("Failed to execute gio mount -u: {}", e))?;

        if result.status.success() {
            let msg = format!("GVFS share unmounted successfully: {}", mount_point);
            info!("{}", msg);
            return Ok(msg);
        }
        let stderr = String::from_utf8_lossy(&result.stderr);
        return Err(format!(
            "Failed to unmount GVFS share {}: {}",
            mount_point,
            stderr.trim()
        ));
    }

    // First, try to find the device for this mount point from /proc/mounts
    let device = find_device_for_mount(mount_point);

    // Try udisksctl first (user-level, no sudo needed)
    if let Some(ref dev) = device {
        // AF-RUST-C01: Validate device path before passing to subprocess
        validate_device_path(dev)?;

        let result = std::process::Command::new("udisksctl")
            .args(["unmount", "-b", dev])
            .output();

        if let Ok(output) = result {
            if output.status.success() {
                let msg = format!(
                    "Volume unmounted successfully via udisksctl: {}",
                    mount_point
                );
                info!("{}", msg);
                return Ok(msg);
            }
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!("udisksctl unmount failed: {}", stderr);
        }
    }

    // Fallback to umount
    let result = std::process::Command::new("umount")
        .arg(mount_point)
        .output()
        .map_err(|e| format!("Failed to execute umount: {}", e))?;

    if result.status.success() {
        let msg = format!("Volume unmounted successfully: {}", mount_point);
        info!("{}", msg);
        return Ok(msg);
    }

    // For FUSE-based network mounts (sshfs, rclone, etc.), try fusermount
    for cmd in &["fusermount3", "fusermount"] {
        if let Ok(output) = std::process::Command::new(cmd)
            .args(["-u", mount_point])
            .output()
        {
            if output.status.success() {
                let msg = format!("Volume unmounted successfully via {}: {}", cmd, mount_point);
                info!("{}", msg);
                return Ok(msg);
            }
        }
    }

    let stderr = String::from_utf8_lossy(&result.stderr);
    Err(format!(
        "Failed to unmount {}: {}",
        mount_point,
        stderr.trim()
    ))
}

/// Find the device path associated with a mount point by reading /proc/mounts.
#[cfg(target_os = "linux")]
fn find_device_for_mount(mount_point: &str) -> Option<String> {
    let content = std::fs::read_to_string("/proc/mounts").ok()?;
    for line in content.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 2 && unescape_octal(parts[1]) == mount_point {
            return Some(parts[0].to_string());
        }
    }
    None
}

#[cfg(target_os = "macos")]
async fn eject_volume_macos(mount_point: &str) -> Result<String, String> {
    let result = std::process::Command::new("diskutil")
        .args(["eject", mount_point])
        .output()
        .map_err(|e| format!("Failed to execute diskutil: {}", e))?;

    if result.status.success() {
        let msg = format!("Volume ejected successfully: {}", mount_point);
        info!("{}", msg);
        Ok(msg)
    } else {
        let stderr = String::from_utf8_lossy(&result.stderr);
        Err(format!(
            "Failed to eject {}: {}",
            mount_point,
            stderr.trim()
        ))
    }
}

/// Eject a removable volume on Windows.
///
/// Mirrors the tray "Safely Remove Hardware and Eject Media" behaviour by
/// invoking the Shell.Application COM "Eject" verb on the drive, via a
/// PowerShell child process (same shell-out strategy as the Linux/macOS arms,
/// no extra crate dependency).
///
/// `mount_point` arrives as a drive root such as `E:\`; we derive the bare
/// drive letter `E:` and validate it strictly (`^[A-Za-z]:$`) before
/// interpolating it into the command, mirroring `validate_device_path`.
#[cfg(target_os = "windows")]
async fn eject_volume_windows(mount_point: &str) -> Result<String, String> {
    // Derive the bare drive letter, e.g. "E:\\" or "E:/" or "E:" -> "E:".
    let trimmed = mount_point.trim();
    let drive = trimmed.trim_end_matches(|c| c == '\\' || c == '/');

    // Strict validation: exactly one ASCII letter followed by a colon.
    let bytes = drive.as_bytes();
    let valid = bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    if !valid {
        return Err(format!("Invalid Windows drive letter: {}", mount_point));
    }

    // Namespace(17) = ssfDRIVES (This PC); ParseName(<drive>) selects the drive;
    // InvokeVerb('Eject') performs the safe removal.
    //
    // InvokeVerb('Eject') is fire-and-forget: the shell completes the eject
    // asynchronously and the COM object must stay alive until it finishes. A
    // process that only calls InvokeVerb and exits immediately tears down the COM
    // object before the eject completes, so the drive is never removed yet the
    // verb still returns success (silent false positive). We therefore poll until
    // the drive disappears and map the outcome to a meaningful exit code
    // (0 = ejected, 1 = still present) so a genuine failure surfaces as Err and
    // raises the failure toast instead of a fake success.
    let script = format!(
        "$sh = New-Object -comObject Shell.Application; \
         $sh.Namespace(17).ParseName('{drive}').InvokeVerb('Eject'); \
         $deadline = (Get-Date).AddSeconds(10); \
         while ((Test-Path '{drive}') -and (Get-Date) -lt $deadline) {{ Start-Sleep -Milliseconds 200 }}; \
         if (Test-Path '{drive}') {{ exit 1 }} else {{ exit 0 }}",
        drive = drive
    );

    // #351: hidden_command suppresses the console window flash powershell would
    // otherwise pop up when spawned from the GUI process.
    let result = crate::hidden_command("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &script,
        ])
        .output()
        .map_err(|e| format!("Failed to execute powershell: {}", e))?;

    if result.status.success() {
        let msg = format!("Volume ejected successfully: {}", drive);
        info!("{}", msg);
        Ok(msg)
    } else {
        let stderr = String::from_utf8_lossy(&result.stderr);
        let detail = stderr.trim();
        if detail.is_empty() {
            // exit 1 from the poll: the drive is still present after the timeout,
            // typically because a file or app is still holding it open.
            Err(format!(
                "Failed to eject {}: the drive is still in use",
                drive
            ))
        } else {
            Err(format!("Failed to eject {}: {}", drive, detail))
        }
    }
}

// ─── Unmounted Partition Detection ────────────────────────────────────────────

/// List unmounted partitions via `lsblk -J -b` (Linux only).
/// Shows partitions that exist on disk but are not currently mounted,
/// allowing users to mount them from the sidebar (like Nautilus).
#[cfg(target_os = "linux")]
#[tauri::command]
pub async fn list_unmounted_partitions() -> Result<Vec<UnmountedPartition>, String> {
    let output = std::process::Command::new("lsblk")
        .args(["-J", "-b", "-o", "NAME,SIZE,TYPE,FSTYPE,LABEL,MOUNTPOINT"])
        .output()
        .map_err(|e| format!("Failed to run lsblk: {}", e))?;

    if !output.status.success() {
        return Err("lsblk returned non-zero exit code".to_string());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).map_err(|e| format!("Failed to parse lsblk JSON: {}", e))?;

    let mut partitions = Vec::new();

    let devices = parsed["blockdevices"]
        .as_array()
        .ok_or("Invalid lsblk output: missing blockdevices")?;

    // Collect partitions from top-level and children
    let mut all_parts: Vec<&serde_json::Value> = Vec::new();
    for dev in devices {
        let dev_type = dev["type"].as_str().unwrap_or("");
        if dev_type == "part" {
            all_parts.push(dev);
        }
        if let Some(children) = dev["children"].as_array() {
            for child in children {
                if child["type"].as_str().unwrap_or("") == "part" {
                    all_parts.push(child);
                }
            }
        }
    }

    for part in all_parts {
        // Skip if already mounted
        if !part["mountpoint"].is_null() && !part["mountpoint"].as_str().unwrap_or("").is_empty() {
            continue;
        }

        let fs_type = match part["fstype"].as_str() {
            Some(fs) if !fs.is_empty() => fs,
            _ => continue, // Skip partitions with no filesystem (MSR, unformatted)
        };

        // Some lsblk versions return size as string, not number (audit fix FS-002)
        let size = part["size"]
            .as_u64()
            .or_else(|| part["size"].as_str().and_then(|s| s.parse::<u64>().ok()))
            .unwrap_or(0);
        let label = part["label"].as_str().unwrap_or("");
        let name_raw = part["name"].as_str().unwrap_or("");

        // Skip swap partitions
        if fs_type == "swap" || fs_type == "linux-swap" {
            continue;
        }

        // Skip small vfat partitions (EFI/boot, typically < 1GB)
        if fs_type == "vfat" && size < 1_073_741_824 {
            continue;
        }

        // Skip recovery partitions
        let label_lower = label.to_lowercase();
        if label_lower.contains("recovery")
            || label_lower.contains("windows re")
            || label_lower.contains("winre")
        {
            continue;
        }

        let display_name = if !label.is_empty() {
            label.to_string()
        } else {
            name_raw.to_string()
        };

        partitions.push(UnmountedPartition {
            name: display_name,
            device: format!("/dev/{}", name_raw),
            fs_type: fs_type.to_string(),
            size_bytes: size,
        });
    }

    info!("Detected {} unmounted partitions", partitions.len());
    Ok(partitions)
}

#[cfg(not(target_os = "linux"))]
#[tauri::command]
pub async fn list_unmounted_partitions() -> Result<Vec<UnmountedPartition>, String> {
    Ok(Vec::new())
}

/// Mount an unmounted partition via `udisksctl` (unprivileged, PolicyKit).
/// Returns the mount point path on success (e.g. "/media/user/Windows").
#[cfg(target_os = "linux")]
#[tauri::command]
pub async fn mount_partition(device: String) -> Result<String, String> {
    validate_device_path(&device)?;

    let output = std::process::Command::new("udisksctl")
        .args(["mount", "-b", &device, "--no-user-interaction"])
        .output()
        .map_err(|e| format!("Failed to run udisksctl: {}", e))?;

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        // Parse "Mounted /dev/nvme0n1p3 at /media/axpdev/Windows."
        if let Some(at_pos) = stdout.find(" at ") {
            let mount_point = stdout[at_pos + 4..].trim().trim_end_matches('.');
            info!("Mounted {} at {}", device, mount_point);
            return Ok(mount_point.to_string());
        }
        info!("Mounted {}: {}", device, stdout.trim());
        Ok(stdout.trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("Failed to mount {}: {}", device, stderr.trim()))
    }
}

#[cfg(not(target_os = "linux"))]
#[tauri::command]
pub async fn mount_partition(device: String) -> Result<String, String> {
    Err(format!(
        "Mounting partitions is not supported on this platform (device: {})",
        device
    ))
}

// ─── Structs (AeroFile Phase B+C) ───────────────────────────────────────────

/// Detailed file/directory properties including permissions, ownership, and symlink info.
#[derive(Serialize, Clone)]
pub struct DetailedFileProperties {
    pub name: String,
    pub path: String,
    pub size: u64,
    pub is_dir: bool,
    pub created: Option<String>,
    pub modified: Option<String>,
    pub accessed: Option<String>,
    pub permissions_mode: Option<u32>,
    pub permissions_text: Option<String>,
    pub owner: Option<String>,
    pub group: Option<String>,
    pub is_symlink: bool,
    pub link_target: Option<String>,
    pub inode: Option<u64>,
    pub hard_links: Option<u64>,
    /// Read-only flag. On Windows this is the FILE_ATTRIBUTE_READONLY bit; on
    /// Unix it is derived from the owner write bit being clear.
    pub is_readonly: Option<bool>,
    /// Hidden flag. On Windows this is the FILE_ATTRIBUTE_HIDDEN bit; on Unix
    /// it is the dotfile convention (name starts with `.`, excluding `.` and `..`).
    pub is_hidden: Option<bool>,
}

/// Result of a recursive folder size calculation.
#[derive(Serialize, Clone)]
pub struct FolderSizeResult {
    pub total_bytes: u64,
    pub file_count: u64,
    pub dir_count: u64,
}

/// An item in the system trash/recycle bin.
#[derive(Serialize, Clone)]
pub struct TrashItem {
    pub id: String,
    pub name: String,
    pub original_path: String,
    pub deleted_at: Option<String>,
    pub size: u64,
    pub is_dir: bool,
}

// ─── Command 5: get_file_properties ─────────────────────────────────────────

/// Returns detailed metadata for a file or directory, including permissions,
/// ownership, timestamps, symlink target, inode, and hard link count.
#[tauri::command]
pub async fn get_file_properties(path: String) -> Result<DetailedFileProperties, String> {
    validate_path(&path)?;

    let metadata = tokio::fs::symlink_metadata(&path)
        .await
        .map_err(|_| "Failed to read file metadata".to_string())?;

    let name = Path::new(&path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    let to_iso = |time: std::io::Result<std::time::SystemTime>| -> Option<String> {
        time.ok().and_then(|t| {
            t.duration_since(std::time::UNIX_EPOCH).ok().and_then(|d| {
                let secs = d.as_secs() as i64;
                chrono::DateTime::from_timestamp(secs, 0)
                    .map(|dt| dt.format("%Y-%m-%dT%H:%M:%S").to_string())
            })
        })
    };

    let is_symlink = metadata.file_type().is_symlink();
    let link_target = if is_symlink {
        tokio::fs::read_link(&path)
            .await
            .ok()
            .map(|p| p.to_string_lossy().to_string())
    } else {
        None
    };

    // For symlinks, also read the target metadata for size/is_dir
    let effective_metadata = if is_symlink {
        tokio::fs::metadata(&path).await.unwrap_or(metadata.clone())
    } else {
        metadata.clone()
    };

    #[cfg(unix)]
    let (permissions_mode, permissions_text, owner, group, inode, hard_links) = {
        use std::os::unix::fs::MetadataExt;
        let mode = metadata.mode();
        let file_mode = mode & 0o7777;

        let to_rwx = |n: u32| -> String {
            format!(
                "{}{}{}",
                if n & 4 != 0 { "r" } else { "-" },
                if n & 2 != 0 { "w" } else { "-" },
                if n & 1 != 0 { "x" } else { "-" }
            )
        };

        let prefix = if metadata.is_dir() {
            "d"
        } else if is_symlink {
            "l"
        } else {
            "-"
        };
        let text = format!(
            "{}{}{}{}",
            prefix,
            to_rwx((file_mode >> 6) & 7),
            to_rwx((file_mode >> 3) & 7),
            to_rwx(file_mode & 7)
        );

        let uid = metadata.uid();
        let gid = metadata.gid();
        let owner_name = format!("{}", uid);
        let group_name = format!("{}", gid);

        (
            Some(file_mode),
            Some(text),
            Some(owner_name),
            Some(group_name),
            Some(metadata.ino()),
            Some(metadata.nlink()),
        )
    };

    #[cfg(not(unix))]
    let (permissions_mode, permissions_text, owner, group, inode, hard_links) =
        { (None, None, None, None, None, None) };

    // ── Read-only + hidden flags (cross-platform) ──────────────────────────
    #[cfg(unix)]
    let (is_readonly, is_hidden) = {
        use std::os::unix::fs::MetadataExt;
        let mode = metadata.mode();
        // Owner write bit clear → read-only for the current user. Group/other
        // bits are not considered because uid is what the running app sees.
        let ro = mode & 0o200 == 0;
        let hidden = name.starts_with('.') && name != "." && name != "..";
        (Some(ro), Some(hidden))
    };

    #[cfg(windows)]
    let (is_readonly, is_hidden) = {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_READONLY: u32 = 0x0000_0001;
        const FILE_ATTRIBUTE_HIDDEN: u32 = 0x0000_0002;
        let attrs = metadata.file_attributes();
        (
            Some(attrs & FILE_ATTRIBUTE_READONLY != 0),
            Some(attrs & FILE_ATTRIBUTE_HIDDEN != 0),
        )
    };

    #[cfg(not(any(unix, windows)))]
    let (is_readonly, is_hidden) = (None, None);

    Ok(DetailedFileProperties {
        name,
        path: path.clone(),
        size: effective_metadata.len(),
        is_dir: effective_metadata.is_dir(),
        created: to_iso(metadata.created()),
        modified: to_iso(metadata.modified()),
        accessed: to_iso(metadata.accessed()),
        permissions_mode,
        permissions_text,
        owner,
        group,
        is_symlink,
        link_target,
        inode,
        hard_links,
        is_readonly,
        is_hidden,
    })
}

// ─── Command 6: calculate_folder_size ───────────────────────────────────────

/// Recursively calculates the total size, file count, and subdirectory count of a folder.
#[tauri::command]
pub async fn calculate_folder_size(path: String) -> Result<FolderSizeResult, String> {
    validate_path(&path)?;

    let path_ref = Path::new(&path);
    if !path_ref.is_dir() {
        return Err("Path is not a directory".to_string());
    }

    let mut total_bytes: u64 = 0;
    let mut file_count: u64 = 0;
    let mut dir_count: u64 = 0;
    // AF-RUST-H02: Resource exhaustion limits
    const MAX_ENTRIES: u64 = 1_000_000;
    let mut entry_count: u64 = 0;

    for entry in walkdir::WalkDir::new(&path)
        .follow_links(false)
        .max_depth(100)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        entry_count += 1;
        if entry_count > MAX_ENTRIES {
            warn!(
                "calculate_folder_size: entry limit ({}) reached, returning partial result",
                MAX_ENTRIES
            );
            break;
        }
        if entry.file_type().is_file() {
            total_bytes += entry.metadata().map(|m| m.len()).unwrap_or(0);
            file_count += 1;
        } else if entry.file_type().is_dir() && entry.path() != path_ref {
            dir_count += 1;
        }
    }

    Ok(FolderSizeResult {
        total_bytes,
        file_count,
        dir_count,
    })
}

// ─── Command 7: delete_to_trash ─────────────────────────────────────────────

/// Moves a file or directory to the system trash/recycle bin.
#[tauri::command]
pub async fn delete_to_trash(path: String) -> Result<(), String> {
    validate_path(&path)?;

    trash::delete(&path).map_err(|e| format!("Failed to move to trash: {}", e))
}

// ─── Command 8: list_trash_items ────────────────────────────────────────────

/// Lists items currently in the system trash (up to 200, sorted by deletion time).
/// GAP-C01: Uses `trash` crate for cross-platform support (Linux + Windows).
/// macOS uses manual ~/.Trash scanning (crate doesn't support list on macOS).
#[tauri::command]
pub async fn list_trash_items() -> Result<Vec<TrashItem>, String> {
    // Linux and Windows: use trash crate's os_limited API
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    {
        let crate_items =
            trash::os_limited::list().map_err(|e| format!("Failed to list trash: {}", e))?;

        let mut items: Vec<TrashItem> = crate_items
            .iter()
            .map(|item| {
                let name = item.name.to_string_lossy().to_string();
                let original_path = item.original_path().to_string_lossy().to_string();
                let deleted_at = chrono::DateTime::from_timestamp(item.time_deleted, 0)
                    .map(|dt| dt.format("%Y-%m-%dT%H:%M:%S").to_string());

                // Get size and is_dir via metadata (best-effort)
                let (size, is_dir) = trash::os_limited::metadata(item)
                    .map(|m| match m.size {
                        trash::TrashItemSize::Bytes(b) => (b, false),
                        trash::TrashItemSize::Entries(_) => (0, true),
                    })
                    .unwrap_or((0, false));

                TrashItem {
                    id: item.id.to_string_lossy().to_string(),
                    name,
                    original_path,
                    deleted_at,
                    size,
                    is_dir,
                }
            })
            .collect();

        items.sort_by(|a, b| b.deleted_at.cmp(&a.deleted_at));
        items.truncate(200);
        Ok(items)
    }

    // macOS: manual ~/.Trash scanning (trash crate doesn't support list on macOS)
    #[cfg(target_os = "macos")]
    {
        let home = dirs::home_dir().ok_or("Cannot find home directory")?;
        let trash_dir = home.join(".Trash");

        if !trash_dir.exists() {
            return Ok(Vec::new());
        }

        let mut items = Vec::new();

        if let Ok(entries) = std::fs::read_dir(&trash_dir) {
            for entry in entries.filter_map(|e| e.ok()) {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with('.') {
                    continue;
                }

                let (size, is_dir, deleted_at) = match entry.metadata() {
                    Ok(m) => {
                        let deleted = m.modified().ok().and_then(|t| {
                            t.duration_since(std::time::UNIX_EPOCH).ok().and_then(|d| {
                                chrono::DateTime::from_timestamp(d.as_secs() as i64, 0)
                                    .map(|dt| dt.format("%Y-%m-%dT%H:%M:%S").to_string())
                            })
                        });
                        (m.len(), m.is_dir(), deleted)
                    }
                    Err(_) => (0, false, None),
                };

                items.push(TrashItem {
                    id: name.clone(),
                    name: name.clone(),
                    original_path: String::new(),
                    deleted_at,
                    size,
                    is_dir,
                });
            }
        }

        items.sort_by(|a, b| b.deleted_at.cmp(&a.deleted_at));
        items.truncate(200);
        Ok(items)
    }
}

// ─── Command 9: restore_trash_item ──────────────────────────────────────────

/// Restores a previously deleted item from the trash back to its original location.
/// GAP-C01: Cross-platform via `trash` crate on Linux + Windows.
#[tauri::command]
pub async fn restore_trash_item(id: String, original_path: String) -> Result<(), String> {
    // Linux and Windows: use trash crate's os_limited API
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    {
        let _ = &original_path; // used for validation below

        // Sanitize trash item id
        if id.contains("..") || id.contains('\0') {
            return Err("Invalid trash item ID".to_string());
        }

        // Find the matching item in trash by id
        let all_items =
            trash::os_limited::list().map_err(|e| format!("Failed to list trash: {}", e))?;

        let target = all_items
            .into_iter()
            .find(|item| item.id.to_string_lossy() == id)
            .ok_or_else(|| "Item not found in trash".to_string())?;

        // Ensure parent directory exists before restore
        let restore_path = target.original_path();
        if let Some(parent) = restore_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| format!("Failed to create parent directory: {}", e))?;
        }

        trash::os_limited::restore_all([target])
            .map_err(|e| format!("Failed to restore from trash: {}", e))?;

        Ok(())
    }

    #[cfg(target_os = "macos")]
    {
        let _ = (&id, &original_path);
        Err("Restore from trash is not yet supported on macOS".to_string())
    }
}

// ─── Command 10: empty_trash ────────────────────────────────────────────────

/// Permanently deletes all items in the system trash. Returns the number of items removed.
/// GAP-C01: Cross-platform via `trash` crate on Linux + Windows.
#[tauri::command]
pub async fn empty_trash() -> Result<u64, String> {
    // Linux and Windows: use trash crate's os_limited API
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    {
        let items =
            trash::os_limited::list().map_err(|e| format!("Failed to list trash: {}", e))?;

        let count = items.len() as u64;

        if !items.is_empty() {
            trash::os_limited::purge_all(items)
                .map_err(|e| format!("Failed to empty trash: {}", e))?;
        }

        Ok(count)
    }

    // macOS: manual ~/.Trash clearing (trash crate doesn't support purge on macOS)
    #[cfg(target_os = "macos")]
    {
        let home = dirs::home_dir().ok_or("Cannot find home directory")?;
        let trash_dir = home.join(".Trash");
        let mut count: u64 = 0;

        if trash_dir.exists() {
            if let Ok(entries) = std::fs::read_dir(&trash_dir) {
                for entry in entries.filter_map(|e| e.ok()) {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if name.starts_with('.') {
                        continue;
                    }
                    let path = entry.path();
                    if path.is_dir() {
                        let _ = std::fs::remove_dir_all(&path);
                    } else {
                        let _ = std::fs::remove_file(&path);
                    }
                    count += 1;
                }
            }
        }

        Ok(count)
    }
}

// ─── Structs (Duplicate Finder) ─────────────────────────────────────────────

/// A group of duplicate files.
/// For exact mode: hash is the content hash.
/// For non-identical mode: hash may be a representative id (e.g. first file), similarity and distance carry the cluster info.
#[derive(Serialize, Clone)]
pub struct DuplicateGroup {
    pub hash: String,
    pub size: u64,
    pub files: Vec<String>,
    /// "perceptual" / "text" / etc when non-identical; None for exact
    #[serde(skip_serializing_if = "Option::is_none")]
    pub similarity: Option<String>,
    /// Hamming distance of the cluster (representative); None for exact
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distance: Option<u32>,
    /// Per-file fuzzy signature, parallel to `files`. None in exact mode, where
    /// `hash` is the one BLAKE3 every member shares.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_hashes: Option<Vec<String>>,
}

// ─── Command 11: find_duplicate_files ───────────────────────────────────────

/// What a duplicate scan has got through so far, emitted on
/// `duplicate-scan-progress`. A scan of a real photo library takes long enough
/// that a bare spinner leaves the user unable to tell work from a hang
/// (discussion #347), so the dialog shows these counters and its own clock.
///
/// `phase` is "walk" while the tree is being enumerated and "analyze" while
/// file contents are read and hashed; `files_total` is 0 during the walk, when
/// the total is not yet knowable.
#[derive(Serialize, Clone)]
pub struct DuplicateScanProgress {
    pub phase: String,
    pub files_scanned: u64,
    pub dirs_scanned: u64,
    pub bytes_scanned: u64,
    pub max_depth: u32,
    pub files_processed: u64,
    pub files_total: u64,
    pub current_path: String,
}

/// Emit at most this often. A tick per file would flood the event bridge on a
/// large tree and slow the scan it is reporting on.
const DUPLICATE_PROGRESS_INTERVAL_MS: u128 = 120;

/// Scans a directory recursively for duplicate files.
/// - mode "exact" (default): byte-identical using BLAKE3 (original behavior).
/// - mode "non-identical": delegates to shared dedupe engine (perceptual for images, simhash for text/SVG).
/// Returns groups sorted by wasted space descending (for non-identical, sizes may vary within group).
#[tauri::command]
pub async fn find_duplicate_files(
    app: tauri::AppHandle,
    path: String,
    min_size: Option<u64>,
    mode: Option<String>,
    // Fuzzy-hash cutoff for non-identical mode: how far apart two signatures may
    // be and still be called duplicates. None keeps the engine defaults (raster
    // <=10, text <=3, other <=100). Exposed because the right threshold is a
    // judgement about the user's own library, not a constant (discussion #347).
    distance: Option<u32>,
) -> Result<Vec<DuplicateGroup>, String> {
    validate_path(&path)?;

    let path_ref = Path::new(&path);
    if !path_ref.is_dir() {
        return Err("Path is not a directory".to_string());
    }

    let min = min_size.unwrap_or(1);

    let is_non_identical = mode
        .as_deref()
        .map(|m| m.eq_ignore_ascii_case("non-identical") || m.eq_ignore_ascii_case("nonidentical"))
        .unwrap_or(false);

    // AF-RUST-H03: Resource exhaustion limits
    const MAX_FILE_COUNT: u64 = 100_000;
    const MAX_TOTAL_HASH_BYTES: u64 = 2_000_000_000; // 2 GB max total hash I/O

    // Phase 0: one walk for both modes, so the progress the user sees is the
    // same shape whichever mode they picked. Non-identical used to let the
    // engine do its own walk (find_similar_in_dir); it now gets the file list
    // from here and reports its own analysis pass through the callback below.
    let walk_started = std::time::Instant::now();
    let mut last_emit = walk_started;
    let mut walked: Vec<(PathBuf, u64)> = Vec::new();
    let mut dirs_scanned: u64 = 0;
    let mut bytes_scanned: u64 = 0;
    let mut max_depth: u32 = 0;
    let mut scan_count: u64 = 0;

    for entry in walkdir::WalkDir::new(&path)
        .follow_links(false)
        .max_depth(100)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        max_depth = max_depth.max(entry.depth() as u32);
        if entry.file_type().is_dir() {
            dirs_scanned += 1;
            continue;
        }
        if !entry.file_type().is_file() {
            continue;
        }
        scan_count += 1;
        if scan_count > MAX_FILE_COUNT {
            warn!(
                "find_duplicate_files: file limit ({}) reached, scanning partial results",
                MAX_FILE_COUNT
            );
            break;
        }
        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        bytes_scanned += size;
        if last_emit.elapsed().as_millis() >= DUPLICATE_PROGRESS_INTERVAL_MS {
            last_emit = std::time::Instant::now();
            let _ = app.emit(
                "duplicate-scan-progress",
                DuplicateScanProgress {
                    phase: "walk".to_string(),
                    files_scanned: scan_count,
                    dirs_scanned,
                    bytes_scanned,
                    max_depth,
                    files_processed: 0,
                    files_total: 0,
                    current_path: entry.path().to_string_lossy().to_string(),
                },
            );
        }
        walked.push((entry.into_path(), size));
    }

    let files_scanned = scan_count.min(MAX_FILE_COUNT);
    // The analysis pass reports against a known total, so the dialog can show a
    // real fraction instead of a number that only goes up.
    let mut emit_analysis = {
        let app = app.clone();
        let mut last = std::time::Instant::now();
        move |files_processed: u64, files_total: u64, force: bool| {
            if !force && last.elapsed().as_millis() < DUPLICATE_PROGRESS_INTERVAL_MS {
                return;
            }
            last = std::time::Instant::now();
            let _ = app.emit(
                "duplicate-scan-progress",
                DuplicateScanProgress {
                    phase: "analyze".to_string(),
                    files_scanned,
                    dirs_scanned,
                    bytes_scanned,
                    max_depth,
                    files_processed,
                    files_total,
                    current_path: String::new(),
                },
            );
        }
    };

    if is_non_identical {
        // The perceptual/SimHash logic stays in the shared engine
        // (src-tauri/src/dedupe/) — Phase 1 source of truth. Only the walk moved
        // out, so this call cannot drift from the exact-mode file list.
        let paths: Vec<PathBuf> = walked.into_iter().map(|(p, _)| p).collect();
        let paths_total = paths.len() as u64;
        let engine_groups = crate::dedupe::find_similar_local_with_progress(
            &paths,
            crate::dedupe::SimilarityMode::NonIdentical,
            distance, // None keeps the engine defaults: raster <=10, text <=3
            min_size,
            &mut |p| emit_analysis(p.files_processed, p.files_total, false),
        );
        // The throttle can swallow the last tick, leaving the bar short of the
        // total for the rest of the dialog's life. Force one at the end.
        emit_analysis(paths_total, paths_total, true);
        let result: Vec<DuplicateGroup> = engine_groups
            .into_iter()
            .map(|g| DuplicateGroup {
                // Use first file as stable identifier for React key (no single content hash in non-id mode).
                hash: g.files.first().cloned().unwrap_or_default(),
                size: g.size,
                files: g.files,
                similarity: g.modality,
                distance: g.distance,
                file_hashes: g.file_hashes,
            })
            .collect();
        return Ok(result);
    }

    // Phase 1: group files by size
    let mut size_groups: std::collections::HashMap<u64, Vec<PathBuf>> =
        std::collections::HashMap::new();
    for (file_path, size) in walked {
        if size < min {
            continue;
        }
        size_groups.entry(size).or_default().push(file_path);
    }

    // Phase 2: hash only files with matching sizes (2+ files)
    let mut hash_groups: std::collections::HashMap<String, (u64, Vec<String>)> =
        std::collections::HashMap::new();
    let mut total_hash_bytes: u64 = 0;
    let mut budget_exceeded = false;
    // Only the files that share a size are ever hashed, so that — not the whole
    // tree — is the denominator the user should be watching.
    let hash_total: u64 = size_groups
        .values()
        .filter(|files| files.len() >= 2)
        .map(|files| files.len() as u64)
        .sum();
    let mut hashed: u64 = 0;

    for (size, files) in size_groups {
        if budget_exceeded {
            break;
        }
        if files.len() < 2 {
            continue;
        }

        for file_path in &files {
            // Check total hash I/O budget
            total_hash_bytes += size;
            if total_hash_bytes > MAX_TOTAL_HASH_BYTES {
                warn!(
                    "find_duplicate_files: total hash I/O limit ({} bytes) reached",
                    MAX_TOTAL_HASH_BYTES
                );
                budget_exceeded = true;
                break;
            }
            hashed += 1;
            emit_analysis(hashed, hash_total, false);
            match compute_file_hash(file_path) {
                Ok(hash) => {
                    let entry = hash_groups
                        .entry(hash)
                        .or_insert_with(|| (size, Vec::new()));
                    entry.1.push(file_path.to_string_lossy().to_string());
                }
                Err(_) => continue,
            }
        }
    }

    emit_analysis(hashed, hash_total, true);

    // Phase 3: collect groups with 2+ files, sort by wasted space
    let mut result: Vec<DuplicateGroup> = hash_groups
        .into_iter()
        .filter(|(_, (_, files))| files.len() >= 2)
        .map(|(hash, (size, files))| DuplicateGroup {
            hash,
            size,
            files,
            similarity: None,
            distance: None,
            file_hashes: None,
        })
        .collect();

    result.sort_by(|a, b| {
        let wasted_a = a.size * (a.files.len() as u64 - 1);
        let wasted_b = b.size * (b.files.len() as u64 - 1);
        wasted_b.cmp(&wasted_a)
    });

    Ok(result)
}

/// Compute BLAKE3 hash of a file using buffered reading (A4-07: replaced MD5).
fn compute_file_hash(path: &Path) -> Result<String, std::io::Error> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0u8; 65536];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

// ─── Structs (Disk Usage) ───────────────────────────────────────────────────

/// A node in the disk usage tree representing a file or directory.
#[derive(Serialize, Clone)]
pub struct DiskUsageNode {
    pub name: String,
    pub path: String,
    pub size: u64,
    pub is_dir: bool,
    pub children: Option<Vec<DiskUsageNode>>,
}

// ─── Command 12: scan_disk_usage ────────────────────────────────────────────

/// Scans a directory and returns a tree structure with sizes for treemap visualization.
/// max_depth limits recursion (default 3). Children sorted by size descending, capped at 50 per level.
#[tauri::command]
pub async fn scan_disk_usage(
    path: String,
    max_depth: Option<u32>,
    max_entries: Option<u32>,
    max_duration_secs: Option<u64>,
) -> Result<DiskUsageNode, String> {
    validate_path(&path)?;

    let path_ref = Path::new(&path);
    if !path_ref.is_dir() {
        return Err("Path is not a directory".to_string());
    }

    // AF-RUST-H04: Cap max_depth to prevent unbounded recursion
    let depth = max_depth.unwrap_or(3).min(50);
    let start = std::time::Instant::now();
    let max_dur = std::time::Duration::from_secs(max_duration_secs.unwrap_or(30));
    let max_ent = max_entries.unwrap_or(10000) as usize;
    let entry_count = std::sync::atomic::AtomicUsize::new(0);
    let node = build_usage_tree(path_ref, depth, &start, &max_dur, max_ent, &entry_count)?;
    Ok(node)
}

/// Recursively builds a disk usage tree with depth limiting and safety bounds.
fn build_usage_tree(
    path: &Path,
    remaining_depth: u32,
    start: &std::time::Instant,
    max_dur: &std::time::Duration,
    max_ent: usize,
    entry_count: &std::sync::atomic::AtomicUsize,
) -> Result<DiskUsageNode, String> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string());

    let metadata =
        std::fs::symlink_metadata(path).map_err(|_| "Failed to read file metadata".to_string())?;

    if !metadata.is_dir() {
        return Ok(DiskUsageNode {
            name,
            path: path.to_string_lossy().to_string(),
            size: metadata.len(),
            is_dir: false,
            children: None,
        });
    }

    if remaining_depth == 0 {
        // At max depth, just calculate total size without building children
        // AF-RUST-H04: Add max_depth and entry limit to prevent resource exhaustion
        let mut total: u64 = 0;
        let mut leaf_count: u64 = 0;
        const MAX_LEAF_ENTRIES: u64 = 500_000;
        for entry in walkdir::WalkDir::new(path)
            .follow_links(false)
            .max_depth(50)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            leaf_count += 1;
            let global = entry_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if leaf_count > MAX_LEAF_ENTRIES || global >= max_ent || start.elapsed() > *max_dur {
                break;
            }
            if entry.file_type().is_file() {
                total += entry.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
        return Ok(DiskUsageNode {
            name,
            path: path.to_string_lossy().to_string(),
            size: total,
            is_dir: true,
            children: None,
        });
    }

    let entries = std::fs::read_dir(path).map_err(|_| "Failed to read directory".to_string())?;

    let mut children: Vec<DiskUsageNode> = Vec::new();

    for entry in entries.filter_map(|e| e.ok()) {
        let global = entry_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if global >= max_ent || start.elapsed() > *max_dur {
            break;
        }
        let child_path = entry.path();
        // Skip symlinks to avoid cycles
        if let Ok(m) = std::fs::symlink_metadata(&child_path) {
            if m.file_type().is_symlink() {
                continue;
            }
        }
        match build_usage_tree(
            &child_path,
            remaining_depth - 1,
            start,
            max_dur,
            max_ent,
            entry_count,
        ) {
            Ok(node) => children.push(node),
            Err(_) => continue, // Skip permission errors etc.
        }
    }

    // Sort by size descending, keep top 50
    children.sort_by_key(|b| std::cmp::Reverse(b.size));
    children.truncate(50);

    let total_size: u64 = children.iter().map(|c| c.size).sum();

    Ok(DiskUsageNode {
        name,
        path: path.to_string_lossy().to_string(),
        size: total_size,
        is_dir: true,
        children: Some(children),
    })
}

// ─── Volume change detection ─────────────────────────────────────────────────

/// Cached hash of last known mount state. Used by `volumes_changed` to detect
/// changes without spawning subprocesses or querying disk space.
#[cfg(target_os = "linux")]
static LAST_MOUNTS_HASH: LazyLock<Mutex<u64>> = LazyLock::new(|| Mutex::new(0));

/// DJB2 hash function: fast, deterministic, no crypto needed.
#[cfg(target_os = "linux")]
fn djb2_hash(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 5381;
    for &b in bytes {
        hash = hash.wrapping_mul(33).wrapping_add(b as u64);
    }
    hash
}

/// Returns true if mounted volumes have changed since the last call.
///
/// Uses a fast hash of `/proc/mounts` content combined with the GVFS directory
/// listing to detect changes without spawning subprocesses or doing disk space
/// queries. The frontend polls this cheaply and only performs a full volume
/// refresh when it returns true.
///
/// `async` because of the GVFS half. `/proc/mounts` is a kernel file and always
/// answers, but `/run/user/N/gvfs` is a FUSE mount served by a userspace daemon:
/// listing it goes out to `gvfsd`, and if a network share behind it has gone
/// unreachable the `readdir` blocks until that share's own timeout. The frontend
/// polls this every 30 seconds, so on a sync command that is a periodic freeze
/// of the GTK thread, arriving on its own with no user action to blame it on.
#[cfg(target_os = "linux")]
#[tauri::command]
pub async fn volumes_changed() -> bool {
    tokio::task::spawn_blocking(volumes_changed_blocking)
        .await
        .unwrap_or_else(|err| {
            warn!("volumes_changed blocking task failed: {err}");
            true // same fallback as an unreadable /proc/mounts: assume changed
        })
}

#[cfg(target_os = "linux")]
fn volumes_changed_blocking() -> bool {
    let content = match std::fs::read_to_string("/proc/mounts") {
        Ok(c) => c,
        Err(_) => return true, // On error, assume changed
    };

    // Also check GVFS directory listing for network mounts
    let gvfs_listing = if let Some(uid) = get_current_uid() {
        let gvfs_dir = format!("/run/user/{}/gvfs", uid);
        std::fs::read_dir(&gvfs_dir)
            .ok()
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_default()
    } else {
        String::new()
    };

    let combined = format!("{}{}", content, gvfs_listing);
    let hash = djb2_hash(combined.as_bytes());

    let mut last = LAST_MOUNTS_HASH.lock().unwrap_or_else(|e| e.into_inner());
    if *last == hash {
        false
    } else {
        *last = hash;
        true
    }
}

/// Cached logical drive bitmask from the last `volumes_changed` call on Windows.
/// Bit N set means drive letter `'A' + N` is present (`GetLogicalDrives`).
#[cfg(target_os = "windows")]
static LAST_DRIVE_MASK: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Returns true if the set of logical drive letters changed since the last call.
///
/// Compares the `GetLogicalDrives` bitmask (cheap kernel query) so the 30s frontend
/// poll only triggers a full volume refresh when a drive was inserted or removed.
/// Label renames without a mask change are rare and are still covered by the
/// real-time `WM_DEVICECHANGE` watcher emit and the focus-refetch safety net.
/// First call with a non-zero mask reports changed (same pattern as Linux hash at 0).
///
/// `GetLogicalDrives` is a cheap in-kernel bitmask and needs no blocking pool,
/// but the command is `async` anyway so the three platform variants present the
/// same surface: one of them (Linux) must be off the main thread, and a command
/// whose asyncness depends on the target is a trap for the next reader.
#[cfg(target_os = "windows")]
#[tauri::command]
pub async fn volumes_changed() -> bool {
    use std::sync::atomic::Ordering;
    use windows::Win32::Storage::FileSystem::GetLogicalDrives;

    let mask = unsafe { GetLogicalDrives() };
    let last = LAST_DRIVE_MASK.swap(mask, Ordering::SeqCst);
    last != mask
}

/// Non-Linux, non-Windows platforms always report changed (poll full refresh).
/// `async` to match the other two variants; see the Windows one for why.
#[cfg(not(any(target_os = "linux", target_os = "windows")))]
#[tauri::command]
pub async fn volumes_changed() -> bool {
    true
}

// ─── Mount watcher (event-driven, replaces 5s setInterval) ──────────────────

/// Start a background thread that watches for mount/volume table changes and
/// emits a `volumes-changed` Tauri event so the frontend can react immediately
/// instead of waiting on the 30s poll fallback.
///
/// - Linux: `poll()` on `/proc/mounts` + `inotify` on GVFS
/// - Windows: hidden message window handling `WM_DEVICECHANGE` (volume arrival/removal)
/// - Other platforms: no-op (frontend poll + focus refetch remain the safety net)
#[cfg(target_os = "linux")]
pub fn start_mount_watcher(app_handle: tauri::AppHandle) {
    use std::io::{Read, Seek, SeekFrom};
    use std::os::fd::AsRawFd;

    std::thread::Builder::new()
        .name("mount-watcher".to_string())
        .spawn(move || {
            // Open /proc/mounts: poll() returns POLLPRI|POLLERR on mount table changes
            let mut mounts_file = match std::fs::File::open("/proc/mounts") {
                Ok(f) => f,
                Err(e) => {
                    warn!("mount-watcher: cannot open /proc/mounts: {}", e);
                    return;
                }
            };

            // Initial read to establish baseline (required before first poll)
            let mut buf = String::new();
            let _ = mounts_file.read_to_string(&mut buf);

            // Set up inotify for GVFS directory changes (network mounts via Nautilus/gio)
            let gvfs_dir = get_current_uid()
                .map(|uid| format!("/run/user/{}/gvfs", uid))
                .unwrap_or_default();

            // SAFETY: inotify_init1 with IN_NONBLOCK|IN_CLOEXEC creates a non-blocking fd
            // that is automatically closed on exec. Returns -1 on failure (checked below).
            let inotify_fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
            if inotify_fd >= 0 && !gvfs_dir.is_empty() {
                if let Ok(c_path) = std::ffi::CString::new(gvfs_dir.as_str()) {
                    // SAFETY: inotify_fd is valid (checked >= 0), c_path is NUL-terminated
                    unsafe {
                        libc::inotify_add_watch(
                            inotify_fd,
                            c_path.as_ptr(),
                            libc::IN_CREATE
                                | libc::IN_DELETE
                                | libc::IN_MOVED_FROM
                                | libc::IN_MOVED_TO,
                        );
                    }
                }
            }

            let mounts_fd = mounts_file.as_raw_fd();

            loop {
                // Build pollfd array: /proc/mounts + optional inotify
                let mut fds = [
                    libc::pollfd {
                        fd: mounts_fd,
                        events: libc::POLLPRI | libc::POLLERR,
                        revents: 0,
                    },
                    libc::pollfd {
                        fd: inotify_fd,
                        events: libc::POLLIN,
                        revents: 0,
                    },
                ];
                let nfds: libc::nfds_t = if inotify_fd >= 0 { 2 } else { 1 };

                let ret = unsafe { libc::poll(fds.as_mut_ptr(), nfds, -1) };

                if ret < 0 {
                    let err = std::io::Error::last_os_error();
                    if err.kind() == std::io::ErrorKind::Interrupted {
                        continue; // EINTR: retry
                    }
                    error!("mount-watcher poll error: {}", err);
                    break;
                }
                if ret == 0 {
                    continue; // Spurious wakeup
                }

                // /proc/mounts changed: seek to 0 and re-read to reset the event
                if fds[0].revents != 0 {
                    let _ = mounts_file.seek(SeekFrom::Start(0));
                    buf.clear();
                    let _ = mounts_file.read_to_string(&mut buf);
                }

                // Drain inotify events (GVFS directory changed)
                if inotify_fd >= 0 && fds[1].revents != 0 {
                    let mut event_buf = [0u8; 4096];
                    while unsafe {
                        libc::read(
                            inotify_fd,
                            event_buf.as_mut_ptr() as *mut libc::c_void,
                            event_buf.len(),
                        )
                    } > 0
                    {}
                }

                // Debounce: wait 300ms for rapid mount events to settle
                std::thread::sleep(std::time::Duration::from_millis(300));

                // Emit event to frontend
                let _ = app_handle.emit("volumes-changed", ());
            }

            // Cleanup
            if inotify_fd >= 0 {
                unsafe {
                    libc::close(inotify_fd);
                }
            }

            info!("mount-watcher thread exited");
        })
        .ok();
}

// ─── Windows mount watcher (WM_DEVICECHANGE) ────────────────────────────────

/// Win32 dbt.h constants (avoid Win32_Devices_* feature thrash for three values).
#[cfg(target_os = "windows")]
const DBT_DEVICEARRIVAL: usize = 0x8000;
#[cfg(target_os = "windows")]
const DBT_DEVICEREMOVECOMPLETE: usize = 0x8004;
#[cfg(target_os = "windows")]
const DBT_DEVTYP_VOLUME: u32 = 0x0000_0002;
/// ERROR_CLASS_ALREADY_EXISTS (winerror.h) -- class still usable for CreateWindow.
#[cfg(target_os = "windows")]
const ERROR_CLASS_ALREADY_EXISTS: u32 = 1410;

/// Minimal DEV_BROADCAST_HDR for filtering volume device-change events.
#[cfg(target_os = "windows")]
#[repr(C)]
struct DevBroadcastHdr {
    dbch_size: u32,
    dbch_devicetype: u32,
    dbch_reserved: u32,
}

/// AppHandle for the Windows mount-watcher WndProc -> emit path.
/// Set once when the watcher thread starts; never cleared (process lifetime).
#[cfg(target_os = "windows")]
static WIN_MOUNT_WATCHER_APP: std::sync::OnceLock<tauri::AppHandle> = std::sync::OnceLock::new();

/// Coalesce rapid WM_DEVICECHANGE bursts into a single delayed emit (mirrors Linux 300ms).
#[cfg(target_os = "windows")]
static WIN_MOUNT_DEBOUNCE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Schedule a debounced `volumes-changed` emit from the Windows device-change path.
#[cfg(target_os = "windows")]
fn schedule_windows_volumes_changed() {
    use std::sync::atomic::Ordering;

    // If a debounce is already pending, drop this event; the pending emit will
    // re-list after the settle window and pick up the final drive mask.
    if WIN_MOUNT_DEBOUNCE.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::Builder::new()
        .name("mount-watcher-debounce".to_string())
        .spawn(|| {
            std::thread::sleep(std::time::Duration::from_millis(300));
            WIN_MOUNT_DEBOUNCE.store(false, Ordering::SeqCst);
            if let Some(app) = WIN_MOUNT_WATCHER_APP.get() {
                let _ = app.emit("volumes-changed", ());
            }
        })
        .ok();
}

/// WndProc for the hidden mount-watcher window. Emits on volume arrival/removal.
#[cfg(target_os = "windows")]
unsafe extern "system" fn mount_watcher_wndproc(
    hwnd: windows::Win32::Foundation::HWND,
    msg: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::Foundation::LRESULT;
    use windows::Win32::UI::WindowsAndMessaging::{DefWindowProcW, WM_DEVICECHANGE};

    if msg == WM_DEVICECHANGE {
        let event = wparam.0;
        if event == DBT_DEVICEARRIVAL || event == DBT_DEVICEREMOVECOMPLETE {
            // Prefer volume-only events; if the header is missing, emit anyway
            // (list_mounted_volumes is cheap and the 300ms debounce coalesces noise).
            let is_volume = if lparam.0 == 0 {
                true
            } else {
                // SAFETY: lParam is a DEV_BROADCAST_HDR* for arrival/removecomplete
                // when non-null (MSDN WM_DEVICECHANGE).
                let hdr = &*(lparam.0 as *const DevBroadcastHdr);
                hdr.dbch_devicetype == DBT_DEVTYP_VOLUME
            };
            if is_volume {
                schedule_windows_volumes_changed();
            }
        }
        // TRUE acknowledges the notification; irrelevant for arrival/remove but harmless.
        return LRESULT(1);
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

/// Start a background thread with a hidden top-level window that receives
/// `WM_DEVICECHANGE` broadcasts for volume arrival/removal and emits
/// `volumes-changed` (debounced 300ms). Message-only windows do NOT receive
/// system device-change broadcasts, so this uses a zero-size tool window.
///
/// Frontend 30s poll + focus refetch remain as safety nets.
#[cfg(target_os = "windows")]
pub fn start_mount_watcher(app_handle: tauri::AppHandle) {
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{GetLastError, HINSTANCE, HWND};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DispatchMessageW, GetMessageW, RegisterClassExW, TranslateMessage,
        CS_HREDRAW, CS_VREDRAW, HMENU, MSG, WNDCLASSEXW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
        WS_POPUP,
    };

    if WIN_MOUNT_WATCHER_APP.set(app_handle).is_err() {
        // Already running (e.g. setup called twice); keep the first handle.
        warn!("mount-watcher: Windows watcher already started");
        return;
    }

    std::thread::Builder::new()
        .name("mount-watcher".to_string())
        .spawn(move || {
            let hinstance: HINSTANCE = match unsafe { GetModuleHandleW(None) } {
                Ok(h) => h.into(),
                Err(e) => {
                    warn!(
                        "mount-watcher: GetModuleHandleW failed: {}; frontend poll remains",
                        e
                    );
                    return;
                }
            };

            let class_name = w!("AeroFTPMountWatcher");
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(mount_watcher_wndproc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: hinstance,
                hIcon: Default::default(),
                hCursor: Default::default(),
                hbrBackground: Default::default(),
                lpszMenuName: PCWSTR::null(),
                lpszClassName: class_name,
                hIconSm: Default::default(),
            };

            // SAFETY: wc points at valid WNDCLASSEXW for the duration of the call.
            let atom = unsafe { RegisterClassExW(&wc) };
            if atom == 0 {
                let err = unsafe { GetLastError() };
                // Class may already exist after a hot-reload; CreateWindow can still use it.
                if err.0 != ERROR_CLASS_ALREADY_EXISTS {
                    warn!(
                        "mount-watcher: RegisterClassExW failed (win32 {}); frontend poll remains",
                        err.0
                    );
                    return;
                }
            }

            // Hidden top-level tool window: receives WM_DEVICECHANGE broadcasts.
            // Zero size, no activate, not on the taskbar. Do not ShowWindow.
            // SAFETY: class registered (or already existed); null parent = top-level.
            let hwnd = match unsafe {
                CreateWindowExW(
                    WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                    class_name,
                    w!(""),
                    WS_POPUP,
                    0,
                    0,
                    0,
                    0,
                    HWND::default(),
                    HMENU::default(),
                    hinstance,
                    None,
                )
            } {
                Ok(h) if !h.is_invalid() => h,
                Ok(_) => {
                    warn!(
                        "mount-watcher: CreateWindowExW returned null HWND; frontend poll remains"
                    );
                    return;
                }
                Err(e) => {
                    warn!(
                        "mount-watcher: CreateWindowExW failed: {}; frontend poll remains",
                        e
                    );
                    return;
                }
            };

            // Keep the HWND alive for the message loop (system owns the window object).
            let _hwnd = hwnd;
            info!("mount-watcher: Windows WM_DEVICECHANGE watcher started");

            let mut msg = MSG::default();
            loop {
                // SAFETY: msg is a valid writable MSG; filter range 0..0 = all messages.
                let ret = unsafe { GetMessageW(&mut msg, None, 0, 0) };
                if ret.0 == 0 {
                    // WM_QUIT
                    break;
                }
                if ret.0 == -1 {
                    warn!("mount-watcher: GetMessageW error; exiting watcher thread");
                    break;
                }
                unsafe {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }

            info!("mount-watcher thread exited");
        })
        .ok();
}

/// macOS and other non-Linux/non-Windows targets: no real-time watcher yet.
/// Frontend 30s poll + focus refetch remain the path.
#[cfg(not(any(target_os = "linux", target_os = "windows")))]
pub fn start_mount_watcher(_app_handle: tauri::AppHandle) {
    // No-op: rely on frontend polling fallback
}

#[cfg(test)]
mod main_thread_tests {
    use super::*;

    /// `list_subdirectories` must never run on the main thread.
    ///
    /// A synchronous `#[tauri::command]` is dispatched on the main thread, which
    /// on Linux is the GTK thread. This command takes its path from the
    /// frontend and walks it with `read_dir` plus a `stat` per entry plus
    /// another `read_dir` per subdirectory, none of it bounded by anything we
    /// control: on a network mount whose server has gone, that is the whole
    /// window frozen for the mount's own timeout.
    ///
    /// The pin is `block_on`, and it acts at **compile time**: `block_on` takes
    /// a future, so turning the command back into a `pub fn` stops this test
    /// building (E0277) instead of failing an assertion somebody can delete.
    /// What it asserts on top of that is the other half: the wrapper returns
    /// what the blocking body computed, it does not quietly change the answer.
    #[test]
    fn list_subdirectories_stays_off_the_main_thread() {
        let base = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir(base.path().join("beta")).expect("mkdir beta");
        std::fs::create_dir(base.path().join("alpha")).expect("mkdir alpha");
        std::fs::write(base.path().join("not-a-dir.txt"), b"x").expect("write file");
        let path = base.path().to_string_lossy().to_string();

        let rt = tokio::runtime::Runtime::new().expect("test runtime");
        let from_command = rt
            .block_on(list_subdirectories(path.clone()))
            .expect("command should list the temp dir");

        let names: Vec<&str> = from_command.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["alpha", "beta"],
            "only directories, sorted case-insensitively"
        );
        assert_eq!(
            from_command,
            list_subdirectories_blocking(path).expect("blocking body"),
            "the async wrapper must report what the blocking body computed"
        );
    }

    /// The error path has to survive the move onto the blocking pool too:
    /// a missing path is still a plain error, not a task failure.
    #[test]
    fn list_subdirectories_still_rejects_a_path_that_is_not_there() {
        let base = tempfile::tempdir().expect("temp dir");
        let missing = base.path().join("nope").to_string_lossy().to_string();

        let rt = tokio::runtime::Runtime::new().expect("test runtime");
        let err = rt
            .block_on(list_subdirectories(missing))
            .expect_err("a path that does not exist must be an error");
        assert_eq!(err, "Path does not exist");
    }

    /// Same pin for the volume poll: the frontend runs it every 30 seconds, so
    /// a synchronous version would freeze the GTK thread on a schedule, with no
    /// user action to attribute it to.
    #[test]
    fn volumes_changed_stays_off_the_main_thread() {
        let rt = tokio::runtime::Runtime::new().expect("test runtime");
        // First call primes the cached state, the second must agree with it.
        let _ = rt.block_on(volumes_changed());
        let second = rt.block_on(volumes_changed());
        #[cfg(target_os = "linux")]
        assert!(
            !second,
            "nothing mounted between the two calls, so the poll must report no change"
        );
        #[cfg(not(target_os = "linux"))]
        let _ = second;
    }
}
