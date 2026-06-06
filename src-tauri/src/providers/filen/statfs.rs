// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Optional `filen statfs` quota source (Ehud wishlist #275).
//!
//! When the official Filen CLI (`filen`) is installed, `filen statfs` is a
//! cleaner quota source than the REST `/v3/user/info` call. This module mirrors
//! `mega_df::resolve_mega_cmd`: it resolves the binary, runs `filen statfs`,
//! and parses used/total bytes.
//!
//! IMPORTANT (caveat surfaced to users): the Filen CLI keeps its OWN login,
//! separate from the AeroFTP profile's API key. So `statfs` reports whatever
//! account the CLI is logged into. The caller therefore treats this as an
//! OPPORTUNISTIC optimisation guarded by plausibility checks, and falls back to
//! the authoritative REST call on any error, an unparseable output, or values
//! that do not look like a real byte quota.

use tokio::process::Command;

use crate::providers::ProviderError;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

const FILEN_STATFS_TIMEOUT_SECS: u64 = 15;

/// Smallest total we accept from the CLI. Filen's free tier is 10 GB and paid
/// plans are larger, so a "total" below 1 MB means we misread a count/field and
/// must fall back to REST rather than surface a bogus quota.
const MIN_PLAUSIBLE_TOTAL: u64 = 1_000_000;

/// Resolve the Filen CLI executable (checks PATH and common install locations).
pub(crate) fn resolve_filen_cmd() -> String {
    #[cfg(windows)]
    {
        let program_files =
            std::env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".to_string());
        let local_appdata = std::env::var("LOCALAPPDATA").unwrap_or_default();
        let candidates = [
            format!(r"{}\Filen\filen.exe", program_files),
            format!(r"{}\filen\filen.exe", local_appdata),
            format!(r"{}\Programs\filen\filen.exe", local_appdata),
        ];
        for candidate in &candidates {
            if std::path::Path::new(candidate).exists() {
                return candidate.clone();
            }
        }
    }
    #[cfg(unix)]
    {
        let mut candidates = vec![
            "/usr/local/bin/filen".to_string(),
            "/usr/bin/filen".to_string(),
            "/opt/homebrew/bin/filen".to_string(),
            "/snap/bin/filen".to_string(),
        ];
        if let Ok(home) = std::env::var("HOME") {
            candidates.push(format!("{}/.local/bin/filen", home));
        }
        for candidate in &candidates {
            if std::path::Path::new(candidate).exists() {
                return candidate.clone();
            }
        }
    }
    "filen".to_string()
}

/// Query account quota via the Filen CLI. Returns `(used, total)` in bytes.
///
/// Tries `filen statfs --json` first (unambiguous), then `filen statfs`. Any
/// failure (binary missing, non-zero exit, timeout, unparseable, implausible
/// values) returns `Err` so the caller falls back to the REST quota.
pub(crate) async fn filen_statfs_query() -> Result<(u64, u64), ProviderError> {
    let resolved = resolve_filen_cmd();
    let attempts: [&[&str]; 2] = [&["statfs", "--json"], &["statfs"]];
    let mut last_err =
        ProviderError::NotSupported("filen statfs produced no usable output".to_string());

    for args in attempts {
        match run_statfs(&resolved, args).await {
            Ok(stdout) => match parse_filen_statfs(&stdout) {
                Some((used, total)) if total >= MIN_PLAUSIBLE_TOTAL && used <= total => {
                    return Ok((used, total));
                }
                _ => {
                    last_err = ProviderError::ParseError(format!(
                        "filen statfs output not understood: {}",
                        stdout.trim()
                    ));
                }
            },
            Err(e) => last_err = e,
        }
    }
    Err(last_err)
}

