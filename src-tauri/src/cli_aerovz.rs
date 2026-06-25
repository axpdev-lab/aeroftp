use clap::Subcommand;
use serde::Serialize;
use std::io::Read;
use std::path::{Path, PathBuf};

use super::{
    archive_input_size, format_size, print_error, print_json, resolve_local_path, Cli, OutputFormat,
};

const PRODUCT_ARCHIVE_EXT: &str = "aerozip";
const PRODUCT_ARCHIVE_EXTENSION: &str = ".aerozip";
const PRODUCT_ARCHIVE_MIME: &str = "application/x-aerozip";
const DEFAULT_RECOVERY_PCT: u32 = 20;

/// Plaintext `.aerozip` archive operations.
///
/// `.aerozip` is the AeroFTP app-layer extension for a passwordless,
/// unencrypted archive: compression + integrity + Reed-Solomon recovery,
/// NOT confidentiality. Anyone with the file can read the contents.
#[derive(Subcommand)]
pub(crate) enum ArchiveCommands {
    /// Create an unencrypted `.aerozip` archive (application/x-aerozip).
    ///
    /// This lane never accepts a password. It provides compression, integrity
    /// checks and recovery parity; it does NOT provide confidentiality.
    Create {
        /// Output archive path. Must end in `.aerozip`.
        output: String,
        /// Files and/or folders to add to the archive.
        #[arg(required = true)]
        paths: Vec<String>,
        /// Compression profile: fast | balanced | archive. Defaults to archive for AeroVault Zip.
        #[arg(long = "compression-profile", alias = "profile", default_value = "archive", value_parser = ["fast", "balanced", "archive"])]
        profile: String,
        /// Reed-Solomon recovery overhead percentage: low, medium, quartile,
        /// high, or a number from 5 to 50. Default 20. Use 0 (or `off`/`none`)
        /// to disable recovery entirely for the smallest possible archive.
        #[arg(long = "recovery-level", default_value = "20")]
        recovery_level: String,
        /// Refused: `.aerozip` is an unencrypted lane and has no password.
        #[arg(long, short = 'p')]
        password: Option<String>,
    },
    /// Extract an unencrypted `.aerozip` archive.
    ///
    /// If recovery parity can repair detected damage, extraction repairs the
    /// archive all-or-nothing first and reports the repaired chunk count.
    Extract {
        /// Input archive path. Header detection accepts renamed files too.
        archive: String,
        /// Output directory.
        #[arg(default_value = ".")]
        outdir: String,
        /// Refused: `.aerozip` is an unencrypted lane and has no password.
        #[arg(long, short = 'p')]
        password: Option<String>,
    },
    /// List entries in an unencrypted `.aerozip` archive.
    List {
        /// Input archive path. Header detection accepts renamed files too.
        archive: String,
        /// Refused: `.aerozip` is an unencrypted lane and has no password.
        #[arg(long, short = 'p')]
        password: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize)]
struct AerovzEntryReport {
    path: String,
    size: u64,
    is_dir: bool,
    modified: String,
    chunk_count: usize,
}

#[derive(Debug, Clone, Serialize)]
struct AerovzRecoveryReport {
    enabled: bool,
    embedded: bool,
    detached: bool,
    manifest_parity: bool,
    header_parity: bool,
    overhead_pct: u32,
}

#[derive(Debug, Clone, Serialize)]
struct AerovzCreateReport {
    status: &'static str,
    operation: &'static str,
    format: &'static str,
    extension: &'static str,
    mime: &'static str,
    encrypted: bool,
    confidential: bool,
    integrity_recovery: bool,
    output: String,
    inputs: usize,
    file_count: usize,
    entry_count: usize,
    chunk_count: usize,
    dedup_chunks: usize,
    compression_profile: String,
    compression_level: i32,
    input_bytes: u64,
    output_bytes: u64,
    ratio_pct: f64,
    space_saved_pct: f64,
    recovery: AerovzRecoveryReport,
}

#[derive(Debug, Clone, Serialize)]
struct AerovzExtractReport {
    status: &'static str,
    operation: &'static str,
    format: &'static str,
    extension: &'static str,
    mime: &'static str,
    encrypted: bool,
    confidential: bool,
    integrity_recovery: bool,
    archive: String,
    output_dir: String,
    archive_bytes: u64,
    extracted_bytes: u64,
    file_count: u64,
    damaged_chunks: usize,
    repaired_chunks: usize,
    repair_source: String,
}

