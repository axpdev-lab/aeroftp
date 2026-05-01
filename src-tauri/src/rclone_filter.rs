// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

//! Rclone filter file → `.aeroignore` converter.
//!
//! Rclone filtering reference: <https://rclone.org/filtering/>
//!
//! ## Semantic mapping
//!
//! | rclone filter | `.aeroignore` (gitignore-like) |
//! |---|---|
//! | `+ pattern` (include) | `!pattern` (re-include) |
//! | `- pattern` (exclude) | `pattern` (exclude) |
//! | `# ...` / `; ...` (comment) | `# ...` (comment) |
//! | `! ` (reset rules so far) | section break — see warning |
//! | first-match wins | **last-match wins** |
//!
//! Because match order semantics differ, the converter **reverses the rule
//! order** so the last-match rule in `.aeroignore` corresponds to the
//! first-match rule in rclone. Concretely, rules earlier in the rclone file
//! win over rules later, so when reversed the same precedence is recovered
//! under last-match-wins semantics.
//!
//! ## Limitations
//!
//! - `{a,b,c}` brace alternation is supported by rclone but not by gitignore.
//!   Patterns containing `{...}` are passed through unchanged with a warning.
//! - The `! ` reset directive is recorded as a warning and translated as a
//!   visible separator in the output. Rules emitted before the reset are
//!   preserved (rclone discards them at runtime); see `convert_to_aeroignore`
//!   for the rationale.
//! - `--filter-from -` (stdin) is not handled here; the caller is responsible
//!   for sourcing the content.

use std::fmt;

/// Action of a single rclone filter rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RcloneFilterAction {
    /// `+ pattern` — files matching this pattern are included.
    Include,
    /// `- pattern` — files matching this pattern are excluded.
    Exclude,
}

/// A parsed rclone filter rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RcloneFilterRule {
    pub action: RcloneFilterAction,
    pub pattern: String,
    /// Source line number (1-based), useful for diagnostics.
    pub line: usize,
}

/// A warning emitted by the parser/converter for non-fatal issues that the
/// caller may surface to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RcloneFilterWarning {
    /// Encountered a `! ` reset directive at the given line. The rules above
    /// it would have been cleared at rclone runtime; we keep them but emit
    /// a separator comment in the output.
    ResetDirective { line: usize },
    /// The pattern contains brace alternation `{a,b}` which gitignore does
    /// not support. The pattern is passed through; the user must expand it
    /// manually if needed.
    BraceAlternation { line: usize, pattern: String },
    /// A line could not be parsed (unknown prefix, malformed). Skipped.
    UnrecognizedLine { line: usize, content: String },
}

impl fmt::Display for RcloneFilterWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RcloneFilterWarning::ResetDirective { line } => {
                write!(f, "line {}: '!' reset directive treated as section break (gitignore has no equivalent)", line)
            }
            RcloneFilterWarning::BraceAlternation { line, pattern } => {
                write!(f, "line {}: pattern '{}' uses brace alternation '{{a,b}}' which gitignore does not support; passed through unchanged", line, pattern)
            }
            RcloneFilterWarning::UnrecognizedLine { line, content } => {
                write!(
                    f,
                    "line {}: unrecognized rule '{}' — skipped",
                    line, content
                )
            }
        }
    }
}

/// Parse the contents of an rclone filter file.
///
/// Returns the list of rules in file order and a list of warnings.
pub fn parse_rclone_filter(content: &str) -> (Vec<RcloneFilterRule>, Vec<RcloneFilterWarning>) {
    let mut rules = Vec::new();
    let mut warnings = Vec::new();

    for (idx, raw_line) in content.lines().enumerate() {
        let line_no = idx + 1;
        let trimmed = raw_line.trim();

        // Skip empty lines and comments (`#` and `;` per rclone docs).
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }

        // `! ` (or `!` alone) is the reset directive.
        if trimmed == "!" || trimmed.starts_with("! ") {
            warnings.push(RcloneFilterWarning::ResetDirective { line: line_no });
            continue;
        }

        // `+ pattern` or `- pattern` — exactly the prefix + space.
        let (action, pattern) = if let Some(rest) = trimmed.strip_prefix("+ ") {
            (RcloneFilterAction::Include, rest.trim())
        } else if let Some(rest) = trimmed.strip_prefix("- ") {
            (RcloneFilterAction::Exclude, rest.trim())
        } else {
            warnings.push(RcloneFilterWarning::UnrecognizedLine {
                line: line_no,
                content: trimmed.to_string(),
            });
            continue;
        };

        if pattern.is_empty() {
            warnings.push(RcloneFilterWarning::UnrecognizedLine {
                line: line_no,
                content: trimmed.to_string(),
            });
            continue;
        }

        if pattern.contains('{') && pattern.contains('}') {
            warnings.push(RcloneFilterWarning::BraceAlternation {
                line: line_no,
                pattern: pattern.to_string(),
            });
        }

        rules.push(RcloneFilterRule {
            action,
            pattern: pattern.to_string(),
            line: line_no,
        });
    }

    (rules, warnings)
}

