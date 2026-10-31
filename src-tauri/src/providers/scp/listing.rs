//! Directory listing for SCP mode, from two shell commands.
//!
//! SCP has no listing of its own, and a stock OpenWrt has no `stat`, so the
//! metadata comes from `LC_ALL=C ls -lan --full-time -- '<dir>'`, which
//! BusyBox, GNU coreutils and uutils all support: type and mode, size, and an
//! mtime with seconds and a UTC offset. `ls` cannot carry every name,
//! though: BusyBox prints a control byte as `?`, and GNU prints a newline as
//! is, which splits the line and lets a crafted name print a line that looks
//! like another entry's. The names therefore come from
//! `find '<dir>' -mindepth 1 -maxdepth 1 -print0`, byte exact, and each name
//! is matched to its `ls` line:
//!
//! - a name with a control byte, or one that matches no line or several,
//!   is not guessed: [`resolve_script`] runs `ls -land` on it alone and
//!   [`parse_resolved`] takes the metadata from the first line of that
//!   output, which holds every field before the name;
//! - an `ls` line that matches no name is ignored, so neither the fragment of
//!   a split line nor a spoofed one becomes an entry.
//!
//! Names stay bytes here: a server may hold names that are not UTF-8.
//! Measurements and fixtures: `fixtures/scp/` (BusyBox 1.36.1 from OpenWrt
//! 24.10.4, GNU coreutils 9.4, uutils 0.10.0).

use crate::providers::shell_quote::shell_quote_bytes;
use crate::providers::ProviderError;

/// The file type, from the first letter of the `ls` mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
    Symlink,
    CharDevice,
    BlockDevice,
    Fifo,
    Socket,
}

/// The fields of one `ls -lan --full-time` line, name aside.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Meta {
    pub kind: Kind,
    /// Permission bits with setuid, setgid and sticky (`0o4755`).
    pub mode: u32,
    /// Bytes for a file; 0 for a device, whose `ls` field is `major, minor`.
    pub size: u64,
    /// Seconds since the Unix epoch.
    pub mtime: i64,
    pub uid: String,
    pub gid: String,
}

/// One directory entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The exact name, from `find`.
    pub name: Vec<u8>,
    pub meta: Meta,
    /// What `ls` printed after `name -> `, for display only: `ls` alters a
    /// target that holds a control byte. `None` when it was not printed in
    /// a form that can be split off safely.
    pub link_target: Option<Vec<u8>>,
}

/// The result of matching `find` names to `ls` lines.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Merged {
    /// Entries in `find` order.
    pub entries: Vec<Entry>,
    /// Names whose metadata needs [`resolve_script`].
    pub unresolved: Vec<Vec<u8>>,
}

/// A parsed `ls` line: its metadata and the bytes after the UTC offset.
#[derive(Debug, PartialEq, Eq)]
struct Line<'a> {
    meta: Meta,
    rest: &'a [u8],
}

/// Splits off the next run of non-space bytes after any spaces.
fn token<'a>(s: &mut &'a [u8]) -> Option<&'a [u8]> {
    let start = s.iter().position(|&b| b != b' ')?;
    let t = &s[start..];
    let end = t.iter().position(|&b| b == b' ').unwrap_or(t.len());
    let (tok, after) = t.split_at(end);
    *s = after;
    Some(tok)
}

fn ascii(t: &[u8]) -> Option<&str> {
    std::str::from_utf8(t).ok().filter(|s| s.is_ascii())
}

