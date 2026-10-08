// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use super::engine::many_files_result;
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
    assert_eq!(cfg.operations, vec!["upload", "download"]);
}

#[test]
fn a_refused_trash_purge_is_reported() {
    let mut errors = Vec::new();
    assert!(!note_trash_purge("aeroftp-bench", Ok(false), &mut errors));
    assert!(note_trash_purge("aeroftp-bench", Ok(true), &mut errors));
    assert!(errors.is_empty(), "{errors:?}");
    assert!(!note_trash_purge(
        "aeroftp-bench",
        Err(ProviderError::Other("2 items named 'aeroftp-bench'".into())),
        &mut errors,
    ));
    assert_eq!(errors.len(), 1);
    assert!(
        errors[0].starts_with("trash purge of aeroftp-bench failed")
            && errors[0].ends_with("2 items named 'aeroftp-bench'"),
        "{errors:?}"
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
    let (base, _) = benchmark_remote_roots(" /srv/data/ ", "rid");
    assert_eq!(base, "/srv/data/aeroftp-bench");
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
            Ok("/".into())
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
            let bytes = std::fs::read(l).map_err(|e| ProviderError::Other(e.to_string()))?;
            let mut remote = self.0.lock().unwrap();
            remote.uploads += 1;
            remote.files.insert(r.into(), bytes);
            Ok(())
        }
        async fn mkdir(&mut self, p: &str) -> Result<(), ProviderError> {
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
            cancel,
        }
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
        assert_eq!(ops, vec!["upload", "download"]);
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
