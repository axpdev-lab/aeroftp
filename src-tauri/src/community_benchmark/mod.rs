// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! The community benchmark: one measurement engine and one report schema for
//! every surface that runs it.
//!
//! The CLI `benchmark` command and AeroAgent's `aeroftp_benchmark` tool both
//! call [`run`] on a provider they connected themselves, so a report means the
//! same thing whichever of them produced it. The engine used to live inside the
//! CLI binary, and the MCP tool reached it by spawning that binary; a GUI
//! cannot do the same, because a child process reopens the vault on its own
//! and a vault protected by a master password stays locked to it ("Vault is
//! locked", exit 5).
//!
//! Schema contract: `docs/dev/roadmap/APPENDIX-BENCHMARK/01_JSON-Schema-v1.md`.
//! A report leaves the process only through [`sanitized_report_json`], which
//! substitutes every PII pattern and then refuses a payload that still matches
//! one.

mod engine;

pub use engine::{
    run, BenchmarkEvent, BenchmarkObserver, BenchmarkOptions, BenchmarkOutcome,
    BENCHMARK_LATENCY_SAMPLES,
};

use serde::{Deserialize, Serialize};

use crate::providers::{ProviderError, ProviderType};
use crate::util::size::{format_size, parse_size_filter};

/// Largest single payload, and largest many-small-files aggregate.
pub const BENCHMARK_MAX_SIZE_BYTES: u64 = 5 * 1024 * 1024 * 1024;
/// Default wall-clock cap of one profile's run.
pub const BENCHMARK_TOTAL_TIMEOUT_SECS: u64 = 60 * 60;
/// Upper bound on `--file-count` for the many-small-files workload. A run of
/// 100k metadata round-trips is already long enough to be meaningful; beyond
/// this the wall time is dominated by the workload itself, not the protocol.
pub const BENCHMARK_MAX_FILE_COUNT: u32 = 100_000;

/// Warning text shown when the public IP changed between the start and end of a
/// benchmark (issue #368 #1): the run spanned a VPN/network switch, so latency
/// and throughput numbers across profiles are not comparable. No raw IP is
/// included so the message is safe to surface in the report's error list.
pub const BENCHMARK_IP_CHANGED_WARNING: &str =
    "public IP changed during the benchmark (VPN/network switch detected): results across profiles are NOT comparable, re-run on a stable connection";

// ── Report (schema v1) ────────────────────────────────────────────────

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum BenchmarkLevel {
    Quick,
    Standard,
    Deep,
    Custom,
}

impl BenchmarkLevel {
    /// The level a tool argument names: `quick`, `standard`, `deep`, `custom`.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "quick" => Some(Self::Quick),
            "standard" => Some(Self::Standard),
            "deep" => Some(Self::Deep),
            "custom" => Some(Self::Custom),
            _ => None,
        }
    }
}