async fn run_statfs(resolved: &str, args: &[&str]) -> Result<String, ProviderError> {
    let mut cmd = Command::new(resolved);
    cmd.args(args);
    #[cfg(windows)]
    {
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(FILEN_STATFS_TIMEOUT_SECS),
        cmd.output(),
    )
    .await
    .map_err(|_| ProviderError::Timeout)?
    .map_err(|e| {
        ProviderError::NotSupported(format!("filen CLI not available ({}): {}", resolved, e))
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(ProviderError::ServerError(format!(
            "filen statfs failed: {}",
            if stderr.is_empty() {
                String::from_utf8_lossy(&output.stdout).trim().to_string()
            } else {
                stderr
            }
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Parse `filen statfs` stdout into `(used, total)` bytes. Handles a JSON object
/// (preferred) and a human "Label: value [unit]" listing. Returns `None` when
/// neither used nor total can be recovered.
fn parse_filen_statfs(output: &str) -> Option<(u64, u64)> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(pair) = parse_json_statfs(trimmed) {
        return Some(pair);
    }
    parse_text_statfs(trimmed)
}

fn parse_json_statfs(text: &str) -> Option<(u64, u64)> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    // Some CLIs wrap the payload under `data`; accept either shape.
    let obj = value.get("data").unwrap_or(&value);
    let used = json_bytes(obj, &["used", "storageUsed", "usedStorage", "usedBytes"])?;
    let total = json_bytes(
        obj,
        &[
            "max",
            "total",
            "maxStorage",
            "totalStorage",
            "size",
            "quota",
            "maxBytes",
        ],
    )?;
    Some((used, total))
}

fn json_bytes(obj: &serde_json::Value, keys: &[&str]) -> Option<u64> {
    for key in keys {
        if let Some(v) = obj.get(*key) {
            if let Some(n) = v.as_u64() {
                return Some(n);
            }
            if let Some(f) = v.as_f64() {
                if f >= 0.0 {
                    return Some(f as u64);
                }
            }
            if let Some(s) = v.as_str() {
                if let Some(b) = parse_size_to_bytes(s) {
                    return Some(b);
                }
            }
        }
    }
    None
}

fn parse_text_statfs(text: &str) -> Option<(u64, u64)> {
    let mut used = None;
    let mut total = None;
    for raw in text.lines() {
        let line = raw.trim();
        let Some((label, rest)) = line.split_once(':') else {
            continue;
        };
        let label = label.trim().to_ascii_lowercase();
        let rest = rest.trim();
        let is_used = label.contains("used") && !label.contains("unused");
        let is_total = label.contains("max")
            || label.contains("total")
            || label.contains("capacity")
            || label.contains("quota")
            || label.contains("size");
        if is_used && used.is_none() {
            used = parse_size_to_bytes(rest);
        } else if is_total && total.is_none() {
            total = parse_size_to_bytes(rest);
        }
    }
    match (used, total) {
        (Some(u), Some(t)) => Some((u, t)),
        _ => None,
    }
}

/// Parse a size string ("10 GB", "1.5GiB", "1234567 bytes", "1234567") into
/// bytes. Decimal units use 1000, binary (`*iB`) use 1024. Returns `None` when
/// no number is present.
fn parse_size_to_bytes(input: &str) -> Option<u64> {
    let s = input.trim();
    if s.is_empty() {
        return None;
    }
    // Split the leading numeric part from the trailing unit.
    let num_end = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == ',' || c == '_'))
        .unwrap_or(s.len());
    let num_str: String = s[..num_end]
        .chars()
        .filter(|c| *c != ',' && *c != '_')
        .collect();
    if num_str.is_empty() {
        return None;
    }
    let value: f64 = num_str.parse().ok()?;
    let unit = s[num_end..].trim().to_ascii_lowercase();
    let multiplier: f64 = match unit.as_str() {
        "" | "b" | "byte" | "bytes" => 1.0,
        "kb" | "k" => 1_000.0,
        "mb" | "m" => 1_000_000.0,
        "gb" | "g" => 1_000_000_000.0,
        "tb" | "t" => 1_000_000_000_000.0,
        "pb" => 1_000_000_000_000_000.0,
        "kib" => 1024.0,
        "mib" => 1024.0 * 1024.0,
        "gib" => 1024.0 * 1024.0 * 1024.0,
        "tib" => 1024.0f64.powi(4),
        "pib" => 1024.0f64.powi(5),
        _ => return None,
    };
    let bytes = value * multiplier;
    if bytes.is_finite() && bytes >= 0.0 {
        Some(bytes as u64)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_json_bytes() {
        let out = r#"{"used": 1610612736, "max": 10737418240}"#;
        assert_eq!(
            parse_filen_statfs(out),
            Some((1_610_612_736, 10_737_418_240))
        );
    }

    #[test]
    fn parses_json_nested_under_data() {
        let out = r#"{"data": {"storageUsed": 500, "maxStorage": 2000000}}"#;
        assert_eq!(parse_filen_statfs(out), Some((500, 2_000_000)));
    }

    #[test]
    fn parses_human_lines_with_units() {
        let out = "Used: 1.5 GB\nMax: 10 GB\n";
        assert_eq!(
            parse_filen_statfs(out),
            Some((1_500_000_000, 10_000_000_000))
        );
    }

    #[test]
    fn parses_binary_units() {
        assert_eq!(parse_size_to_bytes("1 GiB"), Some(1_073_741_824));
        assert_eq!(parse_size_to_bytes("10GiB"), Some(10_737_418_240));
    }

    #[test]
    fn rejects_unparseable() {
        assert_eq!(parse_filen_statfs("not a quota at all"), None);
        assert_eq!(parse_filen_statfs(""), None);
    }

    #[test]
    fn plausibility_guard_rejects_tiny_total() {
        // A misread "total" of a few bytes must not pass the query guard. We
        // assert the constant directly since the guard lives in the async query.
        let (used, total) = parse_filen_statfs("Used: 5\nMax: 42\n").unwrap();
        assert!(used <= total);
        assert!(total < MIN_PLAUSIBLE_TOTAL);
    }
}
