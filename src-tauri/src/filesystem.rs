//! Filesystem utilities for AeroFile Places Sidebar
//!
//! Provides 12 Tauri commands for local filesystem navigation and management:
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
//! - `find_duplicate_files`: Scan directory for duplicate files (MD5 hash)
//! - `scan_disk_usage`: Disk usage tree for treemap visualization

use serde::Serialize;
use std::path::{Path, PathBuf, Component};
use tracing::{info, warn};

// ─── Path validation ────────────────────────────────────────────────────────

/// Validate a filesystem path. Rejects null bytes, `..` traversal, and excessive length.
pub fn validate_path(path: &str) -> Result<(), String> {
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

/// A subdirectory entry for tree/breadcrumb navigation.
#[derive(Serialize, Clone)]
pub struct SubDirectory {
    pub name: String,
    pub path: String,
    pub has_children: bool,
}

// ─── Well-known hidden directories ──────────────────────────────────────────

/// Hidden directories that should still be shown in listings.
const WELL_KNOWN_HIDDEN: &[&str] = &[
    ".config", ".local", ".ssh", ".gnupg", ".cargo", ".rustup",
    ".npm", ".nvm", ".vscode", ".git",
];

// ─── Command 1: get_user_directories ────────────────────────────────────────

/// Returns standard user directories detected via the `dirs` crate.
/// Only directories that exist on disk are included.
#[tauri::command]
pub fn get_user_directories() -> Vec<UserDirectory> {
    let mappings: Vec<(Option<PathBuf>, &str, &str)> = vec![
        (dirs::home_dir(),     "home",      "Home"),
        (dirs::desktop_dir(),  "desktop",   "Monitor"),
        (dirs::document_dir(), "documents", "FileText"),
        (dirs::picture_dir(),  "pictures",  "Image"),
        (dirs::audio_dir(),    "music",     "Music"),
        (dirs::download_dir(), "downloads", "Download"),
        (dirs::video_dir(),    "videos",    "Video"),
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
#[cfg(target_os = "linux")]
const PSEUDO_FS_TYPES: &[&str] = &[
    "proc", "sysfs", "devtmpfs", "tmpfs", "cgroup", "cgroup2",
    "overlay", "squashfs", "devpts", "securityfs", "pstore",
    "efivarfs", "bpf", "autofs", "mqueue", "hugetlbfs", "debugfs",
    "tracefs", "fusectl", "configfs", "ramfs", "rpc_pipefs", "nfsd",
    "fuse.portal", "fuse.gvfsd-fuse",
];

/// Network filesystem types.
#[cfg(any(target_os = "linux", target_os = "macos"))]
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
        let mount_point = parts[1];
        let fs_type = parts[2];

        // Skip pseudo-filesystems
        if PSEUDO_FS_TYPES.contains(&fs_type) {
            continue;
        }

        // Skip system mount points
        if should_skip_mount_point(mount_point) {
            continue;
        }

        // Get disk space via df
        let (total_bytes, free_bytes) = get_disk_space(mount_point);

        // Determine volume type
        let volume_type = classify_volume(mount_point, fs_type);
        let is_ejectable = volume_type == "removable";

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

    info!("Detected {} mounted volumes", volumes.len());
    Ok(volumes)
}

/// Check if a mount point should be skipped (system pseudo-paths).
#[cfg(target_os = "linux")]
fn should_skip_mount_point(mount_point: &str) -> bool {
    // Always skip /proc, /sys, /dev (except /dev/shm)
    if mount_point.starts_with("/proc")
        || mount_point.starts_with("/sys")
    {
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

    false
}

/// Classify a volume as internal, removable, or network.
#[cfg(any(target_os = "linux", target_os = "macos"))]
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

/// Get disk space (total, free) in bytes for a mount point using `df -B1`.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn get_disk_space(mount_point: &str) -> (u64, u64) {
    let output = std::process::Command::new("df")
        .args(["-B1", mount_point])
        .output();

    match output {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            // df -B1 output: Filesystem 1B-blocks Used Available Use% Mounted on
            // Second line contains the data
            if let Some(data_line) = stdout.lines().nth(1) {
                let fields: Vec<&str> = data_line.split_whitespace().collect();
                if fields.len() >= 4 {
                    let total = fields[1].parse::<u64>().unwrap_or(0);
                    let free = fields[3].parse::<u64>().unwrap_or(0);
                    return (total, free);
                }
            }
            (0, 0)
        }
        Err(_) => (0, 0),
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

/// Unescape octal-encoded characters in volume labels (e.g. `\040` → space).
#[cfg(target_os = "linux")]
fn unescape_label(label: &str) -> String {
    let mut result = String::with_capacity(label.len());
    let mut chars = label.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            // Try to read 3 octal digits
            let mut octal = String::new();
            for _ in 0..3 {
                if let Some(&next) = chars.as_str().chars().collect::<Vec<_>>().first() {
                    if next.is_ascii_digit() && next < '8' {
                        octal.push(next);
                        chars.next();
                    } else {
                        break;
                    }
                }
            }
            if octal.len() == 3 {
                if let Ok(byte) = u8::from_str_radix(&octal, 8) {
                    result.push(byte as char);
                } else {
                    result.push('\\');
                    result.push_str(&octal);
                }
            } else {
                result.push('\\');
                result.push_str(&octal);
            }
        } else {
            result.push(c);
        }
    }
    result
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
        volumes.insert(0, VolumeInfo {
            name: "Macintosh HD".to_string(),
            mount_point: "/".to_string(),
            volume_type: "internal".to_string(),
            total_bytes: total,
            free_bytes: free,
            fs_type: "apfs".to_string(),
            is_ejectable: false,
        });
    }

    info!("Detected {} mounted volumes (macOS)", volumes.len());
    Ok(volumes)
}

