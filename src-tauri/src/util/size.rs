// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Human byte sizes, both ways: the display form and the `K/M/G` input form.
//!
//! The CLI wrote both, and the community benchmark (which now runs from the
//! library for the CLI and for AeroAgent alike) words its errors and reads its
//! `--sizes` through them, so they live here where both can reach them.

/// `bytes` as a short binary-unit string: `512 B`, `1.5 KB`, `10.0 MB`.
pub fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;
    const TB: u64 = 1024 * GB;

    if bytes >= TB {
        format!("{:.1} TB", bytes as f64 / TB as f64)
    } else if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

/// Parse a size like `100`, `64K`, `1.5M`, `2G` into bytes. A bare number is
/// bytes; the suffixes are binary (1K = 1024).
pub fn parse_size_filter(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("Empty size".into());
    }
    let (num_str, multiplier) = match s.as_bytes().last() {
        Some(b'k' | b'K') => (&s[..s.len() - 1], 1024u64),
        Some(b'm' | b'M') => (&s[..s.len() - 1], 1024 * 1024),
        Some(b'g' | b'G') => (&s[..s.len() - 1], 1024 * 1024 * 1024),
        _ => (s, 1u64),
    };
    let value = num_str
        .trim()
        .parse::<f64>()
        .map_err(|e| format!("Invalid size '{}': {}", s, e))?;
    let bytes = value * multiplier as f64;
    // Reject NaN, infinities and negatives: the saturating `as u64` cast
    // would silently turn them into 0, which for a cutoff means "segment
    // everything" (CodeRabbit on #920).
    if !bytes.is_finite() || bytes < 0.0 {
        return Err(format!(
            "Invalid size '{}': not a non-negative finite number",
            s
        ));
    }
    Ok(bytes as u64)
}
