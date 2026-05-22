// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Shared runtime transfer settings resolution.

use serde::{Deserialize, Serialize};

use crate::sync::RetryPolicy;
use crate::transfer_dag::{TransferBudget, TransferCapabilities};

pub const DEFAULT_MAX_CONCURRENT: u32 = 5;
pub const DEFAULT_RETRY_COUNT: u32 = 3;
pub const DEFAULT_TIMEOUT_SECONDS: u64 = 30;
pub const MIN_MAX_CONCURRENT: u32 = 1;
pub const MAX_MAX_CONCURRENT: u32 = 8;
pub const MIN_RETRY_COUNT: u32 = 0;
pub const MAX_RETRY_COUNT: u32 = 5;
pub const MIN_TIMEOUT_SECONDS: u64 = 10;
pub const MAX_TIMEOUT_SECONDS: u64 = 300;

/// Intra-file range parallelism (PD-Core / GTC-1).
///
/// `segments == 1` means single-stream legacy behaviour. Cap mirrors
/// `pget_effective_segments` in `bin/aeroftp_cli.rs` (capability gate
/// + runtime min-chunk anti-fragmentation is applied at attempt time).
pub const DEFAULT_DOWNLOAD_SEGMENTS: u32 = 1;
pub const MIN_DOWNLOAD_SEGMENTS: u32 = 1;
pub const MAX_DOWNLOAD_SEGMENTS: u32 = 16;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct TransferSettingsInput {
    #[serde(default)]
    pub max_concurrent: Option<u32>,
    #[serde(default)]
    pub retry_count: Option<u32>,
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
    #[serde(default)]
    pub download_segments: Option<u32>,
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
    /// Requested intra-file download segments (GTC-1). 1 = single-stream.
    /// The executor still gates on provider capability, file size, and
    /// session-pool kind at attempt time before honouring this value.
    #[serde(default = "default_download_segments")]
    pub download_segments: u32,
}

fn default_download_segments() -> u32 {
    DEFAULT_DOWNLOAD_SEGMENTS
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

    /// Build the DAG budget implied by the resolved concurrency setting.
    ///
    /// This keeps the Settings-panel knob (`maxConcurrentTransfers` on the
    /// frontend, `max_concurrent` in commands) and the graph scheduler's
    /// `file_slots` vocabulary tied to the same resolved value.
    pub fn transfer_budget(&self) -> TransferBudget {
        TransferBudget::from_file_slots(self.max_concurrent.min(u16::MAX as u32) as u16)
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
            caps.max_concurrent_cap
                .clamp(MIN_MAX_CONCURRENT, MAX_MAX_CONCURRENT),
        ),
        retry_count: input.retry_count.unwrap_or(DEFAULT_RETRY_COUNT).clamp(
            MIN_RETRY_COUNT,
            caps.max_retry_cap.clamp(MIN_RETRY_COUNT, MAX_RETRY_COUNT),
        ),
        timeout_seconds: input
            .timeout_seconds
            .unwrap_or(DEFAULT_TIMEOUT_SECONDS)
            .clamp(
                caps.min_timeout_seconds.max(MIN_TIMEOUT_SECONDS),
                caps.max_timeout_seconds.max(caps.min_timeout_seconds),
            ),
        download_segments: input
            .download_segments
            .unwrap_or(DEFAULT_DOWNLOAD_SEGMENTS)
            .clamp(MIN_DOWNLOAD_SEGMENTS, MAX_DOWNLOAD_SEGMENTS),
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

#[allow(dead_code)]
pub fn resolve_transfer_settings_for_capabilities(
    input: TransferSettingsInput,
    capabilities: &TransferCapabilities,
) -> ResolvedTransferSettings {
    let requested = input
        .max_concurrent
        .unwrap_or(DEFAULT_MAX_CONCURRENT)
        .clamp(MIN_MAX_CONCURRENT, MAX_MAX_CONCURRENT);
    let budget =
        TransferBudget::from_file_slots(requested as u16).clamped_for_capabilities(capabilities);

    resolve_transfer_settings(
        input,
        TransferCapabilityCaps {
            max_concurrent_cap: budget.file_slots as u32,
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
            download_segments: None,
        });

        assert_eq!(resolved.requested_max_concurrent, 4);
        assert_eq!(resolved.max_concurrent, 1);
        assert_eq!(resolved.retry_count, 2);
        assert_eq!(resolved.timeout_seconds, 45);
    }

    #[test]
    fn download_segments_default_is_single_stream() {
        let resolved = resolve_provider_transfer_settings(TransferSettingsInput::default());
        assert_eq!(resolved.download_segments, DEFAULT_DOWNLOAD_SEGMENTS);
        assert_eq!(resolved.download_segments, 1);
    }

    #[test]
    fn download_segments_clamp_above_max_drops_to_16() {
        let resolved = resolve_provider_transfer_settings(TransferSettingsInput {
            download_segments: Some(99),
            ..TransferSettingsInput::default()
        });
        assert_eq!(resolved.download_segments, MAX_DOWNLOAD_SEGMENTS);
    }

    #[test]
    fn download_segments_clamp_below_min_floors_to_1() {
        let resolved = resolve_provider_transfer_settings(TransferSettingsInput {
            download_segments: Some(0),
            ..TransferSettingsInput::default()
        });
        assert_eq!(resolved.download_segments, MIN_DOWNLOAD_SEGMENTS);
    }

    #[test]
    fn capability_settings_clamp_to_dag_file_slots() {
        let resolved = resolve_transfer_settings_for_capabilities(
            TransferSettingsInput {
                max_concurrent: Some(6),
                retry_count: None,
                timeout_seconds: None,
                download_segments: None,
            },
            &TransferCapabilities {
                max_file_slots: Some(2),
                ..TransferCapabilities::default()
            },
        );

        assert_eq!(resolved.requested_max_concurrent, 6);
        assert_eq!(resolved.max_concurrent, 2);
    }

    #[test]
    fn resolved_settings_budget_uses_effective_max_concurrent() {
        let resolved = resolve_transfer_settings_for_capabilities(
            TransferSettingsInput {
                max_concurrent: Some(6),
                retry_count: None,
                timeout_seconds: None,
                download_segments: None,
            },
            &TransferCapabilities {
                max_file_slots: Some(2),
                ..TransferCapabilities::default()
            },
        );

        assert_eq!(resolved.transfer_budget().file_slots, 2);
    }
}
