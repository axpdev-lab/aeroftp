// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! `aeroftp_benchmark` for AeroAgent: the community benchmark on a saved
//! profile, the same engine and the same schema-v1 report as `aeroftp-cli
//! benchmark` and the MCP tool.
//!
//! It runs inside the app, on a connection of its own built from the vault the
//! app already holds open. The MCP tool spawns the CLI instead, which works
//! there because the MCP server is the CLI; from the app a child process would
//! have to reopen the vault, and a vault protected by a master password stays
//! locked to it.
//!
//! The run goes on a task of its own. Stopping the turn drops the tool call,
//! not the run: the run sees the turn's token, ends after its current step and
//! still removes its scratch folder from the remote.

use serde_json::Value;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::ai_core::local_tools::{get_str, get_str_opt};
use crate::ai_core::tools::{ToolCtx, ToolError};
use crate::community_benchmark::{
    self, benchmark_service_label, resolve_benchmark_plan, sanitized_report_json, BenchmarkEvent,
    BenchmarkLevel, BenchmarkObserver, BenchmarkOptions, BENCHMARK_TOTAL_TIMEOUT_SECS,
};
use crate::util::size::format_size;

const TOOL: &str = "aeroftp_benchmark";

fn invalid(reason: impl Into<String>) -> ToolError {
    ToolError::InvalidArgs {
        tool: TOOL.to_string(),
        reason: reason.into(),
    }
}

fn opt_u32(args: &Value, key: &str) -> Result<Option<u32>, ToolError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .map(Some)
            .ok_or_else(|| invalid(format!("{key} must be a non-negative integer"))),
    }
}

/// How many progress steps a plan has: one per timed or warmup transfer of
/// the size sweep, one per file of the many-small-files upload and download.
fn planned_steps(
    config: &community_benchmark::BenchmarkConfig,
    many_files: Option<community_benchmark::ManyFilesConfig>,
) -> u32 {
    let directions = ["upload", "download"]
        .iter()
        .filter(|op| config.operations.contains(op))
        .count() as u32;
    // Saturating: the resolver clamps runs, but the count does not rely on it.
    let sweep = u32::try_from(config.sizes_bytes.len())
        .unwrap_or(u32::MAX)
        .saturating_mul(config.warmup_runs.saturating_add(config.runs_per_size))
        .saturating_mul(directions);
    let many = many_files.map_or(0, |mf| mf.file_count.saturating_mul(2));
    sweep.saturating_add(many).max(1)
}

/// Whether step `current` of `total` is reported. A many-small-files run has
/// thousands of steps a few milliseconds apart, and one event each flooded
/// the webview (843 events in 1.5 s, measured on a loopback WebDAV); about a
/// hundred per run move the bar just as well. The last step is always sent.
fn reports_step(current: u32, total: u32) -> bool {
    let stride = (total / 100).max(1);
    current == total || current.is_multiple_of(stride)
}

/// AeroAgent's progress line for a run: the chat shows the step and the
/// phase, through the `ai-tool-progress` event every tool uses.
struct ToolProgressObserver {
    app: tauri::AppHandle,
    done: AtomicU32,
    total: u32,
}

impl ToolProgressObserver {
    fn step(&self, item: &str) {
        let current = (self.done.fetch_add(1, Ordering::Relaxed) + 1).min(self.total);
        if reports_step(current, self.total) {
            crate::ai_tools::emit_tool_progress(&self.app, TOOL, current, self.total, item);
        }
    }
}

impl BenchmarkObserver for ToolProgressObserver {
    fn event(&self, event: BenchmarkEvent<'_>) {
        match event {
            BenchmarkEvent::Run {
                operation,
                size,
                run,
                total,
                warmup,
            } => self.step(&format!(
                "{operation} {} run {run}/{total}{}",
                format_size(size),
                if warmup { " (warmup)" } else { "" }
            )),
            BenchmarkEvent::BatchItem { operation } => self.step(operation),
            _ => {}
        }
    }
}

