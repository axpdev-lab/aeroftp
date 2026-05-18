// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferDagMetrics {
    pub bytes_transferred: u64,
    pub retries: u32,
    pub backpressure_events: u32,
    pub range_fallbacks: u32,
}