#[derive(Serialize)]
pub struct BenchmarkReport {
    pub schema_version: u32,
    pub report_id: String,
    pub generated_at: String,
    pub cli: BenchmarkCliMeta,
    pub level: BenchmarkLevel,
    // Access-method class of the profile (issue #277): the wire protocol for
    // transport providers (SFTP, WebDAV, S3, ...) or the auth/API class for
    // native ones (OAuth 2.0, OAuth 1.0, REST API). Additive to schema v1.
    #[serde(default)]
    pub access: String,
    // Service identity of the profile (issue #277): the catalog company for a
    // preconfigured preset (`Koofr`, `TAB.DIGITAL`), the provider itself for a
    // native API, or `Custom` for a generic transport aimed at the user's own
    // host. A fixed vocabulary drawn from the embedded catalog, never user
    // text, so the report stays as anonymous as it was. Additive to schema v1.
    #[serde(default)]
    pub service: String,
    // Transport mode this run measured when `--all-protocols` expanded one
    // profile into several runs (issue #277 B4): `api`, `webdav`, `s3` or `ftp`.
    // Absent for an ordinary single-mode run. Without it a JSON consumer cannot
    // tell the fan-out rows apart, because the per-mode label lives only in the
    // text table. A fixed vocabulary, never user text, so the report stays as
    // anonymous as it was. Additive to schema v1.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    pub environment: BenchmarkEnvironment,
    pub consent: BenchmarkConsent,
    pub results: Vec<BenchmarkResult>,
    pub summary: BenchmarkSummary,
    // Caveats about what the figures measure (#368): a Filen Desktop preset
    // measures the local bridge and its cache, not Filen. A fixed vocabulary,
    // never user text. Absent when empty. Additive to schema v1.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    // How responsive the service is from where the run stands (#368): the
    // round trip of a metadata request, as signal bars. Absent when the run
    // measured no round trip. Additive to schema v1.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal: Option<BenchmarkSignal>,
    // Remote paths of this run (start path, scratch folders). Shown in the
    // local text report, replaced by placeholders in every JSON that leaves
    // the process: a start path can carry an account name
    // (`/remote.php/dav/files/<user>`).
    #[serde(skip)]
    pub private_paths: Vec<(String, &'static str)>,
}

/// The responsiveness of a service, from the round trip of a metadata request
/// (`stat` of the file the run just uploaded).
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct BenchmarkSignal {
    /// 0 to 4 bars, see [`benchmark_signal_bars`]. Absent for a local bridge,
    /// whose round trip ends on this machine and says nothing about the
    /// service behind it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bars: Option<u8>,
    /// Median round trip.
    pub latency_ms: f64,
    /// p95 minus median: how much one request can stray from the usual one.
    pub jitter_ms: f64,
    /// Timed round trips the figures come from.
    pub samples: u32,
    /// The endpoint is a bridge on this machine (MEGAcmd, Filen Desktop, a
    /// loopback WebDAV or S3 server).
    pub local_bridge: bool,
    /// The client answers metadata from a tree it keeps in memory (the
    /// native MEGA client), so its round trip never reaches the service.
    /// No bars either. Absent when false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub metadata_cached: bool,
}

/// Median round trip at or under which a service gets N bars, best first.
pub const BENCHMARK_SIGNAL_THRESHOLDS_MS: [f64; 4] = [80.0, 200.0, 500.0, 1500.0];

/// Signal bars for a metadata round trip: 4 at or under 80 ms, 3 under
/// 200 ms, 2 under 500 ms, 1 under 1.5 s, 0 above. A service whose slow
/// requests stray further from its median than the median itself (p95 more
/// than twice p50) loses one bar: an unsteady service feels slower than its
/// median says.
pub fn benchmark_signal_bars(p50_ms: f64, p95_ms: f64) -> u8 {
    let mut bars = BENCHMARK_SIGNAL_THRESHOLDS_MS
        .iter()
        .position(|&limit| p50_ms <= limit)
        .map_or(0, |i| 4 - i as u8);
    if bars > 0 && p95_ms - p50_ms > p50_ms.max(1.0) {
        bars -= 1;
    }
    bars
}

/// The signal of a run, from its latency probe, or from the per-file `stat`
/// of the many-small-files workload when the run had no size sweep.
pub fn benchmark_signal_from(
    results: &[BenchmarkResult],
    local_bridge: bool,
    metadata_cached: bool,
) -> Option<BenchmarkSignal> {
    let source = results
        .iter()
        .find(|r| r.operation == "latency" && r.runs > 0)
        .or_else(|| {
            results
                .iter()
                .find(|r| r.operation == "stat-all" && r.runs > 0)
        })?;
    let stats = &source.latency_ms;
    Some(BenchmarkSignal {
        bars: (!local_bridge && !metadata_cached)
            .then(|| benchmark_signal_bars(stats.p50, stats.p95)),
        latency_ms: stats.p50,
        jitter_ms: (stats.p95 - stats.p50).max(0.0),
        samples: source.runs,
        local_bridge,
        metadata_cached,
    })
}

/// The concrete client a benchmark measured, as far as its labels need it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BenchmarkClient {
    /// MEGAcmd: commands to a MEGAcmd server running on this machine.
    MegaCmd,
    /// AeroFTP's own MEGA client.
    MegaNative,
    /// Proton's official `proton-drive` CLI, driven by AeroFTP.
    ProtonCli,
    /// AeroFTP's own Proton Drive client.
    ProtonNative,
    /// Anything else.
    Other,
}

/// What a run reports about how it reached the service: the Protocol label,
/// whether a round trip ends on this machine (a local bridge), and whether the
/// client answers metadata from its own cache. `access_label` is the provider
/// type's label, `local_endpoint` a loopback host, `has_note` a measurement
/// note (the Filen Desktop presets).
pub fn benchmark_endpoint_kind(
    client: BenchmarkClient,
    access_label: &str,
    local_endpoint: bool,
    has_note: bool,
) -> (String, bool, bool) {
    let access = match client {
        BenchmarkClient::MegaCmd | BenchmarkClient::ProtonCli => "CLI",
        BenchmarkClient::ProtonNative => "REST API",
        BenchmarkClient::MegaNative | BenchmarkClient::Other => access_label,
    }
    .to_string();
    // Only MEGAcmd talks to a server on this machine; Proton's official CLI
    // reaches the service on every call, so its round trip gets bars.
    let local_bridge = local_endpoint || has_note || client == BenchmarkClient::MegaCmd;
    let metadata_cached = !local_bridge
        && matches!(
            client,
            BenchmarkClient::MegaNative | BenchmarkClient::ProtonNative
        );
    (access, local_bridge, metadata_cached)
}

/// Whether a profile's host is this machine: a local bridge (MEGAcmd WebDAV,
/// Filen Desktop, a test server) rather than the service it fronts.
pub fn benchmark_is_loopback_host(host: &str) -> bool {
    let h = host.trim();
    let h = h
        .split_once("://")
        .map_or(h, |(_, rest)| rest)
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("");
    // Strip the port: `[::1]:4443`, `127.0.0.1:4443`, `localhost:8080`.
    let h = if let Some(rest) = h.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else if h.matches(':').count() == 1 {
        h.split(':').next().unwrap_or("")
    } else {
        h
    };
    let h = h.to_ascii_lowercase();
    h == "localhost"
        || h.ends_with(".localhost")
        || h == "::1"
        || h.parse::<std::net::Ipv4Addr>()
            .is_ok_and(|ip| ip.is_loopback())
}