pub async fn aeroftp_benchmark(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let app = ctx.tauri_app_handle().ok_or_else(|| {
        ToolError::Exec(
            "aeroftp_benchmark runs in the desktop app; from a terminal use `aeroftp-cli --profile <name> benchmark`"
                .to_string(),
        )
    })?;
    let profile = get_str(args, "profile")?;
    if args.get("all_protocols").and_then(Value::as_bool) == Some(true) {
        // The fan-out builds the sibling-mode profiles of a multi-protocol
        // account inside the CLI; refusing beats measuring one mode and
        // presenting it as the whole account.
        return Err(invalid(
            "all_protocols is not available here: run `aeroftp-cli --profile <name> benchmark --all-protocols`, or call this tool once per saved profile",
        ));
    }
    let level = match get_str_opt(args, "level") {
        None => BenchmarkLevel::Quick,
        Some(raw) => BenchmarkLevel::parse(&raw).ok_or_else(|| {
            invalid(format!(
                "unknown level '{raw}': quick, standard, deep or custom"
            ))
        })?,
    };
    let sizes = get_str_opt(args, "sizes");
    let operations = get_str_opt(args, "operations");
    let file_size = get_str_opt(args, "file_size").unwrap_or_else(|| "64K".to_string());
    let anonymize_extra = args
        .get("anonymize_extra")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let (config, many_files) = resolve_benchmark_plan(
        level,
        sizes.as_deref(),
        opt_u32(args, "runs")?,
        operations.as_deref(),
        opt_u32(args, "file_count")?,
        &file_size,
    )
    .map_err(invalid)?;

    let servers = crate::ai_tools::load_saved_servers().map_err(ToolError::Exec)?;
    let server = crate::ai_tools::find_server_by_name_or_id(&servers, &profile).map_err(invalid)?;
    let provider = crate::ai_tools::create_temp_provider(&server)
        .await
        .map_err(ToolError::Exec)?;
    let service = benchmark_service_label(provider.provider_type(), server.provider_id.as_deref());
    let initial_path = server.initial_path.clone().unwrap_or_default();

    let observer = ToolProgressObserver {
        app,
        done: AtomicU32::new(0),
        total: planned_steps(&config, many_files),
    };
    let options = BenchmarkOptions {
        level,
        config,
        many_files,
        consent_publish: false,
        anonymize_extra,
        profile_timeout_secs: BENCHMARK_TOTAL_TIMEOUT_SECS,
        test_root_prefix: None,
        pre_delete: false,
        direct_transfers: false,
        engine_override: crate::transfer_router::Override::None,
        check_public_ip: true,
        service,
        cancel: ctx.cancel_token().cloned(),
    };
    let run = tokio::spawn(async move {
        community_benchmark::run(provider, &initial_path, options, &observer).await
    });
    let outcome = run
        .await
        .map_err(|e| ToolError::Exec(format!("benchmark task ended abnormally: {e}")))?
        .map_err(ToolError::Exec)?;
    let json = sanitized_report_json(&outcome.report).map_err(ToolError::Exec)?;
    serde_json::from_str(&json).map_err(|e| ToolError::Exec(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::community_benchmark::{resolve_benchmark_plan, BenchmarkLevel};

    #[test]
    fn a_long_run_reports_about_a_hundred_steps_and_always_the_last() {
        let sent = |total: u32| (1..=total).filter(|c| reports_step(*c, total)).count();
        assert_eq!(sent(6000), 100);
        assert_eq!(sent(24), 24);
        assert!(reports_step(6001, 6001));
        assert!(sent(6001) <= 101);
    }

    #[test]
    fn the_progress_total_counts_every_transfer_the_plan_makes() {
        let (config, many) =
            resolve_benchmark_plan(BenchmarkLevel::Standard, None, None, None, None, "64K")
                .unwrap();
        // 3 sizes x (1 warmup + 3 runs) x (upload + download).
        assert_eq!(planned_steps(&config, many), 24);
        let (config, many) =
            resolve_benchmark_plan(BenchmarkLevel::Quick, None, None, None, Some(100), "4K")
                .unwrap();
        // The many-files workload alone: 100 uploads and 100 downloads.
        assert!(config.sizes_bytes.is_empty());
        assert_eq!(planned_steps(&config, many), 200);
    }

    #[test]
    fn the_progress_total_saturates_instead_of_overflowing() {
        // The resolver clamps runs to 20, but the count must not depend on it:
        // a plan built another way would panic here in a debug build.
        let config = community_benchmark::BenchmarkConfig {
            sizes_bytes: vec![1024; 3],
            runs_per_size: u32::MAX,
            warmup_runs: 1,
            operations: vec!["upload", "download"],
        };
        assert_eq!(planned_steps(&config, None), u32::MAX);
    }
}
