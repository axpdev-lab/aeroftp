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
        "MTP backend not linked on this build (Phase 1 scaffold; libmtp/WPD land next)";
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

/// Standalone discovery helper (no open session).
pub async fn list_mtp_devices() -> Result<Vec<MtpDeviceInfo>, ProviderError> {
    NullMtpBackend::new().list_devices().await
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