// ─── Windows implementation (stub) ──────────────────────────────────────────

#[cfg(target_os = "windows")]
async fn list_mounted_volumes_windows() -> Result<Vec<VolumeInfo>, String> {
    // Stub: return C: drive as a basic entry
    let mut volumes = Vec::new();

    let c_drive = Path::new("C:\\");
    if c_drive.exists() {
        volumes.push(VolumeInfo {
            name: "Local Disk (C:)".to_string(),
            mount_point: "C:\\".to_string(),
            volume_type: "internal".to_string(),
            total_bytes: 0,
            free_bytes: 0,
            fs_type: "ntfs".to_string(),
            is_ejectable: false,
        });
    }

    info!("Detected {} mounted volumes (Windows stub)", volumes.len());
    Ok(volumes)
}

// ─── Command 3: list_subdirectories ─────────────────────────────────────────

/// Returns subdirectories of the given path, sorted alphabetically (case-insensitive).
/// Used for breadcrumb dropdowns and tree expansion in the Places sidebar.
/// Skips hidden directories unless they are well-known (e.g. `.config`, `.ssh`).
#[tauri::command]
pub fn list_subdirectories(path: String) -> Result<Vec<SubDirectory>, String> {
    validate_path(&path)?;

    let dir_path = Path::new(&path);
    if !dir_path.exists() {
        return Err("Path does not exist".to_string());
    }
    if !dir_path.is_dir() {
        return Err("Path is not a directory".to_string());
    }

    let entries = std::fs::read_dir(dir_path)
        .map_err(|_| "Failed to read directory".to_string())?;

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
    subdirs.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

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
        Err("Volume ejection is not yet supported on Windows".to_string())
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
        || !suffix.chars().all(|c| c.is_ascii_alphanumeric() || c == '/' || c == '_' || c == '-')
    {
        return Err("Device path contains invalid characters".to_string());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
async fn eject_volume_linux(mount_point: &str) -> Result<String, String> {
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
                let msg = format!("Volume unmounted successfully via udisksctl: {}", mount_point);
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
        Ok(msg)
    } else {
        let stderr = String::from_utf8_lossy(&result.stderr);
        Err(format!("Failed to unmount {}: {}", mount_point, stderr.trim()))
    }
}

/// Find the device path associated with a mount point by reading /proc/mounts.
#[cfg(target_os = "linux")]
fn find_device_for_mount(mount_point: &str) -> Option<String> {
    let content = std::fs::read_to_string("/proc/mounts").ok()?;
    for line in content.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 2 && parts[1] == mount_point {
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
        Err(format!("Failed to eject {}: {}", mount_point, stderr.trim()))
    }
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

    let metadata = tokio::fs::symlink_metadata(&path).await
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
        tokio::fs::read_link(&path).await.ok().map(|p| p.to_string_lossy().to_string())
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
            format!("{}{}{}",
                if n & 4 != 0 { "r" } else { "-" },
                if n & 2 != 0 { "w" } else { "-" },
                if n & 1 != 0 { "x" } else { "-" })
        };

        let prefix = if metadata.is_dir() { "d" } else if is_symlink { "l" } else { "-" };
        let text = format!("{}{}{}{}", prefix,
            to_rwx((file_mode >> 6) & 7),
            to_rwx((file_mode >> 3) & 7),
            to_rwx(file_mode & 7));

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
    let (permissions_mode, permissions_text, owner, group, inode, hard_links) = {
        (None, None, None, None, None, None)
    };

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
            warn!("calculate_folder_size: entry limit ({}) reached, returning partial result", MAX_ENTRIES);
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

/// Percent-decode a string (for FreeDesktop .trashinfo Path= values).
fn percent_decode(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '%' {
            let hex: String = chars.by_ref().take(2).collect();
            if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                result.push(byte as char);
            } else {
                result.push('%');
                result.push_str(&hex);
            }
        } else {
            result.push(c);
        }
    }
    result
}

