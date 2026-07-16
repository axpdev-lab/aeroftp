//! Tauri commands for MTP portable-device discovery and session open/close.
//!
//! Phase 2 surface for later PLACES wiring (Phase 4). Whole-file transfers go
//! through [`crate::providers::mtp::provider::MtpProvider`] once a session is open.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use std::sync::Mutex;

use serde::Serialize;

use crate::providers::mtp::backend::{platform_backend, MtpBackend, MtpDeviceInfo, MtpStorage};
use crate::providers::types::ProviderError;

/// Discovery row for the frontend.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MtpDeviceInfoDto {
    pub device_id: String,
    pub display_name: String,
    pub serial: Option<String>,
    pub bus_location: Option<String>,
    pub platform: String,
    pub storages_hint: u32,
}

impl From<MtpDeviceInfo> for MtpDeviceInfoDto {
    fn from(d: MtpDeviceInfo) -> Self {
        Self {
            device_id: d.device_id,
            display_name: d.display_name,
            serial: d.serial,
            bus_location: d.bus_location,
            platform: d.platform,
            storages_hint: d.storages_hint,
        }
    }
}

/// Storage partition on an open device.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MtpStorageDto {
    pub storage_id: String,
    pub display_name: String,
    pub total_bytes: Option<u64>,
    pub free_bytes: Option<u64>,
}

impl From<MtpStorage> for MtpStorageDto {
    fn from(s: MtpStorage) -> Self {
        Self {
            storage_id: s.storage_id,
            display_name: s.display_name,
            total_bytes: s.total_bytes,
            free_bytes: s.free_bytes,
        }
    }
}

/// Session info returned by [`mtp_open_device`].
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MtpSessionInfoDto {
    pub device_id: String,
    pub display_name: String,
    pub platform: String,
    pub backend_linked: bool,
    pub storages: Vec<MtpStorageDto>,
}

struct OpenSession {
    device_id: String,
    backend: Box<dyn MtpBackend>,
}

/// Process-wide single open MTP session (matches max_file_slots = 1 model).
/// Never hold this mutex across `.await`.
static SESSION: Mutex<Option<OpenSession>> = Mutex::new(None);

fn err_str(e: ProviderError) -> String {
    e.to_string()
}

fn take_session() -> Result<Option<OpenSession>, String> {
    SESSION
        .lock()
        .map_err(|_| "MTP session lock poisoned".to_string())
        .map(|mut g| g.take())
}

fn put_session(session: OpenSession) -> Result<(), String> {
    let mut g = SESSION
        .lock()
        .map_err(|_| "MTP session lock poisoned".to_string())?;
    *g = Some(session);
    Ok(())
}

/// Whether this build linked a real MTP platform backend (libmtp / WPD).
pub fn mtp_backend_linked() -> bool {
    crate::providers::mtp::backend::mtp_backend_linked()
}

/// List attached MTP devices (empty when none, or when backend not linked).
#[tauri::command]
pub async fn list_mtp_devices() -> Result<Vec<MtpDeviceInfoDto>, String> {
    let backend = platform_backend();
    let devices = backend.list_devices().await.map_err(err_str)?;
    Ok(devices.into_iter().map(MtpDeviceInfoDto::from).collect())
}

/// Open an MTP device session. Closes any previous session first.
#[tauri::command]
pub async fn mtp_open_device(device_id: String) -> Result<MtpSessionInfoDto, String> {
    if device_id.trim().is_empty() {
        return Err("MTP device_id is required".into());
    }

    // Drop previous session (if any) before opening a new one.
    if let Some(mut prev) = take_session()? {
        let _ = prev.backend.close().await;
    }

    let linked = mtp_backend_linked();
    let mut backend = platform_backend();
    backend.open(&device_id).await.map_err(err_str)?;
    let display = backend
        .device_display_name()
        .unwrap_or_else(|| format!("MTP device ({device_id})"));

    let storages = if linked {
        match backend.list_storages().await {
            Ok(s) => s.into_iter().map(MtpStorageDto::from).collect(),
            Err(ProviderError::NotSupported(_)) => Vec::new(),
            Err(e) => {
                let _ = backend.close().await;
                return Err(err_str(e));
            }
        }
    } else {
        Vec::new()
    };

    let platform = if linked && cfg!(target_os = "linux") {
        "linux-libmtp".to_string()
    } else if linked && cfg!(windows) {
        "windows-wpd".to_string()
    } else {
        std::env::consts::OS.to_string()
    };

    put_session(OpenSession {
        device_id: device_id.clone(),
        backend,
    })?;

    Ok(MtpSessionInfoDto {
        device_id,
        display_name: display,
        platform,
        backend_linked: linked,
        storages,
    })
}

/// Close the open MTP device session (no-op if none).
#[tauri::command]
pub async fn mtp_close_device(device_id: Option<String>) -> Result<(), String> {
    let mut session = match take_session()? {
        Some(s) => s,
        None => return Ok(()),
    };

    if let Some(want) = device_id.as_ref() {
        if !want.is_empty() && want != &session.device_id {
            let open_id = session.device_id.clone();
            put_session(session)?;
            return Err(format!("open MTP session is {open_id} not {want}"));
        }
    }
    session.backend.close().await.map_err(err_str)
}

/// Diagnostic: backend linkage and open session (no USB side effects beyond status).
#[tauri::command]
pub fn mtp_backend_status() -> MtpBackendStatusDto {
    let open_id = SESSION
        .lock()
        .ok()
        .and_then(|g| g.as_ref().map(|s| s.device_id.clone()));
    MtpBackendStatusDto {
        linked: mtp_backend_linked(),
        platform: std::env::consts::OS.to_string(),
        open_device_id: open_id,
        build_backend: option_env!("AEROFTP_MTP_BACKEND")
            .unwrap_or("unknown")
            .to_string(),
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MtpBackendStatusDto {
    pub linked: bool,
    pub platform: String,
    pub open_device_id: Option<String>,
    pub build_backend: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn list_devices_command_ok() {
        let result = list_mtp_devices().await;
        assert!(result.is_ok(), "{result:?}");
    }

    #[tokio::test]
    async fn open_close_null_or_linked() {
        assert!(mtp_open_device(String::new()).await.is_err());
        let open = mtp_open_device("usb:0:0".into()).await;
        match open {
            Ok(info) => {
                assert_eq!(info.device_id, "usb:0:0");
                mtp_close_device(Some("usb:0:0".into())).await.unwrap();
            }
            Err(msg) => {
                assert!(
                    msg.contains("not found")
                        || msg.contains("failed to open")
                        || msg.contains("busy")
                        || msg.contains("not linked")
                        || msg.contains("Not connected")
                        || msg.contains("MTP")
                        || msg.contains("invalid"),
                    "unexpected open error: {msg}"
                );
            }
        }
        mtp_close_device(None).await.unwrap();
    }

    #[test]
    fn status_reports_platform() {
        let s = mtp_backend_status();
        assert!(!s.platform.is_empty());
    }

    #[test]
    fn dto_from_device_info() {
        let d = MtpDeviceInfo {
            device_id: "usb:1:2".into(),
            display_name: "Phone".into(),
            serial: Some("abc".into()),
            bus_location: Some("1:2".into()),
            platform: "linux-libmtp".into(),
            storages_hint: 1,
        };
        let dto = MtpDeviceInfoDto::from(d);
        assert_eq!(dto.device_id, "usb:1:2");
        assert_eq!(dto.storages_hint, 1);
    }

    #[test]
    fn null_backend_constructible() {
        let _ = crate::providers::mtp::backend::NullMtpBackend::new();
    }
}
