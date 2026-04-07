// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

//! Shared runtime transfer settings resolution.

use serde::{Deserialize, Serialize};

use crate::sync::RetryPolicy;

pub const DEFAULT_MAX_CONCURRENT: u32 = 5;
pub const DEFAULT_RETRY_COUNT: u32 = 3;
pub const DEFAULT_TIMEOUT_SECONDS: u64 = 30;
pub const MIN_MAX_CONCURRENT: u32 = 1;
pub const MAX_MAX_CONCURRENT: u32 = 8;
pub const MIN_RETRY_COUNT: u32 = 0;
pub const MAX_RETRY_COUNT: u32 = 5;
pub const MIN_TIMEOUT_SECONDS: u64 = 10;
pub const MAX_TIMEOUT_SECONDS: u64 = 300;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct TransferSettingsInput {
    #[serde(default)]
    pub max_concurrent: Option<u32>,
    #[serde(default)]
    pub retry_count: Option<u32>,
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TransferCapabilityCaps {
    pub max_concurrent_cap: u32,
    pub max_retry_cap: u32,
    pub min_timeout_seconds: u64,
    pub max_timeout_seconds: u64,
}

impl Default for TransferCapabilityCaps {
    fn default() -> Self {
        Self {
            max_concurrent_cap: MAX_MAX_CONCURRENT,
            max_retry_cap: MAX_RETRY_COUNT,
            min_timeout_seconds: MIN_TIMEOUT_SECONDS,
            max_timeout_seconds: MAX_TIMEOUT_SECONDS,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ResolvedTransferSettings {
    pub requested_max_concurrent: u32,
    pub max_concurrent: u32,
    pub retry_count: u32,
    pub timeout_seconds: u64,
}

impl ResolvedTransferSettings {
    pub fn retry_policy(&self) -> RetryPolicy {
        RetryPolicy {
            max_retries: self.retry_count,
            base_delay_ms: 500,
            max_delay_ms: 10_000,
            timeout_ms: self.timeout_seconds.saturating_mul(1000),
            backoff_multiplier: 2.0,
        }
    }
}

pub fn resolve_transfer_settings(
    input: TransferSettingsInput,
    caps: TransferCapabilityCaps,
) -> ResolvedTransferSettings {
    let requested_max_concurrent = input
        .max_concurrent
        .unwrap_or(DEFAULT_MAX_CONCURRENT)
        .clamp(MIN_MAX_CONCURRENT, MAX_MAX_CONCURRENT);

    ResolvedTransferSettings {
        requested_max_concurrent,
        max_concurrent: requested_max_concurrent.clamp(
            MIN_MAX_CONCURRENT,
            caps.max_concurrent_cap.clamp(MIN_MAX_CONCURRENT, MAX_MAX_CONCURRENT),
        ),
        retry_count: input
            .retry_count
            .unwrap_or(DEFAULT_RETRY_COUNT)
            .clamp(MIN_RETRY_COUNT, caps.max_retry_cap.clamp(MIN_RETRY_COUNT, MAX_RETRY_COUNT)),
        timeout_seconds: input
            .timeout_seconds
            .unwrap_or(DEFAULT_TIMEOUT_SECONDS)
            .clamp(
                caps.min_timeout_seconds.max(MIN_TIMEOUT_SECONDS),
                caps.max_timeout_seconds.max(caps.min_timeout_seconds),
            ),
    }
}

pub fn resolve_ftp_transfer_settings(input: TransferSettingsInput) -> ResolvedTransferSettings {
    resolve_transfer_settings(input, TransferCapabilityCaps::default())
}

pub fn resolve_provider_transfer_settings(
    input: TransferSettingsInput,
) -> ResolvedTransferSettings {
    resolve_transfer_settings(
        input,
        TransferCapabilityCaps {
            max_concurrent_cap: 1,
            ..TransferCapabilityCaps::default()
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_settings_are_serialized_until_multi_session_support_exists() {
        let resolved = resolve_provider_transfer_settings(TransferSettingsInput {
            max_concurrent: Some(4),
            retry_count: Some(2),
            timeout_seconds: Some(45),
        });

        assert_eq!(resolved.requested_max_concurrent, 4);
        assert_eq!(resolved.max_concurrent, 1);
        assert_eq!(resolved.retry_count, 2);
        assert_eq!(resolved.timeout_seconds, 45);
    }
}
