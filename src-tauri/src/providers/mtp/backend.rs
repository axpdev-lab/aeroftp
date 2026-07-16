//! Platform-isolated MTP backend trait.
//!
//! Phase 1 ships only [`NullMtpBackend`] (empty device list / NotSupported).
//! Phase 2+ fill Linux (libmtp) and Windows (WPD) implementations behind this
//! surface. See APPENDIX-MTP/03 and /04.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use std::path::Path;

use async_trait::async_trait;

use crate::providers::types::ProviderError;

/// Stable-enough id for one attachment session (opaque to the UI).
pub type MtpDeviceId = String;

/// Storage partition on a device (Internal, SD card, ...).
#[derive(Debug, Clone)]
pub struct MtpStorage {
    pub storage_id: String,
    pub display_name: String,
    pub total_bytes: Option<u64>,
    pub free_bytes: Option<u64>,
}

/// Object identity inside an open device session.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MtpObjectId {
    pub storage_id: String,
    /// Backend-native handle (libmtp object id / WPD object id string).
    pub handle: String,
}

/// One listed object (file or folder).
#[derive(Debug, Clone)]
pub struct MtpObject {
    pub id: MtpObjectId,
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<String>,
}

/// Discovery row for PLACES (Phase 4+) and diagnostics.
#[derive(Debug, Clone)]
pub struct MtpDeviceInfo {
    pub device_id: MtpDeviceId,
    pub display_name: String,
    pub serial: Option<String>,
    pub bus_location: Option<String>,
    pub platform: String,
    pub storages_hint: u32,
}

/// Progress callback: (bytes_transferred, total_bytes_or_zero_if_unknown).
pub type MtpProgress = Box<dyn Fn(u64, u64) + Send>;

/// Platform backend for one process. `Send + Sync` matches `StorageProvider`
/// so the provider can live in shared session maps; ops are still serialized
/// by the single-session model (`max_file_slots = 1`).
#[async_trait]
pub trait MtpBackend: Send + Sync {
    async fn list_devices(&self) -> Result<Vec<MtpDeviceInfo>, ProviderError>;

    async fn open(&mut self, device_id: &str) -> Result<(), ProviderError>;

    async fn close(&mut self) -> Result<(), ProviderError>;

    fn is_open(&self) -> bool;

    fn device_display_name(&self) -> Option<String>;

    async fn list_storages(&mut self) -> Result<Vec<MtpStorage>, ProviderError>;

    async fn list_objects(
        &mut self,
        parent: Option<&MtpObjectId>,
        storage_id: Option<&str>,
    ) -> Result<Vec<MtpObject>, ProviderError>;

    async fn get_object(
        &mut self,
        id: &MtpObjectId,
        dest: &Path,
        on_progress: Option<MtpProgress>,
    ) -> Result<(), ProviderError>;

    async fn send_object(
        &mut self,
        parent: Option<&MtpObjectId>,
        storage_id: &str,
        src: &Path,
        name: &str,
        on_progress: Option<MtpProgress>,
    ) -> Result<MtpObjectId, ProviderError>;

    async fn delete_object(&mut self, id: &MtpObjectId) -> Result<(), ProviderError>;

    async fn create_folder(
        &mut self,
        parent: Option<&MtpObjectId>,
        storage_id: &str,
        name: &str,
    ) -> Result<MtpObjectId, ProviderError>;
}

/// Placeholder backend until platform FFI lands (Phase 2/3).
///
/// `list_devices` returns empty (no USB probing). Open/transfer ops return
/// NotSupported with a stable message so callers never confuse this with a
/// real empty phone.
pub struct NullMtpBackend {
    open: bool,
    device_id: Option<String>,
}

impl NullMtpBackend {
    pub fn new() -> Self {
        Self {
            open: false,
            device_id: None,
        }
    }

    const NOT_LINKED: &'static str =
        "MTP backend not linked on this build (install libmtp-dev and rebuild on Linux, or use Windows WPD when available)";
}