#[derive(Debug, Clone, Serialize)]
struct AerovzListReport {
    status: &'static str,
    operation: &'static str,
    format: &'static str,
    extension: &'static str,
    mime: &'static str,
    encrypted: bool,
    confidential: bool,
    integrity_recovery: bool,
    archive: String,
    archive_bytes: u64,
    file_count: usize,
    entry_count: usize,
    chunk_count: usize,
    dedup_chunks: usize,
    compression_level: i32,
    recovery: AerovzRecoveryReport,
    entries: Vec<AerovzEntryReport>,
}

pub(crate) fn cmd_archive(command: &ArchiveCommands, cli: &Cli, format: OutputFormat) -> i32 {
    // The plaintext lane never consumes a password from ANY source. The
    // per-subcommand `--password` is caught by reject_password below; the
    // GLOBAL `--password-stdin` flag would otherwise be silently ignored and
    // leave the user believing the archive was encrypted, so reject it here
    // too (same message + config exit code as the inline flag).
    if cli.password_stdin {
        return fail(format, PASSWORD_REJECTED_MSG, 5);
    }
    let result = match command {
        ArchiveCommands::Create {
            output,
            paths,
            profile,
            recovery_level,
            password,
        } => {
            if let Err(e) = reject_password(password) {
                return fail(format, &e, 5);
            }
            create_archive(output, paths, profile, recovery_level).map(ArchiveCommandResult::Create)
        }
        ArchiveCommands::Extract {
            archive,
            outdir,
            password,
        } => {
            if let Err(e) = reject_password(password) {
                return fail(format, &e, 5);
            }
            extract_archive(archive, outdir).map(ArchiveCommandResult::Extract)
        }
        ArchiveCommands::List { archive, password } => {
            if let Err(e) = reject_password(password) {
                return fail(format, &e, 5);
            }
            list_archive(archive).map(ArchiveCommandResult::List)
        }
    };

    match result {
        Ok(ArchiveCommandResult::Create(report)) => {
            match format {
                OutputFormat::Json => print_json(&report),
                OutputFormat::Text => {
                    println!(
                        "Created .aerozip archive: {} ({} input(s), {} file(s), {}, {:.1}% of original, recovery {}%)",
                        report.output,
                        report.inputs,
                        report.file_count,
                        format_size(report.output_bytes),
                        report.ratio_pct,
                        report.recovery.overhead_pct,
                    );
                    if !cli.quiet {
                        eprintln!(
                            "Note: .aerozip is unencrypted: integrity + recovery, not confidentiality."
                        );
                    }
                }
            }
            0
        }
        Ok(ArchiveCommandResult::Extract(report)) => {
            match format {
                OutputFormat::Json => print_json(&report),
                OutputFormat::Text => {
                    println!(
                        "Extracted .aerozip archive: {} -> {} ({} unpacked, {} file(s))",
                        report.archive,
                        report.output_dir,
                        format_size(report.extracted_bytes),
                        report.file_count,
                    );
                    if report.repaired_chunks > 0 && !cli.quiet {
                        eprintln!(
                            "Repaired {} damaged chunk(s) from {} recovery before extraction.",
                            report.repaired_chunks, report.repair_source
                        );
                    }
                }
            }
            0
        }
        Ok(ArchiveCommandResult::List(report)) => {
            match format {
                OutputFormat::Json => print_json(&report),
                OutputFormat::Text => {
                    println!(
                        "{}: {} file(s), {} entries, {}, recovery {}",
                        report.archive,
                        report.file_count,
                        report.entry_count,
                        format_size(report.archive_bytes),
                        if report.recovery.enabled {
                            "enabled"
                        } else {
                            "not present"
                        }
                    );
                    for entry in &report.entries {
                        let kind = if entry.is_dir { "dir " } else { "file" };
                        println!(
                            "{kind} {:>10} {:>4} chunks  {}",
                            format_size(entry.size),
                            entry.chunk_count,
                            entry.path
                        );
                    }
                }
            }
            0
        }
        Err(e) => fail(format, &e, archive_exit_code(&e)),
    }
}

