//! Native, remount-stable directory identities where the filesystem exposes them.
//! No marker is written into the user's tree. Unsupported filesystems retain
//! a guaranteed nonreused mount-instance identity, or no deletion authority.

use serde_json::{json, Value};
use std::fs::File;

pub(super) fn open_directory(path: &std::path::Path) -> Option<File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x02000000); // FILE_FLAG_BACKUP_SEMANTICS.
    }
    let file = options.open(path).ok()?;
    file.metadata().ok()?.is_dir().then_some(file)
}

/// The UNIQUE token, unlike mountinfo's ID or st_dev, is not recycled this boot.
#[cfg(target_os = "linux")]
pub(super) fn unique_mount_id(file: &File) -> Option<u64> {
    use std::os::fd::AsRawFd;
    // SAFETY: statx is an integer-only C output structure, fully zero initialized.
    let mut state: libc::statx = unsafe { std::mem::zeroed() };
    // SAFETY: the live directory fd and empty C string identify that same object.
    if unsafe {
        libc::statx(
            file.as_raw_fd(),
            c"".as_ptr(),
            libc::AT_EMPTY_PATH,
            libc::STATX_MNT_ID_UNIQUE,
            &mut state,
        )
    } != 0
        || state.stx_mask & libc::STATX_MNT_ID_UNIQUE == 0
        || state.stx_mnt_id == 0
    {
        return None;
    }
    Some(state.stx_mnt_id)
}

/// Ephemeral scan/cache guard; prefer the cheap Linux token, otherwise native ID.
pub(super) fn guard(file: &File) -> Option<[u8; 32]> {
    use sha2::Digest;
    #[cfg(target_os = "linux")]
    if let Some(id) = unique_mount_id(file) {
        let mut hash = sha2::Sha256::new();
        hash.update(id.to_le_bytes());
        // Native handles include inode generation. Reusing an inode on the
        // same mount must not hit an old native identity in the cycle cache.
        if let Some(handle) = file_handle(file) {
            hash.update(handle.kind.to_le_bytes());
            hash.update(&handle.bytes);
        }
        if let Ok(created) = file.metadata().and_then(|meta| meta.created()) {
            hash.update(serde_json::to_vec(&created).ok()?);
        }
        return Some(hash.finalize().into());
    }
    native(file).map(|value| sha2::Sha256::digest(value.to_string().as_bytes()).into())
}

#[cfg(target_os = "linux")]
pub(super) fn mount_bound(file: &File) -> Option<Value> {
    use std::os::unix::fs::MetadataExt;
    let mount = unique_mount_id(file)?;
    let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
    let boot = uuid::Uuid::parse_str(boot.trim()).ok()?;
    Some(
        json!({"kind":"linux-unique-mount-v1", "boot":boot.to_string(),
        "mount":mount, "inode":file.metadata().ok()?.ino(), "generation":hex::encode(guard(file)?)}),
    )
}

#[cfg(not(target_os = "linux"))]
pub(super) fn mount_bound(_file: &File) -> Option<Value> {
    None
}

#[cfg(target_os = "linux")]
struct LinuxHandle {
    kind: i32,
    bytes: Vec<u8>,
    mount_id: i32,
}

#[cfg(target_os = "linux")]
fn file_handle(file: &File) -> Option<LinuxHandle> {
    use std::os::fd::AsRawFd;
    #[repr(C)]
    struct Handle {
        bytes: u32,
        kind: i32,
        data: [u8; 128],
    }
    let mut handle = Handle {
        bytes: 128,
        kind: 0,
        data: [0; 128],
    };
    let mut mount_id = 0;
    // SAFETY: an open directory fd, a NUL-terminated empty path, and writable
    // file_handle storage with the advertised 128-byte payload. No privileged
    // open_by_handle_at is used; mount_id only selects metadata, never identity.
    if unsafe {
        libc::name_to_handle_at(
            file.as_raw_fd(),
            c"".as_ptr(),
            (&mut handle as *mut Handle).cast(),
            &mut mount_id,
            libc::AT_EMPTY_PATH,
        )
    } != 0
        || handle.bytes == 0
        || handle.bytes > 128
    {
        return None;
    }
    Some(LinuxHandle {
        kind: handle.kind,
        bytes: handle.data[..handle.bytes as usize].to_vec(),
        mount_id,
    })
}