/// `rwsr-x--T` and the like (the nine letters after the type) to mode bits.
fn mode_bits(perm: &[u8]) -> Option<u32> {
    let perm: &[u8; 9] = perm.try_into().ok()?;
    let mut mode = 0;
    for (i, &c) in perm.iter().enumerate() {
        let bit = 1 << (8 - i);
        // The execute letter of each triplet also carries setuid, setgid or
        // sticky: lower case with the execute bit, upper case without.
        let special = match i {
            2 => Some((0o4000, b's', b'S')),
            5 => Some((0o2000, b's', b'S')),
            8 => Some((0o1000, b't', b'T')),
            _ => None,
        };
        let plain = b"rwx"[i % 3];
        mode |= match (c, special) {
            (b'-', _) => 0,
            (c, _) if c == plain => bit,
            (c, Some((s, on, _))) if c == on => bit | s,
            (c, Some((s, _, off))) if c == off => s,
            _ => return None,
        };
    }
    Some(mode)
}

/// One line of `ls -lan --full-time`; `None` for anything else (`total`, a
/// fragment of a split name, a malformed line).
fn parse_line(line: &[u8]) -> Option<Line<'_>> {
    let mut s = line;
    let mode_tok = token(&mut s)?;
    // GNU appends `.` (SELinux context) or `+` (ACL) to the ten letters.
    let mode_tok = match mode_tok.len() {
        10 => mode_tok,
        11 if matches!(mode_tok[10], b'.' | b'+' | b'@') => &mode_tok[..10],
        _ => return None,
    };
    let kind = match mode_tok[0] {
        b'-' => Kind::File,
        b'd' => Kind::Dir,
        b'l' => Kind::Symlink,
        b'c' => Kind::CharDevice,
        b'b' => Kind::BlockDevice,
        b'p' => Kind::Fifo,
        b's' => Kind::Socket,
        _ => return None,
    };
    let mode = mode_bits(&mode_tok[1..])?;
    ascii(token(&mut s)?)?.parse::<u64>().ok()?; // link count
    let uid = ascii(token(&mut s)?)?.to_string();
    let gid = ascii(token(&mut s)?)?.to_string();
    let size_tok = ascii(token(&mut s)?)?;
    let size = if matches!(kind, Kind::CharDevice | Kind::BlockDevice) {
        // `major, minor`, or `major,minor` when the minor is wide.
        let (major, minor) = size_tok.split_once(',')?;
        major.parse::<u64>().ok()?;
        if minor.is_empty() {
            ascii(token(&mut s)?)?.parse::<u64>().ok()?;
        } else {
            minor.parse::<u64>().ok()?;
        }
        0
    } else {
        size_tok.parse::<u64>().ok()?
    };
    let date = ascii(token(&mut s)?)?;
    let time = ascii(token(&mut s)?)?;
    let offset = ascii(token(&mut s)?)?;
    let stamp = format!("{date} {time} {offset}");
    let mtime = chrono::DateTime::parse_from_str(&stamp, "%Y-%m-%d %H:%M:%S%.f %z")
        .ok()?
        .timestamp();
    // Exactly one space separates the offset from the name, which may itself
    // begin with spaces.
    let rest = s.strip_prefix(b" ")?;
    if rest.is_empty() {
        return None;
    }
    Some(Line {
        meta: Meta {
            kind,
            mode,
            size,
            mtime,
            uid,
            gid,
        },
        rest,
    })
}

/// A name `ls` may print differently from its bytes: one with a control
/// byte, which BusyBox turns into `?` and GNU prints raw (a newline splits
/// the line).
fn needs_resolution(name: &[u8]) -> bool {
    name.iter().any(|&b| b < 0x20 || b == 0x7f)
}

/// The exact names of a `find <dir> -mindepth 1 -maxdepth 1 -print0`. Each
/// record is the directory, a slash and one name, and a name cannot hold a
/// slash, so the name is everything after the last one.
pub fn find_names(output: &[u8]) -> Vec<Vec<u8>> {
    output
        .split(|&b| b == 0)
        .filter_map(|rec| {
            let slash = rec.iter().rposition(|&b| b == b'/')?;
            let name = &rec[slash + 1..];
            (!name.is_empty() && name != b"." && name != b"..").then(|| name.to_vec())
        })
        .collect()
}

