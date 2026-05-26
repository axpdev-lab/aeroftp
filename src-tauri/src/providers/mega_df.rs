//! Shared MEGAcmd `mega-df` quota helper.
//!
//! `mega-df` is useful beyond quota parsing: when the MEGAcmd Server has
//! been stopped with `quit` or `exit`, invoking any `mega-*` command starts
//! it again in the background. Issue #253 relies on that warm-up behavior.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use tokio::process::Command;

use super::ProviderError;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

const MEGA_DF_TIMEOUT_SECS: u64 = 30;

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
pub async fn mega_df_query() -> Result<(u64, u64), ProviderError> {
    let resolved_cmd = resolve_mega_cmd("mega-df");
    let mut cmd = Command::new(&resolved_cmd);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
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

pub(crate) fn parse_mega_df_output(output: &str) -> Result<(u64, u64), ProviderError> {
    let mut used = None;
    let mut total = None;

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

    Ok((used, total))
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
            (5_368_709_120, 21_474_836_480)
        );
    }

    #[test]
    fn parses_total_of_format() {
        let output = "Total: 5368709120 of 21474836480 bytes used";
        assert_eq!(
            parse_mega_df_output(output).unwrap(),
            (5_368_709_120, 21_474_836_480)
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
            (848_257_285, 3_303_903_592_448)
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
}
