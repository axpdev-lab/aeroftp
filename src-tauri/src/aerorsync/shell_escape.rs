//! POSIX shell escaping for SSH `exec` command lines.
//!
//! SSH exec receives one command string on the remote side. OpenSSH-style
//! servers hand that string to the user's login shell, so every argv segment
//! must be quoted before joining.

/// Escape one argument for a POSIX shell command line.
///
/// Safe atoms are left unchanged to preserve the byte shape of the existing
/// rsync fixture commands. Everything else is single-quoted, with embedded
/// single quotes represented as `'\''`.
pub fn shell_escape_posix(arg: &str) -> String {
    if is_safe_shell_atom(arg) {
        return arg.to_string();
    }

    let mut out = String::with_capacity(arg.len() + 2);
    out.push('\'');
    for ch in arg.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

fn is_safe_shell_atom(arg: &str) -> bool {
    !arg.is_empty()
        && arg.bytes().all(|b| {
            matches!(
                b,
                b'A'..=b'Z'
                    | b'a'..=b'z'
                    | b'0'..=b'9'
                    | b'_'
                    | b'.'
                    | b'/'
                    | b'@'
                    | b'%'
                    | b'+'
                    | b'='
                    | b':'
                    | b'-'
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_atoms_are_unchanged() {
        for arg in [
            "rsync",
            "--server",
            "-logDtprze.iLsfxCIvu",
            "/workspace/upload/target.bin",
            "user@example:/a-b+c%25",
        ] {
            assert_eq!(shell_escape_posix(arg), arg);
        }
    }

    #[test]
    fn adversarial_atoms_are_single_quoted() {
        let cases = [
            ("", "''"),
            ("path with spaces/file.bin", "'path with spaces/file.bin'"),
            ("it's.bin", "'it'\\''s.bin'"),
            ("$(whoami)", "'$(whoami)'"),
            ("`id`", "'`id`'"),
            ("semi;colon", "'semi;colon'"),
            ("pipe|amp&", "'pipe|amp&'"),
            ("line\nbreak", "'line\nbreak'"),
            ("unicodé", "'unicodé'"),
        ];
        for (input, expected) in cases {
            assert_eq!(shell_escape_posix(input), expected);
        }
    }

    #[cfg(unix)]
    #[test]
    fn escaped_command_round_trips_through_posix_shell() {
        use std::process::Command;

        let args = [
            "rsync",
            "--server",
            "path with spaces/file.bin",
            "it's.bin",
            "$(whoami)",
            "`id`",
            "line\nbreak",
        ];
        let command_line = args
            .iter()
            .map(|arg| shell_escape_posix(arg))
            .collect::<Vec<_>>()
            .join(" ");
        let script = format!("for arg in {command_line}; do printf '%s\\0' \"$arg\"; done");
        let output = Command::new("/bin/sh")
            .arg("-c")
            .arg(script)
            .output()
            .expect("shell round-trip");
        assert!(
            output.status.success(),
            "shell failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let round_tripped = output
            .stdout
            .split(|b| *b == 0)
            .filter(|part| !part.is_empty())
            .map(|part| String::from_utf8(part.to_vec()).expect("utf8 arg"))
            .collect::<Vec<_>>();
        assert_eq!(round_tripped, args);
    }
}