/// Lists items currently in the system trash (up to 200, sorted by deletion time).
/// Uses FreeDesktop Trash spec on Linux, filesystem scanning on macOS.
#[tauri::command]
pub async fn list_trash_items() -> Result<Vec<TrashItem>, String> {
    #[cfg(target_os = "linux")]
    {
        let home = dirs::home_dir().ok_or("Cannot find home directory")?;
        let trash_info_dir = home.join(".local/share/Trash/info");
        let trash_files_dir = home.join(".local/share/Trash/files");

        if !trash_info_dir.exists() {
            return Ok(Vec::new());
        }

        let mut items = Vec::new();

        if let Ok(entries) = std::fs::read_dir(&trash_info_dir) {
            for entry in entries.filter_map(|e| e.ok()) {
                let info_name = entry.file_name().to_string_lossy().to_string();
                if !info_name.ends_with(".trashinfo") { continue; }

                let base_name = info_name.trim_end_matches(".trashinfo").to_string();
                let info_path = entry.path();

                let content = match std::fs::read_to_string(&info_path) {
                    Ok(c) => c,
                    Err(_) => continue,
                };

                let mut original_path = String::new();
                let mut deleted_at = None;

                for line in content.lines() {
                    if let Some(path_val) = line.strip_prefix("Path=") {
                        original_path = percent_decode(path_val);
                    }
                    if let Some(date) = line.strip_prefix("DeletionDate=") {
                        deleted_at = Some(date.to_string());
                    }
                }

                let file_in_trash = trash_files_dir.join(&base_name);
                let (size, is_dir) = match std::fs::metadata(&file_in_trash) {
                    Ok(m) => (m.len(), m.is_dir()),
                    Err(_) => (0, false),
                };

                items.push(TrashItem {
                    id: base_name.clone(),
                    name: base_name,
                    original_path,
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
                if name.starts_with('.') { continue; }

                let (size, is_dir, deleted_at) = match entry.metadata() {
                    Ok(m) => {
                        let deleted = m.modified().ok().and_then(|t| {
                            t.duration_since(std::time::UNIX_EPOCH).ok().and_then(|d| {
                                chrono::DateTime::from_timestamp(d.as_secs() as i64, 0)
                                    .map(|dt| dt.format("%Y-%m-%dT%H:%M:%S").to_string())
                            })
                        });
                        (m.len(), m.is_dir(), deleted)
                    },
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

    #[cfg(target_os = "windows")]
    {
        Ok(Vec::new())
    }
}

// ─── Command 9: restore_trash_item ──────────────────────────────────────────

/// Restores a previously deleted item from the trash back to its original location.
/// Currently supported on Linux only (FreeDesktop Trash spec).
#[tauri::command]
pub async fn restore_trash_item(id: String, original_path: String) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        // Sanitize trash item id - reject path separators and traversal
        if id.contains('/') || id.contains('\\') || id.contains("..") || id.contains('\0') {
            return Err("Invalid trash item ID".to_string());
        }

        // AF-RUST-C02: Validate original_path to prevent path traversal
        // Reject paths containing '..' components
        for component in Path::new(&original_path).components() {
            if matches!(component, Component::ParentDir) {
                return Err("Restore path must not contain '..' traversal".to_string());
            }
        }
        // Must be absolute
        if !Path::new(&original_path).is_absolute() {
            return Err("Restore path must be absolute".to_string());
        }
        // Must be within user's home directory
        let home = dirs::home_dir().ok_or("Cannot find home directory")?;
        let canonical_home = home.to_string_lossy().to_string();
        if !original_path.starts_with(&canonical_home) {
            return Err("Restore path must be within user's home directory".to_string());
        }

        let trash_files_dir = home.join(".local/share/Trash/files");
        let trash_info_dir = home.join(".local/share/Trash/info");

        let source = trash_files_dir.join(&id);
        let info_file = trash_info_dir.join(format!("{}.trashinfo", id));

        if !source.exists() {
            return Err("File not found in trash".to_string());
        }

        // Ensure parent directory exists
        if let Some(parent) = Path::new(&original_path).parent() {
            tokio::fs::create_dir_all(parent).await
                .map_err(|e| format!("Failed to create parent directory: {}", e))?;
        }

        // Move file back to original location
        tokio::fs::rename(&source, &original_path).await
            .map_err(|e| format!("Failed to restore file: {}", e))?;

        // Remove .trashinfo file
        let _ = tokio::fs::remove_file(&info_file).await;

        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = (&id, &original_path);
        Err("Restore from trash is not yet supported on this platform".to_string())
    }
}

// ─── Command 10: empty_trash ────────────────────────────────────────────────

/// Permanently deletes all items in the system trash. Returns the number of items removed.
#[tauri::command]
pub async fn empty_trash() -> Result<u64, String> {
    #[cfg(target_os = "linux")]
    {
        let home = dirs::home_dir().ok_or("Cannot find home directory")?;
        let trash_files_dir = home.join(".local/share/Trash/files");
        let trash_info_dir = home.join(".local/share/Trash/info");

        let mut count: u64 = 0;

        // Remove all files in trash
        if trash_files_dir.exists() {
            if let Ok(entries) = std::fs::read_dir(&trash_files_dir) {
                for entry in entries.filter_map(|e| e.ok()) {
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

        // Remove all .trashinfo files
        if trash_info_dir.exists() {
            if let Ok(entries) = std::fs::read_dir(&trash_info_dir) {
                for entry in entries.filter_map(|e| e.ok()) {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }

        Ok(count)
    }

    #[cfg(target_os = "macos")]
    {
        let home = dirs::home_dir().ok_or("Cannot find home directory")?;
        let trash_dir = home.join(".Trash");
        let mut count: u64 = 0;

        if trash_dir.exists() {
            if let Ok(entries) = std::fs::read_dir(&trash_dir) {
                for entry in entries.filter_map(|e| e.ok()) {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if name.starts_with('.') { continue; }
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

    #[cfg(target_os = "windows")]
    {
        Err("Empty trash is not yet supported on Windows".to_string())
    }
}

// ─── Structs (Duplicate Finder) ─────────────────────────────────────────────

/// A group of files that are exact duplicates (same MD5 hash and size).
#[derive(Serialize, Clone)]
pub struct DuplicateGroup {
    pub hash: String,
    pub size: u64,
    pub files: Vec<String>,
}

// ─── Command 11: find_duplicate_files ───────────────────────────────────────

/// Scans a directory recursively for duplicate files. Groups by size first,
/// then computes MD5 hash only for size-matched candidates for performance.
/// Returns groups sorted by wasted space descending.
#[tauri::command]
pub async fn find_duplicate_files(
    path: String,
    min_size: Option<u64>,
) -> Result<Vec<DuplicateGroup>, String> {
    validate_path(&path)?;

    let path_ref = Path::new(&path);
    if !path_ref.is_dir() {
        return Err("Path is not a directory".to_string());
    }

    let min = min_size.unwrap_or(1);

    // AF-RUST-H03: Resource exhaustion limits
    const MAX_FILE_COUNT: u64 = 100_000;
    const MAX_TOTAL_HASH_BYTES: u64 = 2_000_000_000; // 2 GB max total hash I/O

    // Phase 1: group files by size
    let mut size_groups: std::collections::HashMap<u64, Vec<PathBuf>> = std::collections::HashMap::new();
    let mut scan_count: u64 = 0;

    for entry in walkdir::WalkDir::new(&path)
        .follow_links(false)
        .max_depth(100)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_file() { continue; }
        scan_count += 1;
        if scan_count > MAX_FILE_COUNT {
            warn!("find_duplicate_files: file limit ({}) reached, scanning partial results", MAX_FILE_COUNT);
            break;
        }
        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        if size < min { continue; }
        size_groups.entry(size).or_default().push(entry.into_path());
    }

    // Phase 2: hash only files with matching sizes (2+ files)
    let mut hash_groups: std::collections::HashMap<String, (u64, Vec<String>)> = std::collections::HashMap::new();
    let mut total_hash_bytes: u64 = 0;
    let mut budget_exceeded = false;

    for (size, files) in size_groups {
        if budget_exceeded { break; }
        if files.len() < 2 { continue; }

        for file_path in &files {
            // Check total hash I/O budget
            total_hash_bytes += size;
            if total_hash_bytes > MAX_TOTAL_HASH_BYTES {
                warn!("find_duplicate_files: total hash I/O limit ({} bytes) reached", MAX_TOTAL_HASH_BYTES);
                budget_exceeded = true;
                break;
            }
            match compute_md5(file_path) {
                Ok(hash) => {
                    let entry = hash_groups.entry(hash).or_insert_with(|| (size, Vec::new()));
                    entry.1.push(file_path.to_string_lossy().to_string());
                }
                Err(_) => continue,
            }
        }
    }

    // Phase 3: collect groups with 2+ files, sort by wasted space
    let mut result: Vec<DuplicateGroup> = hash_groups
        .into_iter()
        .filter(|(_, (_, files))| files.len() >= 2)
        .map(|(hash, (size, files))| DuplicateGroup { hash, size, files })
        .collect();

    result.sort_by(|a, b| {
        let wasted_a = a.size * (a.files.len() as u64 - 1);
        let wasted_b = b.size * (b.files.len() as u64 - 1);
        wasted_b.cmp(&wasted_a)
    });

    Ok(result)
}

/// Compute MD5 hash of a file using buffered reading.
fn compute_md5(path: &Path) -> Result<String, std::io::Error> {
    use md5::Md5;
    use md5::Digest;
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Md5::new();
    let mut buffer = [0u8; 65536];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 { break; }
        hasher.update(&buffer[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
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
    let name = path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string());

    let metadata = std::fs::symlink_metadata(path)
        .map_err(|_| "Failed to read file metadata".to_string())?;

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

    let entries = std::fs::read_dir(path)
        .map_err(|_| "Failed to read directory".to_string())?;

    let mut children: Vec<DiskUsageNode> = Vec::new();

    for entry in entries.filter_map(|e| e.ok()) {
        let global = entry_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if global >= max_ent || start.elapsed() > *max_dur {
            break;
        }
        let child_path = entry.path();
        // Skip symlinks to avoid cycles
        if let Ok(m) = std::fs::symlink_metadata(&child_path) {
            if m.file_type().is_symlink() { continue; }
        }
        match build_usage_tree(&child_path, remaining_depth - 1, start, max_dur, max_ent, entry_count) {
            Ok(node) => children.push(node),
            Err(_) => continue, // Skip permission errors etc.
        }
    }

    // Sort by size descending, keep top 50
    children.sort_by(|a, b| b.size.cmp(&a.size));
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
