//! The SCP control lines, as every server-side `scp` speaks them.
//!
//! SCP has no specification: the reference is the legacy mode of OpenSSH's
//! `scp.c`, which Dropbear's multi-call `scp` follows. A transfer is an exec
//! of `scp -t <target>` (the server receives) or `scp -f <source>` (the server
//! sends), and the channel carries these lines:
//!
//! - `C<mode> <size> <name>\n`: a regular file follows, `size` bytes, then a
//!   `\0` from the sender;
//! - `D<mode> 0 <name>\n` and `E\n`: enter and leave a directory;
//! - `T<mtime> 0 <atime> 0\n`: times for the next `C` or `D`;
//! - one byte answering each of them: `\0` for OK, `\x01` for a warning and
//!   `\x02` for a fatal error, the last two followed by a message line.
//!
//! A name in a `C` or `D` line is a single path component. The names a server
//! sends are validated before anything uses them (the CVE-2019-6111 class:
//! a malicious server naming a file the client never asked for).

use crate::providers::ProviderError;

/// The longest control or message line accepted, newline excluded.
pub const MAX_LINE: usize = 8192;

/// One control line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Control {
    File { mode: u32, size: u64, name: String },
    Dir { mode: u32, name: String },
    EndDir,
    Times { mtime: u64, atime: u64 },
}

/// The answer to a control line or to a file's data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    Ok,
    Warning(String),
    Fatal(String),
}

fn protocol_error(what: impl std::fmt::Display) -> ProviderError {
    ProviderError::ParseError(format!("SCP protocol: {what}"))
}

/// A name a `C` or `D` line may carry: one component, not `.` or `..`, with
/// nothing that would end the line or name another directory.
pub fn validate_name(name: &str) -> Result<(), ProviderError> {
    if name.is_empty() || name == "." || name == ".." {
        return Err(ProviderError::InvalidPath(format!(
            "{name:?} is not a file name SCP can carry"
        )));
    }
    if name.contains(['/', '\n', '\0']) {
        return Err(ProviderError::InvalidPath(format!(
            "{name:?}: SCP cannot carry a name with a slash, a newline or a NUL byte"
        )));
    }
    Ok(())
}

/// The `C` line announcing a file of `size` bytes.
pub fn file_line(mode: u32, size: u64, name: &str) -> Result<Vec<u8>, ProviderError> {
    validate_name(name)?;
    Ok(format!("C{:04o} {} {}\n", mode & 0o7777, size, name).into_bytes())
}

/// The `T` line carrying modification and access times, in seconds.
pub fn times_line(mtime: u64, atime: u64) -> Vec<u8> {
    format!("T{mtime} 0 {atime} 0\n").into_bytes()
}

fn parse_mode(s: &str) -> Result<u32, ProviderError> {
    if s.len() != 4 || !s.bytes().all(|b| (b'0'..=b'7').contains(&b)) {
        return Err(protocol_error(format_args!("bad mode {s:?}")));
    }
    u32::from_str_radix(s, 8).map_err(|_| protocol_error(format_args!("bad mode {s:?}")))
}

fn parse_u64(s: &str, what: &str) -> Result<u64, ProviderError> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return Err(protocol_error(format_args!("bad {what} {s:?}")));
    }
    s.parse::<u64>()
        .map_err(|_| protocol_error(format_args!("{what} {s:?} out of range")))
}

/// Parse one control line, its trailing newline already removed.
pub fn parse_control(line: &[u8]) -> Result<Control, ProviderError> {
    if line.len() > MAX_LINE {
        return Err(protocol_error("control line too long"));
    }
    let line =
        std::str::from_utf8(line).map_err(|_| protocol_error("control line is not UTF-8"))?;
    // `get` and not `split_at`: a server may start the line with a multi-byte
    // character, and byte 1 is then no boundary. Such a line, like an empty
    // one, has no kind and is refused below.
    let kind = line.get(..1).unwrap_or("");
    let rest = &line[kind.len()..];
    match kind {
        "C" | "D" => {
            let (mode, rest) = rest
                .split_once(' ')
                .ok_or_else(|| protocol_error(format_args!("truncated {kind} line")))?;
            let (size, name) = rest
                .split_once(' ')
                .ok_or_else(|| protocol_error(format_args!("truncated {kind} line")))?;
            let mode = parse_mode(mode)?;
            let size = parse_u64(size, "size")?;
            validate_name(name)?;
            Ok(if kind == "C" {
                Control::File {
                    mode,
                    size,
                    name: name.to_string(),
                }
            } else {
                Control::Dir {
                    mode,
                    name: name.to_string(),
                }
            })
        }
        "E" if rest.is_empty() => Ok(Control::EndDir),
        "T" => {
            let parts: Vec<&str> = rest.split(' ').collect();
            if parts.len() != 4 {
                return Err(protocol_error("bad T line"));
            }
            let mtime = parse_u64(parts[0], "mtime")?;
            parse_u64(parts[1], "mtime microseconds")?;
            let atime = parse_u64(parts[2], "atime")?;
            parse_u64(parts[3], "atime microseconds")?;
            Ok(Control::Times { mtime, atime })
        }
        _ => Err(protocol_error(format_args!(
            "unexpected control line {:?}",
            line.chars().take(40).collect::<String>()
        ))),
    }
}

