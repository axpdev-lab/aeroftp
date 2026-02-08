//! Filesystem utilities for AeroFile Places Sidebar
//!
//! Provides 4 Tauri commands for local filesystem navigation:
//! - `get_user_directories`: Standard XDG user directories (cross-platform via `dirs` crate)
//! - `list_mounted_volumes`: Mounted filesystem volumes with space info
//! - `list_subdirectories`: Subdirectories of a given path (for tree/breadcrumb)
//! - `eject_volume`: Unmount/eject a removable volume

use serde::Serialize;
use std::path::{Path, PathBuf, Component};
use tracing::{info, warn};

// ─── Path validation ────────────────────────────────────────────────────────

/// Validate a filesystem path. Rejects null bytes, `..` traversal, and excessive length.
fn validate_path(path: &str) -> Result<(), String> {
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
        return Err(format!("Path does not exist: {}", path));
    }
    if !dir_path.is_dir() {
        return Err(format!("Path is not a directory: {}", path));
    }

    let entries = std::fs::read_dir(dir_path)
        .map_err(|e| format!("Failed to read directory {}: {}", path, e))?;

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
async fn eject_volume_linux(mount_point: &str) -> Result<String, String> {
    // First, try to find the device for this mount point from /proc/mounts
    let device = find_device_for_mount(mount_point);

    // Try udisksctl first (user-level, no sudo needed)
    if let Some(ref dev) = device {
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
