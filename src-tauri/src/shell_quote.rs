// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
//
// Quoting for the command lines AeroFTP prints for a person or an agent to
// run: the CLI's `Next:` hints and the AeroAgent tools' `suggested_next_command`.
// One rule in one place, so a profile name, a path or a pattern can never turn
// a suggested command into a different one.

/// One argument for the shell the user most likely types into on this
/// platform: POSIX (`sh`, Bash, zsh) on Unix, PowerShell on Windows, where it
/// is the default shell. The result is the whole argument: callers write it
/// as it is, without adding quotes.
pub fn shell_arg(value: &str) -> String {
    #[cfg(windows)]
    {
        powershell_arg(value)
    }
    #[cfg(not(windows))]
    {
        posix_arg(value)
    }
}

/// One POSIX shell argument, quoted the way Python's `shlex.quote` does it: a
/// value made only of characters no shell treats specially is left bare, and
/// anything else goes inside single quotes, where nothing expands (`$`,
/// backticks, `\`, and the `!` an interactive Bash would read as a history
/// event), with each `'` written as `'"'"'`.
pub fn posix_arg(value: &str) -> String {
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

/// One PowerShell argument to a native program such as `aeroftp-cli`. Bare
/// when it holds only characters PowerShell reads literally in an argument.
/// Otherwise two layers, inside out. First the Windows command-line rule the
/// program's C runtime reads back (`CommandLineToArgvW`): a `"` in the value
/// is written `\"`, with the backslashes right before it doubled, and when the
/// value has whitespace (Windows PowerShell 5.1 then wraps the argument in
/// quotes) its trailing backslashes are doubled too. Then PowerShell's own
/// double-quoted form, with a backtick before `$`, the backtick, and every
/// double quote PowerShell recognises (`"`, U+201C, U+201D, U+201E).
///
/// Written for Windows PowerShell 5.1, the one every Windows ships: it passes
/// native arguments in the legacy way that needs the first layer. PowerShell
/// 7.3 and later escape embedded quotes themselves, so there a value with a
/// `"` in it (never a Windows path) keeps a backslash and needs editing.
/// Double quotes also make the usual values (a name with spaces, a Windows
/// path, an apostrophe) one argument in cmd.exe.
pub fn powershell_arg(value: &str) -> String {
    let safe = !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-/\\:".contains(c));
    if safe {
        return value.to_string();
    }
    let mut native = String::with_capacity(value.len() + 4);
    let mut backslashes = 0usize;
    for c in value.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                native.push_str(&"\\".repeat(backslashes * 2 + 1));
                backslashes = 0;
            }
            _ => {
                native.push_str(&"\\".repeat(backslashes));
                backslashes = 0;
            }
        }
        if c != '\\' {
            native.push(c);
        }
    }
    let wrapped = value.chars().any(char::is_whitespace);
    native.push_str(&"\\".repeat(if wrapped {
        backslashes * 2
    } else {
        backslashes
    }));
    let mut out = String::with_capacity(native.len() + 2);
    out.push('"');
    for c in native.chars() {
        if matches!(c, '$' | '`' | '"' | '\u{201C}' | '\u{201D}' | '\u{201E}') {
            out.push('`');
        }
        out.push(c);
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::{posix_arg, powershell_arg, shell_arg};

    #[test]
    fn quotes_like_shlex() {
        assert_eq!(posix_arg("/var/www/app.js"), "/var/www/app.js");
        assert_eq!(posix_arg(""), "''");
        assert_eq!(posix_arg("My Server"), "'My Server'");
        assert_eq!(posix_arg("$(id) `id` !x \\"), "'$(id) `id` !x \\'");
        assert_eq!(posix_arg("it's"), r#"'it'"'"'s'"#);
    }

    /// On Windows the hints were POSIX-quoted, and cmd.exe splits
    /// `'My Server'` into two arguments; the old double-quoted hints worked.
    #[test]
    fn quotes_for_powershell_with_double_quotes() {
        assert_eq!(powershell_arg(r"C:\Users\me"), r"C:\Users\me");
        assert_eq!(powershell_arg(""), "\"\"");
        assert_eq!(powershell_arg("My Server"), "\"My Server\"");
        assert_eq!(powershell_arg("it's"), "\"it's\"");
        assert_eq!(powershell_arg("a,b"), "\"a,b\"");
        assert_eq!(powershell_arg("@x"), "\"@x\"");
        assert_eq!(
            powershell_arg("$(id) `id` \"q\" \u{201C}t\u{201D}"),
            "\"`$(id) ``id`` \\`\"q\\`\" `\u{201C}t`\u{201D}\""
        );
        // Backslashes before a quote are doubled, trailing ones too when the
        // value has a space (Windows PowerShell 5.1 wraps it in quotes).
        assert_eq!(powershell_arg(r#"x\"y"#), r#""x\\\`"y""#);
        assert_eq!(powershell_arg(r"sp ace\"), r#""sp ace\\""#);
        assert_eq!(powershell_arg(r"a,b\"), r#""a,b\""#);
    }

    #[test]
    fn the_platform_picks_its_shell() {
        let expected = if cfg!(windows) {
            powershell_arg("My Server")
        } else {
            posix_arg("My Server")
        };
        assert_eq!(shell_arg("My Server"), expected);
    }

    /// Windows PowerShell 5.1 is the judge: it runs a native program with
    /// each quoted value, and that program must receive the value byte for
    /// byte, with no substitution run. Python stands in for aeroftp-cli as the
    /// native program that prints its first argument.
    #[cfg(windows)]
    #[test]
    fn powershell_passes_every_value_to_a_native_program_unchanged() {
        let dir = tempfile::tempdir().expect("temp dir");
        let marker = dir.path().join("ran");
        let dump = dir.path().join("dump.py");
        std::fs::write(
            &dump,
            "import sys\nsys.stdout.buffer.write(('[' + sys.argv[1] + ']').encode('utf-8'))\n",
        )
        .expect("write the dumper");
        let mut values = hostile_values(&marker);
        values.retain(|v| !v.is_empty()); // 5.1 drops an empty native argument
        values.extend([
            "typo\u{201C}graphic\u{201D} \u{201E}q".to_string(),
            "it's, @a".to_string(),
            r#"x\"y"#.to_string(),
            r"trail\".to_string(),
            r"sp ace\".to_string(),
            r#"q"uote"#.to_string(),
        ]);
        for value in values {
            let script = format!("& python '{}' {}", dump.display(), powershell_arg(&value));
            let out = std::process::Command::new("powershell")
                .args(["-NoProfile", "-NonInteractive", "-Command", &script])
                .output()
                .expect("run powershell");
            assert_eq!(
                String::from_utf8_lossy(&out.stdout),
                format!("[{value}]"),
                "{value:?} as {}: {}",
                powershell_arg(&value),
                String::from_utf8_lossy(&out.stderr)
            );
        }
        assert!(!marker.exists(), "a substitution ran");
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
                .arg(format!("printf '[%s]' {}", posix_arg(&value)))
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
        // Every Unix CI runner and developer station here has bash: a missing
        // one is a broken bench, not a pass.
        std::process::Command::new("bash")
            .arg("--version")
            .output()
            .expect("bash is installed");
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
            // Proves history expansion is on in this shell: without it the
            // test would not exercise the `!` it exists for.
            writeln!(
                child.stdin.as_mut().expect("stdin"),
                "[[ $- == *H* ]] && echo HISTEXPAND-ON\nprintf '[%s]\\n' {}",
                posix_arg(&value)
            )
            .expect("write the line");
            let out = child.wait_with_output().expect("bash output");
            let stdout = String::from_utf8_lossy(&out.stdout);
            assert!(
                stdout.contains("HISTEXPAND-ON"),
                "history expansion is off: {stdout:?}"
            );
            assert!(
                stdout.contains(&format!("[{value}]")),
                "{value:?}: stdout {stdout:?} stderr {:?}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        assert!(!marker.exists(), "a substitution ran");
    }
}