impl Default for NullMtpBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MtpBackend for NullMtpBackend {
    async fn list_devices(&self) -> Result<Vec<MtpDeviceInfo>, ProviderError> {
        Ok(Vec::new())
    }

    async fn open(&mut self, device_id: &str) -> Result<(), ProviderError> {
        if device_id.trim().is_empty() {
            return Err(ProviderError::InvalidConfig(
                "MTP device_id is required".to_string(),
            ));
        }
        // Accept open for unit tests that exercise the session shell; transfer
        // still fails with NotSupported until a real backend is wired.
        self.device_id = Some(device_id.to_string());
        self.open = true;
        Ok(())
    }

    async fn close(&mut self) -> Result<(), ProviderError> {
        self.open = false;
        self.device_id = None;
        Ok(())
    }

    fn is_open(&self) -> bool {
        self.open
    }

    fn device_display_name(&self) -> Option<String> {
        self.device_id
            .as_ref()
            .map(|id| format!("MTP device ({id})"))
    }

    async fn list_storages(&mut self) -> Result<Vec<MtpStorage>, ProviderError> {
        if !self.open {
            return Err(ProviderError::NotConnected);
        }
        Err(ProviderError::NotSupported(Self::NOT_LINKED.to_string()))
    }

    async fn list_objects(
        &mut self,
        _parent: Option<&MtpObjectId>,
        _storage_id: Option<&str>,
    ) -> Result<Vec<MtpObject>, ProviderError> {
        if !self.open {
            return Err(ProviderError::NotConnected);
        }
        Err(ProviderError::NotSupported(Self::NOT_LINKED.to_string()))
    }

    async fn get_object(
        &mut self,
        _id: &MtpObjectId,
        _dest: &Path,
        _on_progress: Option<MtpProgress>,
    ) -> Result<(), ProviderError> {
        Err(ProviderError::NotSupported(Self::NOT_LINKED.to_string()))
    }

    async fn send_object(
        &mut self,
        _parent: Option<&MtpObjectId>,
        _storage_id: &str,
        _src: &Path,
        _name: &str,
        _on_progress: Option<MtpProgress>,
    ) -> Result<MtpObjectId, ProviderError> {
        Err(ProviderError::NotSupported(Self::NOT_LINKED.to_string()))
    }

    async fn delete_object(&mut self, _id: &MtpObjectId) -> Result<(), ProviderError> {
        Err(ProviderError::NotSupported(Self::NOT_LINKED.to_string()))
    }

    async fn create_folder(
        &mut self,
        _parent: Option<&MtpObjectId>,
        _storage_id: &str,
        _name: &str,
    ) -> Result<MtpObjectId, ProviderError> {
        Err(ProviderError::NotSupported(Self::NOT_LINKED.to_string()))
    }
}

/// In-memory demo backend for unit tests (and CI without USB hardware).
///
/// Not a product path: one fake phone, one storage, a DCIM folder and a file.
/// Whole-file get/send write real bytes under a temp root when paths are set.
pub struct FakeMtpBackend {
    open: bool,
    device_id: Option<String>,
    /// storage_id -> (display_name, objects keyed by parent handle or "" for root)
    storages: Vec<MtpStorage>,
    /// key: parent handle ("" or "storage:{id}" for storage root, else object handle)
    children: std::collections::HashMap<String, Vec<MtpObject>>,
    next_handle: u64,
}

impl FakeMtpBackend {
    /// Demo tree: Internal shared storage / DCIM / IMG_001.JPG
    pub fn with_demo_tree() -> Self {
        let storage = MtpStorage {
            storage_id: "s1".into(),
            display_name: "Internal shared storage".into(),
            total_bytes: Some(64 * 1024 * 1024 * 1024),
            free_bytes: Some(32 * 1024 * 1024 * 1024),
        };
        let dcim = MtpObject {
            id: MtpObjectId {
                storage_id: "s1".into(),
                handle: "obj:1".into(),
            },
            name: "DCIM".into(),
            is_dir: true,
            size: 0,
            modified: None,
        };
        let photo = MtpObject {
            id: MtpObjectId {
                storage_id: "s1".into(),
                handle: "obj:2".into(),
            },
            name: "IMG_001.JPG".into(),
            is_dir: false,
            size: 4,
            modified: Some("2026-01-01T00:00:00Z".into()),
        };
        let mut children = std::collections::HashMap::new();
        children.insert("storage:s1".into(), vec![dcim]);
        children.insert("obj:1".into(), vec![photo]);
        Self {
            open: false,
            device_id: None,
            storages: vec![storage],
            children,
            next_handle: 10,
        }
    }
}

