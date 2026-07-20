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
    /// Total time dispatched nodes spent waiting before their runner started
    /// (adaptive-permit plus resource-lease acquisition), summed over nodes.
    #[serde(default)]
    pub wait_nanos_total: u64,
    /// Total runner execution time summed over dispatched nodes. Concurrent
    /// nodes each contribute their full duration, so this can exceed wall
    /// clock; divide by node count for a mean, never treat it as elapsed.
    #[serde(default)]
    pub run_nanos_total: u64,
    /// High-water mark of concurrently dispatched node tasks observed by the
    /// executor scheduling loop for this run.
    #[serde(default)]
    pub slot_peak: u32,
    /// Sum of time-to-first-byte samples measured at provider call
    /// boundaries where a first-byte moment is well defined (single GET/PUT,
    /// multipart part, ranged segment). Divide by `ttfb_samples` for a mean.
    /// Paths that cannot expose a first-byte moment honestly record NOTHING
    /// here; their operation-level latency is already covered by
    /// `run_nanos_total` and is never relabeled as TTFB.
    #[serde(default)]
    pub ttfb_nanos_total: u64,
    /// Number of samples folded into `ttfb_nanos_total`.
    #[serde(default)]
    pub ttfb_samples: u32,
    /// Native copy decisions that degraded to an observed download-upload
    /// graph, including capability-unavailable shaping.
    #[serde(default)]
    pub copy_fallbacks: u32,
}

/// DAG-P2-07 (block E): the single engine-level stats source for one completed
/// transfer job.
///
/// Every surface reads this one struct so they can never diverge: the CLI
/// `--json` output for folder/sync transfers embeds it in-band from the batch
/// result, the GUI receives it in a single job-end event, and the MCP
/// `aeroftp_transfer_stats` accessor reads the most recent one published to the
/// process-global store. It is a pure read-model: additive fields only, every
/// field `serde(default)` so an older reader still deserializes, and no field
/// is ever removed.
///
/// Honesty boundary: `metrics` is folded from real per-subgraph runs by the
/// streaming frontier (never fabricated), `wall_clock_ms` is real elapsed time
/// (NOT the summed per-node `run_nanos_total`, which double-counts concurrent
/// nodes), and `resources` is `None` wherever the platform sampler produced no
/// sample (non-Linux, or `/proc` unreadable) rather than a zeroed guess.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineTransferStats {
    /// Aggregated DAG metrics for the whole job (byte triple, retries,
    /// wait/run nanos, `slot_peak`, TTFB), folded from every subgraph.
    #[serde(default)]
    pub metrics: TransferDagMetrics,
    /// Real wall-clock elapsed for the whole job, in milliseconds.
    #[serde(default)]
    pub wall_clock_ms: u64,
    /// Process resource cost bracketing the job (CPU user/system nanos, RSS
    /// start/end, FD start/end). Absent when the sampler produced no sample.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<crate::proc_stats::ProcessResourceDelta>,
}

impl EngineTransferStats {
    /// Build a job stats snapshot from its three honest inputs.
    pub fn from_job(
        metrics: TransferDagMetrics,
        wall_clock_ms: u64,
        resources: Option<crate::proc_stats::ProcessResourceDelta>,
    ) -> Self {
        Self {
            metrics,
            wall_clock_ms,
            resources,
        }
    }

    /// Fold a second engine-job snapshot into this one. Used where a single
    /// caller-visible result covers more than one engine job (the CLI `sync`
    /// path runs an upload batch and a download batch under one `CliSyncResult`):
    /// the DAG metrics accumulate via [`TransferDagMetrics::absorb`], wall clock
    /// sums, and the resource delta keeps whichever job actually sampled (the
    /// second when present, else the first) rather than inventing a merged
    /// process delta across two disjoint brackets.
    pub fn absorb(&mut self, other: &EngineTransferStats) {
        self.metrics.absorb(&other.metrics);
        self.wall_clock_ms = self.wall_clock_ms.saturating_add(other.wall_clock_ms);
        if other.resources.is_some() {
            self.resources = other.resources;
        }
    }
}