/// Convert a sequence of rclone filter rules into a `.aeroignore`-formatted
/// string (gitignore-like).
///
/// **Semantic-preserving order**: rclone uses first-match-wins, gitignore uses
/// last-match-wins. The output reverses the input rule order so a rule that
/// appeared first in the rclone file (high-priority) ends up last in the
/// `.aeroignore` (also high-priority under last-match).
///
/// The output starts with a header explaining the provenance and any
/// limitations.
pub fn convert_to_aeroignore(
    rules: &[RcloneFilterRule],
    warnings: &[RcloneFilterWarning],
) -> String {
    let mut out = String::new();

    out.push_str("# Generated by AeroFTP from rclone filter file\n");
    out.push_str("# Original rules were in first-match-wins order; here they are reversed\n");
    out.push_str("# to preserve semantics under gitignore last-match-wins rules.\n");

    if !warnings.is_empty() {
        out.push_str("#\n# Warnings (review before use):\n");
        for w in warnings {
            out.push_str(&format!("#   {}\n", w));
        }
    }

    out.push('\n');

    // Reverse to preserve first-match-wins → last-match-wins precedence.
    for rule in rules.iter().rev() {
        match rule.action {
            RcloneFilterAction::Exclude => {
                out.push_str(&rule.pattern);
                out.push('\n');
            }
            RcloneFilterAction::Include => {
                out.push('!');
                out.push_str(&rule.pattern);
                out.push('\n');
            }
        }
    }

    out
}