/// The reply a status byte opens. `message` is the line after a warning or
/// fatal byte (newline removed); it is ignored for `\0`.
pub fn reply(status: u8, message: &[u8]) -> Result<Reply, ProviderError> {
    let text = || String::from_utf8_lossy(&message[..message.len().min(MAX_LINE)]).into_owned();
    match status {
        0 => Ok(Reply::Ok),
        1 => Ok(Reply::Warning(text())),
        2 => Ok(Reply::Fatal(text())),
        other => Err(protocol_error(format_args!(
            "unexpected status byte {other:#04x}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_line_round_trips() {
        let line = file_line(0o100644, 1234, "a b.txt").unwrap();
        assert_eq!(line, b"C0644 1234 a b.txt\n");
        assert_eq!(
            parse_control(&line[..line.len() - 1]).unwrap(),
            Control::File {
                mode: 0o644,
                size: 1234,
                name: "a b.txt".to_string()
            }
        );
    }

    #[test]
    fn dir_end_and_times_lines_parse() {
        assert_eq!(
            parse_control(b"D0755 0 sub").unwrap(),
            Control::Dir {
                mode: 0o755,
                name: "sub".to_string()
            }
        );
        assert_eq!(parse_control(b"E").unwrap(), Control::EndDir);
        assert_eq!(
            times_line(1_700_000_000, 1_700_000_001),
            b"T1700000000 0 1700000001 0\n"
        );
        assert_eq!(
            parse_control(b"T1700000000 0 1700000001 0").unwrap(),
            Control::Times {
                mtime: 1_700_000_000,
                atime: 1_700_000_001
            }
        );
    }

    #[test]
    fn names_a_server_must_not_send_are_refused() {
        for bad in [
            &b"C0644 1 ../etc/passwd"[..],
            b"C0644 1 sub/file",
            b"C0644 1 ..",
            b"C0644 1 .",
            b"C0644 1 ",
            b"D0755 0 /abs",
        ] {
            assert!(
                parse_control(bad).is_err(),
                "{:?}",
                String::from_utf8_lossy(bad)
            );
        }
    }

    #[test]
    fn malformed_lines_are_protocol_errors() {
        for bad in [
            &b"C644 1 f"[..],
            b"C0948 1 f",
            b"C0644 -1 f",
            b"C0644 99999999999999999999999 f",
            b"C0644 1",
            b"Ex",
            b"T1 0 2",
            b"X0644 1 f",
            b"",
            "é0644 1 f".as_bytes(),
        ] {
            assert!(
                parse_control(bad).is_err(),
                "{:?}",
                String::from_utf8_lossy(bad)
            );
        }
        assert!(parse_control(&vec![b'E'; MAX_LINE + 1]).is_err());
    }

    #[test]
    fn writing_a_name_scp_cannot_carry_is_refused() {
        for bad in ["", ".", "..", "a/b", "new\nline", "nul\0"] {
            assert!(file_line(0o644, 0, bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn status_bytes_map_to_replies() {
        assert_eq!(reply(0, b"").unwrap(), Reply::Ok);
        assert_eq!(
            reply(1, b"scp: x: No such file").unwrap(),
            Reply::Warning("scp: x: No such file".to_string())
        );
        assert_eq!(
            reply(2, b"fatal").unwrap(),
            Reply::Fatal("fatal".to_string())
        );
        assert!(reply(b'C', b"").is_err());
    }
}
