// AeroFTP Master Password UI State
// Manages auto-lock timer and lock state for the Universal Vault
// All crypto operations are handled by credential_store.rs
//
// v2.0 — February 2026

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;
use serde::Serialize;
use tracing::info;

// ============ Global State ============

/// Thread-safe global state for master password lock/unlock
pub struct MasterPasswordState {
    /// Whether the app is currently locked
    locked: AtomicBool,
    /// Timestamp of last activity (for auto-lock)
    last_activity_ms: AtomicU64,
    /// Auto-lock timeout in seconds (0 = disabled)
    timeout_seconds: AtomicU64,
    /// Start time for activity tracking
    start_instant: Instant,
}

impl MasterPasswordState {
    pub fn new() -> Self {
        Self {
            locked: AtomicBool::new(false),
            last_activity_ms: AtomicU64::new(0),
            timeout_seconds: AtomicU64::new(0),
            start_instant: Instant::now(),
        }
    }

    /// Update last activity timestamp (call on user interaction)
    pub fn update_activity(&self) {
        let now = self.start_instant.elapsed().as_millis() as u64;
        self.last_activity_ms.store(now, Ordering::SeqCst);
    }

    /// Check if auto-lock timeout has expired
    pub fn check_timeout(&self) -> bool {
        let timeout = self.timeout_seconds.load(Ordering::SeqCst);
        if timeout == 0 {
            return false;
        }

        let last = self.last_activity_ms.load(Ordering::SeqCst);
        let now = self.start_instant.elapsed().as_millis() as u64;
        let elapsed_secs = (now.saturating_sub(last)) / 1000;

        elapsed_secs >= timeout
    }

    /// Check if locked
    pub fn is_locked(&self) -> bool {
        self.locked.load(Ordering::SeqCst)
    }

    /// Set locked state
    pub fn set_locked(&self, locked: bool) {
        self.locked.store(locked, Ordering::SeqCst);
        if locked {
            info!("App locked");
        } else {
            info!("App unlocked");
        }
    }

    /// Set auto-lock timeout
    pub fn set_timeout(&self, seconds: u64) {
        self.timeout_seconds.store(seconds, Ordering::SeqCst);
    }

    /// Get current timeout setting
    pub fn get_timeout(&self) -> u64 {
        self.timeout_seconds.load(Ordering::SeqCst)
    }
}

// ============ Status Response ============

#[derive(Serialize)]
pub struct MasterPasswordStatus {
    pub is_set: bool,
    pub is_locked: bool,
    pub timeout_seconds: u64,
}

impl MasterPasswordStatus {
    pub fn new(state: &MasterPasswordState) -> Self {
        Self {
            is_set: crate::credential_store::CredentialStore::is_master_mode(),
            is_locked: state.is_locked(),
            timeout_seconds: state.get_timeout(),
        }
    }
}