#[derive(Serialize)]
pub struct BenchmarkCliMeta {
    pub version: String,
    pub build_target: String,
    pub rustc: String,
}

#[derive(Serialize)]
pub struct BenchmarkEnvironment {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asn_bucket: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub country_bucket: Option<String>,
    pub tod_bucket: String,
    pub os_family: String,
    pub os_arch: String,
    pub cpu_class: String,
}

#[derive(Serialize)]
pub struct BenchmarkConsent {
    pub publish: bool,
    pub anonymize_extra: bool,
}

#[derive(Serialize)]
pub struct BenchmarkResult {
    pub protocol: String,
    pub provider_hint: Option<String>,
    pub provider_hash: Option<String>,
    pub operation: String,
    pub payload_size_bytes: u64,
    pub runs: u32,
    pub warmup_runs_discarded: u32,
    // Many-small-files axis (schema v1, additive): number of files exercised by
    // a batch operation (upload-all / download-all / list-dir / stat-all /
    // delete-all) and the resulting files-per-second. Omitted for the
    // single-file size sweep, where `latency_ms` and `throughput_mbps` already
    // carry the meaningful numbers. See APPENDIX-BENCHMARK schema doc.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files_per_second: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub throughput_mbps: Option<BenchmarkStats>,
    pub latency_ms: BenchmarkStats,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tls_handshake_ms: Option<BenchmarkStats>,
    pub errors: BenchmarkErrors,
    pub raw_runs: Vec<BenchmarkRawRun>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct BenchmarkStats {
    pub p50: f64,
    pub p95: f64,
    pub stddev: f64,
    pub min: f64,
    pub max: f64,
}

#[derive(Serialize)]
pub struct BenchmarkErrors {
    pub transient: u32,
    pub fatal: u32,
}

#[derive(Serialize)]
pub struct BenchmarkRawRun {
    pub duration_ms: u64,
    pub bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub throughput_mbps: Option<f64>,
}

#[derive(Serialize)]
pub struct BenchmarkSummary {
    pub total_runs: u32,
    pub total_bytes_transferred: u64,
    pub total_duration_ms: u64,
    pub errors: Vec<String>,
}

/// Whether any result of the run counted a fatal error.
pub fn results_have_fatal(results: &[BenchmarkResult]) -> bool {
    results.iter().any(|r| r.errors.fatal > 0)
}

// ── Configuration ─────────────────────────────────────────────────────

/// The resolved single-file size sweep of one run.
#[derive(Debug, Clone)]
pub struct BenchmarkConfig {
    pub sizes_bytes: Vec<u64>,
    pub runs_per_size: u32,
    pub warmup_runs: u32,
    pub operations: Vec<&'static str>,
}

/// The sweep a level stands for, with the overrides applied. `sizes`, `runs`
/// and `operations` replace the level's own at any level, not only `custom`.
pub fn resolve_benchmark_config(
    level: BenchmarkLevel,
    sizes_override: Option<&str>,
    runs_override: Option<u32>,
    operations_override: Option<&str>,
) -> Result<BenchmarkConfig, String> {
    let mib: u64 = 1024 * 1024;
    let gib: u64 = 1024 * mib;
    let (default_sizes, default_runs, default_warmup, default_ops): (
        Vec<u64>,
        u32,
        u32,
        Vec<&'static str>,
    ) = match level {
        BenchmarkLevel::Quick => (vec![10 * mib], 1, 0, vec!["upload", "download", "latency"]),
        BenchmarkLevel::Standard => (
            vec![mib, 100 * mib, gib],
            3,
            1,
            vec!["upload", "download", "latency", "list", "stat", "delete"],
        ),
        BenchmarkLevel::Deep => (
            vec![mib, 10 * mib, 100 * mib, gib, 5 * gib],
            5,
            1,
            vec!["upload", "download", "latency", "list", "stat", "delete"],
        ),
        BenchmarkLevel::Custom => (
            vec![100 * mib],
            3,
            1,
            vec!["upload", "download", "latency", "list", "stat", "delete"],
        ),
    };

    let sizes_bytes = if let Some(s) = sizes_override {
        let parsed: Result<Vec<u64>, String> = s
            .split(',')
            .filter(|t| !t.trim().is_empty())
            .map(|tok| parse_benchmark_size(tok.trim()))
            .collect();
        let v = parsed?;
        if v.is_empty() {
            return Err("--sizes must contain at least one value".into());
        }
        v
    } else {
        default_sizes
    };

    for &sz in &sizes_bytes {
        if sz == 0 {
            return Err("benchmark size cannot be zero".into());
        }
        if sz > BENCHMARK_MAX_SIZE_BYTES {
            return Err(format!(
                "benchmark size {} exceeds 5 GiB cap",
                format_size(sz)
            ));
        }
    }

    let runs_per_size = runs_override.unwrap_or(default_runs).clamp(1, 20);

    let operations: Vec<&'static str> = if let Some(o) = operations_override {
        let mut out = Vec::new();
        for tok in o.split(',') {
            let trimmed = tok.trim();
            if trimmed.is_empty() {
                continue;
            }
            match trimmed {
                "upload" => out.push("upload"),
                "download" => out.push("download"),
                "latency" => out.push("latency"),
                "list" => out.push("list"),
                "stat" => out.push("stat"),
                "delete" => out.push("delete"),
                other => return Err(format!("unknown operation: {}", other)),
            }
        }
        if out.is_empty() {
            return Err("--operations must contain at least one value".into());
        }
        out
    } else {
        default_ops
    };

