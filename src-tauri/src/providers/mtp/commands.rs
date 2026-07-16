//! Tauri commands for MTP portable-device discovery and session open/close.
//!
//! Open installs an [`MtpProvider`] into [`ProviderState`] so the existing
//! `provider_list_files` / download / upload fabric can browse and transfer
//! without a second parallel API. See APPENDIX-MTP Phase 5.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use std::sync::atomic::Ordering;
use std::time::Duration;

use serde::Serialize;
use tauri::State;
use tracing::warn;

use crate::provider_commands::{drain_in_flight_transfers, ProviderState};
use crate::providers::mtp::backend::{platform_backend, MtpDeviceInfo, MtpStorage};
use crate::providers::mtp::provider::MtpProvider;
use crate::providers::types::{ProviderConfig, ProviderError, ProviderType};
use crate::providers::StorageProvider;

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

fn err_str(e: ProviderError) -> String {
    e.to_string()
}

fn mtp_platform_label(linked: bool) -> String {
    if linked && cfg!(target_os = "linux") {
        "linux-libmtp".to_string()
    } else if linked && cfg!(windows) {
        "windows-wpd".to_string()
    } else {
        std::env::consts::OS.to_string()
    }
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

/// Install `provider` into `ProviderState`, disconnecting any previous slot.
async fn install_provider(
    state: &ProviderState,
    provider: Box<dyn crate::providers::StorageProvider>,
    config: ProviderConfig,
) {
    drain_in_flight_transfers(state, Duration::from_secs(30)).await;
    {
        let mut prov_lock = state.provider.lock().await;
        if let Some(mut previous) = prov_lock.take() {
            if let Err(err) = previous.disconnect().await {
                warn!(
                    "mtp_open_device: previous provider disconnect failed: {}",
                    err
                );
            }
        }
        *prov_lock = Some(provider);
    }
    state.active_crypt_overlay.store(false, Ordering::SeqCst);
    state.overlay_wrapped.store(false, Ordering::SeqCst);
    {
        let mut config_lock = state.config.lock().await;
        *config_lock = Some(config);
    }
}

/// Clear ProviderState when the live slot is an MTP session (or when forced).
async fn clear_mtp_provider_slot(
    state: &ProviderState,
    want_device_id: Option<&str>,
) -> Result<(), String> {
    drain_in_flight_transfers(state, Duration::from_secs(30)).await;

    let mut prov_lock = state.provider.lock().await;
    let is_mtp = prov_lock
        .as_ref()
        .map(|p| p.provider_type() == ProviderType::Mtp)
        .unwrap_or(false);

    if !is_mtp {
        // No MTP provider installed: still clear a stale MTP config if present.
        drop(prov_lock);
        let mut config_lock = state.config.lock().await;
        if config_lock
            .as_ref()
            .map(|c| c.provider_type == ProviderType::Mtp)
            .unwrap_or(false)
        {
            *config_lock = None;
        }
        return Ok(());
    }

    if let Some(want) = want_device_id {
        if !want.is_empty() {
            let open_id = {
                let config_lock = state.config.lock().await;
                config_lock
                    .as_ref()
                    .filter(|c| c.provider_type == ProviderType::Mtp)
                    .map(|c| c.host.clone())
            };
            if let Some(open_id) = open_id {
                if open_id != want {
                    return Err(format!("open MTP session is {open_id} not {want}"));
                }
            }
        }
    }

    if let Some(mut provider) = prov_lock.take() {
        if let Err(err) = provider.disconnect().await {
            warn!("mtp_close_device: disconnect failed: {}", err);
            return Err(format!("Disconnect failed: {err}"));
        }
    }
    drop(prov_lock);

    let mut config_lock = state.config.lock().await;
    *config_lock = None;
    state.active_crypt_overlay.store(false, Ordering::SeqCst);
    state.overlay_wrapped.store(false, Ordering::SeqCst);
    Ok(())
}

/// Open an MTP device session and install it into ProviderState.
///
/// Closes any previous provider (MTP or otherwise) first, so the single
/// ProviderState slot remains the sole owner of the USB session.
pub async fn mtp_open_device_inner(
    state: &ProviderState,
    device_id: String,
) -> Result<MtpSessionInfoDto, String> {
    if device_id.trim().is_empty() {
        return Err("MTP device_id is required".into());
    }

    let linked = mtp_backend_linked();
    let mut mtp = MtpProvider::with_platform_backend();
    mtp.open_device(&device_id).await.map_err(err_str)?;

    let display = mtp.display_name();
    let storages = match mtp.list_storage_info().await {
        Ok(s) => s.into_iter().map(MtpStorageDto::from).collect(),
        Err(ProviderError::NotSupported(_)) => Vec::new(),
        Err(e) => {
            let _ = mtp.disconnect().await;
            return Err(err_str(e));
        }
    };

    let platform = mtp_platform_label(linked);
    let config = ProviderConfig {
        name: display.clone(),
        provider_type: ProviderType::Mtp,
        host: device_id.clone(),
        port: None,
        username: None,
        password: None,
        initial_path: Some("/".to_string()),
        extra: Default::default(),
    };

    install_provider(state, Box::new(mtp), config).await;

    Ok(MtpSessionInfoDto {
        device_id,
        display_name: display,
        platform,
        backend_linked: linked,
        storages,
    })
}

/// Open an MTP device session. Closes any previous provider first.
#[tauri::command]
pub async fn mtp_open_device(
    state: State<'_, ProviderState>,
    device_id: String,
) -> Result<MtpSessionInfoDto, String> {
    mtp_open_device_inner(&state, device_id).await
}

/// Close the open MTP device session (no-op if none).
pub async fn mtp_close_device_inner(
    state: &ProviderState,
    device_id: Option<String>,
) -> Result<(), String> {
    clear_mtp_provider_slot(state, device_id.as_deref()).await
}

/// Close the open MTP device session (no-op if none).
#[tauri::command]
pub async fn mtp_close_device(
    state: State<'_, ProviderState>,
    device_id: Option<String>,
) -> Result<(), String> {
    mtp_close_device_inner(&state, device_id).await
}

/// Diagnostic: backend linkage and open session (no USB side effects beyond status).
#[tauri::command]
pub async fn mtp_backend_status(
    state: State<'_, ProviderState>,
) -> Result<MtpBackendStatusDto, String> {
    let open_device_id = {
        let config_lock = state.config.lock().await;
        config_lock
            .as_ref()
            .filter(|c| c.provider_type == ProviderType::Mtp)
            .map(|c| c.host.clone())
    };
    // Prefer config; fall back to live provider type if config was cleared mid-flight.
    let open_device_id = if open_device_id.is_some() {
        open_device_id
    } else {
        let prov = state.provider.lock().await;
        if prov
            .as_ref()
            .map(|p| p.provider_type() == ProviderType::Mtp)
            .unwrap_or(false)
        {
            Some("(open)".to_string())
        } else {
            None
        }
    };
    Ok(MtpBackendStatusDto {
        linked: mtp_backend_linked(),
        platform: std::env::consts::OS.to_string(),
        open_device_id,
        build_backend: option_env!("AEROFTP_MTP_BACKEND")
            .unwrap_or("unknown")
            .to_string(),
    })
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
    use crate::providers::mtp::backend::{FakeMtpBackend, MtpBackend};
    use crate::providers::StorageProvider;

    #[tokio::test]
    async fn list_devices_command_ok() {
        let result = list_mtp_devices().await;
        assert!(result.is_ok(), "{result:?}");
    }

    #[tokio::test]
    async fn open_installs_into_provider_state() {
        let state = ProviderState::new();
        assert!(mtp_open_device_inner(&state, String::new()).await.is_err());

        // Null / unlinked: open still installs the session shell so FE can
        // clear it via close; list may be empty or NotSupported.
        let open = mtp_open_device_inner(&state, "usb:0:0".into()).await;
        match open {
            Ok(info) => {
                assert_eq!(info.device_id, "usb:0:0");
                {
                    let lock = state.provider.lock().await;
                    let p = lock.as_ref().expect("provider installed");
                    assert_eq!(p.provider_type(), ProviderType::Mtp);
                    assert!(p.is_connected());
                }
                {
                    let cfg = state.config.lock().await;
                    let c = cfg.as_ref().expect("config installed");
                    assert_eq!(c.provider_type, ProviderType::Mtp);
                    assert_eq!(c.host, "usb:0:0");
                }
                mtp_close_device_inner(&state, Some("usb:0:0".into()))
                    .await
                    .unwrap();
                assert!(state.provider.lock().await.is_none());
                assert!(state.config.lock().await.is_none());
            }
            Err(msg) => {
                // Real libmtp may reject a fake id when linked.
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
        mtp_close_device_inner(&state, None).await.unwrap();
    }

    #[tokio::test]
    async fn fake_backend_open_lists_storages_via_provider() {
        let state = ProviderState::new();
        // Direct provider path (bypasses platform_backend) to prove list works.
        let mut mtp = MtpProvider::new(Box::new(FakeMtpBackend::with_demo_tree()));
        mtp.open_device("fake-phone").await.unwrap();
        let storages = mtp.list_storage_info().await.unwrap();
        assert_eq!(storages.len(), 1);
        assert_eq!(storages[0].display_name, "Internal shared storage");
        let entries = mtp.list("/").await.unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].is_dir);
        let kids = mtp.list("/Internal shared storage").await.unwrap();
        assert!(kids.iter().any(|e| e.name == "DCIM" && e.is_dir));
        let files = mtp.list("/Internal shared storage/DCIM").await.unwrap();
        assert!(files.iter().any(|e| e.name == "IMG_001.JPG" && !e.is_dir));

        let config = ProviderConfig {
            name: mtp.display_name(),
            provider_type: ProviderType::Mtp,
            host: "fake-phone".into(),
            port: None,
            username: None,
            password: None,
            initial_path: Some("/".into()),
            extra: Default::default(),
        };
        install_provider(&state, Box::new(mtp), config).await;
        {
            let mut lock = state.provider.lock().await;
            let p = lock.as_mut().unwrap();
            let listed = p.list("/").await.unwrap();
            assert_eq!(listed.len(), 1);
        }
        mtp_close_device_inner(&state, Some("fake-phone".into()))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn close_wrong_id_errors_when_open() {
        let state = ProviderState::new();
        let mut mtp = MtpProvider::new(Box::new(FakeMtpBackend::with_demo_tree()));
        mtp.open_device("dev-a").await.unwrap();
        let config = ProviderConfig {
            name: "Phone".into(),
            provider_type: ProviderType::Mtp,
            host: "dev-a".into(),
            port: None,
            username: None,
            password: None,
            initial_path: Some("/".into()),
            extra: Default::default(),
        };
        install_provider(&state, Box::new(mtp), config).await;
        let err = mtp_close_device_inner(&state, Some("dev-b".into()))
            .await
            .unwrap_err();
        assert!(err.contains("dev-a"), "{err}");
        mtp_close_device_inner(&state, Some("dev-a".into()))
            .await
            .unwrap();
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

    #[tokio::test]
    async fn fake_backend_list_devices() {
        let b = FakeMtpBackend::with_demo_tree();
        let devices = b.list_devices().await.unwrap();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].device_id, "fake-phone");
    }

    /// Live USB smoke: phone in File Transfer mode + libmtp linked.
    /// `cargo test --lib mtp_live_phone_smoke -- --ignored --nocapture`
    #[tokio::test]
    #[ignore = "live USB phone"]
    async fn mtp_live_phone_smoke() {
        assert!(
            mtp_backend_linked(),
            "libmtp not linked; install libmtp-dev and rebuild"
        );
        let devices = list_mtp_devices().await.expect("list_mtp_devices");
        eprintln!("devices: {devices:#?}");
        assert!(
            !devices.is_empty(),
            "no MTP devices; unlock phone, set File Transfer"
        );
        let id = devices[0].device_id.clone();
        eprintln!("opening {} ({id})", devices[0].display_name);

        let state = ProviderState::new();
        let session = mtp_open_device_inner(&state, id.clone())
            .await
            .expect("mtp_open_device");
        eprintln!("session: {session:#?}");
        assert!(
            !session.storages.is_empty(),
            "no storages; unlock phone / allow USB data"
        );

        {
            let mut lock = state.provider.lock().await;
            let p = lock.as_mut().expect("provider");
            let root = p.list("/").await.expect("list root");
            eprintln!("root: {root:#?}");
            assert!(!root.is_empty());
            let first = &root[0];
            if first.is_dir {
                let kids = p.list(&first.path).await.expect("list storage");
                eprintln!(
                    "first 20 under {}: {:#?}",
                    first.path,
                    kids.iter().take(20).collect::<Vec<_>>()
                );
            }
        }

        mtp_close_device_inner(&state, Some(id))
            .await
            .expect("close");
        assert!(state.provider.lock().await.is_none());
        eprintln!("live smoke OK");
    }
}
