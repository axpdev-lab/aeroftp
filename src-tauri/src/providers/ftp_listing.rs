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

/// Read a size field, saying whether it was readable.
///
/// Three parsers answered this question three ways: two produced `0` without
/// saying so, and the DOS one rejected the whole row. `0` is not neutral, it
/// drives skip and overwrite decisions and cannot be told from an empty file;
/// and a rejected row is worse still in the context that matters, because a
/// file that vanishes from a SOURCE listing makes its counterpart on the
/// destination an orphan, and orphans get deleted.
///
/// So the entry is kept with `0` and the fact is reported, and all three agree.
/// `RemoteEntry.size` is a `u64` and still cannot say "unknown"; closing that
/// is a change to the shared type and is queued separately.
fn read_size(token: &str) -> (u64, bool) {
    match token.parse() {
        Ok(value) => (value, true),
        Err(_) => (0, false),
    }
}

/// Marker put on an entry whose size could not be read.
///
/// It travels with the entry rather than through a channel beside it, so a
/// consumer that cares can see it and the listing can count it without parsing
/// anything twice. `RemoteEntry.size` is a `u64` and cannot hold "unknown"
/// itself; until it can, this is where the fact lives instead of nowhere.
pub(crate) const SIZE_UNREADABLE: &str = "ftp.size_unreadable";

fn mark_size_unreadable(entry: &mut RemoteEntry, size_read: bool) {
    if !size_read {
        entry
            .metadata
            .insert(SIZE_UNREADABLE.to_string(), "1".to_string());
    }
}

/// Why a line produced no entry.
///
/// The distinction between the last two is the whole point. A server's listing
/// carries lines that are not entries and never were (the `total 12` header of
/// `ls`, the `path:` headers of a recursive listing, the `.` and `..` rows),
/// and it can also carry a row we simply could not read. Today both vanish
/// identically: `parse_listing` returns `None` and both callers drop it, so a
/// server whose dialect we do not understand produces a short listing and no
/// signal at all. That is the same shape as a missing directory listing as an
/// empty one, one level down.
///
/// Reporting both would be as bad in the other direction: every `total 12` and
/// every `.` would become a warning, and a recursive listing would turn into a
/// wall of noise about lines that are not entries. So there are two outcomes,
/// and the classification lives here rather than in each caller, because
/// leaving each caller to filter for itself is exactly what the shared module
/// was extracted to stop.
pub(crate) enum LineProblem {
    /// Not a listing row: skipped, not counted, not reported.
    NotARow,
    /// It had the shape of a row and could not be read: counted and reported.
    Unreadable,
}

/// Read one line, saying which of the three it was.
pub(crate) fn read_listing_line(line: &str, base_path: &str) -> Result<RemoteEntry, LineProblem> {
    if let Some(entry) = parse_listing(line, base_path) {
        return Ok(entry);
    }
    if is_not_a_listing_row(line) {
        Err(LineProblem::NotARow)
    } else {
        Err(LineProblem::Unreadable)
    }
}

/// Lines a listing carries that are not entries.
///
/// Kept beside the parsers, not in the callers: the legacy caller had three
/// hand-rolled versions of these checks and the provider had none, which is how
/// the two sides came to disagree about what a listing contains.
fn is_not_a_listing_row(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return true;
    }
    // `ls` writes a block-count header before the entries.
    if trimmed.starts_with("total ") || trimmed.starts_with("Total ") {
        return true;
    }
    // A recursive listing announces each directory as `/path/to/dir:`.
    //
    // The absence of whitespace alone is not the test, and relying on it was a
    // false positive with teeth: `/pub/my files:` is an ordinary header and was
    // being counted as a row we failed to read. On its own that was invisible,
    // because an unread count was only logged; now that an all-unreadable
    // listing is refused, an EMPTY directory in a recursive listing would come
    // back as a parse error, since its header is the only line there is. The
    // machinery built to notice unreadable rows would have been the thing that
    // broke reading them.
    //
    // An absolute header is accepted with spaces and all, because no listing
    // row can be confused with it: a Unix row opens with a mode string and a
    // DOS row with a date, and neither can start with a separator. The
    // whitespace-free form stays for relative headers, which have nothing else
    // to distinguish them.
    if trimmed.ends_with(':')
        && (trimmed.starts_with('/') || !trimmed.contains(char::is_whitespace))
    {
        return true;
    }
    // The `.` and `..` rows that `LIST -a` adds. They parse as far as the name
    // and are then dropped inside the parsers, so the name has to be recovered
    // the same way the parser would have built it.
    let first = trimmed.split_whitespace().next().unwrap_or("");
    let name_field = if is_dos_date(first) { 3 } else { 8 };
    matches!(field_tail(trimmed, name_field), Some("." | ".."))
}