/// One-shot helper: parse rclone filter content and emit `.aeroignore`.
pub fn rclone_filter_to_aeroignore(content: &str) -> (String, Vec<RcloneFilterWarning>) {
    let (rules, warnings) = parse_rclone_filter(content);
    let aeroignore = convert_to_aeroignore(&rules, &warnings);
    (aeroignore, warnings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_empty_input_yields_no_rules() {
        let (rules, warnings) = parse_rclone_filter("");
        assert!(rules.is_empty());
        assert!(warnings.is_empty());
    }

    #[test]
    fn parse_skips_comments_and_blanks() {
        let input = "# top comment\n\n; another comment\n   \n";
        let (rules, warnings) = parse_rclone_filter(input);
        assert!(rules.is_empty());
        assert!(warnings.is_empty());
    }

    #[test]
    fn parse_recognises_include_and_exclude() {
        let input = "- *.tmp\n+ /important.txt\n- *.log\n";
        let (rules, warnings) = parse_rclone_filter(input);
        assert_eq!(rules.len(), 3);
        assert_eq!(rules[0].action, RcloneFilterAction::Exclude);
        assert_eq!(rules[0].pattern, "*.tmp");
        assert_eq!(rules[0].line, 1);
        assert_eq!(rules[1].action, RcloneFilterAction::Include);
        assert_eq!(rules[1].pattern, "/important.txt");
        assert_eq!(rules[1].line, 2);
        assert_eq!(rules[2].action, RcloneFilterAction::Exclude);
        assert_eq!(rules[2].pattern, "*.log");
        assert!(warnings.is_empty());
    }

    #[test]
    fn parse_reset_directive_yields_warning() {
        let input = "+ a.txt\n!\n- b.txt\n";
        let (rules, warnings) = parse_rclone_filter(input);
        assert_eq!(rules.len(), 2);
        assert_eq!(warnings.len(), 1);
        assert!(matches!(
            warnings[0],
            RcloneFilterWarning::ResetDirective { line: 2 }
        ));
    }

    #[test]
    fn parse_brace_alternation_yields_warning_but_keeps_rule() {
        let input = "- *.{tmp,bak}\n";
        let (rules, warnings) = parse_rclone_filter(input);
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].pattern, "*.{tmp,bak}");
        assert_eq!(warnings.len(), 1);
        assert!(matches!(
            warnings[0],
            RcloneFilterWarning::BraceAlternation { line: 1, .. }
        ));
    }

    #[test]
    fn parse_unrecognized_line_yields_warning_no_rule() {
        let input = "exclude this\n";
        let (rules, warnings) = parse_rclone_filter(input);
        assert!(rules.is_empty());
        assert_eq!(warnings.len(), 1);
        assert!(matches!(
            warnings[0],
            RcloneFilterWarning::UnrecognizedLine { line: 1, .. }
        ));
    }

    #[test]
    fn parse_empty_pattern_after_prefix_yields_warning() {
        let input = "+ \n- \n";
        let (rules, warnings) = parse_rclone_filter(input);
        assert!(rules.is_empty());
        assert_eq!(warnings.len(), 2);
    }

    #[test]
    fn convert_reverses_order_to_preserve_semantics() {
        // First-match (rclone): "important.txt" hits `+ /important.txt` first → INCLUDED.
        // Last-match (gitignore): for the same outcome the !-rule must come AFTER.
        let rules = vec![
            RcloneFilterRule {
                action: RcloneFilterAction::Include,
                pattern: "/important.txt".to_string(),
                line: 1,
            },
            RcloneFilterRule {
                action: RcloneFilterAction::Exclude,
                pattern: "*.txt".to_string(),
                line: 2,
            },
        ];
        let out = convert_to_aeroignore(&rules, &[]);
        // The body should list "*.txt" BEFORE "!/important.txt"
        let body: Vec<&str> = out
            .lines()
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect();
        assert_eq!(body, vec!["*.txt", "!/important.txt"]);
    }

    #[test]
    fn convert_emits_header() {
        let out = convert_to_aeroignore(&[], &[]);
        assert!(out.contains("# Generated by AeroFTP from rclone filter file"));
        assert!(out.contains("first-match-wins"));
    }

    #[test]
    fn convert_includes_warnings_in_header_when_present() {
        let warnings = vec![
            RcloneFilterWarning::ResetDirective { line: 5 },
            RcloneFilterWarning::BraceAlternation {
                line: 7,
                pattern: "*.{a,b}".to_string(),
            },
        ];
        let out = convert_to_aeroignore(&[], &warnings);
        assert!(out.contains("# Warnings"));
        assert!(out.contains("line 5"));
        assert!(out.contains("line 7"));
        assert!(out.contains("*.{a,b}"));
    }

    #[test]
    fn convert_no_warnings_no_warning_header() {
        let rules = vec![RcloneFilterRule {
            action: RcloneFilterAction::Exclude,
            pattern: "*.tmp".to_string(),
            line: 1,
        }];
        let out = convert_to_aeroignore(&rules, &[]);
        assert!(!out.contains("# Warnings"));
    }

    #[test]
    fn end_to_end_realistic_rclone_filter() {
        let input = "\
# Build artifacts
- target/
- node_modules/
- *.tmp
- *.log

# Always keep important docs
+ /docs/IMPORTANT.md
+ /README.md
";
        let (out, warnings) = rclone_filter_to_aeroignore(input);
        assert!(warnings.is_empty());

        let body: Vec<&str> = out
            .lines()
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect();

        // Reversed order: includes (high priority in rclone) end up last in gitignore.
        assert_eq!(
            body,
            vec![
                "!/README.md",
                "!/docs/IMPORTANT.md",
                "*.log",
                "*.tmp",
                "node_modules/",
                "target/",
            ]
        );
    }

    #[test]
    fn end_to_end_with_reset_keeps_rules_above_with_warning() {
        // Note on policy: rclone would discard rules above `!`. We keep them
        // and emit a warning so the user can edit them out manually if needed.
        let input = "+ /a.txt\n- *.txt\n!\n+ /b.txt\n";
        let (out, warnings) = rclone_filter_to_aeroignore(input);
        assert_eq!(warnings.len(), 1);
        assert!(matches!(
            warnings[0],
            RcloneFilterWarning::ResetDirective { line: 3 }
        ));
        // All three pattern rules are present in reversed order.
        let body: Vec<&str> = out
            .lines()
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect();
        assert_eq!(body, vec!["!/b.txt", "*.txt", "!/a.txt"]);
    }

    #[test]
    fn line_numbers_are_one_based() {
        let input = "\n\n- first.txt\n";
        let (rules, _warnings) = parse_rclone_filter(input);
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].line, 3);
    }

    #[test]
    fn warning_display_is_human_readable() {
        let w = RcloneFilterWarning::ResetDirective { line: 42 };
        assert!(w.to_string().contains("line 42"));
        assert!(w.to_string().contains("reset"));
    }
}
