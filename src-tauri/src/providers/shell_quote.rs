//! Shell quoting for every path AeroFTP puts on a remote command line.
//!
//! An SSH exec request is run by the account's login shell (`sh -c` on every
//! POSIX server), so a path in `sha256sum` (the SFTP checksum), `scp -t`, `ls`
//! or `mv` (the SCP engine) is a word the remote shell parses. One function quotes all of them: the path
//! goes inside single quotes, where a POSIX shell interprets nothing, and each
//! single quote in it becomes `'\''` (close the quote, an escaped quote, open
//! again). A NUL byte cannot reach any POSIX tool, so a path holding one is
//! refused instead of being cut short on the server.

use crate::providers::ProviderError;

/// `path` as one single-quoted shell word.
pub fn shell_quote(path: &str) -> Result<String, ProviderError> {
    let quoted = shell_quote_bytes(path.as_bytes())?;
    // Only ASCII quotes were added to valid UTF-8, so this never fails.
    String::from_utf8(quoted)
        .map_err(|_| ProviderError::InvalidPath("quoting broke the path's UTF-8".to_string()))
}

/// [`shell_quote`] for a name read back from the server, which need not be
/// UTF-8: inside single quotes the shell passes every byte but NUL as is.
pub fn shell_quote_bytes(path: &[u8]) -> Result<Vec<u8>, ProviderError> {
    if path.contains(&0) {
        return Err(ProviderError::InvalidPath(
            "a path with a NUL byte cannot be passed to the server".to_string(),
        ));
    }
    let mut out = Vec::with_capacity(path.len() + 2);
    out.push(b'\'');
    for &b in path {
        if b == b'\'' {
            out.extend_from_slice(b"'\\''");
        } else {
            out.push(b);
        }
    }
    out.push(b'\'');
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Names for the real-shell test below, which needs a POSIX `sh`.
    #[cfg(unix)]
    const HOSTILE: &[&str] = &[
        "plain",
        "a b",
        "it's",
        "''",
        "-rf",
        "--",
        "$(touch pwned)",
        "`touch pwned`",
        "a;touch pwned",
        "a|b&c>d<e",
        "back\\slash",
        "new\nline",
        "tab\tx",
        "*",
        "~root",
        "café",
        "\"double\"",
        "'; rm -rf / ; echo '",
        "",
    ];

    #[test]
    fn quoting_wraps_and_escapes_single_quotes_only() {
        assert_eq!(shell_quote("a b").unwrap(), "'a b'");
        assert_eq!(shell_quote("it's").unwrap(), "'it'\\''s'");
        assert_eq!(shell_quote("").unwrap(), "''");
        assert_eq!(shell_quote("$(x)").unwrap(), "'$(x)'");
    }

    #[test]
    fn a_nul_byte_is_refused() {
        assert!(matches!(
            shell_quote("a\0b"),
            Err(ProviderError::InvalidPath(_))
        ));
    }

    /// The property that matters, checked against a real shell: whatever the
    /// name, the shell hands it back byte for byte and runs nothing.
    #[cfg(unix)]
    #[test]
    fn a_real_shell_reads_every_quoted_name_back_unchanged() {
        let dir = std::env::temp_dir().join(format!("scp-quote-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in HOSTILE {
            let script = format!("printf '%s' {}", shell_quote(name).unwrap());
            let out = std::process::Command::new("sh")
                .arg("-c")
                .arg(&script)
                .current_dir(&dir)
                .output()
                .expect("run sh");
            assert!(out.status.success(), "{name:?}: {out:?}");
            assert_eq!(String::from_utf8(out.stdout).unwrap(), *name);
        }
        assert!(!dir.join("pwned").exists(), "a quoted name ran a command");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