/// Map an archive-lane error string to the CLI's structured exit-code contract
/// (see `docs/CLI-GUIDE.md`): not-found -> 2, already-exists / refuse-overwrite
/// -> 9, everything else -> 1. The lane surfaces `String` errors rather than a
/// typed `ProviderError`, so this matches on the stable message fragments the
/// lane itself emits (`No such file or directory`, `Refusing to overwrite`).
fn archive_exit_code(msg: &str) -> i32 {
    let m = msg.to_ascii_lowercase();
    if m.contains("no such file") || m.contains("not readable") || m.contains("not found") {
        2
    } else if m.contains("refusing to overwrite") || m.contains("already exists") {
        9
    } else {
        1
    }
}

enum ArchiveCommandResult {
    Create(AerovzCreateReport),
    Extract(AerovzExtractReport),
    List(AerovzListReport),
}

pub(crate) fn vault_plaintext_lane_rejection(command: &super::VaultCommands) -> Option<String> {
    let path = vault_command_path(command)?;
    if is_aerozip_path(path) || is_plaintext_archive_file(path) {
        Some(
            ".aerozip is the unencrypted archive lane (integrity + recovery, not confidentiality). Use `aeroftp archive ...`; encrypted vault commands require `.aerovault`.".to_string(),
        )
    } else {
        None
    }
}

fn vault_command_path(command: &super::VaultCommands) -> Option<&str> {
    match command {
        super::VaultCommands::Create { path, .. }
        | super::VaultCommands::Add { path, .. }
        | super::VaultCommands::AddDir { path, .. }
        | super::VaultCommands::ExtractAll { path, .. }
        | super::VaultCommands::Mkdir { path, .. }
        | super::VaultCommands::Rm { path, .. }
        | super::VaultCommands::Mv { path, .. }
        | super::VaultCommands::Cp { path, .. }
        | super::VaultCommands::ChangePassword { path, .. }
        | super::VaultCommands::Compact { path, .. }
        | super::VaultCommands::SyncCompare { path, .. }
        | super::VaultCommands::SyncApply { path, .. }
        | super::VaultCommands::RecoveryStatus { path }
        | super::VaultCommands::Info { path, .. }
        | super::VaultCommands::Extract { path, .. }
        | super::VaultCommands::Scrub { path, .. }
        | super::VaultCommands::Repair { path, .. }
        | super::VaultCommands::ExportParity { path, .. }
        | super::VaultCommands::StripParity { path, .. }
        | super::VaultCommands::ChangeMode { path, .. } => Some(path.as_str()),
        super::VaultCommands::ScanDir { .. } => None,
    }
}

fn fail(format: OutputFormat, msg: &str, code: i32) -> i32 {
    print_error(format, msg, code);
    code
}

fn create_archive(
    output: &str,
    paths: &[String],
    profile: &str,
    recovery_level: &str,
) -> Result<AerovzCreateReport, String> {
    let recovery_pct = parse_recovery_level(recovery_level)?;
    let out_string = resolve_local_path(output);
    ensure_aerozip_path(&out_string)?;
    let out_path = PathBuf::from(&out_string);
    if out_path.exists() {
        return Err(format!(
            "Refusing to overwrite existing .aerozip archive: {}",
            out_path.display()
        ));
    }
    if let Some(parent) = out_path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| format!("Create output directory: {e}"))?;
    }

    let sources = resolve_archive_sources(paths)?;
    let resolved_for_size: Vec<String> = sources
        .iter()
        .map(|source| source.path.to_string_lossy().to_string())
        .collect();
    let input_bytes = archive_input_size(&resolved_for_size);

    let create_result = create_archive_inner(&out_path, &sources, profile, recovery_pct);
    if let Err(e) = create_result {
        let _ = std::fs::remove_file(&out_path);
        return Err(e);
    }

    let vault = open_plaintext_archive(&out_path)?;
    let summary = aerovault::v3::VaultV3::summary(&vault);
    let output_bytes = std::fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0);
    let ratio_pct = ratio_pct(output_bytes, input_bytes);
    let recovery = recovery_report(&out_path, recovery_pct)?;

    Ok(AerovzCreateReport {
        status: "ok",
        operation: "archive.create",
        format: "aerozip",
        extension: PRODUCT_ARCHIVE_EXTENSION,
        mime: PRODUCT_ARCHIVE_MIME,
        encrypted: false,
        confidential: false,
        integrity_recovery: recovery_pct != 0,
        output: out_string,
        inputs: sources.len(),
        file_count: summary.file_count,
        entry_count: summary.entries.len(),
        chunk_count: summary.chunk_count,
        dedup_chunks: summary.dedup_chunks,
        compression_profile: profile.to_string(),
        compression_level: summary.compression_level,
        input_bytes,
        output_bytes,
        ratio_pct,
        space_saved_pct: 100.0 - ratio_pct,
        recovery,
    })
}

