// AeroFTP Master Password UI State
// Manages auto-lock timer and lock state for the Universal Vault
// All crypto operations are handled by credential_store.rs
//
// v2.0 — February 2026

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::Instant;
use serde::Serialize;
use tracing::{info, warn};

/// Maximum failed unlock attempts before throttling kicks in
const THROTTLE_THRESHOLD: u32 = 5;
/// Base backoff delay in seconds (doubles with each consecutive failure beyond threshold)
const THROTTLE_BASE_DELAY_SECS: u64 = 30;
/// Maximum backoff delay in seconds (15 minutes)
const THROTTLE_MAX_DELAY_SECS: u64 = 900;

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
    /// Consecutive failed unlock attempts (M69 throttling)
    failed_attempts: AtomicU32,
    /// Timestamp (ms since start) when throttle lockout expires
    throttle_until_ms: AtomicU64,
}

impl MasterPasswordState {
    pub fn new() -> Self {
        Self {
            locked: AtomicBool::new(false),
            last_activity_ms: AtomicU64::new(0),
            timeout_seconds: AtomicU64::new(0),
            start_instant: Instant::now(),
            failed_attempts: AtomicU32::new(0),
            throttle_until_ms: AtomicU64::new(0),
        }
    }

    /// Check if unlock attempts are currently throttled.
    /// Returns Err with wait time if throttled, Ok(()) if allowed.
    pub fn check_throttle(&self) -> Result<(), u64> {
        let now_ms = self.start_instant.elapsed().as_millis() as u64;
        let until_ms = self.throttle_until_ms.load(Ordering::SeqCst);
        if now_ms < until_ms {
            let remaining_secs = (until_ms - now_ms + 999) / 1000; // ceil division
            Err(remaining_secs)
        } else {
            Ok(())
        }
    }

    /// Record a failed unlock attempt and apply exponential backoff after threshold
    pub fn record_failed_attempt(&self) {
        let count = self.failed_attempts.fetch_add(1, Ordering::SeqCst) + 1;
        if count >= THROTTLE_THRESHOLD {
            let excess = (count - THROTTLE_THRESHOLD) as u32;
            let delay_secs = THROTTLE_BASE_DELAY_SECS
                .saturating_mul(1u64.checked_shl(excess.min(10)).unwrap_or(u64::MAX))
                .min(THROTTLE_MAX_DELAY_SECS);
            let now_ms = self.start_instant.elapsed().as_millis() as u64;
            self.throttle_until_ms.store(now_ms + delay_secs * 1000, Ordering::SeqCst);
            warn!(
                "Unlock throttled: {} failed attempts, backoff {}s",
                count, delay_secs
            );
        }
    }

    /// Reset failed attempt counter on successful unlock
    pub fn reset_throttle(&self) {
        self.failed_attempts.store(0, Ordering::SeqCst);
        self.throttle_until_ms.store(0, Ordering::SeqCst);
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
