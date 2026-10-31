// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use super::engine::{many_files_result, purge_deleted_files_with_identities, scratch_dir_error};
use super::*;

#[test]
fn test_parse_benchmark_size_bare_number_is_mib() {
    // Issue #277: a bare benchmark size is MiB, not bytes.
    assert_eq!(parse_benchmark_size("1").unwrap(), 1024 * 1024);
    assert_eq!(parse_benchmark_size("10").unwrap(), 10 * 1024 * 1024);
    assert_eq!(parse_benchmark_size(" 100 ").unwrap(), 100 * 1024 * 1024);
    // Explicit suffixes keep their 1024-power meaning.
    assert_eq!(parse_benchmark_size("64K").unwrap(), 64 * 1024);
    assert_eq!(parse_benchmark_size("4M").unwrap(), 4 * 1024 * 1024);
    assert_eq!(parse_benchmark_size("1G").unwrap(), 1024 * 1024 * 1024);
    assert!(parse_benchmark_size("").is_err());
    assert!(parse_benchmark_size("abc").is_err());
}

#[test]
fn test_many_files_config_bare_file_size_is_mib() {
    // `--file-count 10 --file-size 1` must be ten 1 MiB files, not 1-byte.
    let mf = resolve_many_files_config(Some(10), "1")
        .expect("valid config")
        .expect("workload requested");
    assert_eq!(mf.file_count, 10);
    assert_eq!(mf.file_size_bytes, 1024 * 1024);
}

#[test]
fn benchmark_percentile_handles_edge_cases() {
    assert_eq!(benchmark_percentile(&[], 50.0), 0.0);
    assert_eq!(benchmark_percentile(&[42.0], 50.0), 42.0);
    assert_eq!(benchmark_percentile(&[42.0], 95.0), 42.0);
    let v = vec![1.0, 2.0, 3.0, 4.0, 5.0];
    assert_eq!(benchmark_percentile(&v, 50.0), 3.0);
    assert!((benchmark_percentile(&v, 95.0) - 4.8).abs() < 1e-9);
    assert_eq!(benchmark_percentile(&v, 0.0), 1.0);
    assert_eq!(benchmark_percentile(&v, 100.0), 5.0);
}

#[test]
fn benchmark_stats_reject_mean_field_only_returns_p50_p95() {
    let v = vec![10.0, 20.0, 30.0, 40.0, 50.0];
    let stats = benchmark_stats_from(&v);
    assert_eq!(stats.min, 10.0);
    assert_eq!(stats.max, 50.0);
    assert_eq!(stats.p50, 30.0);
    // The schema explicitly forbids `mean`; verify Serialize does not
    // emit one and that the only fields are the documented five.
    let json = serde_json::to_value(&stats).unwrap();
    let obj = json.as_object().expect("stats is an object");
    let mut keys: Vec<&str> = obj.keys().map(|k| k.as_str()).collect();
    keys.sort();
    assert_eq!(keys, vec!["max", "min", "p50", "p95", "stddev"]);
}

#[test]
fn benchmark_stats_stddev_is_population() {
    // variance = ((-2)^2 + (-1)^2 + 0 + 1^2 + 2^2) / 5 = 10/5 = 2
    // stddev = sqrt(2) ≈ 1.41421356
    let v = vec![1.0, 2.0, 3.0, 4.0, 5.0];
    let stats = benchmark_stats_from(&v);
    assert!((stats.stddev - 2_f64.sqrt()).abs() < 1e-9);
}

#[test]
fn benchmark_resolve_config_quick_defaults() {
    let cfg = resolve_benchmark_config(BenchmarkLevel::Quick, None, None, None).unwrap();
    assert_eq!(cfg.sizes_bytes, vec![10 * 1024 * 1024]);
    assert_eq!(cfg.runs_per_size, 1);
    assert_eq!(cfg.warmup_runs, 0);
    assert_eq!(cfg.operations, vec!["upload", "download", "latency"]);
}

#[test]
fn a_refused_trash_purge_is_reported() {
    let (mut errors, mut notes) = (Vec::new(), Vec::new());
    assert!(!note_trash_purge(
        "aeroftp-bench",
        Ok(false),
        &mut errors,
        &mut notes
    ));
    assert!(note_trash_purge(
        "aeroftp-bench",
        Ok(true),
        &mut errors,
        &mut notes
    ));
    assert!(errors.is_empty(), "{errors:?}");
    assert!(!note_trash_purge(
        "aeroftp-bench",
        Err(ProviderError::Other("2 items named 'aeroftp-bench'".into())),
        &mut errors,
        &mut notes,
    ));
    assert!(notes.is_empty(), "{notes:?}");
    assert_eq!(errors.len(), 1);
    assert!(
        errors[0].starts_with("trash purge of aeroftp-bench failed")
            && errors[0].ends_with("2 items named 'aeroftp-bench'"),
        "{errors:?}"
    );
}

/// #368 (Ehud's OneDrive): the error of a scratch folder that could not be
/// made names the folder and where it was made, so a report says which
/// folder the provider refused.
#[test]
fn ehud_368_a_refused_scratch_folder_names_where_it_was_made() {
    let error = ProviderError::Other("mkdir failed: itemNotFound".into());
    let relative = scratch_dir_error("aeroftp-bench/run", "", &error);
    assert!(
        relative.starts_with(
            "benchmark cannot create its scratch folder 'aeroftp-bench/run' in the profile's start folder '/': mkdir failed: itemNotFound"
        ),
        "{relative}"
    );
    let absolute = scratch_dir_error("/Docs/aeroftp-bench/run", "/Docs", &error);
    assert!(
        absolute
            .starts_with("benchmark cannot create its scratch folder '/Docs/aeroftp-bench/run': "),
        "{absolute}"
    );
}

/// #368 (Ehud): a client that cannot purge its provider's trash by design
/// (pCloud through OAuth) is told as a note: the folder was deleted and stays
/// in the trash. It is not a failed purge.
#[test]
fn a_trash_this_login_cannot_purge_is_a_note_not_a_failure() {
    let (mut errors, mut notes) = (Vec::new(), Vec::new());
    assert!(!note_trash_purge(
        "aeroftp-bench",
        Err(ProviderError::NotSupported(
            "an OAuth login cannot empty it".into()
        )),
        &mut errors,
        &mut notes,
    ));
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(
        notes,
        ["aeroftp-bench was deleted and stays in the provider's trash: an OAuth login cannot empty it"]
    );
}

#[test]
fn benchmark_overrides_apply_at_a_preset_level() {
    // The MCP tool describes `sizes`, `runs` and `operations` as applying at
    // any level; this is the behaviour that description rests on (#368).
    let cfg = resolve_benchmark_config(
        BenchmarkLevel::Quick,
        Some("1M,4M"),
        Some(2),
        Some("upload"),
    )
    .unwrap();
    assert_eq!(cfg.sizes_bytes, vec![1024 * 1024, 4 * 1024 * 1024]);
    assert_eq!(cfg.runs_per_size, 2);
    assert_eq!(cfg.operations, vec!["upload"]);
}

#[test]
fn benchmark_resolve_config_standard_defaults() {
    let cfg = resolve_benchmark_config(BenchmarkLevel::Standard, None, None, None).unwrap();
    assert_eq!(cfg.sizes_bytes.len(), 3);
    assert_eq!(cfg.runs_per_size, 3);
    assert_eq!(cfg.warmup_runs, 1);
    assert!(cfg.operations.contains(&"upload"));
    assert!(cfg.operations.contains(&"download"));
    assert!(cfg.operations.contains(&"list"));
}

#[test]
fn benchmark_resolve_config_rejects_invalid_operations() {
    let err = resolve_benchmark_config(BenchmarkLevel::Standard, None, None, Some("upload,nope"))
        .unwrap_err();
    assert!(err.contains("unknown operation"));
}

#[test]
fn benchmark_resolve_config_rejects_oversized_payload() {
    // 10 GiB > 5 GiB cap
    let err =
        resolve_benchmark_config(BenchmarkLevel::Custom, Some("10G"), None, None).unwrap_err();
    assert!(err.contains("5 GiB"));
}

#[test]
fn benchmark_resolve_config_rejects_zero_size() {
    let err = resolve_benchmark_config(BenchmarkLevel::Custom, Some("0"), None, None).unwrap_err();
    assert!(err.to_lowercase().contains("zero"));
}

#[test]
fn benchmark_resolve_config_clamps_runs() {
    let cfg = resolve_benchmark_config(BenchmarkLevel::Standard, None, Some(99), None).unwrap();
    assert_eq!(cfg.runs_per_size, 20);
}

#[test]
fn mkdir_ladder_builds_relative_cumulative_paths() {
    assert_eq!(
        benchmark_mkdir_ladder("aeroftp-bench/rid"),
        vec!["aeroftp-bench".to_string(), "aeroftp-bench/rid".to_string()]
    );
}