fn create_archive_inner(
    out_path: &Path,
    sources: &[ArchiveSource],
    profile: &str,
    recovery_pct: u32,
) -> Result<(), String> {
    let opts = apply_archive_profile(
        aerovault::v3::CreateOptionsV3::new_plaintext(out_path.to_path_buf()),
        profile,
    )?;
    if recovery_pct == 0 {
        // Recovery (Reed-Solomon parity) explicitly disabled: create a plain archive
        // with no parity so .aerozip can match canonical compression ratios.
        aerovault::v3::VaultV3::create(&opts).map_err(aerovz_error)?;
    } else {
        aerovault::v3::VaultV3::create_with_error_correction(
            &opts,
            aerovault::v3::RecoveryPlacement::Embedded,
            recovery_pct,
        )
        .map_err(aerovz_error)?;
    }

    let mut vault = open_plaintext_archive(out_path)?;
    for source in sources {
        if source.is_dir {
            let prefix = archive_entry_basename(&source.path)?;
            aerovault::v3::VaultV3::add_directory(&mut vault, &source.path, Some(&prefix))
                .map_err(aerovz_error)?;
        } else {
            let entry_name = archive_entry_basename(&source.path)?;
            let mapped = [(source.path.clone(), entry_name)];
            aerovault::v3::VaultV3::add_files(&mut vault, &mapped).map_err(aerovz_error)?;
        }
    }
    Ok(())
}

fn extract_archive(archive: &str, outdir: &str) -> Result<AerovzExtractReport, String> {
    let archive_string = resolve_local_path(archive);
    let archive_path = PathBuf::from(&archive_string);
    let outdir_string = resolve_local_path(outdir);
    let outdir_path = PathBuf::from(&outdir_string);
    std::fs::create_dir_all(&outdir_path).map_err(|e| format!("Create output directory: {e}"))?;

    let mut vault = open_plaintext_archive(&archive_path)?;
    let damaged_chunks = aerovault::v3::VaultV3::scrub(&vault).len();
    let mut repaired_chunks = 0usize;
    let mut repair_source = "none".to_string();
    if damaged_chunks > 0 {
        let (repaired, source) =
            aerovault::v3::VaultV3::repair(&mut vault, false, None).map_err(aerovz_error)?;
        repaired_chunks = repaired;
        repair_source = source.as_str().to_string();
        drop(vault);
        vault = open_plaintext_archive(&archive_path)?;
    }

    let file_count =
        aerovault::v3::VaultV3::extract_all(&vault, &outdir_path).map_err(aerovz_error)?;
    let archive_bytes = std::fs::metadata(&archive_path)
        .map(|m| m.len())
        .unwrap_or(0);
    let extracted_bytes = archive_input_size(std::slice::from_ref(&outdir_string));
    // F-07: report the real parity presence, not a hardcoded `true`, so a
    // `--recovery-level 0` archive does not claim recovery it does not carry.
    let integrity_recovery = aerovault::v3::VaultV3::recovery_status(&archive_path)
        .map(|s| s.any)
        .unwrap_or(false);

    Ok(AerovzExtractReport {
        status: "ok",
        operation: "archive.extract",
        format: "aerozip",
        extension: PRODUCT_ARCHIVE_EXTENSION,
        mime: PRODUCT_ARCHIVE_MIME,
        encrypted: false,
        confidential: false,
        integrity_recovery,
        archive: archive_string,
        output_dir: outdir_string,
        archive_bytes,
        extracted_bytes,
        file_count,
        damaged_chunks,
        repaired_chunks,
        repair_source,
    })
}