impl TransferDagMetrics {
    /// Fold another run's metrics into this one. Additive counters sum;
    /// `slot_peak` is a high-water mark, so it keeps the max. Used where a
    /// job-level total aggregates per-subgraph runs (the batch/sync
    /// streaming frontier) so bytes, retries, and timing accumulate across
    /// the whole job rather than reflecting only the last subgraph.
    pub fn absorb(&mut self, other: &TransferDagMetrics) {
        self.logical_bytes = self.logical_bytes.saturating_add(other.logical_bytes);
        self.wire_bytes = self.wire_bytes.saturating_add(other.wire_bytes);
        self.local_payload_bytes = self
            .local_payload_bytes
            .saturating_add(other.local_payload_bytes);
        self.bytes_transferred = self
            .bytes_transferred
            .saturating_add(other.bytes_transferred);
        self.retries = self.retries.saturating_add(other.retries);
        self.backpressure_events = self
            .backpressure_events
            .saturating_add(other.backpressure_events);
        self.range_fallbacks = self.range_fallbacks.saturating_add(other.range_fallbacks);
        self.wait_nanos_total = self.wait_nanos_total.saturating_add(other.wait_nanos_total);
        self.run_nanos_total = self.run_nanos_total.saturating_add(other.run_nanos_total);
        self.slot_peak = self.slot_peak.max(other.slot_peak);
        self.ttfb_nanos_total = self.ttfb_nanos_total.saturating_add(other.ttfb_nanos_total);
        self.ttfb_samples = self.ttfb_samples.saturating_add(other.ttfb_samples);
        self.copy_fallbacks = self.copy_fallbacks.saturating_add(other.copy_fallbacks);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proc_stats::ProcessResourceDelta;

    fn delta(rss_end: u64) -> ProcessResourceDelta {
        ProcessResourceDelta {
            cpu_user_nanos: 10,
            cpu_system_nanos: 5,
            rss_start_bytes: 100,
            rss_end_bytes: rss_end,
            rss_peak_observed: None,
            fd_peak_observed: None,
            fd_start_count: 3,
            fd_end_count: 4,
        }
    }

    #[test]
    fn engine_stats_from_job_keeps_its_three_inputs() {
        let metrics = TransferDagMetrics {
            bytes_transferred: 500,
            ..Default::default()
        };
        let stats = EngineTransferStats::from_job(metrics.clone(), 42, Some(delta(200)));
        assert_eq!(stats.metrics, metrics);
        assert_eq!(stats.wall_clock_ms, 42);
        assert_eq!(stats.resources, Some(delta(200)));
    }

    #[test]
    fn engine_stats_absorb_sums_metrics_and_wall_clock() {
        let a_metrics = TransferDagMetrics {
            bytes_transferred: 100,
            slot_peak: 2,
            ..Default::default()
        };
        let b_metrics = TransferDagMetrics {
            bytes_transferred: 250,
            slot_peak: 5,
            ..Default::default()
        };

        let mut a = EngineTransferStats::from_job(a_metrics, 30, None);
        let b = EngineTransferStats::from_job(b_metrics, 70, Some(delta(300)));
        a.absorb(&b);

        assert_eq!(a.metrics.bytes_transferred, 350, "byte totals sum");
        assert_eq!(
            a.metrics.slot_peak, 5,
            "slot_peak keeps the high-water mark"
        );
        assert_eq!(a.wall_clock_ms, 100, "wall clock sums across the two jobs");
        assert_eq!(
            a.resources,
            Some(delta(300)),
            "the second job's sampled delta wins over the first's absent one"
        );
    }

    #[test]
    fn engine_stats_absorb_keeps_first_resources_when_second_absent() {
        let mut a =
            EngineTransferStats::from_job(TransferDagMetrics::default(), 10, Some(delta(200)));
        let b = EngineTransferStats::from_job(TransferDagMetrics::default(), 5, None);
        a.absorb(&b);
        assert_eq!(
            a.resources,
            Some(delta(200)),
            "an absent second delta must not erase the first"
        );
    }

    #[test]
    fn engine_stats_omits_and_defaults_absent_resources() {
        // Additive/back-compat: `None` resources is omitted on serialize
        // (skip_serializing_if) and defaults back to `None` on deserialize, so
        // a reader that predates the field is unaffected and the value
        // round-trips.
        let m = TransferDagMetrics {
            bytes_transferred: 7,
            ..Default::default()
        };
        let stats = EngineTransferStats::from_job(m, 12, None);
        let json = serde_json::to_string(&stats).expect("serialize");
        assert!(
            !json.contains("resources"),
            "absent resources must be omitted, not a null/zero: {json}"
        );
        let back: EngineTransferStats = serde_json::from_str(&json).expect("additive default");
        assert_eq!(back, stats);
        assert!(back.resources.is_none());
    }
}