    Ok(BenchmarkConfig {
        sizes_bytes,
        runs_per_size,
        warmup_runs: default_warmup,
        operations,
    })
}

/// Parse a benchmark payload size (issue #277). Unlike the shared
/// [`parse_size_filter`], a bare number here means MEBIBYTES, not bytes: the
/// benchmark file-size and chunk-size domain is always discussed in MB, so
/// `--sizes 1,100,1024` reads as 1 MiB, 100 MiB, 1 GiB. Explicit K/M/G
/// suffixes keep their usual meaning (`1M`, `64K`, `1G`).
pub fn parse_benchmark_size(s: &str) -> Result<u64, String> {
    let t = s.trim();
    if t.is_empty() {
        return Err("Empty size".into());
    }
    match t.as_bytes().last() {
        Some(b'k' | b'K' | b'm' | b'M' | b'g' | b'G') => parse_size_filter(t),
        _ => t
            .parse::<f64>()
            .map(|n| (n * (1024.0 * 1024.0)) as u64)
            .map_err(|e| format!("Invalid size '{}': {}", t, e)),
    }
}

/// Resolved parameters for the many-small-files axis. Built only when a file
/// count is provided; the single-file size sweep is unaffected.
#[derive(Debug, Clone, Copy)]
pub struct ManyFilesConfig {
    pub file_count: u32,
    pub file_size_bytes: u64,
}

/// Validate and resolve the many-small-files workload parameters. Returns
/// `Ok(None)` when the workload is not requested (`file_count` is `None`/0).
pub fn resolve_many_files_config(
    file_count: Option<u32>,
    file_size: &str,
) -> Result<Option<ManyFilesConfig>, String> {
    let count = match file_count {
        Some(n) if n > 0 => n,
        _ => return Ok(None),
    };
    if count > BENCHMARK_MAX_FILE_COUNT {
        return Err(format!(
            "--file-count {} exceeds the {} cap",
            count, BENCHMARK_MAX_FILE_COUNT
        ));
    }
    let file_size_bytes = parse_benchmark_size(file_size)?;
    if file_size_bytes == 0 {
        return Err("--file-size cannot be zero".into());
    }
    // Bound the aggregate payload so a careless `--file-count 100000 --file-size
    // 1M` cannot try to push 100 GiB. The product is checked in u128 to avoid
    // overflow on the multiply.
    let total = file_size_bytes as u128 * count as u128;
    if total > BENCHMARK_MAX_SIZE_BYTES as u128 {
        return Err(format!(
            "many-files payload {} (file-count {} x file-size {}) exceeds the 5 GiB cap",
            format_size((total.min(u64::MAX as u128)) as u64),
            count,
            format_size(file_size_bytes)
        ));
    }
    Ok(Some(ManyFilesConfig {
        file_count: count,
        file_size_bytes,
    }))
}

/// The whole plan of a run from its arguments: the size sweep and the
/// optional many-small-files workload.
///
/// B6 (issue #277): when the many-small-files workload is requested and the
/// caller did NOT pass explicit sizes, the run does ONLY that workload and
/// skips the single-file size sweep. Ehud expected `--file-count N --file-size
/// S` to transfer N files of S, not also a separate single-file payload
/// alongside. Explicit sizes still run both axes (opt back in).
pub fn resolve_benchmark_plan(
    level: BenchmarkLevel,
    sizes_override: Option<&str>,
    runs_override: Option<u32>,
    operations_override: Option<&str>,
    file_count: Option<u32>,
    file_size: &str,
) -> Result<(BenchmarkConfig, Option<ManyFilesConfig>), String> {
    let many_files = resolve_many_files_config(file_count, file_size)?;
    let mut config =
        resolve_benchmark_config(level, sizes_override, runs_override, operations_override)?;
    if many_files.is_some() && sizes_override.is_none() {
        config.sizes_bytes.clear();
    }
    Ok((config, many_files))
}

// ── Statistics ────────────────────────────────────────────────────────

pub fn benchmark_percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    if sorted.len() == 1 {
        return sorted[0];
    }
    let rank = (p / 100.0) * (sorted.len() - 1) as f64;
    let lo = rank.floor() as usize;
    let hi = rank.ceil() as usize;
    if lo == hi {
        return sorted[lo];
    }
    let frac = rank - lo as f64;
    sorted[lo] * (1.0 - frac) + sorted[hi] * frac
}

pub fn benchmark_stats_from(values: &[f64]) -> BenchmarkStats {
    if values.is_empty() {
        return BenchmarkStats {
            p50: 0.0,
            p95: 0.0,
            stddev: 0.0,
            min: 0.0,
            max: 0.0,
        };
    }
    let mut sorted: Vec<f64> = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = sorted.len() as f64;
    let mean = sorted.iter().sum::<f64>() / n;
    let variance = sorted.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n;
    let stddev = variance.sqrt();
    BenchmarkStats {
        p50: benchmark_percentile(&sorted, 50.0),
        p95: benchmark_percentile(&sorted, 95.0),
        stddev,
        min: sorted[0],
        max: sorted[sorted.len() - 1],
    }
}

