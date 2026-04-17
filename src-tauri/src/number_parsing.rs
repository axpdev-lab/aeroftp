//! Locale-tolerant number parsing for tools that emit human-formatted numbers.
//!
//! The motivating case is `rsync --stats`, whose output uses either `,` or `.`
//! as the thousands separator depending on the process locale (`LC_NUMERIC`):
//!
//! - POSIX / en_US: `sent 156,561 bytes … speedup is 48.22`
//! - it_IT:         `sent 156.561 bytes … speedup is 48,22`
//!
//! Previously the rsync wrapper forced `LC_NUMERIC=C LC_ALL=C` on the child
//! process so our parsers only ever saw the en_US shape. That was a workaround
//! — the correct fix is a parser that understands both conventions. These
//! helpers do exactly that, without depending on any crate.
//!
//! ## Rules
//!
//! 1. Strings containing only digits, `.`, `,`, and whitespace are accepted.
//! 2. If both `.` and `,` appear, the **last** one is the decimal separator
//!    and the other is always a thousands separator.
//! 3. If only one of them appears and it is followed by exactly 1-2 digits
//!    until end of input, it is the decimal separator. Otherwise it is a
//!    thousands separator.
//! 4. Trailing non-numeric characters (e.g. "bytes", "seconds") terminate
//!    parsing cleanly.
//!
//! The rules are deterministic for every rsync output shape we have observed
//! and for every sensible locale (en_US, it_IT, de_DE, fr_FR, POSIX). They
//! can misclassify a pathological input like `"1,234"` with ambiguous intent,
//! but rsync never emits that shape: thousands groups are always 3 digits and
//! the decimal field is always 2 digits, which the rules disambiguate.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

/// Parse an integer from a human-formatted string where thousands may be
/// separated by `.`, `,`, or whitespace. Trailing non-numeric characters are
/// ignored (e.g. `"156.561 bytes"` → `Some(156_561)`).
///
/// Returns `None` if no digits are found or if the numeric value would
/// overflow `u64`.
pub fn parse_u64_loose(s: &str) -> Option<u64> {
    let digits = strip_separators_integer_only(s)?;
    digits.parse().ok()
}

/// Parse a floating-point value from a human-formatted string. The last
/// occurrence of `.` or `,` is treated as the decimal separator (rules 2 and 3
/// in the module doc); all earlier occurrences are dropped as thousands.
///
/// Returns `None` if no digits are found or the value would overflow `f64`.
pub fn parse_f64_loose(s: &str) -> Option<f64> {
    let normalized = normalize_for_float(s)?;
    normalized.parse().ok()
}

/// Strip every `,`, `.`, and whitespace from the digit run of `s`, stopping
/// at the first trailing alphabetic character so `"156.561 bytes"` becomes
/// `"156561"`.
fn strip_separators_integer_only(s: &str) -> Option<String> {
    let mut out = String::with_capacity(s.len());
    let mut seen_digit = false;
    for c in s.chars() {
        if c.is_ascii_digit() {
            out.push(c);
            seen_digit = true;
        } else if c == '.' || c == ',' || c.is_whitespace() {
            if seen_digit {
                // inside or after the number: treat as a separator; if what
                // follows is a non-digit, the loop below will break out.
                continue;
            }
            // not yet inside the number: keep scanning leading whitespace.
        } else if seen_digit {
            // first truly non-numeric char after digits: done.
            break;
        } else {
            // any other char before we started: skip (leading garbage).
        }
    }
    if seen_digit {
        Some(out)
    } else {
        None
    }
}

/// Rewrite `s` into a form `f64::from_str` accepts, deciding decimal vs.
/// thousands per the module-level rules.
fn normalize_for_float(s: &str) -> Option<String> {
    // First isolate the candidate run: digits, '.', ','. Stop at alphabetic
    // trailer ("bytes/sec", "seconds", etc.).
    let mut run = String::with_capacity(s.len());
    let mut seen_digit = false;
    for c in s.chars() {
        if c.is_ascii_digit() {
            run.push(c);
            seen_digit = true;
        } else if c == '.' || c == ',' {
            run.push(c);
        } else if c.is_whitespace() {
            if seen_digit {
                // whitespace after the number: stop
                break;
            }
            // leading whitespace: skip
        } else if seen_digit {
            break;
        }
    }
    if !seen_digit {
        return None;
    }

    // Decide decimal separator per the rules.
    let last_dot = run.rfind('.');
    let last_comma = run.rfind(',');
    let decimal_pos = match (last_dot, last_comma) {
        (Some(d), Some(c)) => Some(d.max(c)),
        (Some(d), None) => decide_single_separator(&run, d),
        (None, Some(c)) => decide_single_separator(&run, c),
        (None, None) => None,
    };

    let mut normalized = String::with_capacity(run.len());
    for (i, c) in run.char_indices() {
        match c {
            '.' | ',' => {
                if Some(i) == decimal_pos {
                    normalized.push('.'); // f64::from_str wants '.'
                }
                // else: drop thousands separator
            }
            d => normalized.push(d),
        }
    }
    Some(normalized)
}