#[async_trait]
impl MtpBackend for FakeMtpBackend {
    async fn list_devices(&self) -> Result<Vec<MtpDeviceInfo>, ProviderError> {
        Ok(vec![MtpDeviceInfo {
            device_id: "fake-phone".into(),
            display_name: "Fake Phone".into(),
            serial: Some("FAKE-SERIAL".into()),
            bus_location: Some("test:0".into()),
            platform: "test-fake".into(),
            storages_hint: 1,
        }])
    }

    async fn open(&mut self, device_id: &str) -> Result<(), ProviderError> {
        if device_id.trim().is_empty() {
            return Err(ProviderError::InvalidConfig(
                "MTP device_id is required".to_string(),
            ));
        }
        self.device_id = Some(device_id.to_string());
        self.open = true;
        Ok(())
    }

    async fn close(&mut self) -> Result<(), ProviderError> {
        self.open = false;
        self.device_id = None;
        Ok(())
    }

    fn is_open(&self) -> bool {
        self.open
    }

    fn device_display_name(&self) -> Option<String> {
        self.device_id
            .as_ref()
            .map(|id| format!("Fake portable ({id})"))
    }

    async fn list_storages(&mut self) -> Result<Vec<MtpStorage>, ProviderError> {
        if !self.open {
            return Err(ProviderError::NotConnected);
        }
        Ok(self.storages.clone())
    }

    async fn list_objects(
        &mut self,
        parent: Option<&MtpObjectId>,
        storage_id: Option<&str>,
    ) -> Result<Vec<MtpObject>, ProviderError> {
        if !self.open {
            return Err(ProviderError::NotConnected);
        }
        let key = match parent {
            None => {
                let sid = storage_id.ok_or_else(|| {
                    ProviderError::InvalidPath("storage_id required for storage root list".into())
                })?;
                format!("storage:{sid}")
            }
            Some(id) => id.handle.clone(),
        };
        Ok(self.children.get(&key).cloned().unwrap_or_default())
    }