// ── Environment buckets ───────────────────────────────────────────────

fn benchmark_os_family() -> String {
    if cfg!(target_os = "linux") {
        "linux".into()
    } else if cfg!(target_os = "windows") {
        "windows".into()
    } else if cfg!(target_os = "macos") {
        "macos".into()
    } else if cfg!(target_os = "freebsd") {
        "freebsd".into()
    } else {
        "other".into()
    }
}

fn benchmark_cpu_class() -> String {
    let arch = std::env::consts::ARCH;
    match arch {
        "x86_64" => "x64-modern".into(),
        "aarch64" => "arm64-modern".into(),
        "x86" | "i686" => "x64-legacy".into(),
        "arm" | "armv7" => "arm64-legacy".into(),
        other => format!("other-{}", other),
    }
}

pub fn benchmark_tod_bucket() -> &'static str {
    use chrono::{Local, Timelike};
    let h = Local::now().hour();
    match h {
        0..=5 => "night",
        6..=11 => "morning",
        12..=17 => "afternoon",
        18..=23 => "evening",
        _ => "unknown",
    }
}

/// Best-effort public-IP probe for the benchmark fairness check (issue #368
/// #1). A single short HTTPS GET to an IP-echo endpoint; any failure (offline,
/// blocked, timeout) yields `None` and the fairness check is silently skipped,
/// so the benchmark never gains a hard network dependency. The value is only
/// compared locally to detect a VPN/network switch mid-run and is never written
/// to the published report (the sanitizer would redact an IPv4 anyway).
pub async fn benchmark_public_ip() -> Option<String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .ok()?;
    // api.ipify.org returns the bare IPv4/IPv6 as text/plain.
    let resp = client.get("https://api.ipify.org").send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let trimmed = resp.text().await.ok()?.trim().to_string();
    if trimmed.is_empty() || trimmed.len() > 64 {
        return None;
    }
    Some(trimmed)
}

fn benchmark_rounded_hour_utc() -> String {
    use chrono::{Datelike, Timelike, Utc};
    let now = Utc::now();
    format!(
        "{:04}-{:02}-{:02}T{:02}:00:00Z",
        now.year(),
        now.month(),
        now.day(),
        now.hour()
    )
}

pub fn benchmark_provider_hint(
    protocol: &str,
    anonymize_extra: bool,
    report_id: &str,
) -> (Option<String>, Option<String>) {
    if anonymize_extra {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(protocol.as_bytes());
        h.update(b"|");
        h.update(report_id.as_bytes());
        let hex = format!("{:x}", h.finalize());
        (None, Some(hex.chars().take(16).collect()))
    } else {
        (Some(protocol.to_string()), None)
    }
}

// ── Service identity ──────────────────────────────────────────────────

/// The catalog the CLI `catalog` command and the GUI "Add Service" list share,
/// generated from `src/components/providerCatalog.ts` (`npm run
/// gen:cli-catalog`). Read here only for the preset-id to company mapping.
const CATALOG_JSON: &str = include_str!("../cli_catalog.json");

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CatalogMethod {
    provider_id: Option<String>,
}

#[derive(Deserialize)]
struct CatalogCompany {
    company: String,
    protocols: Vec<CatalogMethod>,
}

/// Generic transports: the ones a user points at a host of their own, so the
/// profile carries no service identity beyond what the Protocol column says.
/// Every other provider type IS a service (Koofr, kDrive, MEGA, ...).
fn is_generic_transport(provider_type: ProviderType) -> bool {
    matches!(
        provider_type,
        ProviderType::Ftp
            | ProviderType::Ftps
            | ProviderType::Sftp
            | ProviderType::WebDav
            | ProviderType::S3
            | ProviderType::Swift
    )
}

/// Company display name for a catalog preset id, e.g. `koofr` -> `Koofr`,
/// `mega-s4` -> `MEGA`, `tabdigital` -> `TAB.DIGITAL`. `None` when the id is
/// not a known preset. Only ever used as a lookup key: the value returned comes
/// from the embedded catalog, never from user text, so it cannot leak anything
/// into a benchmark report.
fn catalog_company_for_provider_id(provider_id: &str) -> Option<String> {
    serde_json::from_str::<Vec<CatalogCompany>>(CATALOG_JSON)
        .ok()?
        .into_iter()
        .find_map(|c| {
            c.protocols
                .iter()
                .any(|m| m.provider_id.as_deref() == Some(provider_id))
                .then_some(c.company)
        })
}

/// Service identity for the benchmark "Server" column (issue #277, Ehud).
///
/// The column answers "who and where", so it must never repeat the Protocol
/// column, which answers "how". A profile built on a preconfigured preset
/// reports its service (`Koofr`, `TAB.DIGITAL`, `MEGA`); a native-API profile
/// reports the provider itself; and a profile that is only a generic transport
/// aimed at the user's own host reports `Custom`, because the old "WebDAV /
/// WebDAV" pair told the reader nothing the Protocol column had not said.
pub fn benchmark_service_label(provider_type: ProviderType, provider_id: Option<&str>) -> String {
    if provider_type == ProviderType::PCloud {
        return "pCloud Drive".to_string();
    }
    if let Some(company) = provider_id
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .and_then(catalog_company_for_provider_id)
    {
        return company;
    }
    if is_generic_transport(provider_type) {
        return "Custom".to_string();
    }
    provider_type.to_string()
}