fn list_archive(archive: &str) -> Result<AerovzListReport, String> {
    let archive_string = resolve_local_path(archive);
    let archive_path = PathBuf::from(&archive_string);
    let vault = open_plaintext_archive(&archive_path)?;
    let summary = aerovault::v3::VaultV3::summary(&vault);
    let recovery = recovery_report(&archive_path, DEFAULT_RECOVERY_PCT)?;
    let archive_bytes = std::fs::metadata(&archive_path)
        .map(|m| m.len())
        .unwrap_or(0);
    let entries = summary
        .entries
        .iter()
        .map(|entry| AerovzEntryReport {
            path: entry.path.clone(),
            size: entry.size,
            is_dir: entry.is_dir,
            modified: entry.modified.clone(),
            chunk_count: entry.chunk_count,
        })
        .collect();

    Ok(AerovzListReport {
        status: "ok",
        operation: "archive.list",
        format: "aerozip",
        extension: PRODUCT_ARCHIVE_EXTENSION,
        mime: PRODUCT_ARCHIVE_MIME,
        encrypted: false,
        confidential: false,
        // F-07: derive from the actual recovery status (already computed above).
        integrity_recovery: recovery.enabled,
        archive: archive_string,
        archive_bytes,
        file_count: summary.file_count,
        entry_count: summary.entries.len(),
        chunk_count: summary.chunk_count,
        dedup_chunks: summary.dedup_chunks,
        compression_level: summary.compression_level,
        recovery,
        entries,
    })
}

#[derive(Debug)]
struct ArchiveSource {
    path: PathBuf,
    is_dir: bool,
}

fn resolve_archive_sources(paths: &[String]) -> Result<Vec<ArchiveSource>, String> {
    let mut sources = Vec::with_capacity(paths.len());
    for raw in paths {
        let resolved = PathBuf::from(resolve_local_path(raw));
        let meta = std::fs::metadata(&resolved)
            .map_err(|e| format!("Archive input not readable '{}': {e}", resolved.display()))?;
        if !meta.is_file() && !meta.is_dir() {
            return Err(format!(
                "Archive input must be a file or directory: {}",
                resolved.display()
            ));
        }
        sources.push(ArchiveSource {
            path: resolved,
            is_dir: meta.is_dir(),
        });
    }
    Ok(sources)
}

fn apply_archive_profile(
    opts: aerovault::v3::CreateOptionsV3,
    profile: &str,
) -> Result<aerovault::v3::CreateOptionsV3, String> {
    Ok(match profile {
        "fast" => opts.with_zstd_level(3),
        "balanced" | "" => opts,
        "archive" => opts.with_zstd_level(15),
        other => return Err(format!("Unknown .aerozip compression profile: {other}")),
    })
}

fn recovery_report(path: &Path, overhead_pct: u32) -> Result<AerovzRecoveryReport, String> {
    let status = aerovault::v3::VaultV3::recovery_status(path).map_err(aerovz_error)?;
    Ok(AerovzRecoveryReport {
        enabled: status.any,
        embedded: status.embedded,
        detached: status.detached,
        manifest_parity: status.manifest_parity,
        header_parity: status.header_parity,
        // `recovery_status` exposes only presence flags, not the real parity
        // level, so `overhead_pct` is the caller's best estimate (the requested
        // pct on create, the default on inspect). Never report a non-zero
        // overhead for an archive that has no recovery data at all.
        overhead_pct: if status.any { overhead_pct } else { 0 },
    })
}

fn open_plaintext_archive(path: &Path) -> Result<aerovault::v3::OpenVaultV3, String> {
    if matches!(v3_plaintext_flag(path)?, Some(false)) {
        return Err("Not a plaintext .aerozip archive (it is encrypted)".to_string());
    }
    aerovault::v3::VaultV3::open_plaintext(path).map_err(aerovz_error)
}

fn v3_plaintext_flag(path: &Path) -> Result<Option<bool>, String> {
    let mut file = std::fs::File::open(path).map_err(|e| format!("Open archive: {e}"))?;
    let mut header_prefix = [0u8; 12];
    file.read_exact(&mut header_prefix)
        .map_err(|e| format!("Read archive header: {e}"))?;
    if &header_prefix[..10] != aerovault::v3::constants::MAGIC
        || header_prefix[10] != aerovault::v3::constants::VERSION
    {
        return Ok(None);
    }
    Ok(Some(
        header_prefix[11] & aerovault::v3::constants::FLAG_PLAINTEXT_CONTENT != 0,
    ))
}

/// Refusal message shared by every password entry point on the plaintext lane
/// (the per-subcommand `--password` and the global `--password-stdin`).
const PASSWORD_REJECTED_MSG: &str =
    ".aerozip is an unencrypted lane and has no password; use `.aerovault` if you need confidentiality.";

fn reject_password(password: &Option<String>) -> Result<(), String> {
    if password.is_some() {
        Err(PASSWORD_REJECTED_MSG.to_string())
    } else {
        Ok(())
    }
}