#[cfg(target_os = "linux")]
pub(super) fn native(file: &File) -> Option<Value> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let handle = file_handle(file)?;
    let mount_id = handle.mount_id;
    let mounts = std::fs::read_to_string("/proc/self/mountinfo").ok()?;
    let line = mounts.lines().find(|line| {
        line.split_whitespace()
            .next()
            .and_then(|id| id.parse::<i32>().ok())
            == Some(mount_id)
    })?;
    let (before, after) = line.split_once(" - ")?;
    let root = before.split_whitespace().nth(3)?;
    let mut fields = after.split_whitespace();
    let kind = fields.next()?;
    let source = fields.next()?;
    let metadata = file.metadata().ok()?;
    let volume = if matches!(kind, "nfs" | "nfs4") {
        // NFS's opaque server handle includes the exported filesystem identity;
        // its server/export disambiguates handles from different servers.
        json!({"nfs_export": source, "root": root})
    } else {
        // Btrfs assigns subvolumes synthetic st_dev values. The mount selected
        // by the opened fd identifies its real block device, independently.
        let block = std::fs::metadata(source)
            .ok()
            .filter(|device| device.file_type().is_block_device())
            .map_or(metadata.dev(), |device| device.rdev());
        let mut ids = Vec::new();
        for entry in std::fs::read_dir("/dev/disk/by-uuid").ok()?.flatten() {
            if let Ok(device) = std::fs::metadata(entry.path()) {
                if device.file_type().is_block_device() && device.rdev() == block {
                    ids.push(entry.file_name().to_str()?.to_string());
                }
            }
        }
        ids.sort();
        ids.dedup();
        if ids.len() != 1 {
            return None;
        }
        json!({"uuid": ids[0], "root": root})
    };
    Some(
        json!({"kind":"linux-file-handle-v1", "volume":volume, "handle_type":handle.kind, "handle":hex::encode(&handle.bytes)}),
    )
}

#[cfg(target_os = "macos")]
pub(super) fn native(file: &File) -> Option<Value> {
    use std::os::fd::AsRawFd;
    // Darwin forbids FILEID and volume attributes in one request. Both
    // independent reads refer to the same live directory fd.
    #[repr(C, packed(4))]
    struct FileAttributes {
        length: u32,
        file_id: u64,
    }
    #[repr(C, packed(4))]
    struct VolumeAttributes {
        length: u32,
        volume: [u8; 16],
    }
    // SAFETY: attrlist is an integer-only C structure initialized before use.
    let mut request: libc::attrlist = unsafe { std::mem::zeroed() };
    request.bitmapcount = libc::ATTR_BIT_MAP_COUNT;
    request.commonattr = libc::ATTR_CMN_FILEID;
    let mut identity = FileAttributes {
        length: 0,
        file_id: 0,
    };
    // SAFETY: the bitmap and packed buffer describe exactly length+FILEID.
    if unsafe {
        libc::fgetattrlist(
            file.as_raw_fd(),
            (&mut request as *mut libc::attrlist).cast(),
            (&mut identity as *mut FileAttributes).cast(),
            std::mem::size_of::<FileAttributes>(),
            0,
        )
    } != 0
    {
        return None;
    }
    request.commonattr = 0;
    request.volattr = libc::ATTR_VOL_INFO | libc::ATTR_VOL_UUID;
    let mut attributes = VolumeAttributes {
        length: 0,
        volume: [0; 16],
    };
    // SAFETY: the same fd is still live; this request returns length+UUID only.
    if unsafe {
        libc::fgetattrlist(
            file.as_raw_fd(),
            (&mut request as *mut libc::attrlist).cast(),
            (&mut attributes as *mut VolumeAttributes).cast(),
            std::mem::size_of::<VolumeAttributes>(),
            0,
        )
    } != 0
    {
        return None;
    }
    let file_id = identity.file_id;
    let volume = attributes.volume;
    if identity.length as usize != std::mem::size_of::<FileAttributes>()
        || attributes.length as usize != std::mem::size_of::<VolumeAttributes>()
        || file_id == 0
        || volume.iter().all(|byte| *byte == 0)
    {
        return None;
    }
    Some(json!({"kind":"macos-volume-file-v1","volume":hex::encode(volume),"file":file_id}))
}

#[cfg(windows)]
pub(super) fn native(file: &File) -> Option<Value> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::{
        Foundation::HANDLE,
        Storage::FileSystem::{FileIdInfo, GetFileInformationByHandleEx, FILE_ID_INFO},
    };
    let mut info = FILE_ID_INFO::default();
    // SAFETY: File owns a live handle and info is a writable FILE_ID_INFO buffer.
    unsafe {
        GetFileInformationByHandleEx(
            HANDLE(file.as_raw_handle()),
            FileIdInfo,
            (&mut info as *mut FILE_ID_INFO).cast(),
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        )
    }
    .ok()?;
    if info.VolumeSerialNumber == 0 || info.FileId.Identifier.iter().all(|byte| *byte == 0) {
        return None;
    }
    Some(
        json!({"kind":"windows-volume-file-v1", "volume":info.VolumeSerialNumber, "file":hex::encode(info.FileId.Identifier)}),
    )
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub(super) fn native(_file: &File) -> Option<Value> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A development-host probe, separate from portable CI's fallback coverage.
    #[test]
    #[ignore = "live: requires a filesystem exposing native volume and directory identities"]
    fn live_native_identity_survives_reopening_the_same_directory() {
        let root = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let file = File::open(root.path()).unwrap();
        let first = native(&file).expect("native identity unavailable on this filesystem");
        drop(file);
        let reopened = File::open(root.path()).unwrap();
        assert_eq!(native(&reopened), Some(first));
        let other = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        assert_ne!(
            native(&File::open(other.path()).unwrap()),
            native(&reopened)
        );
    }
}