// ── Scratch tree ──────────────────────────────────────────────────────

/// Where a run without `--test-root-prefix` writes: `aeroftp-bench` under the
/// profile's start path, and a per-run folder under it. The second value is
/// the per-run folder.
pub fn benchmark_remote_roots(initial_path: &str, report_id: &str) -> (String, String) {
    // Drop the leading dot: dot-prefixed folders (`.aeroftp-bench`) are rejected by
    // some providers (Internxt, certain WebDAV servers) that treat them as hidden
    // or invalid names. Plain `aeroftp-bench` is portable across all 22 backends.
    let base = initial_path.trim().trim_end_matches('/');
    let bench_base = if base.is_empty() {
        "aeroftp-bench".to_string()
    } else {
        format!("{}/aeroftp-bench", base)
    };
    let test_root = format!("{}/{}", bench_base, report_id);
    (bench_base, test_root)
}

/// The `--test-root-prefix` a run uses on `provider`, whose session starts
/// in `start_folder`. On Proton Drive a relative or empty prefix lands there
/// (My Files in CLI mode), as a relative path of any other command does,
/// instead of between the account's sections (Requested 79). An absolute
/// prefix, and any prefix on another provider, is taken as given: resolving it
/// against the profile's initial folder, as the CLI resolver does for other
/// commands, would move a relative prefix of a profile that has one.
pub fn benchmark_effective_prefix(
    prefix: &str,
    provider: ProviderType,
    start_folder: &str,
) -> String {
    let trimmed = prefix.trim();
    if provider != ProviderType::Proton || trimmed.starts_with('/') {
        return prefix.to_string();
    }
    let relative = trimmed.trim_matches('/');
    let start = start_folder.trim_end_matches('/');
    match (start.is_empty(), relative.is_empty()) {
        (true, true) => "/".to_string(),
        (false, true) => start.to_string(),
        _ => format!("{start}/{relative}"),
    }
}

pub fn benchmark_remote_roots_from_prefix(prefix: &str, report_id: &str) -> (String, String) {
    // Honor `--test-root-prefix` verbatim: the user has chosen a writable
    // sub-path (typically required for kDrive and SeaFile WebDAV which refuse
    // operations on `/`). We still nest a unique scratch dir under it so the
    // base prefix can be reused across runs without colliding.
    let trimmed = prefix.trim();
    let normalized = trimmed.trim_end_matches('/');
    let bench_base = if normalized.is_empty() {
        "/".to_string()
    } else if normalized.starts_with('/') {
        normalized.to_string()
    } else {
        format!("/{}", normalized)
    };
    let test_root = if bench_base == "/" {
        format!("/{}", report_id)
    } else {
        format!("{}/{}", bench_base, report_id)
    };
    (bench_base, test_root)
}

/// Pure path-splitting helper for a `mkdir -p`: turn a remote path into the
/// ordered list of cumulative ancestor paths to create, preserving a leading
/// slash for absolute paths. `"a/b/c"` yields `[a, a/b, a/b/c]`; `"/x/y"`
/// yields `[/x, /x/y]`. An empty (or slash-only) path yields `[]`.
pub fn benchmark_mkdir_ladder(path: &str) -> Vec<String> {
    let trimmed = path.trim_end_matches('/');
    if trimmed.trim_start_matches('/').is_empty() {
        return Vec::new();
    }
    let absolute = trimmed.starts_with('/');
    let mut acc = String::new();
    let mut out = Vec::new();
    for comp in trimmed.split('/').filter(|c| !c.is_empty()) {
        if acc.is_empty() && !absolute {
            acc.push_str(comp);
        } else {
            acc.push('/');
            acc.push_str(comp);
        }
        out.push(acc.clone());
    }
    out
}

/// The benchmark's word on the trash purge of `path`: `true` when it purged, a
/// line in `errors` when the provider refused or failed, nothing when there
/// was nothing to purge.
fn note_trash_purge(
    path: &str,
    outcome: Result<bool, ProviderError>,
    errors: &mut Vec<String>,
) -> bool {
    match outcome {
        Ok(purged) => purged,
        Err(e) => {
            errors.push(format!(
                "trash purge of {} failed, check the provider's trash: {}",
                path, e
            ));
            false
        }
    }
}

// ── Sanitization ──────────────────────────────────────────────────────

