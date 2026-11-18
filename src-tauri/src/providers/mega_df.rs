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

#[cfg(test)]
thread_local! {
    /// The folder a test links the MEGAcmd stand-in into, for the commands
    /// this thread runs (a `#[tokio::test]` runs on one thread).
    static TEST_CMD_DIR: std::cell::RefCell<Option<std::path::PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// Resolve MEGAcmd executable path (checks PATH and common install locations).
pub(crate) fn resolve_mega_cmd(cmd: &str) -> String {
    #[cfg(test)]
    if let Some(dir) = TEST_CMD_DIR.with(|dir| dir.borrow().clone()) {
        return dir.join(cmd).to_string_lossy().into_owned();
    }
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
    cmd.kill_on_drop(true);
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
        if megacmd_not_logged_in(output.status.code(), &detail) {
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

/// MEGAcmd's exit status for "needs logging in" (MCMD_NOTLOGGEDIN = -57 in
/// src/megacmdcommonutils.h, returned positive by the client on every
/// platform).
const MCMD_NOTLOGGEDIN_EXIT: i32 = 57;

/// Whether a failed MEGAcmd command says the session is gone: its exit
/// status decides; the wording is read only when the status says nothing
/// (an older client, a status of 1). #1506 row 164.
fn megacmd_not_logged_in(code: Option<i32>, detail: &str) -> bool {
    if code == Some(MCMD_NOTLOGGEDIN_EXIT) {
        return true;
    }
    let lower = detail.to_ascii_lowercase();
    lower.contains("not logged in")
        || lower.contains("not logged-in")
        || lower.contains("login required")
}

/// Map a non-success MEGAcmd exit to a typed error: "not logged in" becomes an
/// auth error (mirrors `mega_df_query`), everything else a server error.
fn mega_cmd_failure(label: &str, output: &std::process::Output) -> ProviderError {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if stderr.is_empty() { stdout } else { stderr };
    if megacmd_not_logged_in(output.status.code(), &detail) {
        ProviderError::AuthenticationFailed(format!("{}: {}", label, detail))
    } else {
        ProviderError::ServerError(format!("{}: {}", label, detail))
    }
}

/// The version chain of the MEGA file at `path`, current first, as the
/// native MEGAcmd provider lists it: each id is the version's handle
/// (`H:...`), which names that one node. The `<path>#<time>` address the
/// bridge used before names the first version of a second, and two versions
/// written in one second share it (measured 2026-11-18): the older one could
/// not be fetched.
async fn mega_version_rows(path: &str) -> Result<Vec<FileVersion>, ProviderError> {
    let output =
        run_mega_cmd_capture("mega-ls", &["-l", "--versions", "--show-handles", path]).await?;
    if !output.status.success() {
        return Err(mega_cmd_failure("mega-ls --versions", &output));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    super::mega::mega_versions_current_first(super::mega::parse_mega_ls_version_handles(
        &stdout, path,
    )?)
}

/// List the retained versions of a MEGA file (`mega-ls -l --versions
/// --show-handles`).
pub async fn mega_list_versions(remote_path: &str) -> Result<Vec<FileVersion>, ProviderError> {
    mega_version_rows(&mega_abs_path(remote_path)).await
}

/// The listed size of `version_id` among `listed`, the versions of `path`:
/// a handle names any node, so one that is not in the file's chain is
/// refused, as is an id that is not a handle.
fn listed_version_size(
    listed: &[FileVersion],
    path: &str,
    version_id: &str,
) -> Result<u64, ProviderError> {
    if !super::mega::is_mega_handle(version_id) {
        return Err(ProviderError::InvalidPath(format!(
            "'{version_id}' is not a MEGA version id: list its versions again"
        )));
    }
    listed
        .iter()
        .find(|v| v.id == version_id)
        .map(|v| v.size)
        .ok_or_else(|| ProviderError::NotFound(format!("{path} has no version {version_id}")))
}

/// Fetch one version of `path` (`mega-get H:<handle>`) to a fresh temp file
/// (removed when the guard goes), taken only whole: a regular file at the
/// size it was listed with. Returns the handle listed current with it.
async fn mega_fetch_version_to_temp(
    path: &str,
    version_id: &str,
) -> Result<(super::VersionTempFile, Option<String>), ProviderError> {
    let listed = mega_version_rows(path).await?;
    let size = listed_version_size(&listed, path, version_id)?;
    let temp = super::VersionTempFile::new("aeroftp_megaver");
    let temp_str = temp.0.to_string_lossy().to_string();
    let output = run_mega_cmd_capture("mega-get", &[version_id, &temp_str]).await?;
    if !output.status.success() {
        return Err(mega_cmd_failure("mega-get version", &output));
    }
    let written = std::fs::symlink_metadata(&temp.0)
        .ok()
        .filter(|m| m.file_type().is_file())
        .map(|m| m.len());
    if written != Some(size) {
        return Err(ProviderError::TransferFailed(format!(
            "MEGA sent {} of the {size} bytes of version {version_id} of {path}: nothing was \
             changed, try again",
            written.unwrap_or(0)
        )));
    }
    Ok((temp, listed.first().map(|v| v.id.clone())))
}

/// Download a specific MEGA version to `local_path`, written as every
/// download writes ([`super::copy_version_into_place`] where a rename across
/// file systems is not possible).
pub async fn mega_download_version(
    remote_path: &str,
    version_id: &str,
    local_path: &str,
) -> Result<(), ProviderError> {
    let path = mega_abs_path(remote_path);
    let (temp, _) = mega_fetch_version_to_temp(&path, version_id).await?;
    if tokio::fs::rename(&temp.0, local_path).await.is_err() {
        super::copy_version_into_place(&temp.0, local_path).await?;
    }
    Ok(())
}

/// Restore a MEGA file to a previous version. MEGAcmd has no restore verb, so
/// the chosen version is fetched and uploaded over the file, which MEGA keeps
/// as the new current version with the chain intact; refused for the current
/// version and, before the put, when the account has versioning off (MEGA
/// would drop the current version). The bytes of the version then on top,
/// read by its handle, must be the restored ones.
pub async fn mega_restore_version(
    remote_path: &str,
    version_id: &str,
) -> Result<(), ProviderError> {
    let path = mega_abs_path(remote_path);
    let (temp, head) = mega_fetch_version_to_temp(&path, version_id).await?;
    if head.as_deref() == Some(version_id) {
        return Err(ProviderError::NotSupported(
            "This is already the current version".into(),
        ));
    }
    let versioning = run_mega_cmd_capture("mega-userattr", &["^!dv"])
        .await
        .map(|output| {
            format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        });
    if super::mega::mega_versioning_off(versioning)? {
        return Err(ProviderError::NotSupported(
            super::mega::MEGA_VERSIONING_OFF.into(),
        ));
    }
    // mega-put names the file by its path: a file put there while the
    // version downloaded would be overwritten. The path must still hold the
    // version listed current, as in the native provider.
    let now = mega_version_rows(&path)
        .await?
        .first()
        .map(|v| v.id.clone());
    if now != head {
        return Err(ProviderError::NotFound(format!(
            "{path} changed while the version downloaded (current version {}, now {}): list \
             its versions again",
            head.as_deref().unwrap_or("none"),
            now.as_deref().unwrap_or("none")
        )));
    }
    let temp_str = temp.0.to_string_lossy().to_string();
    let output = run_mega_cmd_capture("mega-put", &[&temp_str, &path]).await?;
    if !output.status.success() {
        return Err(mega_cmd_failure("mega-put restore", &output));
    }
    let not_taken = || {
        ProviderError::TransferFailed(format!(
            "{path} does not hold the restored version after the upload: MEGA did not take \
             it, or someone else wrote it meanwhile; list its versions again"
        ))
    };
    let (top, top_size) = mega_version_rows(&path)
        .await?
        .first()
        .map(|v| (v.id.clone(), v.size))
        .ok_or_else(not_taken)?;
    let check = super::VersionTempFile::new("aeroftp_megacheck");
    let check_str = check.0.to_string_lossy().to_string();
    let output = run_mega_cmd_capture("mega-get", &[&top, &check_str]).await?;
    if !output.status.success() {
        return Err(not_taken());
    }
    let (fetched, local) = (check.0.clone(), temp.0.clone());
    let same = tokio::task::spawn_blocking(move || {
        super::mega::fetched_is_version(&fetched, top_size, &local)
    })
    .await
    .map_err(|e| ProviderError::Other(e.to_string()))?
    .map_err(ProviderError::IoError)?;
    if !same {
        return Err(not_taken());
    }
    // The bytes read are those of the handle, whatever the path holds now:
    // that version must still be on top after the read.
    let still = mega_version_rows(&path).await?;
    if still.first().map(|v| v.id.as_str()) != Some(top.as_str()) {
        return Err(not_taken());
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
    code: Option<i32>,
    combined: &str,
) -> Result<(), ProviderError> {
    if success {
        return Ok(());
    }
    let lower = combined.to_ascii_lowercase();
    // No exit status is documented for "already served": the phrase is
    // kept, and it counts only with the served URL parsed from the output.
    if lower.contains("already being served at") && parse_mega_webdav_url(combined).is_some() {
        return Ok(());
    }
    if megacmd_not_logged_in(code, combined) {
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
///
/// Returns the URL the bridge serves the account at, when the output names
/// one: MEGAcmd 2.6 serves it under `/<handle>/Cloud%20Drive`, not at the
/// server root, see [`megacmd_served_base`].
pub async fn ensure_megacmd_webdav_bridge() -> Result<Option<String>, ProviderError> {
    let output = run_mega_cmd_capture("mega-webdav", &["/"]).await?;
    let combined = format!(
        "{} {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    classify_mega_webdav_result(output.status.success(), output.status.code(), &combined)?;
    Ok(parse_mega_webdav_url(&combined))
}

/// The base a MEGAcmd WebDAV profile should connect to, when it is not the
/// one saved. MEGAcmd 2.6 serves the account under `/<handle>/Cloud%20Drive`
/// (`mega-webdav /` prints `Serving via webdav /: http://127.0.0.1:4443/<handle>/Cloud%20Drive`),
/// and a profile aimed at the server root could list but not create a folder:
/// `MKCOL` answered 404 there. `Some(served)` only when the profile points at
/// that server's root (same scheme, host and port, empty path) and the bridge
/// serves a path below it. A profile that names a path of its own, another
/// host or port, or an older MEGAcmd that serves the root, keeps its URL.
/// Pure, for testing; the saved profile is never rewritten.
pub(crate) fn megacmd_served_base(profile_url: &str, served_url: &str) -> Option<String> {
    let profile = reqwest::Url::parse(profile_url.trim()).ok()?;
    let served = reqwest::Url::parse(served_url.trim()).ok()?;
    let same_server = profile.scheme() == served.scheme()
        && profile.host_str() == served.host_str()
        && profile.port_or_known_default() == served.port_or_known_default();
    let profile_at_root = profile.path().trim_matches('/').is_empty();
    let served_below_root = !served.path().trim_matches('/').is_empty();
    (same_server && profile_at_root && served_below_root)
        .then(|| served_url.trim().trim_end_matches('/').to_string())
}

/// The URL a MEGAcmd WebDAV session connects to, chosen on every connect from
/// the profile's own URL, the one the session used so far, and the URL the
/// bridge reports serving now (`None` when `mega-webdav /` printed none).
/// Pure, for testing.
pub(crate) fn megacmd_session_url(
    profile_url: &str,
    current_url: &str,
    served: Option<&str>,
) -> String {
    match served {
        // Always from the profile's URL: after a first move the session's own
        // URL is a served path, which would make every later move look like a
        // profile with a path of its own.
        Some(served) => {
            megacmd_served_base(profile_url, served).unwrap_or_else(|| profile_url.to_string())
        }
        None => current_url.to_string(),
    }
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
    classify_mega_webdav_result(output.status.success(), output.status.code(), &combined)?;
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
        assert!(classify_mega_webdav_result(true, None, "").is_ok());
    }

    #[test]
    fn mega_webdav_already_served_is_ok() {
        assert!(classify_mega_webdav_result(
            false,
            None,
            "/: already being served at http://127.0.0.1:4443/"
        )
        .is_ok());
    }

    #[test]
    fn mega_webdav_failure_with_incidental_already_is_not_success() {
        let result = classify_mega_webdav_result(
            false,
            None,
            "Failed to initialize WebDAV: port 4443 is already allocated",
        );
        assert!(
            matches!(result, Err(ProviderError::ServerError(_))),
            "one incidental word must not erase one failed command"
        );
    }

    #[test]
    fn mega_webdav_not_logged_in_is_auth_error() {
        let err = classify_mega_webdav_result(false, None, "[err: Not logged in.]").unwrap_err();
        assert!(matches!(err, ProviderError::AuthenticationFailed(_)));
        assert!(err.to_string().to_lowercase().contains("mega-login"));
    }

    #[test]
    fn mega_webdav_unknown_failure_is_server_error() {
        // The exit status decides whatever the words: 57 is MCMD_NOTLOGGEDIN.
        assert!(matches!(
            classify_mega_webdav_result(false, Some(57), "Sitzung abgelaufen"),
            Err(ProviderError::AuthenticationFailed(_))
        ));
        assert!(megacmd_not_logged_in(Some(57), "Sessione scaduta"));
        assert!(!megacmd_not_logged_in(Some(53), "Couldn't find /x"));
        let err = classify_mega_webdav_result(false, None, "some other failure").unwrap_err();
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
    fn parses_the_served_path_in_every_megacmd_26_form() {
        let served = "http://127.0.0.1:4443/aBcD1234/Cloud%20Drive";
        for output in [
            "Serving via webdav /: http://127.0.0.1:4443/aBcD1234/Cloud%20Drive",
            "/: already being served at http://127.0.0.1:4443/aBcD1234/Cloud%20Drive",
            "WEBDAV SERVED LOCATIONS:\n/: http://127.0.0.1:4443/aBcD1234/Cloud%20Drive\n",
        ] {
            assert_eq!(
                parse_mega_webdav_url(output).as_deref(),
                Some(served),
                "{output}"
            );
        }
    }

    #[test]
    fn a_root_profile_moves_to_the_served_path_and_nothing_else_does() {
        let served = "http://127.0.0.1:4443/aBcD1234/Cloud%20Drive";
        // The profile the GUI preset saves, with and without a trailing slash.
        for profile in ["http://127.0.0.1:4443/", "http://127.0.0.1:4443"] {
            assert_eq!(
                megacmd_served_base(profile, served).as_deref(),
                Some(served),
                "{profile}"
            );
        }
        // A trailing slash on the served URL is not carried over.
        assert_eq!(
            megacmd_served_base("http://127.0.0.1:4443/", &format!("{served}/")).as_deref(),
            Some(served)
        );
        // A profile that already names a path keeps it.
        assert_eq!(megacmd_served_base(served, served), None);
        assert_eq!(
            megacmd_served_base("http://127.0.0.1:4443/Other", served),
            None
        );
        // Another host, port or scheme is another server.
        assert_eq!(megacmd_served_base("http://localhost:4443/", served), None);
        assert_eq!(megacmd_served_base("http://127.0.0.1:4444/", served), None);
        assert_eq!(megacmd_served_base("https://127.0.0.1:4443/", served), None);
        // An older MEGAcmd that serves the root: nothing to move.
        assert_eq!(
            megacmd_served_base("http://127.0.0.1:4443/", "http://127.0.0.1:4443/"),
            None
        );
        assert_eq!(megacmd_served_base("not a url", served), None);
    }

    #[test]
    fn a_reconnect_follows_the_path_the_bridge_serves_now() {
        let root = "http://127.0.0.1:4443/";
        let first = "http://127.0.0.1:4443/aBcD1234/Cloud%20Drive";
        let second = "http://127.0.0.1:4443/eFgH5678/Cloud%20Drive";
        // First connect of a profile saved at the root.
        assert_eq!(megacmd_session_url(root, root, Some(first)), first);
        // A reconnect of the same session after the bridge moved (another
        // account logged in to MEGAcmd): the new path, not the old one.
        assert_eq!(megacmd_session_url(root, first, Some(second)), second);
        // A bridge that serves the root again: back to the profile's URL.
        assert_eq!(megacmd_session_url(root, first, Some(root)), root);
        // Nothing learned from the bridge: the session keeps its URL.
        assert_eq!(megacmd_session_url(root, first, None), first);
        // A profile with a path of its own is never moved.
        assert_eq!(megacmd_session_url(first, first, Some(second)), first);
    }

    #[test]
    fn webdav_url_absent_returns_none() {
        assert_eq!(parse_mega_webdav_url("[err: Not logged in.]"), None);
    }

    /// The bridge takes the chain as the native provider does: by handle.
    /// Three versions written in one second (measured 2026-11-18, all
    /// `#1795020505`) keep three ids, each fetched as itself; a handle that
    /// is not in the file's chain, or an epoch id, is refused.
    /// The MEGAcmd stand-in (tests/fixtures/megacmd_shim.sh) linked into a
    /// folder this thread's MEGAcmd commands run from, with `flags` set.
    #[cfg(unix)]
    fn bridge_on_stand_in(flags: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let shim =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/megacmd_shim.sh");
        for name in ["mega-ls", "mega-get", "mega-put", "mega-userattr"] {
            std::os::unix::fs::symlink(&shim, dir.path().join(name)).unwrap();
        }
        for flag in flags {
            std::fs::write(dir.path().join(flag), "").unwrap();
        }
        TEST_CMD_DIR.with(|cmd_dir| *cmd_dir.borrow_mut() = Some(dir.path().to_path_buf()));
        dir
    }

    #[cfg(unix)]
    fn bridge_log(dir: &tempfile::TempDir, name: &str) -> Vec<String> {
        std::fs::read_to_string(dir.path().join(name))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// The bridge restores by a put at the path: a file put there while the
    /// version downloaded (y.txt gains a new current version after its first
    /// listing) is not overwritten. A put that MEGA ignored, whose check read
    /// of the head (13 bytes listed) stops at 8 bytes equal to the restored
    /// version, is not the restore. A head that changes after the check read
    /// is not the restore either; the same restore with nothing else written
    /// is done.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_bridge_restore_is_done_only_on_the_head_it_checked() {
        let dir = bridge_on_stand_in(&[]);
        let replaced = mega_restore_version("/y.txt", "H:yAAAAAA1").await;
        assert!(
            matches!(&replaced, Err(ProviderError::NotFound(m)) if m.contains("changed while")),
            "{replaced:?}"
        );
        assert!(bridge_log(&dir, "put.log").is_empty());

        let dir = bridge_on_stand_in(&["put-ignored", "head-get-prefix-of-1"]);
        let cut = mega_restore_version("/v.txt", "H:vAAAAAA1").await;
        assert!(
            matches!(&cut, Err(ProviderError::TransferFailed(m)) if m.contains("did not take")),
            "{cut:?}"
        );
        assert_eq!(bridge_log(&dir, "get.log"), ["H:vAAAAAA1", "H:vAAAAAA3"]);

        let _dir = bridge_on_stand_in(&["foreign-after-check"]);
        let overwritten = mega_restore_version("/v.txt", "H:vAAAAAA2").await;
        assert!(
            matches!(&overwritten, Err(ProviderError::TransferFailed(_))),
            "{overwritten:?}"
        );

        let dir = bridge_on_stand_in(&[]);
        mega_restore_version("/v.txt", "H:vAAAAAA2")
            .await
            .expect("restore");
        assert_eq!(bridge_log(&dir, "put.log"), ["/v.txt"]);
        TEST_CMD_DIR.with(|cmd_dir| *cmd_dir.borrow_mut() = None);
    }

    #[test]
    fn versions_of_one_second_keep_distinct_ids_on_the_bridge() {
        let output = "\
FLAGS VERS      SIZE            DATE          HANDLE NAME
----    3           16 18Nov2026 17:48:25 H:7uoGxRAR d1/a.txt

Versions of d1/a.txt:
----    3           16 18Nov2026 17:48:25 H:7uoGxRAR a.txt#1795020505
----    2           17 18Nov2026 17:48:25 H:zq4y3ZYY a.txt#1795020505
----    1           18 18Nov2026 17:48:25 H:GmoGXB5Q a.txt#1795020505
";
        let listed = super::super::mega::mega_versions_current_first(
            super::super::mega::parse_mega_ls_version_handles(output, "/d1/a.txt").unwrap(),
        )
        .unwrap();
        let ids: Vec<&str> = listed.iter().map(|v| v.id.as_str()).collect();
        assert_eq!(ids, ["H:7uoGxRAR", "H:zq4y3ZYY", "H:GmoGXB5Q"]);
        assert_eq!(
            listed_version_size(&listed, "/d1/a.txt", "H:zq4y3ZYY").unwrap(),
            17
        );
        assert_eq!(
            listed_version_size(&listed, "/d1/a.txt", "H:GmoGXB5Q").unwrap(),
            18
        );
        assert!(listed_version_size(&listed, "/d1/a.txt", "H:other111").is_err());
        assert!(listed_version_size(&listed, "/d1/a.txt", "1795020505").is_err());
        // A folder's listing: no child's chain passes for the folder's.
        let folder = "\
Versions of d1/a.txt:
----    2           17 18Nov2026 17:48:25 H:zq4y3ZYY a.txt#1795020505
";
        assert!(super::super::mega::parse_mega_ls_version_handles(folder, "/d1").is_err());
    }
}
