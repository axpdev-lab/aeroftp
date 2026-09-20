// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Shared runtime transfer settings resolution.

use serde::{Deserialize, Serialize};

use crate::providers::ProviderType;
use crate::sftp_download_tuning::SftpDownloadPreset;
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
const DEFAULT_DOWNLOAD_SEGMENTS: u32 = 1;
pub const MIN_DOWNLOAD_SEGMENTS: u32 = 1;
pub const MAX_DOWNLOAD_SEGMENTS: u32 = 16;

/// Size above which a single download is split into streams on the GUI
/// paths: the same 250 MiB the CLI's `--multi-thread-cutoff` defaults to.
pub const DEFAULT_MULTI_THREAD_CUTOFF_BYTES: u64 = 250 * 1024 * 1024;

/// Intra-file download streams a provider gets when the user left the
/// setting on Auto. Measured on the lab (wired gigabit, 300 MiB, 2026-09-08,
/// two repetitions per point): SFTP 144.75 s at 1 to 25.46 s at 8; WebDAV
/// 37.60 s to 13.45 s at 8, ahead of rclone; FTP 52.95 s to 13.50 s at 8;
/// S3 36.83 s to 31.50 s at 4, a modest 14% with the noisiest single-stream
/// point. Providers without a measurement keep one stream: a default is a
/// claim about a measurement, not about a capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadSegmentsPreference {
    pub provider: ProviderType,
    pub count: u16,
    /// False means one stream because this provider has no measured default.
    pub measured: bool,
}

/// The one table used by capability snapshots and direct provider paths.
pub fn download_segments_preference_for(provider: ProviderType) -> DownloadSegmentsPreference {
    let (count, measured) = match provider {
        ProviderType::Sftp | ProviderType::WebDav | ProviderType::Ftp | ProviderType::Ftps => {
            (8, true)
        }
        ProviderType::S3 => (4, true),
        _ => (DEFAULT_DOWNLOAD_SEGMENTS as u16, false),
    };
    DownloadSegmentsPreference {
        provider,
        count,
        measured,
    }
}

pub fn default_download_segments_for(provider_type: ProviderType) -> u32 {
    u32::from(download_segments_preference_for(provider_type).count)
}

/// A caller must say whether it wants Auto, a user value, or a deliberate
/// single-stream path. There is deliberately no `Default` for this enum.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DownloadSegmentsRequest {
    MeasuredDefault,
    Explicit(u32),
    Single { reason: String },
}