    async fn get_object(
        &mut self,
        id: &MtpObjectId,
        dest: &Path,
        on_progress: Option<MtpProgress>,
    ) -> Result<(), ProviderError> {
        if !self.open {
            return Err(ProviderError::NotConnected);
        }
        // Find object size/name by scanning.
        let mut found: Option<MtpObject> = None;
        for kids in self.children.values() {
            if let Some(o) = kids.iter().find(|o| o.id == *id) {
                found = Some(o.clone());
                break;
            }
        }
        let obj = found.ok_or_else(|| ProviderError::NotFound(id.handle.clone()))?;
        if obj.is_dir {
            return Err(ProviderError::InvalidPath(
                "cannot download a folder as a file".into(),
            ));
        }
        let data = b"JPEG";
        if let Some(parent) = dest.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(ProviderError::IoError)?;
            }
        }
        std::fs::write(dest, data).map_err(ProviderError::IoError)?;
        if let Some(cb) = on_progress {
            cb(data.len() as u64, data.len() as u64);
        }
        let _ = obj;
        Ok(())
    }

    async fn send_object(
        &mut self,
        parent: Option<&MtpObjectId>,
        storage_id: &str,
        src: &Path,
        name: &str,
        on_progress: Option<MtpProgress>,
    ) -> Result<MtpObjectId, ProviderError> {
        if !self.open {
            return Err(ProviderError::NotConnected);
        }
        let meta = std::fs::metadata(src).map_err(ProviderError::IoError)?;
        let size = meta.len();
        if let Some(cb) = on_progress {
            cb(size, size);
        }
        let handle = format!("obj:{}", self.next_handle);
        self.next_handle += 1;
        let id = MtpObjectId {
            storage_id: storage_id.to_string(),
            handle: handle.clone(),
        };
        let obj = MtpObject {
            id: id.clone(),
            name: name.to_string(),
            is_dir: false,
            size,
            modified: None,
        };
        let key = match parent {
            None => format!("storage:{storage_id}"),
            Some(p) => p.handle.clone(),
        };
        self.children.entry(key).or_default().push(obj);
        Ok(id)
    }

    async fn delete_object(&mut self, id: &MtpObjectId) -> Result<(), ProviderError> {
        if !self.open {
            return Err(ProviderError::NotConnected);
        }
        for kids in self.children.values_mut() {
            if let Some(pos) = kids.iter().position(|o| o.id == *id) {
                kids.remove(pos);
                self.children.remove(&id.handle);
                return Ok(());
            }
        }
        Err(ProviderError::NotFound(id.handle.clone()))
    }

    async fn create_folder(
        &mut self,
        parent: Option<&MtpObjectId>,
        storage_id: &str,
        name: &str,
    ) -> Result<MtpObjectId, ProviderError> {
        if !self.open {
            return Err(ProviderError::NotConnected);
        }
        let handle = format!("obj:{}", self.next_handle);
        self.next_handle += 1;
        let id = MtpObjectId {
            storage_id: storage_id.to_string(),
            handle: handle.clone(),
        };
        let obj = MtpObject {
            id: id.clone(),
            name: name.to_string(),
            is_dir: true,
            size: 0,
            modified: None,
        };
        let key = match parent {
            None => format!("storage:{storage_id}"),
            Some(p) => p.handle.clone(),
        };
        self.children.entry(key).or_default().push(obj);
        self.children.entry(handle).or_default();
        Ok(id)
    }
}

/// Whether this build linked a real platform MTP backend
/// (libmtp on Linux, WPD on Windows).
pub fn mtp_backend_linked() -> bool {
    cfg!(all(target_os = "linux", mtp_libmtp)) || cfg!(windows)
}

/// Construct the best available backend for this OS/build.
///
/// - Linux + libmtp linked: [`crate::providers::mtp::linux_libmtp::LinuxLibmtpBackend`]
/// - Windows: [`crate::providers::mtp::windows_wpd::WindowsWpdBackend`]
/// - otherwise: [`NullMtpBackend`] (honest empty list / NotSupported)
pub fn platform_backend() -> Box<dyn MtpBackend> {
    #[cfg(all(target_os = "linux", mtp_libmtp))]
    {
        use crate::providers::mtp::linux_libmtp::LinuxLibmtpBackend;
        return Box::new(LinuxLibmtpBackend::new());
    }
    #[cfg(windows)]
    {
        use crate::providers::mtp::windows_wpd::WindowsWpdBackend;
        return Box::new(WindowsWpdBackend::new());
    }
    #[cfg(not(any(all(target_os = "linux", mtp_libmtp), windows)))]
    {
        Box::new(NullMtpBackend::new())
    }
}

/// Standalone discovery helper (no open session).
pub async fn list_mtp_devices() -> Result<Vec<MtpDeviceInfo>, ProviderError> {
    platform_backend().list_devices().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn null_backend_lists_no_devices() {
        let b = NullMtpBackend::new();
        let devices = b.list_devices().await.unwrap();
        assert!(devices.is_empty());
    }

    #[tokio::test]
    async fn null_backend_open_close_session() {
        let mut b = NullMtpBackend::new();
        assert!(!b.is_open());
        b.open("test-device").await.unwrap();
        assert!(b.is_open());
        assert!(matches!(
            b.list_storages().await,
            Err(ProviderError::NotSupported(_))
        ));
        b.close().await.unwrap();
        assert!(!b.is_open());
    }
}