/// Matches every `find` name to the one `ls` line that belongs to it; see
/// the module documentation for the rules.
pub fn merge(find_output: &[u8], ls_output: &[u8]) -> Merged {
    let lines: Vec<Line<'_>> = ls_output
        .split(|&b| b == b'\n')
        .filter_map(parse_line)
        .collect();
    let mut merged = Merged::default();
    for name in find_names(find_output) {
        if needs_resolution(&name) {
            merged.unresolved.push(name);
            continue;
        }
        let mut arrow = name.clone();
        arrow.extend_from_slice(b" -> ");
        let mut matches = lines.iter().filter(|l| {
            if l.meta.kind == Kind::Symlink {
                l.rest.starts_with(&arrow)
            } else {
                l.rest == name.as_slice()
            }
        });
        match (matches.next(), matches.next()) {
            (Some(line), None) => {
                let link_target =
                    (line.meta.kind == Kind::Symlink).then(|| line.rest[arrow.len()..].to_vec());
                merged.entries.push(Entry {
                    name,
                    meta: line.meta.clone(),
                    link_target,
                });
            }
            _ => merged.unresolved.push(name),
        }
    }
    merged
}

/// One shell command that runs `ls -land --full-time` on each of `names`
/// in `dir` and ends every output with a NUL, which no `ls` output holds, so
/// [`parse_resolved`] can tell the outputs apart whatever the names contain.
pub fn resolve_script(dir: &[u8], names: &[Vec<u8>]) -> Result<Vec<u8>, ProviderError> {
    let mut script = b"for f in".to_vec();
    for name in names {
        let mut path = dir.to_vec();
        if !path.ends_with(b"/") {
            path.push(b'/');
        }
        path.extend_from_slice(name);
        script.push(b' ');
        script.extend_from_slice(&shell_quote_bytes(&path)?);
    }
    script.extend_from_slice(b"; do LC_ALL=C ls -land --full-time -- \"$f\"; printf '\\0'; done");
    Ok(script)
}

