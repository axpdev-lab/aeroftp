// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
//
// Quoting for the command lines AeroFTP prints for a person or an agent to
// run: the CLI's `Next:` hints and the AeroAgent tools' `suggested_next_command`.
// One rule in one place, so a profile name, a path or a pattern can never turn
// a suggested command into a different one.

/// One POSIX shell argument, quoted the way Python's `shlex.quote` does it: a
/// value made only of characters no shell treats specially is left bare, and
/// anything else goes inside single quotes, where nothing expands (`$`,
/// backticks, `\`, and the `!` an interactive Bash would read as a history
/// event), with each `'` written as `'"'"'`. The result is the whole argument:
/// callers write it as it is, without adding quotes.
pub fn shell_arg(value: &str) -> String {
    let safe = !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "@%+=:,./-_".contains(c));
    if safe {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', r#"'"'"'"#))
    }
}

#[cfg(test)]
mod tests {
    use super::shell_arg;

    #[test]
    fn quotes_like_shlex() {
        assert_eq!(shell_arg("/var/www/app.js"), "/var/www/app.js");
        assert_eq!(shell_arg(""), "''");
        assert_eq!(shell_arg("My Server"), "'My Server'");
        assert_eq!(shell_arg("$(id) `id` !x \\"), "'$(id) `id` !x \\'");
        assert_eq!(shell_arg("it's"), r#"'it'"'"'s'"#);
    }

    fn hostile_values(marker: &std::path::Path) -> Vec<String> {
        vec![
            format!("$(touch {})", marker.display()),
            format!("`touch {}`", marker.display()),
            "${HOME}".to_string(),
            r#"back\slash "double" 'single'"#.to_string(),
            "My Server".to_string(),
            "Prod!backup".to_string(),
            "a!!b".to_string(),
            String::new(),
        ]
    }

    /// The shell is the judge: every hostile value comes back byte for byte
    /// through `sh`, and none of the substitutions runs.
    #[cfg(unix)]
    #[test]
    fn sh_reads_every_value_back_unchanged() {
        let dir = tempfile::tempdir().expect("temp dir");
        let marker = dir.path().join("ran");
        for value in hostile_values(&marker) {
            let out = std::process::Command::new("sh")
                .arg("-c")
                .arg(format!("printf '[%s]' {}", shell_arg(&value)))
                .output()
                .expect("run sh");
            assert_eq!(String::from_utf8_lossy(&out.stdout), format!("[{value}]"));
        }
        assert!(!marker.exists(), "a substitution ran");
    }

    /// An interactive Bash has history expansion on, which refuses a
    /// double-quoted `"Prod!backup"` with "event not found". A quoted argument
    /// reads back unchanged there too.
    #[cfg(unix)]
    #[test]
    fn an_interactive_bash_reads_every_value_back_unchanged() {
        use std::io::Write;
        if std::process::Command::new("bash")
            .arg("--version")
            .output()
            .is_err()
        {
            return; // no bash on this host: the sh test covers the rest
        }
        let dir = tempfile::tempdir().expect("temp dir");
        let marker = dir.path().join("ran");
        for value in hostile_values(&marker) {
            let mut child = std::process::Command::new("bash")
                .arg("-i")
                .env("HISTFILE", "/dev/null")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .expect("run bash -i");
            writeln!(
                child.stdin.as_mut().expect("stdin"),
                "printf '[%s]\\n' {}",
                shell_arg(&value)
            )
            .expect("write the line");
            let out = child.wait_with_output().expect("bash output");
            let stdout = String::from_utf8_lossy(&out.stdout);
            assert!(
                stdout.contains(&format!("[{value}]")),
                "{value:?}: stdout {stdout:?} stderr {:?}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        assert!(!marker.exists(), "a substitution ran");
    }
}
