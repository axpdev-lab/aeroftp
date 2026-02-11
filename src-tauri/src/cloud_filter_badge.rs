//! Windows Cloud Filter API integration for Explorer sync badges.
//!
//! Uses `CfSetInSyncState` to show native sync status icons in Explorer
//! (green checkmark for synced, sync arrows for pending) without requiring
//! a COM Shell Icon Overlay DLL.
//!
//! All functions are `#[cfg(windows)]` — this module is only compiled on Windows.

use std::path::{Path, PathBuf};
use std::sync::{LazyLock, RwLock};
use tracing::{error, info, warn};
use windows::core::HSTRING;
use windows::Win32::Storage::CloudFilters::{
    CfRegisterSyncRoot, CfUnregisterSyncRoot, CfSetInSyncState,
    CF_REGISTER_FLAGS, CF_SYNC_ROOT_INFO,
    CF_HYDRATION_POLICY_PRIMARY, CF_HYDRATION_POLICY_MODIFIER,
    CF_POPULATION_POLICY_PRIMARY, CF_POPULATION_POLICY_MODIFIER,
    CF_INSYNC_POLICY,
    CF_IN_SYNC_STATE_IN_SYNC, CF_IN_SYNC_STATE_NOT_IN_SYNC,
    CF_SET_IN_SYNC_FLAGS,
};

use crate::sync_badge::SyncBadgeState;

/// Track registered sync roots so we can clean up on exit
static REGISTERED_ROOTS: LazyLock<RwLock<Vec<PathBuf>>> =
    LazyLock::new(|| RwLock::new(Vec::new()));

/// Register a directory as a Cloud Filter sync root.
/// This enables Explorer to show native sync status badges for files under this path.
pub fn register_cloud_sync_root(path: &Path, display_name: &str) -> Result<(), String> {
    let path_str = path.to_str().ok_or("Invalid UTF-8 in path")?;
    let h_path = HSTRING::from(path_str);
    let h_name = HSTRING::from(display_name);

    let info = CF_SYNC_ROOT_INFO {
        ProviderName: h_name.into(),
        ProviderVersion: HSTRING::from("2.0").into(),
        HydrationPolicy: CF_HYDRATION_POLICY_PRIMARY(0x0002), // FULL — all data available locally
        HydrationPolicyModifier: CF_HYDRATION_POLICY_MODIFIER(0),
        PopulationPolicy: CF_POPULATION_POLICY_PRIMARY(0x0002), // FULL
        PopulationPolicyModifier: CF_POPULATION_POLICY_MODIFIER(0),
        InSyncPolicy: CF_INSYNC_POLICY(0x0001), // CF_INSYNC_POLICY_TRACK_FILE_CREATION_TIME
        ..Default::default()
    };

    // Pass struct directly — no intermediate byte roundtrip needed (audit fix GB2-001)
    unsafe {
        CfRegisterSyncRoot(
            &h_path,
            &info,
            std::mem::size_of::<CF_SYNC_ROOT_INFO>() as u32,
            CF_REGISTER_FLAGS(0),
        )
        .map_err(|e| format!("CfRegisterSyncRoot failed: {}", e))?;
    }

    // Track this root (recover from poisoned lock — audit fix GB2-012)
    {
        let mut roots = REGISTERED_ROOTS.write().unwrap_or_else(|p| p.into_inner());
        if !roots.contains(&path.to_path_buf()) {
            roots.push(path.to_path_buf());
        }
    }

    info!("Registered Cloud Filter sync root: {:?}", path);
    Ok(())
}

/// Unregister a Cloud Filter sync root.
pub fn unregister_cloud_sync_root(path: &Path) -> Result<(), String> {
    let path_str = path.to_str().ok_or("Invalid UTF-8 in path")?;
    let h_path = HSTRING::from(path_str);

    unsafe {
        CfUnregisterSyncRoot(&h_path)
            .map_err(|e| format!("CfUnregisterSyncRoot failed: {}", e))?;
    }

    {
        let mut roots = REGISTERED_ROOTS.write().unwrap_or_else(|p| p.into_inner());
        roots.retain(|r| r != path);
    }

    info!("Unregistered Cloud Filter sync root: {:?}", path);
    Ok(())
}

/// Set the in-sync state for a file in Explorer.
///
/// Maps `SyncBadgeState` to Cloud Filter states:
/// - `Synced` → green checkmark (CF_IN_SYNC_STATE_IN_SYNC)
/// - Everything else → sync arrows (CF_IN_SYNC_STATE_NOT_IN_SYNC)
pub fn set_cloud_sync_state(path: &Path, state: SyncBadgeState) -> Result<(), String> {
    let path_str = path.to_str().ok_or("Invalid UTF-8 in path")?;
    let h_path = HSTRING::from(path_str);

    let cf_state = match state {
        SyncBadgeState::Synced => CF_IN_SYNC_STATE_IN_SYNC,
        _ => CF_IN_SYNC_STATE_NOT_IN_SYNC,
    };

    unsafe {
        CfSetInSyncState(
            &h_path,
            cf_state,
            CF_SET_IN_SYNC_FLAGS(0),
            None, // No USN journal update
        )
        .map_err(|e| format!("CfSetInSyncState failed for {:?}: {}", path, e))?;
    }

    Ok(())
}

/// Deregister all tracked Cloud Filter sync roots.
/// Called on app exit or when user uninstalls badge support.
pub fn cleanup_all_roots() -> Result<(), String> {
    let roots: Vec<PathBuf> = {
        let guard = REGISTERED_ROOTS.read().unwrap_or_else(|p| p.into_inner());
        guard.clone()
    };

    let mut errors = Vec::new();
    for root in &roots {
        if let Err(e) = unregister_cloud_sync_root(root) {
            warn!("Failed to deregister sync root {:?}: {}", root, e);
            errors.push(e);
        }
    }

    if errors.is_empty() {
        info!("All Cloud Filter sync roots cleaned up ({} total)", roots.len());
        Ok(())
    } else {
        Err(format!("{} root(s) failed to deregister", errors.len()))
    }
}