/// The entries from the output of [`resolve_script`] for the same `names`,
/// in order. A name whose output is empty (it went away in between) or
/// cannot be parsed is left out.
pub fn parse_resolved(names: &[Vec<u8>], output: &[u8]) -> Vec<Entry> {
    names
        .iter()
        .zip(output.split(|&b| b == 0))
        .filter_map(|(name, out)| {
            let first = out.split(|&b| b == b'\n').next()?;
            let line = parse_line(first)?;
            Some(Entry {
                name: name.clone(),
                meta: line.meta,
                // The line starts with the full path, and a name that needed
                // resolving is the kind `ls` alters: no target is split off.
                link_target: None,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIR: &[u8] = b"/tmp/scpfx";
    const SPOOF: &[u8] =
        b"spoof\n-rw-r--r--    1 0        0            9999 2001-01-01 00:00:00 +0000 a b";

    fn entry<'a>(m: &'a Merged, name: &[u8]) -> &'a Entry {
        m.entries
            .iter()
            .find(|e| e.name == name)
            .unwrap_or_else(|| panic!("no entry {:?}", String::from_utf8_lossy(name)))
    }

    fn sorted(mut v: Vec<Vec<u8>>) -> Vec<Vec<u8>> {
        v.sort();
        v
    }

    /// What every server must list the same way, whichever `ls` it runs
    /// (`a b` only where no forged line made it ambiguous).
    fn assert_common(m: &Merged) {
        if let Some(ab) = m.entries.iter().find(|e| e.name == b"a b") {
            assert_eq!((ab.meta.kind, ab.meta.size), (Kind::File, 5));
        }
        let ten = entry(m, b"ten");
        assert_eq!(ten.meta.size, 10);
        assert_eq!(ten.meta.mode, 0o4755);
        assert_eq!(ten.meta.mtime, 1_700_000_000);
        assert_eq!(entry(m, b"sub").meta.kind, Kind::Dir);
        assert_eq!(entry(m, b"sub").meta.mode, 0o750);
        assert_eq!(entry(m, b"dir").meta.kind, Kind::Dir);
        let link = entry(m, b"link");
        assert_eq!(link.meta.kind, Kind::Symlink);
        assert_eq!(link.link_target.as_deref(), Some(&b"a b"[..]));
        let dangling = entry(m, b"dangling");
        assert_eq!(dangling.link_target.as_deref(), Some(&b"/nonexistent"[..]));
        // A file whose name holds the arrow is a file, with its whole name.
        let arrow = entry(m, b"x -> y");
        assert_eq!(
            (arrow.meta.kind, arrow.link_target.clone()),
            (Kind::File, None)
        );
        for name in [
            &b"-dash"[..],
            b".hidden",
            b"it's",
            "café".as_bytes(),
            b"raw\xffbyte",
        ] {
            assert_eq!(entry(m, name).meta.kind, Kind::File);
        }
        // `.` and `..` are never entries.
        assert!(m.entries.iter().all(|e| e.name != b"." && e.name != b".."));
        // Nothing is listed under a name `find` did not give.
        assert!(m
            .entries
            .iter()
            .all(|e| !e.name.is_empty() && !e.name.contains(&b'?')));
    }

    #[test]
    fn busybox_listing_merges_and_leaves_control_byte_names_to_resolve() {
        let m = merge(
            include_bytes!("../fixtures/scp/busybox-find.txt"),
            include_bytes!("../fixtures/scp/busybox-ls-lan.txt"),
        );
        assert_common(&m);
        assert_eq!(entry(&m, b"a b").meta.uid, "0");
        assert_eq!(
            sorted(m.unresolved.clone()),
            sorted(vec![
                b"new\nline".to_vec(),
                b"tab\tx".to_vec(),
                SPOOF.to_vec()
            ])
        );
        assert_eq!(m.entries.len() + m.unresolved.len(), 15);
        assert!(m.entries.iter().any(|e| e.name == b"a b"));
    }

    /// GNU and uutils print the newline raw: the crafted name prints a
    /// second `a b` line with a forged size, and `a b` goes to resolution
    /// instead of taking either line. Everything else lists as on BusyBox.
    #[test]
    fn a_split_line_never_lends_its_fields_to_another_entry() {
        for (find, ls) in [
            (
                &include_bytes!("../fixtures/scp/gnu-find.txt")[..],
                &include_bytes!("../fixtures/scp/gnu-ls-lan.txt")[..],
            ),
            (
                &include_bytes!("../fixtures/scp/uutils-find.txt")[..],
                &include_bytes!("../fixtures/scp/uutils-ls-lan.txt")[..],
            ),
        ] {
            let m = merge(find, ls);
            assert!(m.entries.iter().all(|e| e.meta.size != 9999), "{m:?}");
            assert_eq!(
                sorted(m.unresolved.clone()),
                sorted(vec![
                    b"a b".to_vec(),
                    b"new\nline".to_vec(),
                    b"tab\tx".to_vec(),
                    SPOOF.to_vec(),
                ])
            );
            assert_common(&m);
            assert_eq!(m.entries.len() + m.unresolved.len(), 15);
        }
    }

    #[test]
    fn a_resolved_name_takes_the_fields_of_its_first_line_only() {
        // BusyBox prints `new?line` on one line; GNU and uutils split it.
        for out in [
            &include_bytes!("../fixtures/scp/busybox-ls-land-newline.txt")[..],
            &include_bytes!("../fixtures/scp/gnu-ls-land-newline.txt")[..],
            &include_bytes!("../fixtures/scp/uutils-ls-land-newline.txt")[..],
        ] {
            let mut chunk = out.to_vec();
            chunk.push(0);
            let got = parse_resolved(&[b"new\nline".to_vec()], &chunk);
            assert_eq!(got.len(), 1);
            assert_eq!(got[0].name, b"new\nline");
            assert_eq!((got[0].meta.kind, got[0].meta.size), (Kind::File, 0));
            assert_eq!(got[0].link_target, None);
        }
    }

    #[test]
    fn resolved_outputs_are_told_apart_by_the_nul_after_each() {
        let misc = include_bytes!("../fixtures/scp/busybox-ls-land-misc.txt");
        let line = |suffix: &[u8]| -> Vec<u8> {
            misc.split(|&b| b == b'\n')
                .find(|l| l.ends_with(suffix))
                .expect("fixture line")
                .to_vec()
        };
        // `gone` went away before the script ran: its output is empty.
        let names = vec![b"gone".to_vec(), b"link".to_vec(), b"ten".to_vec()];
        let mut out = b"\0".to_vec();
        for suffix in [&b"/link -> a b"[..], b"/ten"] {
            out.extend_from_slice(&line(suffix));
            out.extend_from_slice(b"\n\0");
        }
        let got = parse_resolved(&names, &out);
        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!(
            (got[0].name.as_slice(), got[0].meta.kind),
            (&b"link"[..], Kind::Symlink)
        );
        assert_eq!(got[1].name, b"ten");
        assert_eq!((got[1].meta.size, got[1].meta.mode), (10, 0o4755));
    }

    #[test]
    fn device_lines_parse_with_major_and_minor_in_place_of_a_size() {
        for out in [
            &include_bytes!("../fixtures/scp/busybox-ls-land-dev.txt")[..],
            &include_bytes!("../fixtures/scp/gnu-ls-land-dev.txt")[..],
            &include_bytes!("../fixtures/scp/uutils-ls-land-dev.txt")[..],
        ] {
            let lines: Vec<_> = out
                .split(|&b| b == b'\n')
                .filter(|l| !l.is_empty())
                .map(|l| parse_line(l).expect("device line"))
                .collect();
            assert_eq!(lines.len(), 2);
            assert!(lines
                .iter()
                .all(|l| l.meta.kind == Kind::CharDevice && l.meta.size == 0));
            assert_eq!(lines[0].rest, b"/dev/null");
        }
        let wide = parse_line(b"brw-rw---- 1 0 6 259,131072 2026-10-31 09:30:41 +0000 nvme0n1p1")
            .expect("wide minor");
        assert_eq!(wide.meta.kind, Kind::BlockDevice);
    }

    #[test]
    fn lines_that_are_not_entries_are_refused() {
        for bad in [
            &b"total 24"[..],
            b"line",
            b"",
            b"-rw-r--r-- 1 0 0 5 2026-10-31 09:13:23 +0000",
            b"-rw-r--r-- 1 0 0 5 2026-10-31 09:13:23 +0000 ",
            b"-rw-r--r-- 1 0 0 five 2026-10-31 09:13:23 +0000 a",
            b"-rw-r--r-- 1 0 0 5 Oct 31 09:13 a",
            b"?rw-r--r-- 1 0 0 5 2026-10-31 09:13:23 +0000 a",
            b"-rwq------ 1 0 0 5 2026-10-31 09:13:23 +0000 a",
        ] {
            assert_eq!(parse_line(bad), None, "{:?}", String::from_utf8_lossy(bad));
        }
    }

    #[test]
    fn a_name_keeps_its_leading_spaces_and_the_time_its_offset() {
        let l = parse_line(b"-rw-r--r-- 1 0 0 5 2026-10-31 10:13:23.5 +0100   lead").unwrap();
        assert_eq!(l.rest, b"  lead");
        assert_eq!(l.meta.mtime, 1_793_438_003);
    }

    #[test]
    fn mode_letters_map_to_bits() {
        assert_eq!(mode_bits(b"rwxr-xr-x"), Some(0o755));
        assert_eq!(mode_bits(b"rwsr-sr-t"), Some(0o7755));
        assert_eq!(mode_bits(b"rwSr-Sr-T"), Some(0o7644));
        assert_eq!(mode_bits(b"rwtr-xr-x"), None);
        assert_eq!(mode_bits(b"rwxr-xr-s"), None);
        assert_eq!(mode_bits(b"rwxr-x"), None);
    }

    #[test]
    fn find_names_are_the_part_after_the_last_slash() {
        assert_eq!(
            find_names(b"/tmp/t/a\0/tmp/t//b c\0/c\0/tmp/t/new\nline\0"),
            vec![
                b"a".to_vec(),
                b"b c".to_vec(),
                b"c".to_vec(),
                b"new\nline".to_vec()
            ]
        );
        assert!(find_names(b"").is_empty());
    }

    #[test]
    fn the_resolve_script_quotes_every_path() {
        let script = resolve_script(DIR, &[b"it's".to_vec(), b"new\nline".to_vec()]).unwrap();
        assert_eq!(
            script,
            b"for f in '/tmp/scpfx/it'\\''s' '/tmp/scpfx/new\nline'; do LC_ALL=C ls -land --full-time -- \"$f\"; printf '\\0'; done"
                .to_vec()
        );
        assert!(resolve_script(b"/", &[b"a\0b".to_vec()]).is_err());
        assert!(resolve_script(b"/", &[b"a".to_vec()])
            .unwrap()
            .starts_with(b"for f in '/a';"));
    }

    /// The two listing commands and the resolve script on this machine's own
    /// shell and `ls`, against names built to break them.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_commands_list_a_hostile_folder_exactly_on_a_real_shell() {
        use std::os::unix::ffi::OsStrExt;
        use std::process::Command;
        let dir = std::env::temp_dir().join(format!("scp-listing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let names: Vec<&[u8]> = vec![
            b"a b",
            b"it's",
            b"-dash",
            b"new\nline",
            b"tab\tx",
            b"x -> y",
            b" lead",
            b"$(touch pwned)",
            SPOOF,
        ];
        for (i, name) in names.iter().enumerate() {
            std::fs::write(dir.join(std::ffi::OsStr::from_bytes(name)), vec![b'x'; i]).unwrap();
        }
        let dir_b = dir.as_os_str().as_bytes().to_vec();
        let q = shell_quote_bytes(&dir_b).unwrap();
        let sh = |script: &[u8]| {
            Command::new("sh")
                .current_dir(&dir)
                .arg("-c")
                .arg(std::ffi::OsStr::from_bytes(script))
                .output()
                .unwrap()
        };
        let mut ls_cmd = b"LC_ALL=C ls -lan --full-time -- ".to_vec();
        ls_cmd.extend_from_slice(&q);
        let ls = sh(&ls_cmd);
        // GNU, BusyBox and uutils all take `--full-time`: on Linux a failure
        // here is a failure, not a skip.
        assert!(
            ls.status.success(),
            "{}",
            String::from_utf8_lossy(&ls.stderr)
        );
        let mut find_cmd = b"find ".to_vec();
        find_cmd.extend_from_slice(&q);
        find_cmd.extend_from_slice(b" -mindepth 1 -maxdepth 1 -print0");
        let find = sh(&find_cmd);
        let mut merged = merge(&find.stdout, &ls.stdout);
        let script = resolve_script(&dir_b, &merged.unresolved).unwrap();
        let resolved = sh(&script);
        merged
            .entries
            .extend(parse_resolved(&merged.unresolved, &resolved.stdout));
        assert!(!dir.join("pwned").exists(), "a name ran as a command");
        assert_eq!(merged.entries.len(), names.len(), "{merged:?}");
        for (i, name) in names.iter().enumerate() {
            let e = merged.entries.iter().find(|e| e.name == *name).unwrap();
            assert_eq!(e.meta.size, i as u64, "{:?}", String::from_utf8_lossy(name));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