impl From<Option<u32>> for DownloadSegmentsRequest {
    fn from(value: Option<u32>) -> Self {
        value.map_or(Self::MeasuredDefault, Self::Explicit)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DownloadSegmentsOrigin {
    Explicit,
    Measured { provider: ProviderType },
    Unmeasured { provider: ProviderType },
    SftpPreset { preset: SftpDownloadPreset },
    Single { reason: String },
    NoProvider,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedDownloadSegments {
    count: u32,
    origin: DownloadSegmentsOrigin,
}

impl ResolvedDownloadSegments {
    pub fn count(&self) -> u32 {
        self.count
    }
    pub fn origin(&self) -> &DownloadSegmentsOrigin {
        &self.origin
    }
    pub fn explicit(count: u32) -> Self {
        resolve_download_segments(&DownloadSegmentsRequest::Explicit(count), None)
    }
    pub fn sftp_preset(preset: SftpDownloadPreset) -> Self {
        Self {
            count: preset.resolve().connections as u32,
            origin: DownloadSegmentsOrigin::SftpPreset { preset },
        }
    }
}

impl std::fmt::Display for ResolvedDownloadSegments {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.origin {
            DownloadSegmentsOrigin::Explicit => write!(f, "{} (explicit)", self.count),
            DownloadSegmentsOrigin::Measured { provider } => {
                write!(f, "{} (measured default for {provider:?})", self.count)
            }
            DownloadSegmentsOrigin::Unmeasured { provider } => {
                write!(f, "{} (no measurement for {provider:?})", self.count)
            }
            DownloadSegmentsOrigin::SftpPreset { preset } => {
                write!(f, "{} (SFTP preset {preset})", self.count)
            }
            DownloadSegmentsOrigin::Single { reason } => {
                write!(f, "{} (single stream: {reason})", self.count)
            }
            DownloadSegmentsOrigin::NoProvider => {
                write!(f, "{} (no provider snapshot)", self.count)
            }
        }
    }
}

pub fn resolve_download_segments(
    request: &DownloadSegmentsRequest,
    preference: Option<DownloadSegmentsPreference>,
) -> ResolvedDownloadSegments {
    let (count, origin) = match request {
        DownloadSegmentsRequest::Explicit(count) => (*count, DownloadSegmentsOrigin::Explicit),
        DownloadSegmentsRequest::Single { reason } => (
            DEFAULT_DOWNLOAD_SEGMENTS,
            DownloadSegmentsOrigin::Single {
                reason: reason.clone(),
            },
        ),
        DownloadSegmentsRequest::MeasuredDefault => match preference {
            Some(preference) if preference.measured => (
                u32::from(preference.count),
                DownloadSegmentsOrigin::Measured {
                    provider: preference.provider,
                },
            ),
            Some(preference) => (
                u32::from(preference.count),
                DownloadSegmentsOrigin::Unmeasured {
                    provider: preference.provider,
                },
            ),
            None => (
                DEFAULT_DOWNLOAD_SEGMENTS,
                DownloadSegmentsOrigin::NoProvider,
            ),
        },
    };
    ResolvedDownloadSegments {
        count: count.clamp(MIN_DOWNLOAD_SEGMENTS, MAX_DOWNLOAD_SEGMENTS),
        origin,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferSettingsInput {
    #[serde(default)]
    pub max_concurrent: Option<u32>,
    #[serde(default)]
    pub retry_count: Option<u32>,
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
    pub download_segments: DownloadSegmentsRequest,
    #[serde(default)]
    pub sftp_download_preset: Option<SftpDownloadPreset>,
}

impl Default for TransferSettingsInput {
    fn default() -> Self {
        Self {
            max_concurrent: None,
            retry_count: None,
            timeout_seconds: None,
            download_segments: DownloadSegmentsRequest::MeasuredDefault,
            sftp_download_preset: None,
        }
    }
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedTransferSettings {
    pub requested_max_concurrent: u32,
    pub max_concurrent: u32,
    pub retry_count: u32,
    pub timeout_seconds: u64,
    /// Requested intra-file download segments (GTC-1). 1 = single-stream.
    /// The executor still gates on provider capability, file size, and
    /// session-pool kind at attempt time before honouring this value.
    pub download_segments: ResolvedDownloadSegments,
    /// Explicit SFTP product preset. `None` preserves legacy environment
    /// fallback and leaves provider tuning untouched.
    #[serde(default)]
    pub sftp_download_preset: Option<SftpDownloadPreset>,
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
            .with_resolved_buffer_budget()
    }
}

fn resolve_transfer_settings(
    input: TransferSettingsInput,
    caps: TransferCapabilityCaps,
    preference: Option<DownloadSegmentsPreference>,
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
        download_segments: match (preference, input.sftp_download_preset) {
            (Some(preference), Some(preset)) if preference.provider == ProviderType::Sftp => {
                ResolvedDownloadSegments::sftp_preset(preset)
            }
            _ => resolve_download_segments(&input.download_segments, preference),
        },
        sftp_download_preset: input.sftp_download_preset,
    }
}

pub fn resolve_ftp_transfer_settings(input: TransferSettingsInput) -> ResolvedTransferSettings {
    resolve_transfer_settings(input, TransferCapabilityCaps::default(), None)
}

/// Conservative provider settings when no live capability snapshot is available.
///
/// Prefer [`resolve_transfer_settings_for_capabilities`] on active provider
/// batch paths (DAG-P1-02). This helper remains the fail-closed serial clamp
/// for legacy callers that have not yet resolved runtime capabilities.
pub fn resolve_provider_transfer_settings(
    input: TransferSettingsInput,
) -> ResolvedTransferSettings {
    resolve_transfer_settings(
        input,
        TransferCapabilityCaps {
            max_concurrent_cap: 1,
            ..TransferCapabilityCaps::default()
        },
        None,
    )
}

/// Resolve transfer settings against a runtime capability snapshot (DAG-P1-02).
///
/// `requested_max_concurrent` preserves user intent; `max_concurrent` is the
/// effective value after clamping to `capabilities.max_file_slots` (and the
/// global 1..=8 bounds). Auto download streams use the provider preference
/// in this same snapshot. Callers must pass the composed runtime snapshot from the live
/// provider/executor — not protocol defaults alone.
pub fn resolve_transfer_settings_for_capabilities(
    input: TransferSettingsInput,
    capabilities: &TransferCapabilities,
) -> ResolvedTransferSettings {
    let requested = input
        .max_concurrent
        .unwrap_or(DEFAULT_MAX_CONCURRENT)
        .clamp(MIN_MAX_CONCURRENT, MAX_MAX_CONCURRENT);
    let budget = TransferBudget::from_file_slots(requested as u16)
        .with_resolved_buffer_budget()
        .clamped_for_capabilities(capabilities);

    resolve_transfer_settings(
        input,
        TransferCapabilityCaps {
            max_concurrent_cap: budget.file_slots as u32,
            ..TransferCapabilityCaps::default()
        },
        capabilities.preferred_download_segments,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_settings_without_capabilities_stay_serial() {
        // Fail-closed fallback when no live capability snapshot is available.
        let resolved = resolve_provider_transfer_settings(TransferSettingsInput {
            max_concurrent: Some(4),
            retry_count: Some(2),
            timeout_seconds: Some(45),
            download_segments: DownloadSegmentsRequest::MeasuredDefault,
            sftp_download_preset: None,
        });

        assert_eq!(resolved.requested_max_concurrent, 4);
        assert_eq!(resolved.max_concurrent, 1);
        assert_eq!(resolved.retry_count, 2);
        assert_eq!(resolved.timeout_seconds, 45);
    }

    #[test]
    fn capability_settings_raise_effective_concurrency_for_clone_pool() {
        // Requested 4 with a verified clone-capable ceiling >1 yields effective >1.
        let resolved = resolve_transfer_settings_for_capabilities(
            TransferSettingsInput {
                max_concurrent: Some(4),
                retry_count: Some(2),
                timeout_seconds: Some(45),
                download_segments: DownloadSegmentsRequest::MeasuredDefault,
                sftp_download_preset: None,
            },
            &TransferCapabilities {
                file_parallel: crate::transfer_dag::Capability::Supported,
                session_pool: crate::transfer_dag::Capability::Supported,
                max_file_slots: Some(8),
                ..TransferCapabilities::default()
            },
        );
        assert_eq!(resolved.requested_max_concurrent, 4);
        assert_eq!(resolved.max_concurrent, 4);
        assert_eq!(resolved.retry_count, 2);
        assert_eq!(resolved.timeout_seconds, 45);
    }

    #[test]
    fn capability_settings_clamp_requested_above_runtime_ceiling() {
        let resolved = resolve_transfer_settings_for_capabilities(
            TransferSettingsInput {
                max_concurrent: Some(8),
                retry_count: None,
                timeout_seconds: None,
                download_segments: DownloadSegmentsRequest::MeasuredDefault,
                sftp_download_preset: None,
            },
            &TransferCapabilities {
                max_file_slots: Some(3),
                ..TransferCapabilities::default()
            },
        );
        assert_eq!(resolved.requested_max_concurrent, 8);
        assert_eq!(resolved.max_concurrent, 3);
    }

    #[test]
    fn capability_settings_legacy_single_stays_at_one() {
        let resolved = resolve_transfer_settings_for_capabilities(
            TransferSettingsInput {
                max_concurrent: Some(4),
                ..TransferSettingsInput::default()
            },
            &TransferCapabilities::default(), // max_file_slots = Some(1)
        );
        assert_eq!(resolved.requested_max_concurrent, 4);
        assert_eq!(resolved.max_concurrent, 1);
    }

    #[test]
    fn download_segments_default_is_single_stream_without_a_provider() {
        // The plain resolver has no provider to look at; the per-protocol
        // default is filled by the runtime resolver from the live provider.
        let resolved = resolve_provider_transfer_settings(TransferSettingsInput::default());
        assert_eq!(
            resolved.download_segments.count(),
            DEFAULT_DOWNLOAD_SEGMENTS
        );
        assert_eq!(resolved.download_segments.count(), 1);
        assert_eq!(
            resolved.download_segments.origin(),
            &DownloadSegmentsOrigin::NoProvider
        );
    }

    #[test]
    fn auto_and_single_stream_have_distinct_serialized_provenance() {
        let caps = TransferCapabilities {
            preferred_download_segments: Some(download_segments_preference_for(ProviderType::S3)),
            ..TransferCapabilities::default()
        };
        let auto =
            resolve_transfer_settings_for_capabilities(TransferSettingsInput::default(), &caps);
        assert_eq!(auto.download_segments.count(), 4);
        let json = serde_json::to_value(&auto).unwrap();
        assert_eq!(json["download_segments"]["count"], 4);
        assert_eq!(json["download_segments"]["origin"]["kind"], "measured");
        assert_eq!(json["download_segments"]["origin"]["provider"], "s3");

        let single = resolve_transfer_settings_for_capabilities(
            TransferSettingsInput {
                download_segments: DownloadSegmentsRequest::Single {
                    reason: "legacy executor has no range path".to_string(),
                },
                ..TransferSettingsInput::default()
            },
            &caps,
        );
        assert_eq!(single.download_segments.count(), 1);
        let json = serde_json::to_value(&single).unwrap();
        assert_eq!(json["download_segments"]["origin"]["kind"], "single");
        assert_eq!(
            json["download_segments"]["origin"]["reason"],
            "legacy executor has no range path"
        );
    }

    #[test]
    fn sftp_preset_wins_and_reports_its_actual_count() {
        let caps = TransferCapabilities {
            preferred_download_segments: Some(download_segments_preference_for(ProviderType::Sftp)),
            ..TransferCapabilities::default()
        };
        let resolved = resolve_transfer_settings_for_capabilities(
            TransferSettingsInput {
                download_segments: DownloadSegmentsRequest::Explicit(2),
                sftp_download_preset: Some(SftpDownloadPreset::MaximumTested),
                ..TransferSettingsInput::default()
            },
            &caps,
        );
        assert_eq!(resolved.download_segments.count(), 12);
        assert_eq!(
            resolved.download_segments.origin(),
            &DownloadSegmentsOrigin::SftpPreset {
                preset: SftpDownloadPreset::MaximumTested,
            }
        );
    }

    #[test]
    fn default_download_segments_follow_the_measured_table() {
        assert_eq!(default_download_segments_for(ProviderType::Sftp), 8);
        assert_eq!(default_download_segments_for(ProviderType::WebDav), 8);
        assert_eq!(default_download_segments_for(ProviderType::Ftp), 8);
        assert_eq!(default_download_segments_for(ProviderType::Ftps), 8);
        assert_eq!(default_download_segments_for(ProviderType::S3), 4);
        // Not measured: one stream, not a guess.
        assert_eq!(default_download_segments_for(ProviderType::Backblaze), 1);
        assert_eq!(default_download_segments_for(ProviderType::Koofr), 1);
    }

    #[test]
    fn download_segments_clamp_above_max_drops_to_16() {
        let resolved = resolve_provider_transfer_settings(TransferSettingsInput {
            download_segments: DownloadSegmentsRequest::Explicit(99),
            ..TransferSettingsInput::default()
        });
        assert_eq!(resolved.download_segments.count(), MAX_DOWNLOAD_SEGMENTS);
    }

    #[test]
    fn download_segments_clamp_below_min_floors_to_1() {
        let resolved = resolve_provider_transfer_settings(TransferSettingsInput {
            download_segments: DownloadSegmentsRequest::Explicit(0),
            ..TransferSettingsInput::default()
        });
        assert_eq!(resolved.download_segments.count(), MIN_DOWNLOAD_SEGMENTS);
    }

    #[test]
    fn explicit_sftp_preset_survives_capability_resolution() {
        let resolved = resolve_transfer_settings_for_capabilities(
            TransferSettingsInput {
                sftp_download_preset: Some(SftpDownloadPreset::MaximumTested),
                ..TransferSettingsInput::default()
            },
            &TransferCapabilities::default(),
        );

        assert_eq!(
            resolved.sftp_download_preset,
            Some(SftpDownloadPreset::MaximumTested)
        );
        assert_eq!(
            resolved.sftp_download_preset.unwrap().resolve().connections,
            12
        );
    }

    #[test]
    fn capability_settings_clamp_to_dag_file_slots() {
        let resolved = resolve_transfer_settings_for_capabilities(
            TransferSettingsInput {
                max_concurrent: Some(6),
                retry_count: None,
                timeout_seconds: None,
                download_segments: DownloadSegmentsRequest::MeasuredDefault,
                sftp_download_preset: None,
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
                download_segments: DownloadSegmentsRequest::MeasuredDefault,
                sftp_download_preset: None,
            },
            &TransferCapabilities {
                max_file_slots: Some(2),
                ..TransferCapabilities::default()
            },
        );

        assert_eq!(resolved.transfer_budget().file_slots, 2);
    }
}
