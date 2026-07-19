// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferDagMetrics {
    /// User-visible bytes represented by the logical operation.
    ///
    /// A server-side copy reports the source size here even though no payload
    /// crosses the local client.
    #[serde(default)]
    pub logical_bytes: u64,
    /// Payload bytes carried over the local client's provider data path.
    ///
    /// Native server-side copy is zero. Download-then-upload copy is twice
    /// `logical_bytes` after both legs complete.
    #[serde(default)]
    pub wire_bytes: u64,
    /// Payload bytes materialized locally while executing the operation.
    ///
    /// This stays zero for native copy and equals the temporary file size for
    /// download-then-upload copy.
    #[serde(default)]
    pub local_payload_bytes: u64,
    pub bytes_transferred: u64,
    pub retries: u32,
    pub backpressure_events: u32,
    pub range_fallbacks: u32,
    /// Native copy decisions that degraded to an observed download-upload
    /// graph, including capability-unavailable shaping.
    #[serde(default)]
    pub copy_fallbacks: u32,
}