/// Decide whether the single separator at byte offset `pos` is a decimal
/// separator or a thousands separator. Rule: it is decimal iff followed by
/// exactly 1 or 2 digits to the end of the input. Otherwise thousands.
///
/// This is the heuristic rsync actually satisfies: speedup is always
/// formatted with 2 decimal digits, bytes are never formatted with a decimal.
fn decide_single_separator(run: &str, pos: usize) -> Option<usize> {
    let tail = &run[pos + 1..];
    let tail_len = tail.len();
    if tail_len == 0 || tail_len > 2 {
        return None;
    }
    if tail.chars().all(|c| c.is_ascii_digit()) {
        Some(pos)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- integers ---------------------------------------------------------

    #[test]
    fn u64_en_us_thousands() {
        assert_eq!(parse_u64_loose("156,561"), Some(156_561));
        assert_eq!(parse_u64_loose("1,048,576"), Some(1_048_576));
    }

    #[test]
    fn u64_it_thousands() {
        assert_eq!(parse_u64_loose("156.561"), Some(156_561));
        assert_eq!(parse_u64_loose("1.048.576"), Some(1_048_576));
    }

    #[test]
    fn u64_no_separator() {
        assert_eq!(parse_u64_loose("1024"), Some(1024));
        assert_eq!(parse_u64_loose("0"), Some(0));
    }

    #[test]
    fn u64_with_trailing_units() {
        assert_eq!(parse_u64_loose("156.561 bytes"), Some(156_561));
        assert_eq!(parse_u64_loose("10,485,760 bytes"), Some(10_485_760));
    }

    #[test]
    fn u64_rejects_no_digits() {
        assert_eq!(parse_u64_loose(""), None);
        assert_eq!(parse_u64_loose("bytes"), None);
        assert_eq!(parse_u64_loose("   "), None);
    }

    #[test]
    fn u64_leading_whitespace_ok() {
        assert_eq!(parse_u64_loose("   156.561"), Some(156_561));
    }

    // ---- floats -----------------------------------------------------------

    #[test]
    fn f64_en_us_decimal_and_thousands() {
        assert_eq!(parse_f64_loose("1,048,576.00"), Some(1_048_576.0));
        let v = parse_f64_loose("347,956.00 bytes/sec").unwrap();
        assert!((v - 347_956.0).abs() < 0.001);
    }

    #[test]
    fn f64_it_decimal_and_thousands() {
        assert_eq!(parse_f64_loose("1.048.576,00"), Some(1_048_576.0));
        let v = parse_f64_loose("347.956,00 bytes/sec").unwrap();
        assert!((v - 347_956.0).abs() < 0.001);
    }

    #[test]
    fn f64_single_separator_as_decimal() {
        // Rule 3: 2 digits after '.' → decimal
        let v = parse_f64_loose("48.22").unwrap();
        assert!((v - 48.22).abs() < 0.0001);
        // Rule 3: 2 digits after ',' → decimal (IT speedup shape)
        let v = parse_f64_loose("48,22").unwrap();
        assert!((v - 48.22).abs() < 0.0001);
    }

    #[test]
    fn f64_single_separator_as_thousands() {
        // 3 digits after separator → thousands
        assert_eq!(parse_f64_loose("156,561"), Some(156_561.0));
        assert_eq!(parse_f64_loose("156.561"), Some(156_561.0));
    }

    #[test]
    fn f64_rejects_no_digits() {
        assert_eq!(parse_f64_loose(""), None);
        assert_eq!(parse_f64_loose("N/A"), None);
    }

    #[test]
    fn f64_single_digit_decimal() {
        // Single trailing digit → still treated as decimal (rule 3)
        let v = parse_f64_loose("3.5").unwrap();
        assert!((v - 3.5).abs() < 0.0001);
        let v = parse_f64_loose("3,5").unwrap();
        assert!((v - 3.5).abs() < 0.0001);
    }

    // ---- real rsync fixtures ---------------------------------------------

    #[test]
    fn rsync_summary_line_it_locale() {
        // From an it_IT run: "sent 1.048.938 bytes  received 35 bytes  2.097.946,00 bytes/sec"
        assert_eq!(parse_u64_loose("1.048.938"), Some(1_048_938));
        assert_eq!(parse_u64_loose("35"), Some(35));
        let v = parse_f64_loose("2.097.946,00").unwrap();
        assert!((v - 2_097_946.0).abs() < 0.01);
    }

    #[test]
    fn rsync_summary_line_en_us_locale() {
        // From a C/POSIX run: "sent 1,048,938 bytes  received 35 bytes  2,097,946.00 bytes/sec"
        assert_eq!(parse_u64_loose("1,048,938"), Some(1_048_938));
        assert_eq!(parse_u64_loose("35"), Some(35));
        let v = parse_f64_loose("2,097,946.00").unwrap();
        assert!((v - 2_097_946.0).abs() < 0.01);
    }

    #[test]
    fn rsync_speedup_both_locales() {
        // Speedup always 2 decimals: "speedup is 1.00" / "speedup is 48,22"
        assert!((parse_f64_loose("1.00").unwrap() - 1.0).abs() < 0.001);
        assert!((parse_f64_loose("1,00").unwrap() - 1.0).abs() < 0.001);
        assert!((parse_f64_loose("48.22").unwrap() - 48.22).abs() < 0.001);
        assert!((parse_f64_loose("48,22").unwrap() - 48.22).abs() < 0.001);
    }
}