#[test]
fn mkdir_ladder_preserves_leading_slash() {
    assert_eq!(
        benchmark_mkdir_ladder("/Private/aeroftp-bench/rid"),
        vec![
            "/Private".to_string(),
            "/Private/aeroftp-bench".to_string(),
            "/Private/aeroftp-bench/rid".to_string(),
        ]
    );
}

#[test]
fn mkdir_ladder_empty_for_root_or_blank() {
    assert!(benchmark_mkdir_ladder("").is_empty());
    assert!(benchmark_mkdir_ladder("/").is_empty());
    assert!(benchmark_mkdir_ladder("///").is_empty());
}

#[test]
fn mkdir_ladder_ignores_trailing_and_duplicate_slashes() {
    assert_eq!(
        benchmark_mkdir_ladder("a//b/"),
        vec!["a".to_string(), "a/b".to_string()]
    );
}

#[test]
fn mkdir_ladder_covers_put_recursive_ancestor_chain() {
    // cmd_put_recursive pre-creates this chain before any STOR, so a
    // nested target with no pre-existing parents no longer stalls on
    // FTP/FTPS.
    assert_eq!(
        benchmark_mkdir_ladder("/a/b/src"),
        vec!["/a".to_string(), "/a/b".to_string(), "/a/b/src".to_string(),]
    );
}

#[test]
fn many_files_config_absent_when_not_requested() {
    assert!(resolve_many_files_config(None, "64K").unwrap().is_none());
    assert!(resolve_many_files_config(Some(0), "64K").unwrap().is_none());
}

#[test]
fn many_files_config_resolves_count_and_size() {
    let mf = resolve_many_files_config(Some(100), "64K")
        .unwrap()
        .expect("workload requested");
    assert_eq!(mf.file_count, 100);
    assert_eq!(mf.file_size_bytes, 64 * 1024);
}

#[test]
fn many_files_config_rejects_excessive_count() {
    let err = resolve_many_files_config(Some(BENCHMARK_MAX_FILE_COUNT + 1), "1K").unwrap_err();
    assert!(err.contains("cap"), "got: {}", err);
}

#[test]
fn many_files_config_rejects_zero_size() {
    let err = resolve_many_files_config(Some(10), "0").unwrap_err();
    assert!(err.contains("zero"), "got: {}", err);
}

#[test]
fn many_files_config_rejects_oversized_aggregate() {
    // 100k files x 1 MiB = ~100 GiB, well over the 5 GiB cap.
    let err = resolve_many_files_config(Some(100_000), "1M").unwrap_err();
    assert!(err.contains("5 GiB cap"), "got: {}", err);
}

#[test]
fn many_files_result_carries_files_per_second_and_count() {
    let r = many_files_result(
        "s3",
        false,
        "rid",
        "upload-all",
        64 * 1024,
        3,
        250.0,
        &[3.0, 4.0, 5.0],
        Some(&[120.0, 130.0, 140.0]),
        0,
    );
    assert_eq!(r.operation, "upload-all");
    assert_eq!(r.file_count, Some(3));
    assert_eq!(r.files_per_second, Some(250.0));
    assert_eq!(r.runs, 3);
    assert!(r.throughput_mbps.is_some());
    assert_eq!(r.latency_ms.p50, 4.0);
    assert_eq!(r.raw_runs.len(), 3);
    // Serialized many-files result must still pass the PII sweep.
    let pretty = serde_json::to_string_pretty(&r).unwrap();
    assert!(benchmark_sanitization_sweep(&pretty).is_ok());
    assert!(pretty.contains("files_per_second"));
    assert!(pretty.contains("file_count"));
}

#[test]
fn benchmark_sanitization_sweep_passes_clean_payload() {
    let clean = r#"{"protocol":"s3","provider_hint":"amazon-s3-eu-west-1","level":"standard"}"#;
    assert!(benchmark_sanitization_sweep(clean).is_ok());
}

#[test]
fn benchmark_sanitize_replaces_email() {
    // Provider errors often embed the account email: must be substituted,
    // not rejected.
    let dirty = r#"{"errors":["upload failed for user@example.com"]}"#.to_string();
    let cleaned = benchmark_sanitize(dirty);
    assert!(!cleaned.contains("user@example.com"));
    assert!(cleaned.contains("<redacted>@<redacted>"));
    // After substitution the assertion sweep must pass.
    assert!(benchmark_sanitization_sweep(&cleaned).is_ok());
}

#[test]
fn benchmark_sanitize_replaces_aws_key() {
    let dirty = r#"{"note":"AKIAIOSFODNN7EXAMPLE oops"}"#.to_string();
    let cleaned = benchmark_sanitize(dirty);
    assert!(!cleaned.contains("AKIAIOSFODNN7EXAMPLE"));
    assert!(cleaned.contains("AKIA<redacted>"));
    assert!(benchmark_sanitization_sweep(&cleaned).is_ok());
}

#[test]
fn benchmark_sanitize_replaces_ipv4() {
    let dirty = r#"{"host":"192.168.1.1"}"#.to_string();
    let cleaned = benchmark_sanitize(dirty);
    assert!(!cleaned.contains("192.168.1.1"));
    assert!(cleaned.contains("<redacted-ip>"));
    assert!(benchmark_sanitization_sweep(&cleaned).is_ok());
}

#[test]
fn benchmark_sanitization_sweep_blocks_unsanitized_email() {
    // Belt-and-suspenders: if benchmark_sanitize is bypassed and PII
    // slips through, the assertion sweep must still flag it.
    let dirty = r#"{"submitter":"user@example.com"}"#;
    let err = benchmark_sanitization_sweep(dirty).unwrap_err();
    assert!(err.to_lowercase().contains("email"));
}

#[test]
fn benchmark_sanitization_sweep_blocks_linux_home_path() {
    let dirty = r#"{"path":"/home/alice/secret"}"#;
    let err = benchmark_sanitization_sweep(dirty).unwrap_err();
    assert!(err.contains("Linux"));
}

