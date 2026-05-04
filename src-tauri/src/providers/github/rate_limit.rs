//! GitHub API rate limit tracking
//!
//! Extracts `X-RateLimit-*` headers from every API response and maintains
//! a local snapshot so the provider can warn, pause, or surface quota info
//! without an extra round-trip.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use std::time::{SystemTime, UNIX_EPOCH};

/// Snapshot of the current rate-limit state for the authenticated token.
#[derive(Debug, Clone)]
pub struct RateLimitState {
    /// Requests remaining in the current window.
    pub remaining: u32,
    /// Maximum requests per window (typically 5 000 for token auth).
    pub limit: u32,
    /// Unix timestamp when the window resets.
    pub reset_at: u64,
}

impl RateLimitState {
    /// Create an optimistic initial state (assumes full quota).
    pub fn new() -> Self {
        Self {
            remaining: 5000,
            limit: 5000,
            reset_at: 0,
        }
    }

    /// Update from GitHub response headers.
    ///
    /// Silently ignores missing or malformed headers: the state simply
    /// stays at its previous values.
    pub fn update_from_headers(&mut self, headers: &reqwest::header::HeaderMap) {
        if let Some(v) = headers.get("x-ratelimit-remaining") {
            if let Ok(s) = v.to_str() {
                if let Ok(n) = s.parse::<u32>() {
                    self.remaining = n;
                }
            }
        }
        if let Some(v) = headers.get("x-ratelimit-limit") {
            if let Ok(s) = v.to_str() {
                if let Ok(n) = s.parse::<u32>() {
                    self.limit = n;
                }
            }
        }
        if let Some(v) = headers.get("x-ratelimit-reset") {
            if let Ok(s) = v.to_str() {
                if let Ok(n) = s.parse::<u64>() {
                    self.reset_at = n;
                }
            }
        }
    }

    /// `true` when fewer than 10 % of requests remain.
    #[allow(dead_code)]
    pub fn should_warn(&self) -> bool {
        self.limit > 0 && self.remaining < self.limit / 10
    }

    /// `true` when the quota is fully exhausted.
    #[allow(dead_code)]
    pub fn is_exhausted(&self) -> bool {
        self.remaining == 0 && self.reset_at > 0
    }

    /// Seconds until the current window resets (clamped to 0).
    #[allow(dead_code)]
    pub fn seconds_until_reset(&self) -> u64 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.reset_at.saturating_sub(now)
    }

    /// Human-readable warning string.
    #[allow(dead_code)]
    pub fn format_warning(&self) -> String {
        format!(
            "GitHub API: {}/{} requests remaining, resets in {}m",
            self.remaining,
            self.limit,
            self.seconds_until_reset() / 60,
        )
    }
}

impl Default for RateLimitState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_state_is_optimistic() {
        let s = RateLimitState::new();
        assert_eq!(s.remaining, 5000);
        assert_eq!(s.limit, 5000);
        assert!(!s.should_warn());
        assert!(!s.is_exhausted());
    }

    #[test]
    fn test_should_warn_at_10_percent() {
        let s = RateLimitState {
            remaining: 499,
            limit: 5000,
            reset_at: 0,
        };
        assert!(s.should_warn());
    }

    #[test]
    fn test_exhausted() {
        let s = RateLimitState {
            remaining: 0,
            limit: 5000,
            reset_at: 9999999999,
        };
        assert!(s.is_exhausted());
    }

    #[test]
    fn test_update_from_headers() {
        let mut s = RateLimitState::new();
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("x-ratelimit-remaining", "42".parse().unwrap());
        headers.insert("x-ratelimit-limit", "5000".parse().unwrap());
        headers.insert("x-ratelimit-reset", "1700000000".parse().unwrap());
        s.update_from_headers(&headers);
        assert_eq!(s.remaining, 42);
        assert_eq!(s.limit, 5000);
        assert_eq!(s.reset_at, 1700000000);
    }
}