/// Read a whole listing, and say what could not be read.
///
/// Both callers used `filter_map`, which drops a `None` with no trace, so a
/// server whose rows we cannot parse produced a short listing and no signal.
/// Returning the count and the first offending row here, rather than leaving
/// each caller to notice for itself, is the same reason the parsers were
/// shared: two callers noticing separately is two callers noticing
/// differently.
pub(crate) struct Listing {
    pub entries: Vec<RemoteEntry>,
    /// Rows that looked like entries and could not be read. Lines that were
    /// never entries (headers, `.`, `..`) are not counted.
    pub unreadable: usize,
    /// The first of them, verbatim, so a report can name it.
    pub first_unreadable: Option<String>,
    /// Entries kept with a size of 0 because the field could not be read. They
    /// are NOT dropped: in a delete-enabled sync a file missing from the source
    /// listing makes its counterpart an orphan, so absence is the dangerous
    /// side here and a wrong zero is the lesser one. Counted so the zero is not
    /// silent.
    pub unreadable_sizes: usize,
}

pub(crate) fn read_listing<'a>(
    lines: impl IntoIterator<Item = &'a str>,
    base_path: &str,
) -> Listing {
    let mut out = Listing {
        entries: Vec::new(),
        unreadable: 0,
        first_unreadable: None,
        unreadable_sizes: 0,
    };
    for line in lines {
        match read_listing_line(line, base_path) {
            Ok(entry) => {
                if entry.metadata.contains_key(SIZE_UNREADABLE) {
                    out.unreadable_sizes += 1;
                }
                out.entries.push(entry);
            }
            Err(LineProblem::NotARow) => {}
            Err(LineProblem::Unreadable) => {
                out.unreadable += 1;
                if out.first_unreadable.is_none() {
                    out.first_unreadable = Some(line.to_string());
                }
            }
        }
    }
    out
}

