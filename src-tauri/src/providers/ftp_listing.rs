//! The FTP listing parser, shared by both FTP implementations.
//!
//! Two of them exist: `providers::ftp::FtpProvider` and the legacy
//! `crate::ftp::FtpManager`, which the AI agent fallback, `gui_tools`,
//! `cloud_service`, the transfer-queue scan and the no-protocol branch of the
//! file panel all still reach. Each carried its own copy of this parser and the
//! copies had drifted apart, so a defect fixed in one stayed open in the other.
//!
//! Everything here is pure: a line of text and a base path in, an entry out.
//! No I/O, no connection, no `self`. That is not tidiness, it is the only way
//! to test the cases a live server cannot be made to produce on demand: DOS
//! listings, rows with missing columns, servers without MLSD.
//!
//! The three formats are one question with three grammars, so they live
//! together: `LIST` in its Unix and DOS dialects, and RFC 3659 `MLSD`.

use super::types::RemoteEntry;

/// Byte offset where whitespace-separated field `n` (0-based) begins.
///
/// The name has to be sliced out of the original line, not rebuilt from tokens.
/// Rebuilding joins with a single space, so a name containing two spaces comes
/// back with one, and every later operation addressed by that name (stat,
/// download, delete, rename) targets a path that does not exist. The bytes were
/// on the wire; only the reassembly was losing them.
fn field_start(line: &str, n: usize) -> Option<usize> {
    let mut seen = 0usize;
    let mut in_field = false;
    for (offset, ch) in line.char_indices() {
        if ch.is_whitespace() {
            in_field = false;
        } else if !in_field {
            if seen == n {
                return Some(offset);
            }
            in_field = true;
            seen += 1;
        }
    }
    None
}

/// The tail of the line from field `n` onward, verbatim.
///
/// Only a trailing CR or LF is trimmed. A trailing space is a legal, if
/// hostile, filename, and trimming it would reintroduce the same class of
/// defect from the other end.
fn field_tail(line: &str, n: usize) -> Option<&str> {
    let start = field_start(line, n)?;
    Some(line[start..].trim_end_matches(['\r', '\n']))
}

pub(crate) fn parse_listing(line: &str, base_path: &str) -> Option<RemoteEntry> {
    // Dispatch on the shape of the first token, and try ONE parser.
    //
    // Trying Unix and falling back to DOS made the FAILURE of the first parser
    // the criterion for choosing the second, and a DOS row whose name has
    // enough words to reach nine whitespace tokens satisfies the Unix parser:
    // the date lands in the permissions field, the size in the owner field, and
    // the caller receives a confident entry describing a file that does not
    // exist, while the real one is nowhere in the listing. A wrong entry is
    // worse than a dropped one, because an operation acts on it.
    //
    // `is_dos_date` already existed for this and could only ever guard the
    // inside of the DOS parser, where it arrived too late to decide anything.
    // A Unix permissions field cannot satisfy it, so no Unix row changes route.
    let first = line.split_whitespace().next()?;
    if is_dos_date(first) {
        parse_dos_listing(line, base_path)
    } else {
        parse_unix_listing(line, base_path)
    }
}

/// Shape check for the leading token of a DOS-style listing row:
/// `MM-DD-YY` or `MM-DD-YYYY`, all digits.
pub(crate) fn is_dos_date(token: &str) -> bool {
    let mut fields = token.split('-');
    let valid = match (fields.next(), fields.next(), fields.next()) {
        (Some(month), Some(day), Some(year)) => {
            [month, day].iter().all(|f| f.len() == 2)
                && (year.len() == 2 || year.len() == 4)
                && [month, day, year]
                    .iter()
                    .all(|f| f.bytes().all(|b| b.is_ascii_digit()))
        }
        _ => false,
    };
    valid && fields.next().is_none()
}

pub(crate) fn join_remote_path(base_path: &str, name: &str) -> String {
    if name.starts_with('/') {
        return name.to_string();
    }

    let trimmed_base = base_path.trim_end_matches('/');
    if trimmed_base.is_empty() {
        format!("/{}", name.trim_start_matches('/'))
    } else {
        format!("{}/{}", trimmed_base, name.trim_start_matches('/'))
    }
}

pub(crate) fn normalize_mlsd_name(name: &str) -> String {
    let trimmed = name.trim_end_matches('/');
    std::path::Path::new(trimmed)
        .file_name()
        .map(|value| value.to_string_lossy().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| name.to_string())
}

/// Parse Unix-style listing (ls -l format)
pub(crate) fn parse_unix_listing(line: &str, base_path: &str) -> Option<RemoteEntry> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 9 {
        return None;
    }

    let permissions = parts[0];
    let is_dir = permissions.starts_with('d');
    let is_symlink = permissions.starts_with('l');

    // Get size (might be in different position depending on format)
    let size: u64 = parts[4].parse().unwrap_or(0);

    // Sliced from the original line rather than rejoined from tokens, so a
    // name containing runs of spaces keeps them.
    let name = field_tail(line, 8)?.to_string();

    // Handle symlinks (name -> target)
    let (actual_name, link_target) = if is_symlink && name.contains(" -> ") {
        let parts: Vec<&str> = name.splitn(2, " -> ").collect();
        (
            parts[0].to_string(),
            Some(parts.get(1).unwrap_or(&"").to_string()),
        )
    } else {
        (name, None)
    };

    // Skip . and .. entries
    if actual_name == "." || actual_name == ".." {
        return None;
    }

    let path = join_remote_path(base_path, &actual_name);

    // Parse date (parts[5..8] typically contain month day time/year)
    let modified = if parts.len() >= 8 {
        Some(format!("{} {} {}", parts[5], parts[6], parts[7]))
    } else {
        None
    };

    Some(RemoteEntry {
        name: actual_name,
        path,
        is_dir,
        size,
        modified,
        permissions: Some(permissions.to_string()),
        owner: Some(parts[2].to_string()),
        group: Some(parts[3].to_string()),
        is_symlink,
        link_target,
        mime_type: None,
        metadata: Default::default(),
    })
}