/// Pattern table reused by both [`benchmark_sanitize`] (replacement pass) and
/// [`benchmark_sanitization_sweep`] (assertion pass).
fn benchmark_pii_patterns() -> &'static [(&'static str, &'static str, &'static str)] {
    // (pattern, replacement, label)
    &[
        (r"AKIA[0-9A-Z]{16}", "AKIA<redacted>", "AWS access key"),
        (
            r"xox[baprs]-[0-9a-zA-Z-]{10,}",
            "xox<redacted>",
            "Slack token",
        ),
        (r"ghp_[A-Za-z0-9]{30,}", "ghp_<redacted>", "GitHub PAT"),
        (
            r"gho_[A-Za-z0-9]{30,}",
            "gho_<redacted>",
            "GitHub OAuth token",
        ),
        (
            r"eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]+",
            "<redacted-jwt>",
            "JWT",
        ),
        (
            r"\b(?:25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)\.(?:25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)\.(?:25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)\.(?:25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)\b",
            "<redacted-ip>",
            "IPv4 address",
        ),
        // Any URL: a provider error quotes the request it failed on
        // (`for url (https://host/path)`), which names the host of a
        // self-hosted server and the remote path.
        (
            r#"\b[A-Za-z][A-Za-z0-9+.-]{1,15}://[^\s"'<>\\]+"#,
            "<redacted-url>",
            "URL",
        ),
        // IPv6 has its own pass, see `redact_ipv6`: a pattern cannot tell
        // every compressed form from a clock time or a Rust path.
        // OS user folders. The name runs to the next separator, so a name
        // with a space (`/home/John Smith/`) is redacted whole.
        (r#"/home/[^/"<]+/"#, "/home/<redacted>/", "Linux home path"),
        (
            r#"[A-Za-z]:\\\\Users\\\\[^\\"<]+"#,
            r"C:\\Users\\<redacted>",
            "Windows user path",
        ),
        (
            r#"/Users/[^/"<]+/"#,
            "/Users/<redacted>/",
            "macOS user path",
        ),
        (
            r"\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}\b",
            "<redacted>@<redacted>",
            "email",
        ),
    ]
}

/// Runs of characters an IPv6 address is written with: hex digits, `:`, the
/// dots of an IPv4 tail and a `%` zone. Letters beyond `f` are included so a
/// word such as `std::fmt` is one run that does not parse, instead of a run
/// `::f` cut out of it.
fn ipv6_run_regex() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"[0-9A-Za-z:.%]+").expect("static regex"))
}

/// The first IPv6 address in a run, as a byte range inside it. A candidate
/// starts at the run's start or right after a `:` or a `.` (`host:2001:db8::1`,
/// `foo.2001:db8::1`), the earliest start first, and ends at the run's end or
/// before one of its `:` or `.`, the longest first, so the colon of
/// `"{addr}: {err}"` is left out (`2001:db8::1: Connection refused`), and so
/// is a word glued to the address by a dot (`fe80::1.local`), while an IPv4
/// tail stays whole because the longest end wins. A candidate is an address
/// when the standard parser accepts it with its trailing dots and `%zone`
/// removed, and it has two colons at least, so a clock time (`12:00:00`)
/// never is one. Every compressed form counts: `::1`, `fe80::1`,
/// `2001:db8::`, `::ffff:192.0.2.1`. So does a Rust path whose two segments
/// are hex (`abc::def`), which is redacted too: in a public report a word too
/// many is better than an address left in.
///
/// A candidate is at most [`IPV6_CANDIDATE_MAX`] bytes long, so the scan of a
/// run costs its length times that bound: a provider error can carry a long
/// hex dump, and an unbounded scan of every start and end was cubic in it.
fn ipv6_in_run(run: &str) -> Option<std::ops::Range<usize>> {
    let starts = std::iter::once(0).chain(
        run.char_indices()
            .filter(|(_, c)| *c == ':' || *c == '.')
            .map(|(i, _)| i + 1)
            .filter(|&i| i < run.len()),
    );
    for start in starts {
        let tail = &run[start..];
        // The run is ASCII (the regex class), so a byte bound is a char one.
        let window = &tail[..tail.len().min(IPV6_CANDIDATE_MAX)];
        let ends = (tail.len() <= IPV6_CANDIDATE_MAX)
            .then_some(tail.len())
            .into_iter()
            .chain(
                window
                    .char_indices()
                    .rev()
                    .filter(|(_, c)| *c == ':' || *c == '.')
                    .map(|(i, _)| i),
            );
        for end in ends {
            let candidate = tail[..end].trim_end_matches('.');
            let address = candidate.split('%').next().unwrap_or("");
            if address.matches(':').count() >= 2 && address.parse::<std::net::Ipv6Addr>().is_ok() {
                return Some(start..start + candidate.len());
            }
        }
    }
    None
}

/// Longest text an IPv6 candidate is tried at: a written address is 45
/// bytes at most (`ffff:...:255.255.255.255`), plus room for a `%zone`.
const IPV6_CANDIDATE_MAX: usize = 64;

/// The byte range of the first IPv6 address in `text`, if any.
fn first_ipv6_span(text: &str) -> Option<std::ops::Range<usize>> {
    ipv6_run_regex()
        .find_iter(text)
        .find_map(|m| ipv6_in_run(m.as_str()).map(|r| m.start() + r.start..m.start() + r.end))
}

/// `text` with every IPv6 address replaced by `<redacted-ip>`, all of them
/// in a run that holds several (a hex dump), not only the first.
pub(crate) fn redact_ipv6(text: &str) -> String {
    ipv6_run_regex()
        .replace_all(text, |caps: &regex::Captures<'_>| {
            let mut out = String::new();
            let mut rest = &caps[0];
            while let Some(r) = ipv6_in_run(rest) {
                out.push_str(&rest[..r.start]);
                out.push_str("<redacted-ip>");
                rest = &rest[r.end..];
            }
            out.push_str(rest);
            out
        })
        .into_owned()
}