/// One line per listing, never one per row.
///
/// A recursive walk over a server we cannot parse would otherwise emit a
/// warning for every row of every directory, and a log nobody can read is the
/// same as no log.
pub(crate) fn warn_unreadable_rows(listing: &Listing, context: &str) {
    if let (n, Some(first)) = (listing.unreadable, listing.first_unreadable.as_deref()) {
        if n > 0 {
            tracing::warn!("{context}: {n} listing row(s) could not be parsed; first was: {first}");
        }
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
    let (size, size_read) = read_size(parts[4]);

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

    let mut entry = RemoteEntry {
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
    };
    mark_size_unreadable(&mut entry, size_read);
    Some(entry)
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
    let mut size_read = true;
    let size: u64 = if is_dir {
        0
    } else {
        // Until the dispatcher landed, this rejection was the only thing
        // keeping Unix rows out of the DOS parser: a `.` row from a server
        // rendering numeric uids has a numeric `parts[2]` and was accepted as a
        // bogus DOS file, which recursive delete then tried to DELE. The
        // dispatch now decides on `is_dos_date`, so a Unix row cannot arrive
        // here and the strict check is no longer holding that door shut.
        let (value, read) = read_size(parts[2]);
        size_read = read;
        value
    };
    let name = field_tail(line, 3)?.to_string();

    // Skip . and .. entries
    if name == "." || name == ".." {
        return None;
    }

    let path = join_remote_path(base_path, &name);

    let modified = Some(format!("{} {}", parts[0], parts[1]));

    let mut entry = RemoteEntry {
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
    };
    mark_size_unreadable(&mut entry, size_read);
    Some(entry)
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
    let mut size_read = true;
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
                let (parsed, read) = read_size(value);
                size = parsed;
                size_read = read;
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

    let mut entry = RemoteEntry {
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
    };
    mark_size_unreadable(&mut entry, size_read);
    Some(entry)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The two outcomes are the whole point of D3(b), and they are what makes
    /// removing the caller-side filters safe. Without the distinction, every
    /// `total 12` and every `.` would be reported as an unreadable row and a
    /// recursive listing would become a wall of warnings about lines that were
    /// never entries.
    /// A listing where nothing could be read is distinguishable from an empty
    /// one, which is what lets the caller refuse to call it empty.
    ///
    /// No real server has been observed producing this: the lab servers parse
    /// cleanly, so the condition is constructed here rather than waited for.
    /// Waiting for it would be waiting for something that by construction does
    /// not arrive.
    #[test]
    fn a_listing_read_by_nobody_is_not_an_empty_listing() {
        // Rows that look like entries and cannot be read: eight fields, so the
        // Unix parser tries and fails rather than skipping them as headers.
        let unreadable = [
            "-rw-r--r-- 1 user group 123 Jan 20 10:00",
            "drwxr-xr-x 2 user group 4096 Jan 20 10:00",
        ];
        let nothing_read = read_listing(unreadable, "/srv");
        assert!(nothing_read.entries.is_empty());
        assert_eq!(
            nothing_read.unreadable, 2,
            "the rows have to be COUNTED, not just dropped: the count is the \
             only thing that tells this apart from an empty directory"
        );

        // A genuinely empty directory: the two rows `LIST -a` always adds and
        // nothing else. These are not entries and must not be counted as
        // unreadable, or every empty directory would look like a parse failure.
        let really_empty = read_listing(
            [
                "drwxr-xr-x    2 user     group        4096 Jan 20 10:00 .",
                "drwxr-xr-x    2 user     group        4096 Jan 20 10:00 ..",
                "total 8",
            ],
            "/srv",
        );
        assert!(really_empty.entries.is_empty());
        assert_eq!(
            really_empty.unreadable, 0,
            "an empty directory must not be mistaken for an unreadable one: \
             that would turn every empty folder into an error"
        );

        // And the partial case, which the caller deliberately does NOT refuse:
        // one row read, one not. It is recorded here so the untested half is
        // visible rather than absent.
        let partial = read_listing(
            [
                "-rw-r--r--    1 user     group         123 Jan 20 10:00 real.txt",
                "-rw-r--r-- 1 user group 123 Jan 20 10:00",
            ],
            "/srv",
        );
        assert_eq!(partial.entries.len(), 1);
        assert_eq!(partial.unreadable, 1);
    }

    /// A recursive header whose path contains spaces is still a header.
    ///
    /// It was classified as an unreadable row because the only test was the
    /// absence of whitespace. Harmless while unread rows were merely logged,
    /// and harmful the moment an all-unreadable listing became an error: the
    /// header of an EMPTY directory is the only line in it, so a legitimately
    /// empty folder would have been reported as a listing we could not parse.
    ///
    /// The rows below the divider are the ones that must NOT be swept up by the
    /// widened rule, because a rule written to recognise one thing catching
    /// another that looks like it from where the rule sits is the shape this
    /// branch has already hit twice.
    #[test]
    fn a_recursive_header_with_spaces_is_not_an_unreadable_row() {
        for header in [
            "/pub/my files:",
            "/srv/data/quarterly reports 2026:",
            "/pub:",
            "subdir:",
        ] {
            let listing = read_listing([header], "/");
            assert_eq!(
                listing.unreadable, 0,
                "{header:?} is a recursive header, not a row we failed to read"
            );
            assert!(listing.entries.is_empty(), "{header:?} is not an entry");
        }

        // ---- and what the rule must still refuse to swallow ----
        // A row that really is unreadable and happens to end in a colon. It
        // has EIGHT fields, not nine: the first version of this line had nine
        // and the Unix parser read it correctly, so the assertion failed and
        // the case proved nothing about the header rule. The count is the
        // point, not the colon.
        let short = read_listing(["-rw-r--r-- 1 user group 123 Jan 20 oops:"], "/");
        assert_eq!(
            short.unreadable, 1,
            "a short row is unreadable whatever its last character"
        );
        // A perfectly good entry whose NAME ends in a colon stays an entry.
        let named = read_listing(
            ["-rw-r--r--    1 user     group         123 Jan 20 10:00 notes:"],
            "/",
        );
        assert_eq!(named.entries.len(), 1, "a file called `notes:` is a file");
        assert_eq!(named.unreadable, 0);
    }

    #[test]
    fn lines_that_were_never_entries_are_not_reported_as_unreadable() {
        let lines = [
            "total 12",
            "Total 12",
            "",
            "/pub/data:",
            "drwxr-xr-x    2 user     group        4096 Jan 20 10:00 .",
            "drwxr-xr-x    2 user     group        4096 Jan 20 10:00 ..",
            "-rw-r--r--    1 user     group         123 Jan 20 10:00 real.txt",
        ];
        let listing = read_listing(lines, "/");
        assert_eq!(listing.entries.len(), 1, "only one of these is an entry");
        assert_eq!(
            listing.unreadable, 0,
            "none of the others is an unreadable row: they are not rows at all"
        );
        assert!(listing.first_unreadable.is_none());
    }

    /// And a row that did look like one and could not be read is counted and
    /// kept, so the next server we cannot parse arrives as a report with the
    /// offending line in hand instead of a listing that is quietly short.
    ///
    /// What this can and cannot see is worth stating. The Unix parser accepts
    /// any row of nine or more whitespace-separated tokens whose first token is
    /// not a DOS date, without checking that the fields look like a mode, a
    /// link count or a size. So a line of nine words becomes an entry rather
    /// than an unreadable row, and this report never sees it. What it does see
    /// is the short rows, which is precisely the class that motivated it: a
    /// server omitting a column produces eight tokens and vanishes today.
    /// Making the Unix parser validate its fields is a behaviour change of its
    /// own and is not in this one.
    #[test]
    fn a_row_that_looked_like_one_and_failed_is_counted_and_named() {
        let lines = [
            "-rw-r--r--    1 user     group         123 Jan 20 10:00 real.txt",
            "-rw-r--r-- 1 user group 123 Jan 20 10:00",
            "?????? nonsense",
        ];
        let listing = read_listing(lines, "/");
        assert_eq!(listing.entries.len(), 1);
        assert_eq!(
            listing.unreadable, 2,
            "a row one column short is exactly the case this reports"
        );
        assert_eq!(
            listing.first_unreadable.as_deref(),
            Some("-rw-r--r-- 1 user group 123 Jan 20 10:00"),
            "the report has to name a line someone can look at"
        );
    }

    /// The three parsers used to answer the same question three ways: two
    /// produced a silent `0`, and the DOS one rejected the row outright. They
    /// agree now, and the direction is deliberate.
    ///
    /// Keeping the entry is the safer half, not the lazier one. In a sync with
    /// `--delete`, a file that vanishes from the SOURCE listing makes its
    /// counterpart on the destination an orphan, and orphans are deleted. So a
    /// wrong `0` costs a bad skip, while a dropped row costs a deletion.
    #[test]
    fn an_unreadable_size_keeps_the_entry_and_says_so() {
        let cases = [
            "-rw-r--r--    1 user     group        ???? Jan 20 10:00 odd.txt",
            "01-23-24  10:30AM  ????  odd.txt",
        ];
        for line in cases {
            let entry = parse_listing(line, "/")
                .unwrap_or_else(|| panic!("the row must be kept, not dropped: {line}"));
            assert_eq!(entry.size, 0);
            assert!(
                entry.metadata.contains_key(SIZE_UNREADABLE),
                "a zero that is not a zero has to be marked: {line}"
            );
        }

        // A size that reads fine carries no marker, or the marker would mean
        // nothing.
        let good = parse_listing("01-23-24  10:30AM  12345  file.txt", "/").expect("a DOS row");
        assert_eq!(good.size, 12345);
        assert!(!good.metadata.contains_key(SIZE_UNREADABLE));
    }

    /// And the listing counts them, so the zeros are visible without anyone
    /// inspecting an entry.
    #[test]
    fn the_listing_counts_the_sizes_it_could_not_read() {
        let listing = read_listing(
            [
                "-rw-r--r--    1 user     group         123 Jan 20 10:00 good.txt",
                "-rw-r--r--    1 user     group        ???? Jan 20 10:00 odd.txt",
            ],
            "/",
        );
        assert_eq!(listing.entries.len(), 2, "both are kept");
        assert_eq!(listing.unreadable_sizes, 1);
        assert_eq!(listing.unreadable, 0, "neither row failed to parse");
    }

    /// The dispatcher decides on the shape of the first token, so a DOS row
    /// long enough to reach nine tokens no longer becomes a Unix entry with the
    /// date in its permissions.
    #[test]
    fn a_long_dos_row_is_not_taken_by_the_unix_parser() {
        let entry = parse_listing("01-23-24 10:30AM 12345 a b c d e f", "/").expect("a DOS row");
        assert_eq!(entry.name, "a b c d e f");
        assert_eq!(entry.size, 12345);
        assert!(entry.permissions.is_none(), "a DOS row carries no mode");
    }

    /// The name is sliced from the line, so runs of spaces survive. A name that
    /// comes back altered addresses a path that does not exist.
    #[test]
    fn runs_of_spaces_in_a_name_survive() {
        let unix = parse_listing(
            "-rw-r--r--    1 user     group         123 Jan 20 10:00 a  b.txt",
            "/",
        )
        .expect("a Unix row");
        assert_eq!(unix.name, "a  b.txt");

        let dos = parse_listing("01-23-24  10:30AM  12345  my  file.txt", "/").expect("a DOS row");
        assert_eq!(dos.name, "my  file.txt");
    }

    /// Only CR and LF are trimmed: a trailing space is a legal filename, and
    /// trimming it would be the same defect from the other end.
    #[test]
    fn a_trailing_space_in_a_name_is_kept_but_a_line_ending_is_not() {
        let entry = parse_listing(
            "-rw-r--r--    1 user     group         123 Jan 20 10:00 trailing \r\n",
            "/",
        )
        .expect("a Unix row");
        assert_eq!(entry.name, "trailing ");
    }
}