/// Parse DOS-style listing (Windows FTP servers)
pub(crate) fn parse_dos_listing(line: &str, base_path: &str) -> Option<RemoteEntry> {
    // DOS format: 01-23-24  10:30AM       <DIR>          folder_name
    // Or:         01-23-24  10:30AM           12345      file.txt

    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 4 {
        return None;
    }

    // A DOS row always opens with a `MM-DD-YY[YY]` date. Requiring the
    // shape keeps this parser from resurrecting a Unix line that
    // parse_unix_listing already rejected (e.g. the "." / ".." rows that
    // `LIST -a` adds): a Unix permissions field never looks like a date.
    if !is_dos_date(parts[0]) {
        return None;
    }

    // In the DOS format parts[2] is always either "<DIR>" or the numeric
    // size. Requiring that alone is NOT enough: on a server that renders
    // owner/group as numeric ids (vsftpd's default text_userdb_names=NO)
    // the Unix "." row `drwxr-xr-x 2 1001 1001 4096 Jul 21 09:41 .` has a
    // numeric parts[2] (the uid) and would otherwise become a bogus file
    // named "1001 4096 Jul 21 09:41 ." that recursive delete then DELEs,
    // which the server answers with `550 Delete operation failed`.
    let is_dir = parts[2] == "<DIR>";
    let size: u64 = if is_dir {
        0
    } else {
        match parts[2].parse() {
            Ok(value) => value,
            Err(_) => return None,
        }
    };
    let name = field_tail(line, 3)?.to_string();

    // Skip . and .. entries
    if name == "." || name == ".." {
        return None;
    }

    let path = join_remote_path(base_path, &name);

    let modified = Some(format!("{} {}", parts[0], parts[1]));

    Some(RemoteEntry {
        name,
        path,
        is_dir,
        size,
        modified,
        permissions: None,
        owner: None,
        group: None,
        is_symlink: false,
        link_target: None,
        mime_type: None,
        metadata: Default::default(),
    })
}

/// Parse MLSD/MLST line (RFC 3659 machine-readable format)
/// Format: "fact1=val1;fact2=val2; filename"
pub(crate) fn parse_mlsd_entry(line: &str, base_path: &str) -> Option<RemoteEntry> {
    // Split on first space after semicolons to get facts and filename
    let (facts_str, name) = line.split_once(' ')?;
    let raw_name = name.trim();
    let name = normalize_mlsd_name(raw_name);

    if name == "." || name == ".." {
        return None;
    }

    let mut is_dir = false;
    let mut is_symlink = false;
    let mut size: u64 = 0;
    let mut modified: Option<String> = None;
    let mut permissions: Option<String> = None;
    let mut owner: Option<String> = None;
    let mut group: Option<String> = None;

    for fact in facts_str.split(';') {
        let fact = fact.trim();
        if fact.is_empty() {
            continue;
        }
        let (key, value) = match fact.split_once('=') {
            Some((k, v)) => (k.to_lowercase(), v),
            None => continue,
        };

        match key.as_str() {
            "type" => {
                let v_lower = value.to_lowercase();
                is_dir = v_lower == "dir" || v_lower == "cdir" || v_lower == "pdir";
                is_symlink = v_lower == "os.unix=symlink" || v_lower == "os.unix=slink";
            }
            "size" | "sizd" => {
                size = value.parse().unwrap_or(0);
            }
            "modify" => {
                // YYYYMMDDHHMMSS[.sss] → format nicely
                modified = Some(format_mlsd_time(value));
            }
            "unix.mode" => {
                permissions = Some(value.to_string());
            }
            "unix.owner" | "unix.uid" => {
                owner = Some(value.to_string());
            }
            "unix.group" | "unix.gid" => {
                group = Some(value.to_string());
            }
            "perm"
                // MLSD perm facts (e.g. "rwcedf") - store as metadata
                if permissions.is_none() => {
                    permissions = Some(value.to_string());
                }
            _ => {}
        }
    }

    // Skip cdir/pdir (current/parent directory entries)
    if facts_str.to_lowercase().contains("type=cdir")
        || facts_str.to_lowercase().contains("type=pdir")
    {
        return None;
    }

    let path = join_remote_path(base_path, raw_name);

    Some(RemoteEntry {
        name,
        path,
        is_dir,
        size,
        modified,
        permissions,
        owner,
        group,
        is_symlink,
        link_target: None,
        mime_type: None,
        metadata: Default::default(),
    })
}

/// Format MLSD timestamp (YYYYMMDDHHMMSS) to readable form.
/// Appends 'Z' suffix because MLSD timestamps are always UTC per RFC 3659.
pub(crate) fn format_mlsd_time(ts: &str) -> String {
    if ts.len() >= 14 {
        format!(
            "{}-{}-{} {}:{}:{}Z",
            &ts[0..4],
            &ts[4..6],
            &ts[6..8],
            &ts[8..10],
            &ts[10..12],
            &ts[12..14]
        )
    } else if ts.len() >= 8 {
        format!("{}-{}-{}", &ts[0..4], &ts[4..6], &ts[6..8])
    } else {
        ts.to_string()
    }
}