/// Replacement pass: substitutes any PII match with a fixed placeholder so
/// the rest of the report (provider hint, error strings, summary) stays
/// usable. Errors emitted by remote providers often contain the username
/// (which is an email for most OAuth providers) or a server-side IP. Without
/// this pass, those reports were silently rejected by the sweep on otherwise
/// good runs (FeliCloud, Filen S3 in the 2026-05-07 community sweep).
pub fn benchmark_sanitize(serialized: String) -> String {
    // IPv6 first, so an address with an IPv4 tail (`::ffff:192.0.2.1`) goes
    // whole rather than losing only its tail to the IPv4 pattern.
    let mut out = redact_ipv6(&serialized);
    for (pat, replacement, _label) in benchmark_pii_patterns() {
        match regex::Regex::new(pat) {
            Ok(re) => {
                out = re.replace_all(&out, *replacement).into_owned();
            }
            Err(e) => {
                tracing::warn!(
                    "benchmark_sanitize: bad regex {} ({}); pattern skipped",
                    pat,
                    e
                );
            }
        }
    }
    out
}

/// Final assertion: after [`benchmark_sanitize`] has run, no PII pattern
/// should remain. If any does, that means [`benchmark_sanitize`] missed a
/// case (e.g. a new pattern was added to the table without a placeholder)
/// and we still refuse to write the report rather than risk leaking PII.
pub fn benchmark_sanitization_sweep(serialized: &str) -> Result<(), String> {
    if first_ipv6_span(serialized).is_some() {
        return Err("sanitization sweep matched IPv6 address".to_string());
    }
    for (pat, _repl, label) in benchmark_pii_patterns() {
        let re = regex::Regex::new(pat)
            .map_err(|e| format!("internal: bad sweep regex {}: {}", pat, e))?;
        if re.is_match(serialized) {
            return Err(format!("sanitization sweep matched {}", label));
        }
    }
    Ok(())
}

/// `value` as the pretty JSON a report leaves the process in: serialized,
/// every PII pattern substituted, then swept. The error is the reason the
/// payload was refused, and nothing is returned with it.
pub fn sanitized_report_json<T: Serialize + ?Sized>(value: &T) -> Result<String, String> {
    sanitized_report_json_scrubbing(value, &[])
}

/// [`sanitized_report_json`] for the reports of one or more runs: the remote
/// paths those runs used (see [`BenchmarkReport::private_paths`]) are replaced
/// by their placeholders first, wherever they appear, error text included.
pub fn sanitized_reports_json<T: Serialize + ?Sized>(
    value: &T,
    reports: &[&BenchmarkReport],
) -> Result<String, String> {
    let paths: Vec<(String, &'static str)> = reports
        .iter()
        .flat_map(|r| r.private_paths.iter().cloned())
        .collect();
    sanitized_report_json_scrubbing(value, &paths)
}

/// Replace each remote path in `serialized` by its placeholder, longest path
/// first so a run folder is not half-replaced by the base folder that
/// prefixes it. A path is matched in its JSON-escaped form, the form it has
/// in `serialized`. The root and one-character paths are left alone: they
/// would match everywhere and identify nothing.
pub fn scrub_private_paths(serialized: String, paths: &[(String, &'static str)]) -> String {
    let mut ordered: Vec<(String, &'static str)> = paths
        .iter()
        .map(|(path, placeholder)| (path.trim().trim_end_matches('/').to_string(), *placeholder))
        .filter(|(path, _)| path.trim_start_matches('/').chars().count() > 1)
        .collect();
    ordered.sort_by_key(|(path, _)| std::cmp::Reverse(path.len()));
    ordered.dedup_by(|a, b| a.0 == b.0);
    let mut out = serialized;
    for (path, placeholder) in ordered {
        let escaped = serde_json::to_string(&path).unwrap_or_default();
        let escaped = escaped.trim_matches('"');
        if !escaped.is_empty() {
            out = out.replace(escaped, placeholder);
        }
    }
    out
}

fn sanitized_report_json_scrubbing<T: Serialize + ?Sized>(
    value: &T,
    private_paths: &[(String, &'static str)],
) -> Result<String, String> {
    let serialized = serde_json::to_string_pretty(value)
        .map_err(|e| format!("could not serialize report: {}", e))?;
    let serialized = scrub_private_paths(serialized, private_paths);
    // First pass: substitute any PII (emails, IPs, OS path prefixes, cloud
    // tokens) with placeholders. Provider error strings frequently embed the
    // account email or a server IP that has no place in a public benchmark
    // report. Then run the assertion pass: if anything still matches, the
    // substitution table missed a case and we refuse rather than leak.
    let serialized = benchmark_sanitize(serialized);
    benchmark_sanitization_sweep(&serialized).map_err(|e| {
        format!(
            "report failed sanitization sweep after substitution: {}. \
             This is a bug in benchmark_sanitize patterns: please open an issue.",
            e
        )
    })?;
    Ok(serialized)
}

#[cfg(test)]
mod tests;