fn ensure_aerozip_path(path: &str) -> Result<(), String> {
    if is_aerozip_path(path) {
        Ok(())
    } else {
        Err(format!(
            "Expected a .aerozip archive path (MIME {PRODUCT_ARCHIVE_MIME}): {path}"
        ))
    }
}

pub(crate) fn is_aerozip_path(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case(PRODUCT_ARCHIVE_EXT))
        .unwrap_or(false)
}

fn is_plaintext_archive_file(path: &str) -> bool {
    let p = Path::new(path);
    p.exists() && aerovault::v3::VaultV3::open_plaintext(p).is_ok()
}

fn archive_entry_basename(path: &Path) -> Result<String, String> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("Invalid archive input name: {}", path.display()))?;
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
    {
        return Err(format!("Invalid archive entry name: {name}"));
    }
    Ok(name.to_string())
}

fn parse_recovery_level(raw: &str) -> Result<u32, String> {
    let pct = match raw.trim().to_ascii_lowercase().as_str() {
        "off" | "none" => 0,
        "low" => 7,
        "medium" | "med" => 15,
        "quartile" => 25,
        "high" => 30,
        other => other
            .trim_end_matches('%')
            .parse::<u32>()
            .map_err(|_| format!("Invalid .aerozip --recovery-level: {raw}"))?,
    };
    // 0 (or "off"/"none") fully disables recovery so the archive can match canonical
    // compression ratios; positive parity stays in the 5-50% band (sub-5 is rejected).
    if pct != 0 && !(5..=50).contains(&pct) {
        return Err(format!(
            "Invalid .aerozip --recovery-level {pct}: expected 0 (off) or 5-50"
        ));
    }
    Ok(pct)
}

fn ratio_pct(output_bytes: u64, input_bytes: u64) -> f64 {
    if input_bytes == 0 {
        0.0
    } else {
        output_bytes as f64 / input_bytes as f64 * 100.0
    }
}

