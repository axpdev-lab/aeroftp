// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use serde::{Deserialize, Serialize};

use crate::providers::{ProviderType, TransferOptimizationHints};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    Unsupported,
    Supported,
    SupportedAfterProbe,
    Experimental,
}

impl Capability {
    pub fn from_bool(value: bool) -> Self {
        if value {
            Self::Supported
        } else {
            Self::Unsupported
        }
    }

    pub fn is_available(self) -> bool {
        matches!(
            self,
            Self::Supported | Self::SupportedAfterProbe | Self::Experimental
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferCapabilities {
    pub file_parallel: Capability,
    pub session_pool: Capability,
    pub strict_concurrent_range_download: Capability,
    pub resume_download: Capability,
    pub resume_upload: Capability,
    pub multipart_upload: Capability,
    pub offset_upload: Capability,
    pub upload_session: Capability,
    pub server_side_copy: Capability,
    pub list_parallel: Capability,
    pub batch_list: Capability,
    pub server_checksum: Capability,
    pub atomic_rename: Capability,
    pub rate_limited_api: Capability,
    pub max_file_slots: Option<u16>,
    pub max_chunk_slots: Option<u16>,
    pub max_checker_slots: Option<u16>,
    pub preferred_chunk_size: Option<u64>,
    /// Minimum file size (bytes) at or above which an upload should fan out
    /// into multipart parts. Mirrors the per-provider `multipart_threshold`
    /// hint that the legacy `upload()` path honours. `0` means "unset": the
    /// shaping profile falls back to the chunk size, preserving the historical
    /// "fan out any file larger than one part" behaviour for providers that
    /// declare multipart without a threshold. Defaults to `u64::MAX` so a
    /// capability built without hints never accidentally fans out a small file.
    pub multipart_threshold: u64,
}

impl Default for TransferCapabilities {
    fn default() -> Self {
        Self {
            file_parallel: Capability::Unsupported,
            session_pool: Capability::Unsupported,
            strict_concurrent_range_download: Capability::Unsupported,
            resume_download: Capability::Unsupported,
            resume_upload: Capability::Unsupported,
            multipart_upload: Capability::Unsupported,
            offset_upload: Capability::Unsupported,
            upload_session: Capability::Unsupported,
            server_side_copy: Capability::Unsupported,
            list_parallel: Capability::Unsupported,
            batch_list: Capability::Unsupported,
            server_checksum: Capability::Unsupported,
            atomic_rename: Capability::Unsupported,
            rate_limited_api: Capability::Unsupported,
            max_file_slots: Some(1),
            max_chunk_slots: Some(1),
            max_checker_slots: Some(1),
            preferred_chunk_size: None,
            multipart_threshold: u64::MAX,
        }
    }
}

impl TransferCapabilities {
    pub fn from_provider_hints(
        provider_type: ProviderType,
        hints: &TransferOptimizationHints,
        supports_server_side_copy: bool,
    ) -> Self {
        let mut caps = Self {
            resume_download: Capability::from_bool(hints.supports_resume_download),
            resume_upload: Capability::from_bool(hints.supports_resume_upload),
            multipart_upload: Capability::from_bool(hints.supports_multipart),
            server_checksum: Capability::from_bool(hints.supports_server_checksum),
            server_side_copy: Capability::from_bool(supports_server_side_copy),
            preferred_chunk_size: (hints.multipart_part_size > 0)
                .then_some(hints.multipart_part_size),
            max_chunk_slots: Some(hints.multipart_max_parallel.max(1) as u16),
            // 0 = unset: the profile falls back to the chunk size (preserving
            // pre-fix behaviour). A provider that advertises multipart with a
            // very high threshold can opt out of fan-out without lying about
            // `supports_multipart` (the filen-s3 CreateMultipartUpload hazard).
            multipart_threshold: hints.multipart_threshold,
            ..Self::default()
        };

        match provider_type {
            ProviderType::Ftp | ProviderType::Ftps => {
                caps.file_parallel = Capability::Supported;
                caps.session_pool = Capability::Supported;
                caps.max_file_slots = Some(8);
                caps.max_checker_slots = Some(4);
            }
            ProviderType::S3
            | ProviderType::Azure
            | ProviderType::Swift
            | ProviderType::Backblaze => {
                // Backblaze B2 is S3-class object storage: its
                // `b2_download_file_by_name` endpoint honours `Range` natively
                // (HTTP 206), so concurrent range download needs no per-session
                // probe. Treat it like the other object stores rather than the
                // probe-gated WebDAV family.
                caps.strict_concurrent_range_download =
                    Capability::from_bool(hints.supports_range_download);
                caps.max_file_slots = Some(1);
                caps.max_checker_slots = Some(8);
            }
            ProviderType::WebDav | ProviderType::Koofr => {
                caps.strict_concurrent_range_download = if hints.supports_range_download {
                    Capability::SupportedAfterProbe
                } else {
                    Capability::Unsupported
                };
                caps.max_file_slots = Some(1);
                caps.max_checker_slots = Some(4);
            }
            ProviderType::Sftp => {
                // SFTP has range primitives, but until the shared SFTP pool
                // lands, provider-generic GUI transfers are a single lease.
                caps.session_pool = Capability::Unsupported;
                caps.strict_concurrent_range_download = Capability::Unsupported;
                caps.max_file_slots = Some(1);
                caps.max_checker_slots = Some(4);
            }
            ProviderType::GoogleDrive
            | ProviderType::GooglePhotos
            | ProviderType::Dropbox
            | ProviderType::OneDrive
            | ProviderType::Box
            | ProviderType::PCloud
            | ProviderType::ZohoWorkdrive
            | ProviderType::FourShared
            | ProviderType::YandexDisk
            | ProviderType::KDrive
            | ProviderType::Jottacloud
            | ProviderType::DrimeCloud
            | ProviderType::FileLu
            | ProviderType::OpenDrive => {
                caps.rate_limited_api = Capability::Supported;
                caps.max_file_slots = Some(1);
                caps.max_checker_slots = Some(4);
            }
            ProviderType::GitHub | ProviderType::GitLab => {
                caps.rate_limited_api = Capability::Supported;
                caps.max_file_slots = Some(1);
                caps.max_checker_slots = Some(2);
            }
            ProviderType::AeroCloud
            | ProviderType::Mega
            | ProviderType::Filen
            | ProviderType::Internxt
            | ProviderType::Immich
            | ProviderType::ImageKit
            | ProviderType::Uploadcare
            | ProviderType::Cloudinary
            // AeroShare peer drive: local replica reads, hint defaults apply.
            | ProviderType::Peer => {}
        }

        if !caps.multipart_upload.is_available() {
            caps.max_chunk_slots = caps.max_chunk_slots.map(|slots| slots.min(1));
        }

        caps
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_provider_defaults_to_single_file_lease() {
        let caps = TransferCapabilities::from_provider_hints(
            ProviderType::Sftp,
            &TransferOptimizationHints {
                supports_range_download: true,
                ..TransferOptimizationHints::default()
            },
            false,
        );

        assert_eq!(caps.file_parallel, Capability::Unsupported);
        assert_eq!(caps.session_pool, Capability::Unsupported);
        assert_eq!(caps.max_file_slots, Some(1));
        assert_eq!(
            caps.strict_concurrent_range_download,
            Capability::Unsupported
        );
    }

    #[test]
    fn ftp_advertises_the_existing_pool_without_overclaiming_range() {
        let caps = TransferCapabilities::from_provider_hints(
            ProviderType::Ftp,
            &TransferOptimizationHints {
                supports_resume_download: true,
                supports_resume_upload: true,
                supports_range_download: true,
                ..TransferOptimizationHints::default()
            },
            false,
        );

        assert_eq!(caps.file_parallel, Capability::Supported);
        assert_eq!(caps.session_pool, Capability::Supported);
        assert_eq!(caps.max_file_slots, Some(8));
        assert_eq!(
            caps.strict_concurrent_range_download,
            Capability::Unsupported
        );
    }

    #[test]
    fn webdav_range_is_probe_gated() {
        let caps = TransferCapabilities::from_provider_hints(
            ProviderType::WebDav,
            &TransferOptimizationHints {
                supports_range_download: true,
                ..TransferOptimizationHints::default()
            },
            false,
        );

        assert_eq!(
            caps.strict_concurrent_range_download,
            Capability::SupportedAfterProbe
        );
    }

    #[test]
    fn backblaze_range_is_supported_not_probe_gated() {
        // B2 is S3-class object storage: its download endpoint honours Range
        // natively, so concurrent range download is `Supported` (no per-session
        // probe), unlike the WebDAV family.
        let caps = TransferCapabilities::from_provider_hints(
            ProviderType::Backblaze,
            &TransferOptimizationHints {
                supports_range_download: true,
                ..TransferOptimizationHints::default()
            },
            false,
        );

        assert_eq!(caps.strict_concurrent_range_download, Capability::Supported);
        assert_eq!(caps.max_checker_slots, Some(8));
    }

    #[test]
    fn backblaze_range_off_when_hint_unset() {
        let caps = TransferCapabilities::from_provider_hints(
            ProviderType::Backblaze,
            &TransferOptimizationHints::default(),
            false,
        );

        assert_eq!(
            caps.strict_concurrent_range_download,
            Capability::Unsupported
        );
    }
}
