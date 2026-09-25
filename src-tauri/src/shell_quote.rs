// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
//
// Quoting for the command lines AeroFTP prints for a person or an agent to
// run: the CLI's `Next:` hints and the AeroAgent tools' `suggested_next_command`.
// One rule in one place, so a profile name, a path or a pattern can never turn
// a suggested command into a different one.

/// The body of a POSIX double-quoted word. `\`, `"`, `$` and `` ` `` are
/// escaped, so a value such as `$(rm -rf ~)` or `` `id` `` stays text when the
/// line is pasted into a shell; the caller writes the surrounding quotes.
pub fn double_quote_body(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        if matches!(ch, '\\' | '"' | '$' | '`') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::double_quote_body;

    #[test]
    fn escapes_every_character_a_double_quoted_word_expands() {
        assert_eq!(
            double_quote_body(r#"a "b" $(c) `d` \e"#),
            r#"a \"b\" \$(c) \`d\` \\e"#
        );
    }

    /// The shell is the judge: every hostile value comes back byte for byte,
    /// and none of the substitutions runs.
    #[cfg(unix)]
    #[test]
    fn a_real_shell_reads_the_quoted_value_back_unchanged() {
        let dir = tempfile::tempdir().expect("temp dir");
        let marker = dir.path().join("ran");
        let values = [
            format!("$(touch {})", marker.display()),
            format!("`touch {}`", marker.display()),
            "${HOME}".to_string(),
            r#"back\slash "quoted" 'single'"#.to_string(),
            "My Server".to_string(),
        ];
        for value in values {
            let out = std::process::Command::new("sh")
                .arg("-c")
                .arg(format!("printf %s \"{}\"", double_quote_body(&value)))
                .output()
                .expect("run sh");
            assert_eq!(String::from_utf8_lossy(&out.stdout), value);
        }
        assert!(!marker.exists(), "a substitution ran");
    }
}