#[test]
fn benchmark_provider_hint_anonymize_extra_emits_hash_only() {
    let (hint, hash) = benchmark_provider_hint("s3", true, "report-id-123");
    assert!(hint.is_none());
    let h = hash.expect("hash present");
    assert_eq!(h.len(), 16, "hash truncated to 16 hex chars");
    assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn benchmark_provider_hint_normal_emits_hint_only() {
    let (hint, hash) = benchmark_provider_hint("webdav", false, "rid");
    assert_eq!(hint.as_deref(), Some("webdav"));
    assert!(hash.is_none());
}

#[test]
fn benchmark_remote_roots_respect_profile_initial_path() {
    let (base, root) = benchmark_remote_roots("/workdir", "rid");
    assert_eq!(base, "/workdir/aeroftp-bench");
    assert_eq!(root, "/workdir/aeroftp-bench/rid");
}

#[test]
fn benchmark_remote_roots_stay_relative_without_initial_path() {
    let (base, root) = benchmark_remote_roots("", "rid");
    assert_eq!(base, "aeroftp-bench");
    assert_eq!(root, "aeroftp-bench/rid");
}

/// Requested 79: on Proton Drive a relative or empty `--test-root-prefix`
/// lands where the session starts (My Files in CLI mode), not between the
/// account's sections; an absolute prefix, and any prefix on another
/// provider, is taken as given, as before.
#[test]
fn a_relative_prefix_lands_in_the_proton_start_folder() {
    use crate::providers::ProviderType::{Proton, Sftp};
    let prefix = benchmark_effective_prefix("AeroFTP-live-tests/r83", Proton, "/my-files");
    assert_eq!(prefix, "/my-files/AeroFTP-live-tests/r83");
    assert_eq!(
        benchmark_remote_roots_from_prefix(&prefix, "id"),
        (
            "/my-files/AeroFTP-live-tests/r83".to_string(),
            "/my-files/AeroFTP-live-tests/r83/id".to_string()
        )
    );
    assert_eq!(
        benchmark_effective_prefix("", Proton, "/my-files"),
        "/my-files"
    );
    assert_eq!(
        benchmark_effective_prefix("/my-files/x", Proton, "/my-files"),
        "/my-files/x"
    );
    // Native mode starts at its root, My Files.
    assert_eq!(benchmark_effective_prefix("x", Proton, "/"), "/x");
    // Another provider: unchanged.
    assert_eq!(benchmark_effective_prefix("x", Sftp, ""), "x");
    assert_eq!(benchmark_effective_prefix("", Sftp, ""), "");
}

#[test]
fn benchmark_remote_roots_from_prefix_keeps_user_subpath() {
    let (base, root) = benchmark_remote_roots_from_prefix("/Drive/aeroftp-bench", "rid");
    assert_eq!(base, "/Drive/aeroftp-bench");
    assert_eq!(root, "/Drive/aeroftp-bench/rid");
}

#[test]
fn benchmark_remote_roots_from_prefix_normalizes_trailing_slash() {
    let (base, root) = benchmark_remote_roots_from_prefix("/Drive/bench/", "rid");
    assert_eq!(base, "/Drive/bench");
    assert_eq!(root, "/Drive/bench/rid");
}

#[test]
fn benchmark_remote_roots_from_prefix_prepends_leading_slash() {
    let (base, root) = benchmark_remote_roots_from_prefix("home/aeroftp", "rid");
    assert_eq!(base, "/home/aeroftp");
    assert_eq!(root, "/home/aeroftp/rid");
}

#[test]
fn benchmark_remote_roots_from_prefix_falls_back_to_root() {
    let (base, root) = benchmark_remote_roots_from_prefix("/", "rid");
    assert_eq!(base, "/");
    assert_eq!(root, "/rid");
}

#[test]
fn benchmark_rounded_hour_utc_omits_subhour_precision() {
    let stamp = benchmark_rounded_hour_utc();
    assert!(stamp.ends_with(":00:00Z"), "got {}", stamp);
    assert_eq!(stamp.len(), 20);
}

#[test]
fn benchmark_tod_bucket_returns_known_label() {
    let label = benchmark_tod_bucket();
    assert!(matches!(
        label,
        "night" | "morning" | "afternoon" | "evening"
    ));
}

#[test]
fn benchmark_report_serializes_to_schema_v1() {
    let report = BenchmarkReport {
        schema_version: 1,
        report_id: "11111111-2222-3333-4444-555555555555".into(),
        generated_at: "2026-05-06T15:00:00Z".into(),
        cli: BenchmarkCliMeta {
            version: "3.8.0".into(),
            build_target: "x86_64-unknown-linux-gnu".into(),
            rustc: "1.75.0".into(),
        },
        level: BenchmarkLevel::Quick,
        access: "SFTP".into(),
        service: "Custom".into(),
        mode: None,
        environment: BenchmarkEnvironment {
            asn_bucket: None,
            country_bucket: None,
            tod_bucket: "afternoon".into(),
            os_family: "linux".into(),
            os_arch: "x86_64".into(),
            cpu_class: "x64-modern".into(),
        },
        consent: BenchmarkConsent {
            publish: false,
            anonymize_extra: false,
        },
        results: vec![],
        summary: BenchmarkSummary {
            total_runs: 0,
            total_bytes_transferred: 0,
            total_duration_ms: 0,
            errors: vec![],
        },
        notes: vec![],
        signal: None,
        private_paths: vec![],
    };
    let v: serde_json::Value = serde_json::to_value(&report).unwrap();
    assert!(
        v.get("notes").is_none(),
        "an ordinary run must not carry an empty notes array"
    );
    assert_eq!(v["schema_version"], 1);
    assert_eq!(v["level"], "quick");
    assert!(v["environment"].get("asn_bucket").is_none());
    assert_eq!(v["consent"]["publish"], false);
    // Pass through sanitization sweep on a real serialized payload.
    let pretty = serde_json::to_string_pretty(&report).unwrap();
    assert!(benchmark_sanitization_sweep(&pretty).is_ok());
}

#[test]
fn a_tool_argument_names_a_level() {
    assert_eq!(BenchmarkLevel::parse("quick"), Some(BenchmarkLevel::Quick));
    assert_eq!(BenchmarkLevel::parse(" Deep "), Some(BenchmarkLevel::Deep));
    assert_eq!(BenchmarkLevel::parse("fast"), None);
}

#[test]
fn the_scratch_base_joins_the_start_path_on_a_folder_boundary() {
    let (base, root) = benchmark_remote_roots("/", "rid");
    assert_eq!(
        (base.as_str(), root.as_str()),
        ("aeroftp-bench", "aeroftp-bench/rid")
    );
    // A start path that is a prefix of `aeroftp-bench` is still a folder of
    // its own: the CLI resolver it replaces matched the prefix as a string
    // and dropped it.
    let (base, _) = benchmark_remote_roots("aero", "rid");
    assert_eq!(base, "aero/aeroftp-bench");
    // A start path whose name ends in a space is that folder, not another.
    let (base, _) = benchmark_remote_roots("/srv/data ", "rid");
    assert_eq!(base, "/srv/data /aeroftp-bench");
}

#[test]
fn the_service_column_names_a_preset_and_hides_a_custom_host() {
    assert_eq!(
        benchmark_service_label(ProviderType::WebDav, Some("koofr")),
        "Koofr"
    );
    assert_eq!(benchmark_service_label(ProviderType::Sftp, None), "Custom");
    assert_eq!(
        benchmark_service_label(ProviderType::PCloud, None),
        "pCloud Drive"
    );
}

#[test]
fn a_report_leaves_with_its_pii_substituted() {
    let report = test_report(vec!["upload failed for user@example.com at 10.1.2.3".into()]);
    let json = sanitized_report_json(&report).unwrap();
    assert!(!json.contains("user@example.com"), "{json}");
    assert!(!json.contains("10.1.2.3"), "{json}");
    assert!(json.contains("<redacted>@<redacted>"));
    // The raw serialization carries both: this is what `--format json`
    // printed before it went through this function.
    let raw = serde_json::to_string_pretty(&report).unwrap();
    assert!(raw.contains("user@example.com"));
}

fn test_report(errors: Vec<String>) -> BenchmarkReport {
    BenchmarkReport {
        schema_version: 1,
        report_id: "rid".into(),
        generated_at: "2026-10-08T15:00:00Z".into(),
        cli: BenchmarkCliMeta {
            version: "0".into(),
            build_target: "x".into(),
            rustc: "x".into(),
        },
        level: BenchmarkLevel::Quick,
        access: "SFTP".into(),
        service: "Custom".into(),
        mode: None,
        environment: BenchmarkEnvironment {
            asn_bucket: None,
            country_bucket: None,
            tod_bucket: "afternoon".into(),
            os_family: "linux".into(),
            os_arch: "x86_64".into(),
            cpu_class: "x64-modern".into(),
        },
        consent: BenchmarkConsent {
            publish: false,
            anonymize_extra: false,
        },
        results: vec![],
        summary: BenchmarkSummary {
            total_runs: 0,
            total_bytes_transferred: 0,
            total_duration_ms: 0,
            errors,
        },
        notes: vec![],
        signal: None,
        private_paths: vec![],
    }
}

// ── The engine on an in-memory remote ────────────────────────────────

mod engine_runs {
    use super::*;
    use crate::providers::{RemoteEntry, StorageProvider};
    use async_trait::async_trait;
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::{Arc, Mutex};
    use tokio_util::sync::CancellationToken;

    #[derive(Default)]
    struct Remote {
        files: BTreeMap<String, Vec<u8>>,
        dirs: BTreeSet<String>,
        uploads: u32,
        /// Every upload is refused, as by a server out of quota.
        refuse_uploads: bool,
        /// Every download waits forever, as from an unreachable server.
        hang_downloads: bool,
        /// Every existence check fails, as on a connection that dropped.
        fail_exists: bool,
        /// A folder three levels deep is refused, as FileLu refused the run
        /// folder under a prefix folder it had just created.
        refuse_deep_mkdir: bool,
        /// The folder `pwd` reports, `/` when unset.
        cwd: Option<String>,
        /// Every folder asked for, in order.
        mkdirs: Vec<String>,
    }

    struct MemRemote(Arc<Mutex<Remote>>);

    fn under(path: &str, dir: &str) -> bool {
        path.starts_with(&format!("{}/", dir.trim_end_matches('/')))
    }

    #[async_trait]
    impl StorageProvider for MemRemote {
        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
        fn provider_type(&self) -> ProviderType {
            ProviderType::Sftp
        }
        fn display_name(&self) -> String {
            "mem".into()
        }
        async fn connect(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn disconnect(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        fn is_connected(&self) -> bool {
            true
        }
        async fn list(&mut self, p: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
            let r = self.0.lock().unwrap();
            let child = |q: &String| under(q, p) && !q[p.len() + 1..].contains('/');
            let mut out: Vec<RemoteEntry> = r
                .files
                .iter()
                .filter(|(q, _)| child(q))
                .map(|(q, b)| RemoteEntry::file(q.clone(), q.clone(), b.len() as u64))
                .collect();
            out.extend(
                r.dirs
                    .iter()
                    .filter(|q| child(q))
                    .map(|q| RemoteEntry::directory(q.clone(), q.clone())),
            );
            Ok(out)
        }
        async fn pwd(&mut self) -> Result<String, ProviderError> {
            Ok(self
                .0
                .lock()
                .unwrap()
                .cwd
                .clone()
                .unwrap_or_else(|| "/".into()))
        }
        async fn cd(&mut self, _p: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn cd_up(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn download(
            &mut self,
            r: &str,
            l: &str,
            _cb: Option<Box<dyn Fn(u64, u64) + Send>>,
        ) -> Result<(), ProviderError> {
            if self.0.lock().unwrap().hang_downloads {
                std::future::pending::<()>().await;
            }
            let bytes = self
                .0
                .lock()
                .unwrap()
                .files
                .get(r)
                .cloned()
                .ok_or_else(|| ProviderError::NotFound(r.into()))?;
            std::fs::write(l, bytes).map_err(|e| ProviderError::Other(e.to_string()))
        }
        async fn download_to_bytes(&mut self, r: &str) -> Result<Vec<u8>, ProviderError> {
            self.0
                .lock()
                .unwrap()
                .files
                .get(r)
                .cloned()
                .ok_or_else(|| ProviderError::NotFound(r.into()))
        }
        async fn upload(
            &mut self,
            l: &str,
            r: &str,
            _cb: Option<Box<dyn Fn(u64, u64) + Send>>,
        ) -> Result<(), ProviderError> {
            if self.0.lock().unwrap().refuse_uploads {
                return Err(ProviderError::Other("quota exceeded".into()));
            }
            let bytes = std::fs::read(l).map_err(|e| ProviderError::Other(e.to_string()))?;
            let mut remote = self.0.lock().unwrap();
            remote.uploads += 1;
            remote.files.insert(r.into(), bytes);
            Ok(())
        }
        async fn mkdir(&mut self, p: &str) -> Result<(), ProviderError> {
            self.0.lock().unwrap().mkdirs.push(p.into());
            if self.0.lock().unwrap().refuse_deep_mkdir && p.matches('/').count() >= 3 {
                return Err(ProviderError::ServerError("Folder not found".into()));
            }
            if !self.0.lock().unwrap().dirs.insert(p.into()) {
                return Err(ProviderError::AlreadyExists(p.into()));
            }
            Ok(())
        }
        async fn delete(&mut self, p: &str) -> Result<(), ProviderError> {
            self.0
                .lock()
                .unwrap()
                .files
                .remove(p)
                .map(|_| ())
                .ok_or_else(|| ProviderError::NotFound(p.into()))
        }
        async fn rmdir(&mut self, p: &str) -> Result<(), ProviderError> {
            self.0.lock().unwrap().dirs.remove(p);
            Ok(())
        }
        async fn rmdir_recursive(&mut self, p: &str) -> Result<(), ProviderError> {
            let mut r = self.0.lock().unwrap();
            r.files.retain(|q, _| !under(q, p));
            r.dirs.retain(|q| q != p && !under(q, p));
            Ok(())
        }
        async fn rename(&mut self, _from: &str, _to: &str) -> Result<(), ProviderError> {
            Err(ProviderError::NotSupported("rename".into()))
        }
        async fn stat(&mut self, p: &str) -> Result<RemoteEntry, ProviderError> {
            let r = self.0.lock().unwrap();
            r.files
                .get(p)
                .map(|b| RemoteEntry::file(p.into(), p.into(), b.len() as u64))
                .ok_or_else(|| ProviderError::NotFound(p.into()))
        }
        async fn size(&mut self, p: &str) -> Result<u64, ProviderError> {
            Ok(self
                .0
                .lock()
                .unwrap()
                .files
                .get(p)
                .map_or(0, |b| b.len() as u64))
        }
        async fn exists(&mut self, p: &str) -> Result<bool, ProviderError> {
            let r = self.0.lock().unwrap();
            if r.fail_exists {
                return Err(ProviderError::ConnectionFailed("connection reset".into()));
            }
            Ok(r.files.contains_key(p) || r.dirs.contains(p))
        }
        async fn keep_alive(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn server_info(&mut self) -> Result<String, ProviderError> {
            Ok("mem".into())
        }
    }

    fn options(
        level: BenchmarkLevel,
        sizes: &str,
        cancel: Option<CancellationToken>,
    ) -> BenchmarkOptions {
        BenchmarkOptions {
            level,
            config: resolve_benchmark_config(level, Some(sizes), None, None).unwrap(),
            many_files: None,
            consent_publish: false,
            anonymize_extra: false,
            profile_timeout_secs: BENCHMARK_TOTAL_TIMEOUT_SECS,
            test_root_prefix: None,
            pre_delete: false,
            // The routed path would journal into the real per-user checkpoint
            // store; the provider's own transfers measure the same bytes here.
            direct_transfers: true,
            engine_override: crate::transfer_router::Override::None,
            check_public_ip: false,
            service: "Custom".into(),
            connect_ms: None,
            local_endpoint: false,
            cancel,
        }
    }

    /// Audit L2B-06: after a stop the engine looks again for the run folder,
    /// which a transfer cut short can bring back. A look that fails proves
    /// nothing: the report says the cleanup could not be verified instead of
    /// passing for a clean one.
    #[tokio::test]
    async fn a_post_stop_check_that_fails_is_reported_as_unverified() {
        let (state, provider) = remote();
        state.lock().unwrap().fail_exists = true;
        let token = CancellationToken::new();
        let observer = CancelOn {
            operation: "download",
            token: token.clone(),
        };
        let outcome = run(
            provider,
            "/data",
            options(BenchmarkLevel::Quick, "1K", Some(token)),
            &observer,
        )
        .await
        .unwrap();
        assert!(outcome.cancelled);
        assert!(
            outcome
                .report
                .summary
                .errors
                .iter()
                .any(|e| e.contains("could not be verified") && e.contains("connection reset")),
            "{:?}",
            outcome.report.summary.errors
        );
    }

    /// Live run (#368, FileLu): a run whose scratch folder is refused after
    /// it created the prefix folders removes those folders before it gives
    /// up, instead of leaving them on the server.
    #[tokio::test]
    async fn a_refused_scratch_folder_leaves_no_prefix_folder_behind() {
        let (state, provider) = remote();
        state.lock().unwrap().refuse_deep_mkdir = true;
        let mut opts = options(BenchmarkLevel::Quick, "1K", None);
        opts.test_root_prefix = Some("/data/live-tests".into());
        let error = match run(provider, "/data", opts, &Silent).await {
            Err(error) => error,
            Ok(_) => panic!("the scratch folder was refused"),
        };
        assert!(error.contains("Folder not found"), "{error}");
        let r = state.lock().unwrap();
        assert_eq!(r.dirs, BTreeSet::from(["/data".to_string()]));
    }

    struct Silent;
    impl BenchmarkObserver for Silent {}

    /// Raises the token when the first run of `operation` is announced.
    struct CancelOn {
        operation: &'static str,
        token: CancellationToken,
    }
    impl BenchmarkObserver for CancelOn {
        fn event(&self, event: BenchmarkEvent<'_>) {
            if let BenchmarkEvent::Run { operation, .. } = event {
                if operation == self.operation {
                    self.token.cancel();
                }
            }
        }
    }

    /// A remote whose start path `/data` already exists, as a profile's does.
    fn remote() -> (Arc<Mutex<Remote>>, Box<dyn StorageProvider>) {
        let mut remote = Remote::default();
        remote.dirs.insert("/data".into());
        let state = Arc::new(Mutex::new(remote));
        (state.clone(), Box::new(MemRemote(state)))
    }

    /// A relative start path is the folder connect entered, which the
    /// session stands in: the scratch folder joined to it again was created
    /// in `team /team /aeroftp-bench`, or not at all.
    #[tokio::test]
    async fn a_relative_start_path_is_applied_once() {
        let (state, provider) = remote();
        let home = ["/data", "/home", "/home/u", "/home/u/team "].map(String::from);
        {
            let mut r = state.lock().unwrap();
            r.cwd = Some("/home/u/team ".into());
            r.dirs.extend(home.clone());
        }
        let opts = options(BenchmarkLevel::Quick, "1K", None);
        let outcome = run(provider, "team ", opts, &Silent).await.unwrap();
        assert!(
            outcome.report.summary.errors.is_empty(),
            "{:?}",
            outcome.report.summary.errors
        );
        let r = state.lock().unwrap();
        // Every folder asked for is where the session stands, none relative.
        assert_eq!(r.mkdirs[0], "/home/u/team /aeroftp-bench");
        assert!(
            r.mkdirs
                .iter()
                .all(|dir| dir.starts_with('/') && !dir.contains("team /team ")),
            "{:?}",
            r.mkdirs
        );
        assert!(
            r.mkdirs
                .iter()
                .any(|dir| dir.starts_with("/home/u/team /aeroftp-bench/")),
            "{:?}",
            r.mkdirs
        );
        assert!(r.files.is_empty(), "{:?}", r.files.keys());
        assert_eq!(r.dirs, BTreeSet::from(home));
    }

    #[tokio::test]
    async fn a_run_measures_and_leaves_nothing_behind() {
        let (state, provider) = remote();
        let opts = options(BenchmarkLevel::Quick, "1K", None);
        let outcome = run(provider, "/data", opts, &Silent).await.unwrap();
        let ops: Vec<&str> = outcome
            .report
            .results
            .iter()
            .map(|r| r.operation.as_str())
            .collect();
        assert_eq!(ops, vec!["upload", "download", "latency"]);
        assert!(!outcome.early_abort && !outcome.cancelled);
        assert!(
            outcome.report.summary.errors.is_empty(),
            "{:?}",
            outcome.report.summary.errors
        );
        assert_eq!(outcome.report.summary.total_bytes_transferred, 2048);
        assert_eq!(outcome.report.access, "SFTP");
        let r = state.lock().unwrap();
        assert!(r.files.is_empty(), "{:?}", r.files.keys());
        assert_eq!(r.dirs, BTreeSet::from(["/data".to_string()]));
    }

    #[tokio::test]
    async fn a_run_cancelled_before_it_starts_uploads_nothing_and_cleans_up() {
        let (state, provider) = remote();
        let token = CancellationToken::new();
        token.cancel();
        let outcome = run(
            provider,
            "/data",
            options(BenchmarkLevel::Quick, "1K", Some(token)),
            &Silent,
        )
        .await
        .unwrap();
        assert!(outcome.cancelled && outcome.early_abort);
        assert!(outcome.report.results.is_empty());
        assert!(outcome
            .report
            .summary
            .errors
            .iter()
            .any(|e| e.starts_with("benchmark cancelled")));
        let r = state.lock().unwrap();
        assert_eq!(r.uploads, 0);
        assert!(r.files.is_empty(), "{:?}", r.files.keys());
        assert_eq!(r.dirs, BTreeSet::from(["/data".to_string()]));
    }

    #[tokio::test]
    async fn a_run_cancelled_midway_keeps_what_it_measured_and_cleans_up() {
        let (state, provider) = remote();
        let token = CancellationToken::new();
        let observer = CancelOn {
            operation: "download",
            token: token.clone(),
        };
        let outcome = run(
            provider,
            "/data",
            options(BenchmarkLevel::Quick, "1K", Some(token)),
            &observer,
        )
        .await
        .unwrap();
        assert!(outcome.cancelled);
        let ops: Vec<&str> = outcome
            .report
            .results
            .iter()
            .map(|r| r.operation.as_str())
            .collect();
        // The cancel lands after the upload, which stays measured, and before
        // the download, which is not counted as a failure.
        assert_eq!(ops, vec!["upload"]);
        assert!(!results_have_fatal(&outcome.report.results));
        let r = state.lock().unwrap();
        assert!(r.files.is_empty(), "{:?}", r.files.keys());
        assert_eq!(r.dirs, BTreeSet::from(["/data".to_string()]));
    }

    /// A remote with a trash. `Unpurgeable`: deletes go to a trash this
    /// client cannot purge (Koofr). `LivePath`: a recursive delete purges a
    /// live folder, while many-files uses identities captured before deletion
    /// (Proton's CLI).
    #[derive(Clone, Copy, PartialEq)]
    enum TrashKind {
        Unpurgeable,
        LivePath,
        /// Deletes go to the trash and the purge finds an item there by the
        /// path it had (Google Drive, Dropbox, MEGA and most consumer clouds).
        OriginalPath,
    }

    struct TrashKeepingRemote {
        inner: MemRemote,
        kind: TrashKind,
        trash: Arc<Mutex<Vec<String>>>,
        /// Every path `delete_permanent` was called with, in call order.
        purge_calls: Arc<Mutex<Vec<String>>>,
        /// `delete_permanent` never answers, as a server that stopped.
        hang_purge: bool,
        /// `delete_permanent` raises this token, as a Ctrl+C during the purge.
        cancel_on_purge: Option<CancellationToken>,
        /// A live purge moves the item to the trash and then fails to purge
        /// it there, as Proton's CLI can.
        fail_live_purge: bool,
        /// Capture of this many-files index fails. The others stay `Ok(None)`
        /// or a live-path id, so one `Err` is not the whole batch.
        fail_capture_index: Option<u32>,
    }

    #[async_trait]
    impl StorageProvider for TrashKeepingRemote {
        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
        fn provider_type(&self) -> ProviderType {
            self.inner.provider_type()
        }
        fn display_name(&self) -> String {
            self.inner.display_name()
        }
        async fn connect(&mut self) -> Result<(), ProviderError> {
            self.inner.connect().await
        }
        async fn disconnect(&mut self) -> Result<(), ProviderError> {
            self.inner.disconnect().await
        }
        fn is_connected(&self) -> bool {
            self.inner.is_connected()
        }
        async fn list(&mut self, p: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
            self.inner.list(p).await
        }
        async fn pwd(&mut self) -> Result<String, ProviderError> {
            self.inner.pwd().await
        }
        async fn cd(&mut self, p: &str) -> Result<(), ProviderError> {
            self.inner.cd(p).await
        }
        async fn cd_up(&mut self) -> Result<(), ProviderError> {
            self.inner.cd_up().await
        }
        async fn download(
            &mut self,
            r: &str,
            l: &str,
            cb: Option<Box<dyn Fn(u64, u64) + Send>>,
        ) -> Result<(), ProviderError> {
            self.inner.download(r, l, cb).await
        }
        async fn download_to_bytes(&mut self, r: &str) -> Result<Vec<u8>, ProviderError> {
            self.inner.download_to_bytes(r).await
        }
        async fn upload(
            &mut self,
            l: &str,
            r: &str,
            cb: Option<Box<dyn Fn(u64, u64) + Send>>,
        ) -> Result<(), ProviderError> {
            self.inner.upload(l, r, cb).await
        }
        async fn mkdir(&mut self, p: &str) -> Result<(), ProviderError> {
            self.inner.mkdir(p).await
        }
        async fn delete(&mut self, p: &str) -> Result<(), ProviderError> {
            self.inner.delete(p).await?;
            self.trash.lock().unwrap().push(p.to_string());
            Ok(())
        }
        async fn rmdir(&mut self, p: &str) -> Result<(), ProviderError> {
            self.inner.rmdir(p).await
        }
        async fn rmdir_recursive(&mut self, p: &str) -> Result<(), ProviderError> {
            // A real provider answers for a path that is not there.
            if !self.inner.exists(p).await? {
                return Err(ProviderError::NotFound(p.to_string()));
            }
            self.trash.lock().unwrap().push(p.to_string());
            self.inner.rmdir_recursive(p).await
        }
        async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
            self.inner.rename(from, to).await
        }
        async fn stat(&mut self, p: &str) -> Result<RemoteEntry, ProviderError> {
            self.inner.stat(p).await
        }
        async fn size(&mut self, p: &str) -> Result<u64, ProviderError> {
            self.inner.size(p).await
        }
        async fn exists(&mut self, p: &str) -> Result<bool, ProviderError> {
            self.inner.exists(p).await
        }
        async fn keep_alive(&mut self) -> Result<(), ProviderError> {
            self.inner.keep_alive().await
        }
        async fn server_info(&mut self) -> Result<String, ProviderError> {
            self.inner.server_info().await
        }
        fn trash_left_unpurged(&self) -> Option<&'static str> {
            (self.kind == TrashKind::Unpurgeable)
                .then_some("this provider keeps deleted items in its trash")
        }
        fn purges_live_path(&self) -> bool {
            self.kind == TrashKind::LivePath
        }
        async fn capture_trash_identities(
            &mut self,
            paths: &[String],
        ) -> Vec<Result<Option<String>, ProviderError>> {
            paths
                .iter()
                .enumerate()
                .map(|(index, path)| {
                    if self.fail_capture_index == Some(index as u32) {
                        return Err(ProviderError::Other("capture failed".into()));
                    }
                    if self.kind == TrashKind::LivePath {
                        Ok(Some(format!("uid:{path}")))
                    } else {
                        Ok(None)
                    }
                })
                .collect()
        }
        async fn delete_permanent_with_identity(
            &mut self,
            p: &str,
            identity: Option<&str>,
        ) -> Result<bool, ProviderError> {
            if self.kind == TrashKind::LivePath && identity == Some(format!("uid:{p}").as_str()) {
                let mut trash = self.trash.lock().unwrap();
                let before = trash.len();
                trash.retain(|item| item != p);
                return Ok(trash.len() < before);
            }
            self.delete_permanent(p).await
        }
        async fn delete_permanent(&mut self, p: &str) -> Result<bool, ProviderError> {
            self.purge_calls.lock().unwrap().push(p.to_string());
            if self.hang_purge {
                std::future::pending::<()>().await;
            }
            if let Some(token) = &self.cancel_on_purge {
                token.cancel();
            }
            if self.kind == TrashKind::OriginalPath {
                let mut trash = self.trash.lock().unwrap();
                let before = trash.len();
                trash.retain(|t| t != p);
                return Ok(trash.len() < before);
            }
            if self.kind != TrashKind::LivePath || !self.inner.exists(p).await? {
                // Gone already, as after a recursive delete: nothing found.
                return Ok(false);
            }
            // Trashed and purged in one step: nothing stays in the trash.
            self.inner.rmdir_recursive(p).await?;
            if self.fail_live_purge {
                self.trash.lock().unwrap().push(p.to_string());
                return Err(ProviderError::Other("purge refused".into()));
            }
            Ok(true)
        }
    }

    /// The remote's state, its trash, and the provider to run against.
    type TrashRemote = (
        Arc<Mutex<Remote>>,
        Arc<Mutex<Vec<String>>>,
        Box<dyn StorageProvider>,
    );

    fn trash_remote(kind: TrashKind) -> TrashRemote {
        let (state, _) = remote();
        let trash = Arc::new(Mutex::new(Vec::new()));
        let provider = Box::new(TrashKeepingRemote {
            inner: MemRemote(state.clone()),
            kind,
            trash: trash.clone(),
            purge_calls: Arc::default(),
            hang_purge: false,
            cancel_on_purge: None,
            fail_live_purge: false,
            fail_capture_index: None,
        });
        (state, trash, provider)
    }

    #[tokio::test]
    async fn a_trash_the_run_cannot_purge_is_a_note_not_passed_over() {
        let (_state, _trash, provider) = trash_remote(TrashKind::Unpurgeable);
        let outcome = run(
            provider,
            "/data",
            options(BenchmarkLevel::Quick, "1K", None),
            &Silent,
        )
        .await
        .unwrap();
        // #368 (Ehud): told, as a note: the folder was deleted and stays in
        // the trash by design, which is not a failed purge.
        assert!(
            outcome.report.notes.iter().any(|n| n
                .contains("was deleted and stays in the provider's trash")
                && n.contains("keeps deleted items in its trash")),
            "{:?}",
            outcome.report.notes
        );
        assert!(
            !outcome
                .report
                .summary
                .errors
                .iter()
                .any(|e| e.contains("trash")),
            "{:?}",
            outcome.report.summary.errors
        );
        // Live run (#368): a trash note is not a measurement note, so the
        // round trip still reads as the service's, never as a local bridge.
        let signal = outcome.report.signal.as_ref().expect("latency measured");
        assert!(signal.bars.is_some(), "{signal:?}");
    }

    #[tokio::test]
    async fn a_client_that_purges_a_live_path_leaves_nothing_in_the_trash() {
        let (state, trash, provider) = trash_remote(TrashKind::LivePath);
        let mut opts = options(BenchmarkLevel::Quick, "1K", None);
        opts.test_root_prefix = Some("/tests/run".into());
        let outcome = run(provider, "/data", opts, &Silent).await.unwrap();
        // The run folder and the two prefix folders the run created were
        // purged where they stood, never moved to the trash by a plain delete.
        assert!(
            trash.lock().unwrap().is_empty(),
            "{:?}",
            trash.lock().unwrap()
        );
        assert!(
            outcome.report.summary.errors.is_empty(),
            "{:?}",
            outcome.report.summary.errors
        );
        let r = state.lock().unwrap();
        assert_eq!(r.dirs, BTreeSet::from(["/data".to_string()]));
    }

    #[tokio::test]
    async fn a_live_purge_that_fails_after_the_trash_is_one_error_not_three() {
        let (state, _) = remote();
        let trash = Arc::new(Mutex::new(Vec::new()));
        let provider = Box::new(TrashKeepingRemote {
            inner: MemRemote(state.clone()),
            kind: TrashKind::LivePath,
            trash: trash.clone(),
            purge_calls: Arc::default(),
            hang_purge: false,
            cancel_on_purge: None,
            fail_live_purge: true,
            fail_capture_index: None,
        });
        let outcome = run(
            provider,
            "/data",
            options(BenchmarkLevel::Quick, "1K", None),
            &Silent,
        )
        .await
        .unwrap();
        let errors = &outcome.report.summary.errors;
        // The folders left the drive and sit in the trash, which the purge
        // error says; the plain removal after it found nothing to remove.
        assert!(
            errors.iter().any(|e| e.starts_with("trash purge of")),
            "{errors:?}"
        );
        assert!(
            !errors
                .iter()
                .any(|e| e.starts_with("cleanup of") || e.contains("could not be removed")),
            "{errors:?}"
        );
        assert_eq!(
            state.lock().unwrap().dirs,
            BTreeSet::from(["/data".to_string()])
        );
    }

    fn many_files_options() -> BenchmarkOptions {
        let mut opts = options(BenchmarkLevel::Quick, "1K", None);
        opts.config.sizes_bytes.clear();
        opts.many_files = resolve_many_files_config(Some(3), "1K").unwrap();
        opts
    }

    #[tokio::test]
    async fn the_many_files_workload_purges_every_file_it_deleted() {
        let (state, trash, provider) = trash_remote(TrashKind::OriginalPath);
        let outcome = run(provider, "/data", many_files_options(), &Silent)
            .await
            .unwrap();
        let ops: Vec<&str> = outcome
            .report
            .results
            .iter()
            .map(|r| r.operation.as_str())
            .collect();
        assert!(ops.contains(&"delete-all"), "{ops:?}");
        // Each file delete-all sent to the trash is purged after it, and so is
        // the run folder: nothing of the run is left in the trash.
        assert!(
            trash.lock().unwrap().is_empty(),
            "{:?}",
            trash.lock().unwrap()
        );
        assert!(
            outcome.report.summary.errors.is_empty(),
            "{:?}",
            outcome.report.summary.errors
        );
        assert_eq!(
            state.lock().unwrap().dirs,
            BTreeSet::from(["/data".to_string()])
        );
    }

    #[tokio::test]
    async fn the_many_files_workload_purges_a_live_path_clients_files_by_identity() {
        let (_state, trash, provider) = trash_remote(TrashKind::LivePath);
        let outcome = run(provider, "/data", many_files_options(), &Silent)
            .await
            .unwrap();
        assert!(trash.lock().unwrap().is_empty(), "{trash:?}");
        assert!(
            outcome.report.summary.errors.is_empty(),
            "{:?}",
            outcome.report.summary.errors
        );
    }

    /// File 1's capture fails. The other two are `Ok(None)` and still purge by
    /// path. The failed file must not be purged as if its identity were missing.
    #[tokio::test]
    async fn a_failed_capture_does_not_purge_that_file_by_path() {
        let (state, _) = remote();
        let trash = Arc::new(Mutex::new(Vec::new()));
        let purge_calls = Arc::new(Mutex::new(Vec::new()));
        let provider = Box::new(TrashKeepingRemote {
            inner: MemRemote(state),
            kind: TrashKind::OriginalPath,
            trash: trash.clone(),
            purge_calls: purge_calls.clone(),
            hang_purge: false,
            cancel_on_purge: None,
            fail_live_purge: false,
            fail_capture_index: Some(1),
        });
        let outcome = run(provider, "/data", many_files_options(), &Silent)
            .await
            .unwrap();
        let calls = purge_calls.lock().unwrap().clone();
        assert!(
            calls.iter().any(|path| path.contains("f000000-")),
            "{calls:?}"
        );
        assert!(
            calls.iter().any(|path| path.contains("f000002-")),
            "{calls:?}"
        );
        assert!(
            calls.iter().all(|path| !path.contains("f000001-")),
            "{calls:?}"
        );
        let left: Vec<String> = trash
            .lock()
            .unwrap()
            .iter()
            .filter(|path| path.contains("f000001-"))
            .cloned()
            .collect();
        assert_eq!(left.len(), 1, "{left:?}");
        let errors = &outcome.report.summary.errors;
        assert!(
            errors
                .iter()
                .any(|error| error.contains("identity capture of file 1 failed")),
            "{errors:?}"
        );
        assert!(
            errors.iter().any(|error| error.contains("trash purge of")),
            "{errors:?}"
        );
    }

    #[tokio::test]
    async fn the_many_files_workload_says_when_its_files_stay_in_the_trash() {
        let (_state, _trash, provider) = trash_remote(TrashKind::Unpurgeable);
        let outcome = run(provider, "/data", many_files_options(), &Silent)
            .await
            .unwrap();
        assert!(
            outcome
                .report
                .notes
                .iter()
                .any(|n| n.contains("3 files the many-files workload deleted")),
            "{:?}",
            outcome.report.notes
        );
        assert!(
            !outcome
                .report
                .summary
                .errors
                .iter()
                .any(|e| e.contains("trash")),
            "{:?}",
            outcome.report.summary.errors
        );
    }

    #[tokio::test]
    async fn two_many_files_runs_never_reuse_a_trash_file_name() {
        let (state, trash, provider) = trash_remote(TrashKind::Unpurgeable);
        let first = run(provider, "/data", many_files_options(), &Silent)
            .await
            .unwrap();
        let provider = Box::new(TrashKeepingRemote {
            inner: MemRemote(state),
            kind: TrashKind::Unpurgeable,
            trash: trash.clone(),
            purge_calls: Arc::default(),
            hang_purge: false,
            cancel_on_purge: None,
            fail_live_purge: false,
            fail_capture_index: None,
        });
        let second = run(provider, "/data", many_files_options(), &Silent)
            .await
            .unwrap();
        let names: Vec<String> = trash
            .lock()
            .unwrap()
            .iter()
            .filter(|path| path.ends_with(".dat"))
            .map(|path| path.rsplit('/').next().unwrap().to_string())
            .collect();
        assert_eq!(names.len(), 6, "{names:?}");
        assert_eq!(names.iter().collect::<BTreeSet<_>>().len(), 6, "{names:?}");
        for report in [first.report, second.report] {
            let run_id = &report.report_id[..8];
            assert_eq!(
                names.iter().filter(|name| name.contains(run_id)).count(),
                3,
                "run {run_id}: {names:?}"
            );
        }
    }

    /// A remote with the many-files workload's three files in its trash, the
    /// paths its purge was asked for, and the provider.
    fn purging_remote(
        hang_purge: bool,
        cancel_on_purge: Option<CancellationToken>,
    ) -> (Arc<Mutex<Vec<String>>>, Box<dyn StorageProvider>) {
        let (state, _) = remote();
        let purge_calls = Arc::new(Mutex::new(Vec::new()));
        let provider = Box::new(TrashKeepingRemote {
            inner: MemRemote(state),
            kind: TrashKind::OriginalPath,
            trash: Arc::new(Mutex::new((0..3).map(|i| format!("/data/f{i}")).collect())),
            purge_calls: purge_calls.clone(),
            hang_purge,
            cancel_on_purge,
            fail_live_purge: false,
            fail_capture_index: None,
        });
        (purge_calls, provider)
    }

    #[tokio::test]
    async fn the_many_files_purge_ends_at_its_budget_not_one_bound_per_file() {
        let (calls, mut provider) = purging_remote(true, None);
        let mut errors = Vec::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            purge_deleted_files_with_identities(
                &mut provider,
                &[0, 1, 2],
                &|i| format!("/data/f{i}"),
                &mut vec![Ok(None); 3],
                &CancellationToken::new(),
                std::time::Duration::from_millis(200),
                &mut errors,
                &mut Vec::new(),
            ),
        )
        .await
        .expect("the purge ends at its budget");
        // The first purge never answered: the pass gave up there instead of
        // waiting the same bound on each file after it.
        assert_eq!(calls.lock().unwrap().len(), 1);
        assert!(
            errors
                .iter()
                .any(|e| e
                    .contains("3 of the 3 files the many-files workload deleted were not purged")),
            "{errors:?}"
        );
    }

    #[tokio::test]
    async fn the_many_files_purge_ends_at_a_stop() {
        let stop = CancellationToken::new();
        let (calls, mut provider) = purging_remote(false, Some(stop.clone()));
        let mut errors = Vec::new();
        purge_deleted_files_with_identities(
            &mut provider,
            &[0, 1, 2],
            &|i| format!("/data/f{i}"),
            &mut vec![Ok(None); 3],
            &stop,
            std::time::Duration::from_secs(120),
            &mut errors,
            &mut Vec::new(),
        )
        .await;
        // The stop came during the first purge, which finished; the other
        // two were not started.
        assert_eq!(calls.lock().unwrap().len(), 1);
        assert!(
            errors.iter().any(|e| e
                .contains("2 of the 3 files the many-files workload deleted were not purged")
                && e.contains("stopped")),
            "{errors:?}"
        );
    }

    #[tokio::test]
    async fn a_run_probes_latency_and_reports_a_signal() {
        let (_state, provider) = remote();
        let mut opts = options(BenchmarkLevel::Quick, "1K", None);
        opts.connect_ms = Some(42.0);
        let outcome = run(provider, "/data", opts, &Silent).await.unwrap();
        let report = &outcome.report;
        assert_eq!(report.results[0].operation, "connect");
        assert_eq!(report.results[0].latency_ms.p50, 42.0);
        let latency = report
            .results
            .iter()
            .find(|r| r.operation == "latency")
            .expect("a latency row");
        assert_eq!(
            latency.runs,
            crate::community_benchmark::engine::BENCHMARK_LATENCY_SAMPLES
        );
        let signal = report.signal.as_ref().expect("a signal");
        // An in-memory round trip is well under 80 ms: four bars.
        assert_eq!(signal.bars, Some(4));
        assert!(!signal.local_bridge);
        assert_eq!(signal.samples, latency.runs);
    }

    #[tokio::test]
    async fn a_local_endpoint_gets_no_bars() {
        let (_state, provider) = remote();
        let mut opts = options(BenchmarkLevel::Quick, "1K", None);
        opts.local_endpoint = true;
        let outcome = run(provider, "/data", opts, &Silent).await.unwrap();
        let signal = outcome.report.signal.expect("a signal");
        assert!(signal.local_bridge && !signal.metadata_cached);
        assert_eq!(signal.bars, None);
    }

    #[tokio::test]
    async fn a_prefix_folder_the_run_created_is_removed_and_one_it_found_is_kept() {
        let (state, provider) = remote();
        state.lock().unwrap().dirs.insert("/keep".into());
        let mut opts = options(BenchmarkLevel::Quick, "1K", None);
        opts.test_root_prefix = Some("/keep/new/deeper".into());
        let outcome = run(provider, "/data", opts, &Silent).await.unwrap();
        assert!(
            outcome.report.summary.errors.is_empty(),
            "{:?}",
            outcome.report.summary.errors
        );
        let r = state.lock().unwrap();
        assert_eq!(
            r.dirs,
            BTreeSet::from(["/data".to_string(), "/keep".to_string()])
        );
        assert!(r.files.is_empty(), "{:?}", r.files.keys());
    }

    #[tokio::test]
    async fn a_refused_upload_is_a_failed_row_and_a_failed_run() {
        let (state, provider) = remote();
        state.lock().unwrap().refuse_uploads = true;
        let outcome = run(
            provider,
            "/data",
            options(BenchmarkLevel::Quick, "1K", None),
            &Silent,
        )
        .await
        .unwrap();
        assert!(outcome.failed && outcome.early_abort);
        let upload = outcome
            .report
            .results
            .iter()
            .find(|r| r.operation == "upload")
            .expect("the failed upload is reported");
        assert_eq!(upload.runs, 0);
        assert_eq!(upload.errors.fatal, 1);
        assert!(outcome.report.signal.is_none());
        let r = state.lock().unwrap();
        assert_eq!(r.dirs, BTreeSet::from(["/data".to_string()]));
    }

    #[tokio::test]
    async fn a_server_that_stops_answering_ends_at_the_profile_timeout_and_cleans_up() {
        let (state, provider) = remote();
        state.lock().unwrap().hang_downloads = true;
        let mut opts = options(BenchmarkLevel::Quick, "1K", None);
        opts.profile_timeout_secs = 1;
        let started = std::time::Instant::now();
        let outcome = run(provider, "/data", opts, &Silent).await.unwrap();
        assert!(started.elapsed() < std::time::Duration::from_secs(30));
        assert!(outcome.timed_out && !outcome.cancelled && !outcome.failed);
        assert!(outcome
            .report
            .summary
            .errors
            .iter()
            .any(|e| e.contains("profile-timeout")));
        // The upload measured before the hang is kept.
        assert!(outcome
            .report
            .results
            .iter()
            .any(|r| r.operation == "upload" && r.runs == 1));
        let r = state.lock().unwrap();
        assert!(r.files.is_empty(), "{:?}", r.files.keys());
        assert_eq!(r.dirs, BTreeSet::from(["/data".to_string()]));
    }

    #[tokio::test]
    async fn the_json_of_a_run_names_none_of_its_remote_paths() {
        let (_state, provider) = remote();
        let mut opts = options(BenchmarkLevel::Quick, "1K", None);
        opts.test_root_prefix = Some("/Private Stuff/bench".into());
        let outcome = run(provider, "/data", opts, &Silent).await.unwrap();
        let mut report = outcome.report;
        report
            .summary
            .errors
            .push("delete of /Private Stuff/bench failed".into());
        let json = sanitized_reports_json(&report, &[&report]).unwrap();
        assert!(!json.contains("Private Stuff"), "{json}");
        assert!(json.contains("<bench-folder>"), "{json}");
    }
}

#[test]
fn a_file_count_alone_replaces_the_size_sweep_and_with_sizes_runs_both() {
    let (config, many) =
        resolve_benchmark_plan(BenchmarkLevel::Quick, None, None, None, Some(10), "64K").unwrap();
    assert!(config.sizes_bytes.is_empty());
    assert_eq!(many.map(|m| m.file_count), Some(10));
    let (config, many) = resolve_benchmark_plan(
        BenchmarkLevel::Quick,
        Some("1M"),
        None,
        None,
        Some(10),
        "64K",
    )
    .unwrap();
    assert_eq!(config.sizes_bytes, vec![1024 * 1024]);
    assert!(many.is_some());
    let (config, many) =
        resolve_benchmark_plan(BenchmarkLevel::Quick, None, None, None, None, "64K").unwrap();
    assert_eq!(config.sizes_bytes, vec![10 * 1024 * 1024]);
    assert!(many.is_none());
}

#[test]
fn signal_bars_follow_the_thresholds_and_lose_one_when_unsteady() {
    assert_eq!(benchmark_signal_bars(40.0, 50.0), 4);
    assert_eq!(benchmark_signal_bars(80.0, 90.0), 4);
    assert_eq!(benchmark_signal_bars(150.0, 160.0), 3);
    assert_eq!(benchmark_signal_bars(400.0, 450.0), 2);
    assert_eq!(benchmark_signal_bars(1200.0, 1300.0), 1);
    assert_eq!(benchmark_signal_bars(2000.0, 2100.0), 0);
    // p95 more than twice the median: one bar less.
    assert_eq!(benchmark_signal_bars(40.0, 120.0), 3);
    assert_eq!(benchmark_signal_bars(2000.0, 9000.0), 0);
}

#[test]
fn loopback_hosts_are_local_bridges_and_others_are_not() {
    for host in [
        "127.0.0.1",
        "http://127.0.0.1:4443/",
        "localhost",
        "http://localhost:8080/dav",
        "[::1]:4443",
        "::1",
        "127.0.0.2",
        "bridge.localhost",
    ] {
        assert!(benchmark_is_loopback_host(host), "{host}");
    }
    for host in [
        "jfs.jottacloud.com",
        "https://webdav.yandex.ru",
        "192.168.1.10",
        "10.0.0.1:22",
        "localhost.example.com",
    ] {
        assert!(!benchmark_is_loopback_host(host), "{host}");
    }
}

#[test]
fn urls_ipv6_and_spaced_user_folders_leave_no_trace_and_clock_times_stay() {
    let report = test_report(vec![
        "upload failed: error sending request for url (https://nas.example.org:8443/remote.php/dav/files/alice/x)".into(),
        "connect to 2001:db8:85a3::8a2e:370:7334 refused".into(),
        "connect to fe80:0:0:0:200:f8ff:fe21:67cf refused".into(),
        "local temp C:\\Users\\John Smith\\AppData failed".into(),
        "local temp /home/John Smith/tmp failed".into(),
        "retry at 12:00:00".into(),
    ]);
    let json = sanitized_report_json(&report).unwrap();
    for gone in [
        "nas.example.org",
        "alice",
        "2001:db8",
        "fe80:0",
        "John",
        "Smith",
    ] {
        assert!(!json.contains(gone), "{gone} in {json}");
    }
    assert!(json.contains("<redacted-url>"), "{json}");
    assert!(json.contains("12:00:00"), "{json}");
    assert!(json.contains("2026-10-08T15:00:00Z"), "{json}");
}

#[test]
fn private_paths_are_replaced_longest_first_and_the_root_is_left_alone() {
    let paths = vec![
        ("/Work/bench".to_string(), "<bench-folder>"),
        ("/Work/bench/run-1".to_string(), "<run-folder>"),
        ("/".to_string(), "<start-path>"),
    ];
    let out = scrub_private_paths(
        "cleanup of /Work/bench/run-1 and /Work/bench at /".to_string(),
        &paths,
    );
    assert_eq!(out, "cleanup of <run-folder> and <bench-folder> at /");
}

#[test]
fn compressed_ipv6_addresses_are_redacted_whole() {
    let report = test_report(vec![
        "connect to 2001:db8:85a3::8a2e:370:7334 refused".into(),
        "link-local fe80::200:f8ff:fe21:67cf unreachable".into(),
        "route 2001:db8:: has no gateway".into(),
        "loopback ::1 and fe80::1 refused".into(),
        "mapped ::ffff:192.0.2.1 refused".into(),
        "failed to connect to 2001:db9::1: Connection refused".into(),
        "2001:db9:85a3::8a2e:370:7335: timed out".into(),
        "host foo.2001:dba::1 is down".into(),
        "reset by 2001:dbb::1.Retrying".into(),
        "resolved fe80::201:f8ff:fe21:67ce.local".into(),
        "mapped ::ffff:192.0.2.9.extra".into(),
        "retry at 12:00:00, see std::fmt and lib::providers".into(),
    ]);
    let json = sanitized_report_json(&report).unwrap();
    for gone in [
        "370:7334",
        "8a2e",
        "f8ff:fe21:67cf",
        "fe21",
        "2001:db8",
        "::1",
        "fe80",
        "::ffff",
        "192.0.2.1",
        "2001:db9",
        "370:7335",
        "2001:dba",
        "dba::1",
        "2001:dbb",
        "67ce",
        "fe21",
        "192.0.2.9",
    ] {
        assert!(!json.contains(gone), "{gone} in {json}");
    }
    assert!(
        json.contains("failed to connect to <redacted-ip>: Connection refused"),
        "{json}"
    );
    assert!(json.contains("host foo.<redacted-ip> is down"), "{json}");
    assert!(json.contains("reset by <redacted-ip>.Retrying"), "{json}");
    assert!(json.contains("resolved <redacted-ip>.local"), "{json}");
    assert!(json.contains("mapped <redacted-ip>.extra"), "{json}");
    for kept in [
        "12:00:00",
        "std::fmt",
        "lib::providers",
        "2026-10-08T15:00:00Z",
    ] {
        assert!(json.contains(kept), "{kept} lost in {json}");
    }
    assert!(
        json.contains("route <redacted-ip> has no gateway"),
        "{json}"
    );
}

#[test]
fn the_sweep_refuses_a_compressed_ipv6_left_in_place() {
    assert!(benchmark_sanitization_sweep("at fe80::200:f8ff:fe21:67cf").is_err());
    assert!(benchmark_sanitization_sweep("at 2001:db8::").is_err());
    assert!(benchmark_sanitization_sweep("to 2001:db8::1: Connection refused").is_err());
    assert!(benchmark_sanitization_sweep("by 2001:db8::1.Retrying").is_err());
    assert!(benchmark_sanitization_sweep("at 11:18:10.123 and 23T11:18:10Z").is_ok());
    assert!(benchmark_sanitization_sweep("at 12:00:00 in std::fmt").is_ok());
}

#[test]
fn a_long_run_from_a_remote_error_is_redacted_quickly_and_whole() {
    // A provider error can carry a hex dump: one run of thousands of
    // characters made of hex digits and colons. Eight `ab` groups are already
    // an IPv6 address, so every one of them goes, and the address after the
    // dump is redacted too.
    let dump = "ab:".repeat(7000);
    let report = test_report(vec![format!("body {dump} from fe80::1 refused")]);
    let started = std::time::Instant::now();
    let json = sanitized_report_json(&report).unwrap();
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
    assert!(!json.contains("ab:ab"), "a piece of the dump is left");
    assert!(!json.contains("fe80"), "the address after the dump is left");
    assert!(json.contains("from <redacted-ip> refused"));
}

#[test]
fn a_long_run_with_no_address_in_it_is_scanned_quickly() {
    // No start of this run parses: the worst case for the candidate scan,
    // which must stay bounded by the length of an address, not of the run.
    let dump = "xy:".repeat(7000);
    let started = std::time::Instant::now();
    let json = sanitized_report_json(&test_report(vec![format!("body {dump} end")])).unwrap();
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
    assert!(json.contains("xy:xy:xy"));
}

#[test]
fn each_client_gets_its_own_protocol_label_and_signal_kind() {
    use BenchmarkClient::*;
    // (client, provider type label) -> (Protocol, local bridge, metadata cache)
    let cases = [
        // MEGAcmd answers from its server on this machine.
        (MegaCmd, "REST API", false, false, ("CLI", true, false)),
        // Native MEGA answers stat from the tree it keeps in memory.
        (
            MegaNative,
            "REST API",
            false,
            false,
            ("REST API", false, true),
        ),
        // AeroFTP's own Proton client: Proton's API, answered from its cache.
        (ProtonNative, "CLI", false, false, ("REST API", false, true)),
        // Proton's official CLI reaches the service on every call: bars.
        (ProtonCli, "CLI", false, false, ("CLI", false, false)),
        // A loopback host or a Filen Desktop note is a local bridge.
        (Other, "WebDAV", true, false, ("WebDAV", true, false)),
        (Other, "S3", false, true, ("S3", true, false)),
        (Other, "REST API", false, false, ("REST API", false, false)),
    ];
    for (client, label, local, note, (access, bridge, cached)) in cases {
        assert_eq!(
            benchmark_endpoint_kind(client, label, local, note),
            (access.to_string(), bridge, cached),
            "{client:?}"
        );
    }
}
