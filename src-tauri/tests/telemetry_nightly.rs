// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! DAG-P2-07 (block G): the nightly telemetry benchmark cell.
//!
//! A single deterministic, LOCAL fixture cell that exercises the block-D
//! process resource sampler and the block-E [`EngineTransferStats`] read-model
//! on genuinely-measured work, then serializes the result as JSON for the
//! nightly workflow to upload as an artifact. It needs NO network and NO lab
//! hardware: it moves bytes through a temp file on the runner's own disk.
//!
//! This is explicitly NOT a WAN or competitor benchmark. Those stay manual in
//! `tests/gtc/parity_harness.sh`, which needs real remote endpoints. The
//! nightly must run on a bare GitHub runner, so this cell measures only what a
//! bare runner can honestly measure: local I/O bytes, wall clock, and this
//! process' CPU/RSS/FD delta.
//!
//! Marked `#[ignore]` so the default `cargo test` gate skips it; the nightly
//! runs it with `--ignored`. Output path comes from `AEROFTP_TELEMETRY_JSON`
//! (default `telemetry-nightly.json` in the current dir); byte size from
//! `AEROFTP_TELEMETRY_BYTES` (default 16 MiB).

use std::io::{Read, Write};
use std::time::Instant;

use ftp_client_gui_lib::proc_stats::ResourceSampleGuard;
use ftp_client_gui_lib::transfer_dag::{EngineTransferStats, TransferDagMetrics};

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Move `bytes` through a temp file (write, flush, fsync, read back, verify)
/// and return the honest single-stream metrics plus the real wall clock.
fn run_local_io_cell(bytes: u64) -> (TransferDagMetrics, u64) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("telemetry-fixture.bin");

    // Deterministic payload so the cell is reproducible run to run.
    let payload: Vec<u8> = (0..bytes).map(|i| (i % 251) as u8).collect();

    let started = Instant::now();
    {
        let mut f = std::fs::File::create(&path).expect("create fixture");
        f.write_all(&payload).expect("write fixture");
        f.flush().expect("flush fixture");
        f.sync_all().expect("fsync fixture");
    }
    let mut readback = Vec::with_capacity(payload.len());
    std::fs::File::open(&path)
        .expect("open fixture")
        .read_to_end(&mut readback)
        .expect("read fixture");
    let wall = started.elapsed();

    assert_eq!(readback, payload, "local round-trip must be byte-identical");

    // Single-stream local copy: one runner, so run time is the wall time and
    // the concurrency high-water is one. Only measured quantities are set; no
    // TTFB moment exists for a local file, so those stay zero (never relabeled).
    let metrics = TransferDagMetrics {
        bytes_transferred: bytes,
        logical_bytes: bytes,
        wire_bytes: bytes,
        local_payload_bytes: bytes,
        run_nanos_total: wall.as_nanos().min(u64::MAX as u128) as u64,
        slot_peak: 1,
        ..Default::default()
    };

    (metrics, wall.as_millis() as u64)
}

#[test]
#[ignore = "nightly-only telemetry benchmark cell; run with --ignored"]
fn nightly_local_telemetry_cell_emits_json() {
    let bytes = env_u64("AEROFTP_TELEMETRY_BYTES", 16 * 1024 * 1024);
    let out_path =
        std::env::var("AEROFTP_TELEMETRY_JSON").unwrap_or_else(|_| "telemetry-nightly.json".into());

    // Bracket the whole cell with the block-D process resource sampler.
    let guard = ResourceSampleGuard::begin();
    let (metrics, wall_ms) = run_local_io_cell(bytes);
    let resources = guard.and_then(|g| g.finish());

    let stats = EngineTransferStats::from_job(metrics, wall_ms, resources);

    // Deterministic gate: the byte triple is consistent and the cell actually
    // moved the requested payload.
    assert_eq!(stats.metrics.bytes_transferred, bytes);
    assert_eq!(stats.metrics.logical_bytes, bytes);
    assert_eq!(stats.metrics.slot_peak, 1);

    // Wrap the cell in a small labelled envelope so the uploaded artifact is
    // self-describing (schema version + what kind of cell produced it).
    let envelope = serde_json::json!({
        "spec_version": "1.0.0",
        "cell": "local_io_fixture",
        "note": "Local single-stream I/O telemetry, no WAN. WAN/competitor cells live in tests/gtc/parity_harness.sh.",
        "bytes": bytes,
        "stats": stats,
    });

    let rendered = serde_json::to_string_pretty(&envelope).expect("serialize telemetry json");
    std::fs::write(&out_path, rendered.as_bytes()).expect("write telemetry json");

    // The artifact must round-trip: a downstream reader can parse it back into
    // the same read-model.
    let reparsed: serde_json::Value =
        serde_json::from_str(&rendered).expect("telemetry json is valid");
    assert_eq!(reparsed["stats"]["metrics"]["bytes_transferred"], bytes);

    eprintln!(
        "nightly telemetry cell: {} bytes in {} ms -> {}",
        bytes, wall_ms, out_path
    );
}