fn aerovz_error(error: String) -> String {
    error
        .replace(".aerovz", ".aerozip")
        .replace("aerovz", "aerozip")
        .replace("AEROVZ", "AEROZIP")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::OpenOptions;
    use std::io::{Seek, SeekFrom, Write};

    #[test]
    fn extension_detection_is_case_insensitive() {
        assert!(is_aerozip_path("bundle.aerozip"));
        assert!(is_aerozip_path("bundle.AEROZIP"));
        assert!(!is_aerozip_path("bundle.aerovault"));
    }

    #[test]
    fn password_is_rejected_for_plaintext_lane() {
        let err = reject_password(&Some("secret".to_string())).unwrap_err();
        assert!(err.contains("unencrypted lane"));
    }

    #[test]
    fn archive_exit_code_follows_structured_contract() {
        // Not-found family -> 2
        assert_eq!(
            archive_exit_code(
                "Archive input not readable '/x': No such file or directory (os error 2)"
            ),
            2
        );
        assert_eq!(
            archive_exit_code("Open archive: No such file or directory (os error 2)"),
            2
        );
        // Already-exists / refuse-overwrite -> 9
        assert_eq!(
            archive_exit_code("Refusing to overwrite existing .aerozip archive: /x.aerozip"),
            9
        );
        // Anything else -> generic 1
        assert_eq!(
            archive_exit_code("Cipher block hash mismatch for chunk abc"),
            1
        );
    }

    #[test]
    fn recovery_level_accepts_names_and_rejects_out_of_range() {
        assert_eq!(parse_recovery_level("low").unwrap(), 7);
        assert_eq!(parse_recovery_level("medium").unwrap(), 15);
        assert_eq!(parse_recovery_level("25%").unwrap(), 25);
        // 0 / off / none fully disable recovery (opt-out for maximum compression).
        assert_eq!(parse_recovery_level("0").unwrap(), 0);
        assert_eq!(parse_recovery_level("off").unwrap(), 0);
        assert_eq!(parse_recovery_level("none").unwrap(), 0);
        // Sub-5 positive parity is still rejected; only 0 is a valid below-5 value.
        assert!(parse_recovery_level("4").is_err());
        assert!(parse_recovery_level("51").is_err());
    }

    #[test]
    fn archive_roundtrip_lists_and_extracts_plaintext_lane() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(src.join("nested")).unwrap();
        std::fs::write(src.join("hello.txt"), b"hello aerozip").unwrap();
        std::fs::write(src.join("nested").join("big.txt"), b"abc ".repeat(10_000)).unwrap();

        let archive = dir.path().join("bundle.aerozip");
        let report = create_archive(
            archive.to_str().unwrap(),
            &[src.to_string_lossy().to_string()],
            "balanced",
            "20",
        )
        .unwrap();
        assert_eq!(report.format, "aerozip");
        assert_eq!(report.mime, PRODUCT_ARCHIVE_MIME);
        assert!(!report.encrypted);
        assert!(!report.confidential);
        assert!(report.integrity_recovery);
        assert!(report.recovery.enabled);
        assert_eq!(report.file_count, 2);

        let listing = list_archive(archive.to_str().unwrap()).unwrap();
        assert_eq!(listing.file_count, 2);
        assert!(listing.entries.iter().any(|e| e.path == "src/hello.txt"));
        assert!(listing
            .entries
            .iter()
            .any(|e| e.path == "src/nested/big.txt"));

        let renamed = dir.path().join("bundle.renamed");
        std::fs::copy(&archive, &renamed).unwrap();
        let renamed_listing = list_archive(renamed.to_str().unwrap()).unwrap();
        assert_eq!(renamed_listing.file_count, 2);

        let out = dir.path().join("out");
        let extracted = extract_archive(archive.to_str().unwrap(), out.to_str().unwrap()).unwrap();
        assert_eq!(extracted.file_count, 2);
        assert_eq!(
            std::fs::read(out.join("src").join("hello.txt")).unwrap(),
            b"hello aerozip"
        );
        assert_eq!(
            std::fs::read(out.join("src").join("nested").join("big.txt")).unwrap(),
            b"abc ".repeat(10_000)
        );
    }

    #[test]
    fn archive_extract_self_heals_with_embedded_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        let payload: Vec<u8> = (0..300_000)
            .map(|i| ((i * 31 + i / 7 + 17) % 251) as u8)
            .collect();
        std::fs::write(src.join("payload.bin"), &payload).unwrap();

        let archive = dir.path().join("recover.aerozip");
        create_archive(
            archive.to_str().unwrap(),
            &[src.to_string_lossy().to_string()],
            "balanced",
            "20",
        )
        .unwrap();

        corrupt_data_section(&archive, 600);
        let out = dir.path().join("out");
        let extracted = extract_archive(archive.to_str().unwrap(), out.to_str().unwrap()).unwrap();
        assert!(extracted.damaged_chunks > 0);
        assert!(extracted.repaired_chunks > 0);
        assert_eq!(extracted.repair_source, "embedded");
        assert_eq!(
            std::fs::read(out.join("src").join("payload.bin")).unwrap(),
            payload
        );
    }

    #[test]
    fn archive_open_rejects_encrypted_container_even_with_aerozip_extension() {
        let dir = tempfile::tempdir().unwrap();
        let encrypted = dir.path().join("encrypted.aerozip");
        aerovault::v3::VaultV3::create(&aerovault::v3::CreateOptionsV3::new(
            &encrypted,
            "pw-12345678",
        ))
        .unwrap();
        let err = list_archive(encrypted.to_str().unwrap()).unwrap_err();
        assert!(err.contains("plaintext .aerozip archive"));
        assert!(err.contains("encrypted"));
    }

    #[test]
    fn vault_guard_rejects_plaintext_archive_even_if_renamed() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("file.txt");
        std::fs::write(&src, b"plain").unwrap();
        let archive = dir.path().join("plain.aerozip");
        create_archive(
            archive.to_str().unwrap(),
            &[src.to_string_lossy().to_string()],
            "balanced",
            "20",
        )
        .unwrap();
        let renamed = dir.path().join("plain.aerovault");
        std::fs::rename(&archive, &renamed).unwrap();
        assert!(is_plaintext_archive_file(renamed.to_str().unwrap()));
    }

    fn corrupt_data_section(vault_path: &Path, span: usize) {
        let info = aerovault::v3::VaultV3::peek(vault_path).unwrap();
        assert!(info.data_len as usize >= span);
        let start = 1024 + 8 + 16;
        let mut f = OpenOptions::new()
            .read(true)
            .write(true)
            .open(vault_path)
            .unwrap();
        f.seek(SeekFrom::Start(start)).unwrap();
        f.write_all(&vec![0xA5; span]).unwrap();
        f.flush().unwrap();
    }
}
