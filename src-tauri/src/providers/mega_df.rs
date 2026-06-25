//! Shared MEGAcmd `mega-df` quota helper.
//!
//! `mega-df` is useful beyond quota parsing: when the MEGAcmd Server has
//! been stopped with `quit` or `exit`, invoking any `mega-*` command starts
//! it again in the background. Issue #253 relies on that warm-up behavior.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use tokio::process::Command;

use super::{FileVersion, ProviderError};

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

const MEGA_DF_TIMEOUT_SECS: u64 = 30;
const MEGA_WEBDAV_TIMEOUT_SECS: u64 = 30;

/// Metadata key used to carry the GUI registry preset id through ProviderConfig.
pub const PROVIDER_ID_META_KEY: &str = "_aeroftp_provider_id";

/// Resolve MEGAcmd executable path (checks PATH and common install locations).
pub(crate) fn resolve_mega_cmd(cmd: &str) -> String {
    #[cfg(windows)]
    {
        let program_files =
            std::env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".to_string());
        let local_appdata = std::env::var("LOCALAPPDATA").unwrap_or_default();
        let candidates = [
            format!(r"{}\MEGAcmd\{}.bat", program_files, cmd),
            format!(r"{}\MEGAcmd\{}.exe", program_files, cmd),
            format!(r"{}\MEGAcmd\{}.bat", local_appdata, cmd),
            format!(r"{}\MEGAcmd\{}.exe", local_appdata, cmd),
        ];
        for candidate in &candidates {
            if std::path::Path::new(candidate).exists() {
                return candidate.clone();
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        let candidates = [
            format!("/Applications/MEGAcmd.app/Contents/MacOS/{}", cmd),
            format!("/usr/local/bin/{}", cmd),
            format!("/opt/homebrew/bin/{}", cmd),
        ];
        for candidate in &candidates {
            if std::path::Path::new(candidate).exists() {
                return candidate.clone();
            }
        }
    }
    cmd.to_string()
}

/// True for WebDAV presets backed by the local MEGAcmd bridge.
pub(crate) fn is_megacmd_webdav_provider_id(provider_id: Option<&str>) -> bool {
    matches!(provider_id, Some("megacmd") | Some("megacmd-webdav"))
}

/// Query MEGAcmd account quota by spawning `mega-df`.
///
/// Returns `(used, total, versioning_bytes)`. `versioning_bytes` is the bytes
/// consumed by retained file versions ("Total size taken up by file versions"
/// in `mega-df` output), or `None` when the line is absent.
pub async fn mega_df_query() -> Result<(u64, u64, Option<u64>), ProviderError> {
    let resolved_cmd = resolve_mega_cmd("mega-df");
    let mut cmd = Command::new(&resolved_cmd);
    #[cfg(windows)]
    {
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let output = tokio::time::timeout(
        std::time::Duration::from_secs(MEGA_DF_TIMEOUT_SECS),
        cmd.output(),
    )
    .await
    .map_err(|_| ProviderError::Timeout)?
    .map_err(|e| {
        ProviderError::NotSupported(format!(
            "mega-df is not available (resolved: {}): {}",
            resolved_cmd, e
        ))
    })?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let detail = if stderr.is_empty() {
            stdout.trim().to_string()
        } else {
            stderr
        };
        let lower = detail.to_ascii_lowercase();
        if lower.contains("not logged in")
            || lower.contains("not logged-in")
            || lower.contains("login required")
        {
            return Err(ProviderError::AuthenticationFailed(format!(
                "mega-df: {}",
                detail
            )));
        }
        return Err(ProviderError::ServerError(format!("mega-df: {}", detail)));
    }

    parse_mega_df_output(&stdout)
}

/// Spawn a MEGAcmd command and capture its output with a timeout.
async fn run_mega_cmd_capture(
    cmd: &str,
    args: &[&str],
) -> Result<std::process::Output, ProviderError> {
    let resolved_cmd = resolve_mega_cmd(cmd);
    let mut command = Command::new(&resolved_cmd);
    command.args(args);
    // #360: tear the child down if its future is dropped (connect cancelled) or
    // the timeout fires, instead of orphaning a blocked `mega-webdav /` process.
    command.kill_on_drop(true);
    #[cfg(windows)]
    {
        command.creation_flags(CREATE_NO_WINDOW);
    }
    tokio::time::timeout(
        std::time::Duration::from_secs(MEGA_WEBDAV_TIMEOUT_SECS),
        command.output(),
    )
    .await
    .map_err(|_| ProviderError::Timeout)?
    .map_err(|e| {
        ProviderError::NotSupported(format!(
            "{} is not available (resolved: {}): {}",
            cmd, resolved_cmd, e
        ))
    })
}

/// MEGA-absolute path for a remote path arriving from the WebDAV bridge (which
/// serves the account root) or the native MEGAcmd provider.
fn mega_abs_path(remote_path: &str) -> String {
    if remote_path.starts_with('/') {
        remote_path.to_string()
    } else {
        format!("/{}", remote_path)
    }
}

/// Map a non-success MEGAcmd exit to a typed error: "not logged in" becomes an
/// auth error (mirrors `mega_df_query`), everything else a server error.
fn mega_cmd_failure(label: &str, output: &std::process::Output) -> ProviderError {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if stderr.is_empty() { stdout } else { stderr };
    let lower = detail.to_ascii_lowercase();
    if lower.contains("not logged in")
        || lower.contains("not logged-in")
        || lower.contains("login required")
    {
        ProviderError::AuthenticationFailed(format!("{}: {}", label, detail))
    } else {
        ProviderError::ServerError(format!("{}: {}", label, detail))
    }
}

/// Parse `mega-ls -l --versions <path>` output into the file's version chain.
///
/// MEGAcmd prints the file once, then a `Versions of <name>:` section whose
/// lines mirror the `-l` columns (`FLAGS VERS SIZE DATE TIME NAME`) with a
/// `#<epoch>` suffix on the name; that epoch is the addressable version id
/// (`mega-get "<path>#<epoch>"`). A file with no retained history (VERS 1) has
/// no section, so the chain comes back empty.
pub(crate) fn parse_mega_ls_versions(output: &str) -> Vec<FileVersion> {
    let mut out = Vec::new();
    let mut in_versions = false;
    for raw in output.lines() {
        let trimmed = raw.trim_start();
        if trimmed.starts_with("Versions of ") {
            in_versions = true;
            continue;
        }
        if !in_versions || trimmed.is_empty() {
            continue;
        }
        // flags, vers, size, date, time, name(+#epoch)
        let parts: Vec<&str> = raw.split_whitespace().collect();
        if parts.len() < 6 {
            continue;
        }
        let name = parts[5..].join(" ");
        let version_id = match name.rsplit_once('#') {
            Some((_, epoch)) if !epoch.is_empty() && epoch.bytes().all(|b| b.is_ascii_digit()) => {
                epoch.to_string()
            }
            _ => continue,
        };
        let size = parts[2].parse::<u64>().unwrap_or(0);
        let modified = format!("{} {}", parts[3], parts[4]);
        out.push(FileVersion {
            id: version_id,
            modified: Some(modified),
            size,
            modified_by: None,
        });
    }
    out
}

/// List the retained versions of a MEGA file via `mega-ls -l --versions`.
pub async fn mega_list_versions(remote_path: &str) -> Result<Vec<FileVersion>, ProviderError> {
    let path = mega_abs_path(remote_path);
    let output = run_mega_cmd_capture("mega-ls", &["-l", "--versions", &path]).await?;
    if !output.status.success() {
        return Err(mega_cmd_failure("mega-ls --versions", &output));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_mega_ls_versions(&stdout))
}

/// Fetch a specific MEGA version to a fresh temp file and return its path.
///
/// `mega-get` refuses to overwrite an existing local target (it appends
/// " (NUM)"), so we always download to a unique, non-existent temp path and let
/// the caller place it where it belongs.
async fn mega_fetch_version_to_temp(
    remote_path: &str,
    version_id: &str,
) -> Result<std::path::PathBuf, ProviderError> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let temp = std::env::temp_dir().join(format!("aeroftp-megaver-{}-{}.tmp", version_id, nanos));
    let versioned = format!("{}#{}", mega_abs_path(remote_path), version_id);
    let temp_str = temp.to_string_lossy().to_string();
    match run_mega_cmd_capture("mega-get", &[&versioned, &temp_str]).await {
        Ok(output) if output.status.success() => Ok(temp),
        Ok(output) => {
            let _ = std::fs::remove_file(&temp);
            Err(mega_cmd_failure("mega-get version", &output))
        }
        Err(e) => {
            let _ = std::fs::remove_file(&temp);
            Err(e)
        }
    }
}

/// Download a specific MEGA version to `local_path` via `mega-get "<path>#<id>"`.
pub async fn mega_download_version(
    remote_path: &str,
    version_id: &str,
    local_path: &str,
) -> Result<(), ProviderError> {
    let temp = mega_fetch_version_to_temp(remote_path, version_id).await?;
    // Move into place, overwriting any existing destination (rename is atomic on
    // the same filesystem; fall back to copy across mounts / on Windows).
    if std::fs::rename(&temp, local_path).is_err() {
        let copy = std::fs::copy(&temp, local_path);
        let _ = std::fs::remove_file(&temp);
        copy.map_err(ProviderError::IoError)?;
    }
    Ok(())
}

/// Restore a MEGA file to a previous version. MEGAcmd has no restore verb, so we
/// download the chosen version and re-upload it: MEGA records the re-upload as
/// the new current version while preserving the existing chain.
pub async fn mega_restore_version(
    remote_path: &str,
    version_id: &str,
) -> Result<(), ProviderError> {
    let temp = mega_fetch_version_to_temp(remote_path, version_id).await?;
    let temp_str = temp.to_string_lossy().to_string();
    let path = mega_abs_path(remote_path);
    let put = run_mega_cmd_capture("mega-put", &[&temp_str, &path]).await;
    let _ = std::fs::remove_file(&temp);
    let output = put?;
    if !output.status.success() {
        return Err(mega_cmd_failure("mega-put restore", &output));
    }
    Ok(())
}

/// Interpret a `mega-webdav /` invocation. Pure, for testability.
///
/// `mega-webdav /` is idempotent: re-running it when the bridge is already up is
/// a no-op the user would do themselves. We treat a clean exit, or output that
/// says the location is already served, as success; "not logged in" becomes an
/// actionable auth error; anything else surfaces verbatim.
pub(crate) fn classify_mega_webdav_result(
    success: bool,
    combined: &str,
) -> Result<(), ProviderError> {
    if success {
        return Ok(());
    }
    let lower = combined.to_ascii_lowercase();
    if lower.contains("already") || lower.contains("serving") || lower.contains("served") {
        return Ok(());
    }
    if lower.contains("not logged in")
        || lower.contains("not logged-in")
        || lower.contains("login required")
    {
        return Err(ProviderError::AuthenticationFailed(
            "MEGAcmd has no active session; run `mega-login <email>` once in the MEGAcmd \
             terminal, then reconnect (the anonymous local-WebDAV bridge carries no MEGA \
             credentials, so AeroFTP cannot log in for you)"
                .to_string(),
        ));
    }
    Err(ProviderError::ServerError(format!(
        "mega-webdav /: {}",
        combined.trim()
    )))
}

/// Ensure the local MEGAcmd WebDAV bridge is serving the account root.
///
/// Zero-config bridge (issue #275 17076174): a MEGAcmd Server restart drops the
/// `webdav` location, forcing the user back to the terminal to re-run
/// `webdav /`. Driving it here removes that recurring step. It requires an
/// existing `mega-login` session: the bridge preset is anonymous and carries no
/// MEGA credentials, so the one-time login stays manual until a credentialled
/// MEGAcmd connection mode exists. Best-effort: callers treat any error as
/// non-fatal and fall back to the existing connection probe.
pub async fn ensure_megacmd_webdav_bridge() -> Result<(), ProviderError> {
    let output = run_mega_cmd_capture("mega-webdav", &["/"]).await?;
    let combined = format!(
        "{} {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    classify_mega_webdav_result(output.status.success(), &combined)
}

/// Extract the served WebDAV URL from `mega-webdav /` output. Pure, for testing.
///
/// MEGAcmd prints the address whether the location is freshly served
/// ("Serving via webdav: http://127.0.0.1:4443/") or already up
/// ("/: already being served at http://127.0.0.1:4443/"). We take the first
/// http(s) token and trim any trailing prose punctuation.
pub(crate) fn parse_mega_webdav_url(combined: &str) -> Option<String> {
    let lower = combined.to_ascii_lowercase();
    let start = match (lower.find("http://"), lower.find("https://")) {
        (Some(a), Some(b)) => a.min(b),
        (Some(a), None) => a,
        (None, Some(b)) => b,
        (None, None) => return None,
    };
    let rest = &combined[start..];
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let mut url = rest[..end].to_string();
    while url.ends_with(['.', ',', ';', ')', ']', '"', '\'']) {
        url.pop();
    }
    // Reject a bare scheme with no host.
    if url.starts_with("http://") && url.len() > "http://".len()
        || url.starts_with("https://") && url.len() > "https://".len()
    {
        Some(url)
    } else {
        None
    }
}

/// Resolve the local MEGAcmd WebDAV bridge URL by running `mega-webdav /`.
///
/// Mirrors `mega_df_query`: the same idempotent invocation that ensures the
/// bridge is up also prints its address, so AeroFTP can fill the Endpoint URL
/// field instead of asking the user to copy it from the MEGAcmd terminal
/// (#215). Auth/server failures surface as the actionable errors from
/// `classify_mega_webdav_result`; a clean run with no parseable URL is a
/// parse error.
pub async fn mega_webdav_url_query() -> Result<String, ProviderError> {
    let output = run_mega_cmd_capture("mega-webdav", &["/"]).await?;
    let combined = format!(
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    classify_mega_webdav_result(output.status.success(), &combined)?;
    parse_mega_webdav_url(&combined).ok_or_else(|| {
        ProviderError::ParseError(format!(
            "Could not parse the WebDAV URL from mega-webdav / output: {}",
            combined.trim()
        ))
    })
}

pub(crate) fn parse_mega_df_output(output: &str) -> Result<(u64, u64, Option<u64>), ProviderError> {
    let mut used = None;
    let mut total = None;
    let mut versioning = None;

    for raw_line in output.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }

        let (label, rest) = line
            .split_once(':')
            .map(|(label, rest)| (label.trim().to_ascii_lowercase(), rest.trim()))
            .unwrap_or_else(|| (line.to_ascii_lowercase(), line));

        let numbers = integer_tokens(rest);
        if numbers.is_empty() {
            continue;
        }

        // "Total size taken up by file versions: N" reports the bytes held by
        // retained versions. It contains "total" but is neither the used nor
        // the capacity row, so match it first and skip the used/total checks.
        if label.contains("file versions") || label.contains("file version") {
            versioning = Some(number_before_bytes(rest).unwrap_or(numbers[0]));
            continue;
        }

        let is_used_label = label.contains("used storage")
            || label.contains("storage used")
            || (label.contains("used") && !label.contains("total"));
        let is_total_label = label.contains("total storage")
            || label.contains("storage total")
            || label.contains("capacity");

        let rest_lower = rest.to_ascii_lowercase();
        if rest_lower.contains(" of ")
            && numbers.len() >= 2
            && (is_used_label || is_total_label || label == "total")
        {
            used = Some(numbers[0]);
            if let Some((_, after_of)) = rest_lower.split_once(" of ") {
                if let Some(total_value) = integer_tokens(after_of).first().copied() {
                    total = Some(total_value);
                }
            }
            continue;
        }

        let byte_value = number_before_bytes(rest).unwrap_or(numbers[0]);

        if is_used_label {
            used = Some(byte_value);
        }
        if is_total_label {
            total = Some(byte_value);
        }
    }

    let used = used.ok_or_else(|| {
        ProviderError::ParseError(format!(
            "Could not parse USED STORAGE from mega-df output: {}",
            output.trim()
        ))
    })?;
    let total = total.ok_or_else(|| {
        ProviderError::ParseError(format!(
            "Could not parse TOTAL STORAGE from mega-df output: {}",
            output.trim()
        ))
    })?;

    Ok((used, total, versioning))
}

fn number_before_bytes(text: &str) -> Option<u64> {
    let lower = text.to_ascii_lowercase();
    let bytes_pos = lower.find("bytes")?;
    integer_tokens(&text[..bytes_pos]).pop()
}

fn integer_tokens(text: &str) -> Vec<u64> {
    let mut out = Vec::new();
    let mut current = String::new();

    for ch in text.chars() {
        if ch.is_ascii_digit() {
            current.push(ch);
        } else if (ch == ',' || ch == '_') && !current.is_empty() {
            continue;
        } else if !current.is_empty() {
            if let Ok(value) = current.parse::<u64>() {
                out.push(value);
            }
            current.clear();
        }
    }

    if !current.is_empty() {
        if let Ok(value) = current.parse::<u64>() {
            out.push(value);
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_used_and_total_storage_rows() {
        let output = "\
USED STORAGE: 5368709120 bytes (5.0 GB)
TOTAL STORAGE: 21474836480 bytes (20.0 GB)
";
        assert_eq!(
            parse_mega_df_output(output).unwrap(),
            (5_368_709_120, 21_474_836_480, None)
        );
    }

    #[test]
    fn parses_total_of_format() {
        let output = "Total: 5368709120 of 21474836480 bytes used";
        assert_eq!(
            parse_mega_df_output(output).unwrap(),
            (5_368_709_120, 21_474_836_480, None)
        );
    }

    #[test]
    fn parses_megacmd_stdout_with_total_on_used_storage_row() {
        let output = "\
Cloud drive:             313221381 in      17 file(s) and       6 folder(s)
Inbox:                           0 in       0 file(s) and       0 folder(s)
Rubbish bin:             535035904 in       3 file(s) and       3 folder(s)
---------------------------------------------------------------------------
USED STORAGE:            848257285                   0.03% of 3303903592448
---------------------------------------------------------------------------
Total size taken up by file versions:     31457280
";
        assert_eq!(
            parse_mega_df_output(output).unwrap(),
            (848_257_285, 3_303_903_592_448, Some(31_457_280))
        );
    }

    #[test]
    fn versioning_bytes_is_none_when_line_absent() {
        let output = "\
USED STORAGE: 5368709120 bytes (5.0 GB)
TOTAL STORAGE: 21474836480 bytes (20.0 GB)
";
        assert_eq!(parse_mega_df_output(output).unwrap().2, None);
    }

    #[test]
    fn parses_versioning_bytes_with_bytes_suffix() {
        let output = "\
USED STORAGE:            848257285                   0.03% of 3303903592448
Total size taken up by file versions:     31457280 bytes (30.0 MB)
";
        assert_eq!(
            parse_mega_df_output(output).unwrap(),
            (848_257_285, 3_303_903_592_448, Some(31_457_280))
        );
    }

    #[test]
    fn missing_total_is_parse_error() {
        let err = parse_mega_df_output("USED STORAGE: 5368709120 bytes").unwrap_err();
        assert!(matches!(err, ProviderError::ParseError(_)));
        assert!(err.to_string().contains("TOTAL STORAGE"));
    }

    #[test]
    fn missing_used_is_parse_error() {
        let err = parse_mega_df_output("TOTAL STORAGE: 21474836480 bytes").unwrap_err();
        assert!(matches!(err, ProviderError::ParseError(_)));
        assert!(err.to_string().contains("USED STORAGE"));
    }

    #[test]
    fn empty_output_is_parse_error() {
        let err = parse_mega_df_output("").unwrap_err();
        assert!(matches!(err, ProviderError::ParseError(_)));
    }

    #[test]
    fn resolves_plain_name_when_binary_is_not_in_known_locations() {
        let out = resolve_mega_cmd("mega-df");
        assert!(!out.is_empty());
        assert!(out.contains("mega-df") || out.ends_with("mega-df"));
    }

    #[test]
    fn mega_webdav_clean_exit_is_ok() {
        assert!(classify_mega_webdav_result(true, "").is_ok());
    }

    #[test]
    fn mega_webdav_already_served_is_ok() {
        assert!(classify_mega_webdav_result(
            false,
            "/: already being served at http://127.0.0.1:4443/"
        )
        .is_ok());
    }

    #[test]
    fn mega_webdav_not_logged_in_is_auth_error() {
        let err = classify_mega_webdav_result(false, "[err: Not logged in.]").unwrap_err();
        assert!(matches!(err, ProviderError::AuthenticationFailed(_)));
        assert!(err.to_string().to_lowercase().contains("mega-login"));
    }

    #[test]
    fn mega_webdav_unknown_failure_is_server_error() {
        let err = classify_mega_webdav_result(false, "some other failure").unwrap_err();
        assert!(matches!(err, ProviderError::ServerError(_)));
    }

    #[test]
    fn parses_webdav_url_from_already_served() {
        assert_eq!(
            parse_mega_webdav_url("/: already being served at http://127.0.0.1:4443/"),
            Some("http://127.0.0.1:4443/".to_string())
        );
    }

    #[test]
    fn parses_webdav_url_from_fresh_serve() {
        assert_eq!(
            parse_mega_webdav_url("Serving via webdav: http://127.0.0.1:4443/."),
            Some("http://127.0.0.1:4443/".to_string())
        );
    }

    #[test]
    fn parses_https_webdav_url_when_tls_enabled() {
        assert_eq!(
            parse_mega_webdav_url("served at https://127.0.0.1:4443/ (TLS)"),
            Some("https://127.0.0.1:4443/".to_string())
        );
    }

    #[test]
    fn webdav_url_absent_returns_none() {
        assert_eq!(parse_mega_webdav_url("[err: Not logged in.]"), None);
    }

    #[test]
    fn parses_version_chain_from_mega_ls() {
        // Real `mega-ls -l --versions` output for a two-version file.
        let output = "\
FLAGS VERS      SIZE            DATE       NAME
----    2     31457280 15May2026 16:56:46 report.bin

Versions of report.bin:
----    2     31457280 15May2026 16:56:46 report.bin#1778857006
----    1     31457280 15May2026 16:56:38 report.bin#1778856998
";
        let versions = parse_mega_ls_versions(output);
        assert_eq!(versions.len(), 2);
        assert_eq!(versions[0].id, "1778857006");
        assert_eq!(versions[0].size, 31_457_280);
        assert_eq!(versions[0].modified.as_deref(), Some("15May2026 16:56:46"));
        assert_eq!(versions[1].id, "1778856998");
        assert_eq!(versions[1].modified.as_deref(), Some("15May2026 16:56:38"));
    }

    #[test]
    fn single_version_file_has_empty_chain() {
        // VERS 1 prints no "Versions of" section.
        let output = "\
FLAGS VERS      SIZE            DATE       NAME
----    1          102 28Apr2026 19:21:25 test_readable.txt
";
        assert!(parse_mega_ls_versions(output).is_empty());
    }

    #[test]
    fn version_names_with_spaces_keep_the_epoch_id() {
        let output = "\
Versions of my report.bin:
----    2     10 08Jun2026 16:56:46 my report.bin#1778857006
";
        let versions = parse_mega_ls_versions(output);
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].id, "1778857006");
    }
}
