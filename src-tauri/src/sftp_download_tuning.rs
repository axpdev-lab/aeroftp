// SPDX-License-Identifier: GPL-3.0-or-later

//! Shared SFTP download presets used by the CLI and GUI surfaces.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

pub const SFTP_PRESET_MULTI_CONNECTION_CUTOFF: u64 = 250 * 1024 * 1024;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SftpDownloadPreset {
    Compatibility,
    Efficient,
    Balanced,
    /// The product default since the DAG engine review: eight connections
    /// took a 300 MiB download from 144.75 s to 25.46 s on the lab.
    #[default]
    Fast,
    MaximumTested,
}

impl SftpDownloadPreset {
    pub const ALL: [Self; 5] = [
        Self::Compatibility,
        Self::Efficient,
        Self::Balanced,
        Self::Fast,
        Self::MaximumTested,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Compatibility => "compatibility",
            Self::Efficient => "efficient",
            Self::Balanced => "balanced",
            Self::Fast => "fast",
            Self::MaximumTested => "maximum-tested",
        }
    }

    pub const fn resolve(self) -> ResolvedSftpDownloadTuning {
        let (connections, readahead_window) = match self {
            Self::Compatibility => (1, None),
            Self::Efficient => (1, Some(16)),
            Self::Balanced => (4, Some(16)),
            Self::Fast => (8, Some(16)),
            Self::MaximumTested => (12, Some(16)),
        };
        ResolvedSftpDownloadTuning {
            preset: self,
            connections,
            readahead_window,
            multi_connection_cutoff: SFTP_PRESET_MULTI_CONNECTION_CUTOFF,
        }
    }
}

impl fmt::Display for SftpDownloadPreset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for SftpDownloadPreset {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let normalized = raw.trim().to_ascii_lowercase();
        Self::ALL
            .into_iter()
            .find(|preset| preset.as_str() == normalized)
            .ok_or_else(|| format!("unknown SFTP download preset: {raw}"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedSftpDownloadTuning {
    pub preset: SftpDownloadPreset,
    pub connections: usize,
    pub readahead_window: Option<usize>,
    pub multi_connection_cutoff: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fast_is_the_eight_connection_product_default() {
        assert_eq!(SftpDownloadPreset::default(), SftpDownloadPreset::Fast);
        assert_eq!(
            SftpDownloadPreset::default().resolve(),
            ResolvedSftpDownloadTuning {
                preset: SftpDownloadPreset::Fast,
                connections: 8,
                readahead_window: Some(16),
                multi_connection_cutoff: SFTP_PRESET_MULTI_CONNECTION_CUTOFF,
            }
        );
    }

    #[test]
    fn preset_table_matches_the_audited_configurations() {
        let expected = [
            (SftpDownloadPreset::Compatibility, 1, None),
            (SftpDownloadPreset::Efficient, 1, Some(16)),
            (SftpDownloadPreset::Balanced, 4, Some(16)),
            (SftpDownloadPreset::Fast, 8, Some(16)),
            (SftpDownloadPreset::MaximumTested, 12, Some(16)),
        ];
        for (preset, connections, readahead_window) in expected {
            let resolved = preset.resolve();
            assert_eq!(resolved.connections, connections);
            assert_eq!(resolved.readahead_window, readahead_window);
            assert_eq!(
                resolved.multi_connection_cutoff,
                SFTP_PRESET_MULTI_CONNECTION_CUTOFF
            );
        }
    }

    #[test]
    fn stable_ids_parse_and_round_trip_through_serde() {
        for preset in SftpDownloadPreset::ALL {
            assert_eq!(preset.as_str().parse::<SftpDownloadPreset>(), Ok(preset));
            let json = serde_json::to_string(&preset).unwrap();
            assert_eq!(json, format!("\"{}\"", preset.as_str()));
            assert_eq!(
                serde_json::from_str::<SftpDownloadPreset>(&json).unwrap(),
                preset
            );
        }
        assert!("maximum".parse::<SftpDownloadPreset>().is_err());
    }
}
