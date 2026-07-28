//! Local extended-attribute I/O for B3 (X.3 / X.4 / X.4b / X.5).
//!
//! Wire codec lives in [`crate::aerorsync::real_wire`]; this module is the
//! filesystem side: read `user.*` xattrs from a source path, sanitize a
//! peer-supplied blob before apply, and write xattrs onto a temp file
//! **before** rename (kill-9 invariant of `StreamingAtomicWriter`).
//!
//! Scope pins (owner-decided, appendix X.3–X.5):
//! - default namespace filter: `user.*` only
//! - ENOTSUP on the destination: typed warning + continue, unless
//!   `fail_on_metadata_loss` turns it into a hard error
//! - capability probe once per destination directory, not per file, and
//!   read-only: it inspects the directory, it never writes to it (R5)

#![cfg(feature = "aerorsync")]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::aerorsync::real_wire::{XattrDatum, XattrPair};

/// Only attributes whose name starts with this prefix are read or applied.
pub const USER_XATTR_PREFIX: &str = "user.";

/// Hard ceiling on how many attributes one entry may carry after sanitize.
/// Hostile peers cannot inflate destination metadata beyond this.
pub const MAX_XATTR_PAIRS: usize = 256;

/// Hard ceiling on a single attribute name (including the `user.` prefix).
pub const MAX_XATTR_NAME_LEN: usize = 255;

/// Hard ceiling on the total size of all attribute values on one entry.
pub const MAX_XATTR_TOTAL_BYTES: usize = 1024 * 1024;

/// Why a received xattr blob was refused (X.4b). These are always hard
/// errors: a malformed or hostile peer must not write to disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XattrSanitizeError {
    EmptyName,
    NameTooLong { len: usize },
    NamespaceNotAllowed { name: String },
    TooManyPairs { count: usize },
    TotalBytesTooLarge { total: usize },
    DeferredUnresolved { name: String },
}

impl std::fmt::Display for XattrSanitizeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyName => write!(f, "xattr name is empty"),
            Self::NameTooLong { len } => {
                write!(f, "xattr name length {len} exceeds {MAX_XATTR_NAME_LEN}")
            }
            Self::NamespaceNotAllowed { name } => {
                write!(
                    f,
                    "xattr name {name:?} is outside the allowed {USER_XATTR_PREFIX:?} namespace"
                )
            }
            Self::TooManyPairs { count } => {
                write!(f, "xattr pair count {count} exceeds {MAX_XATTR_PAIRS}")
            }
            Self::TotalBytesTooLarge { total } => {
                write!(
                    f,
                    "xattr total value size {total} exceeds {MAX_XATTR_TOTAL_BYTES}"
                )
            }
            Self::DeferredUnresolved { name } => {
                write!(
                    f,
                    "xattr {name:?} still carries a deferred digest; out-of-band section was not resolved"
                )
            }
        }
    }
}

/// Outcome of applying xattrs to a path (X.4 + X.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XattrApplyOutcome {
    /// Every pair was written.
    Applied { count: usize },
    /// Destination filesystem does not support xattrs (ENOTSUP). No pair
    /// was written; caller may continue the transfer with a warning.
    Unsupported { warnings: Vec<String> },
    /// Hard failure (not ENOTSUP), or ENOTSUP with fail-on-metadata-loss.
    Failed { message: String },
}

/// Sanitize peer-supplied xattrs before they touch the local filesystem.
///
/// Rejects empty names, non-`user.*` namespaces, over-long names, too many
/// pairs, oversized total payload, and unresolved deferred digests.
pub fn sanitize_xattrs_for_apply(
    pairs: &[XattrPair],
) -> Result<Vec<(String, Vec<u8>)>, XattrSanitizeError> {
    if pairs.len() > MAX_XATTR_PAIRS {
        return Err(XattrSanitizeError::TooManyPairs { count: pairs.len() });
    }
    let mut out = Vec::with_capacity(pairs.len());
    let mut total_bytes = 0usize;
    for pair in pairs {
        if pair.name.is_empty() {
            return Err(XattrSanitizeError::EmptyName);
        }
        if pair.name.len() > MAX_XATTR_NAME_LEN {
            return Err(XattrSanitizeError::NameTooLong {
                len: pair.name.len(),
            });
        }
        if !pair.name.starts_with(USER_XATTR_PREFIX) || pair.name == USER_XATTR_PREFIX {
            return Err(XattrSanitizeError::NamespaceNotAllowed {
                name: pair.name.clone(),
            });
        }
        // Names must be plain ASCII path-ish tokens; reject embedded NULs
        // or control characters that would confuse setxattr.
        if pair.name.bytes().any(|b| b < 0x20 || b == 0x7f) {
            return Err(XattrSanitizeError::NamespaceNotAllowed {
                name: pair.name.clone(),
            });
        }
        let value = match &pair.datum {
            XattrDatum::Inline(v) => v.clone(),
            XattrDatum::Deferred { .. } => {
                return Err(XattrSanitizeError::DeferredUnresolved {
                    name: pair.name.clone(),
                });
            }
        };
        total_bytes = total_bytes.saturating_add(value.len());
        if total_bytes > MAX_XATTR_TOTAL_BYTES {
            return Err(XattrSanitizeError::TotalBytesTooLarge { total: total_bytes });
        }
        out.push((pair.name.clone(), value));
    }
    Ok(out)
}

/// Read `user.*` xattrs from `path` (X.3). Returns `None` on non-Unix or
/// when the path has no readable user attributes. Soft errors (e.g. the
/// source filesystem has no xattr support) also yield `None` so a
/// transfer of content still proceeds.
pub fn read_user_xattrs(path: &Path) -> Option<Vec<XattrPair>> {
    #[cfg(unix)]
    {
        read_user_xattrs_unix(path)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// Thin libc wrappers: Linux and macOS disagree on xattr signatures.
/// Linux: listxattr(3), getxattr(4), setxattr(5, flags), removexattr(2).
/// macOS: listxattr(4, options), getxattr/setxattr(6, position+options),
/// removexattr(3, options). Position is 0 (whole attribute).
///
/// The **read** side does not follow symlinks: `llistxattr`/`lgetxattr` on
/// Linux, `XATTR_NOFOLLOW` on macOS. Reading through a link would attribute
/// the *target's* attributes to the *link*, and stock rsync does not do that.
/// On macOS a link can carry its own attributes and those are the ones that
/// belong on the wire. On a regular file the `l`-prefixed calls behave
/// identically to the plain ones.
///
/// R2 originally predicted that following the link would fail as a receiver
/// EPERM, since Linux forbids `user.*` on a symlink. **That prediction was
/// wrong**, and the correction matters because the real failure is worse.
/// Measured on lane 3 against stock rsync 3.2.7 by running the pre-R2 wrappers
/// on purpose: with an inline value nothing fails at all, because the kernel
/// leaves the remote link bare either way and the test passes for the wrong
/// reason. Above `MAX_FULL_DATUM` the pre-R2 read emits a per-file out-of-band
/// section for an entry the peer is not expecting, and the stream
/// desynchronises: `HardRejection(InvalidFrame): expected NDX_DONE (0x00), got
/// 0x01`. An EPERM is visible and local to one file; a desynchronised protocol
/// gives arbitrary behaviour downstream. This is why the wire pin in
/// `delta_upload_symlink_xattr_not_inherited_live_lane_3` deliberately uses an
/// out-of-band value: an inline one cannot fail.
///
/// The **write** side keeps following, because `apply_xattrs` is documented to
/// run on the temp file before rename, which is a regular file by construction.
#[cfg(unix)]
mod sys {
    use std::os::raw::{c_char, c_void};

    /// `listxattr` that does not follow symlinks (R2).
    pub unsafe fn llistxattr(path: *const c_char, list: *mut c_char, size: usize) -> isize {
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        {
            libc::listxattr(path, list, size, libc::XATTR_NOFOLLOW)
        }
        #[cfg(not(any(target_os = "macos", target_os = "ios")))]
        {
            libc::llistxattr(path, list, size)
        }
    }

    /// `listxattr` that **does** follow symlinks, for the capability probe
    /// only (R5). The probe asks a question about a *filesystem*, so when the
    /// destination directory path ends in a symlink the answer that matters is
    /// the target's, not the link's. Everywhere else the read side uses the
    /// `l`-prefixed calls above.
    pub unsafe fn listxattr_following(
        path: *const c_char,
        list: *mut c_char,
        size: usize,
    ) -> isize {
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        {
            libc::listxattr(path, list, size, 0)
        }
        #[cfg(not(any(target_os = "macos", target_os = "ios")))]
        {
            libc::listxattr(path, list, size)
        }
    }

    /// `getxattr` that does not follow symlinks (R2).
    pub unsafe fn lgetxattr(
        path: *const c_char,
        name: *const c_char,
        value: *mut c_void,
        size: usize,
    ) -> isize {
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        {
            libc::getxattr(path, name, value, size, 0, libc::XATTR_NOFOLLOW)
        }
        #[cfg(not(any(target_os = "macos", target_os = "ios")))]
        {
            libc::lgetxattr(path, name, value, size)
        }
    }

    pub unsafe fn setxattr(
        path: *const c_char,
        name: *const c_char,
        value: *const c_void,
        size: usize,
    ) -> i32 {
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        {
            libc::setxattr(path, name, value, size, 0, 0)
        }
        #[cfg(not(any(target_os = "macos", target_os = "ios")))]
        {
            libc::setxattr(path, name, value, size, 0)
        }
    }

    // `removexattr` used to live here, for the sole purpose of cleaning up
    // after the write-based capability probe. R5 replaced that probe with a
    // read-only `listxattr`, so nothing writes an attribute it then has to
    // take back, and the wrapper went with it.
}

#[cfg(unix)]
fn read_user_xattrs_unix(path: &Path) -> Option<Vec<XattrPair>> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let c_path = CString::new(path.as_os_str().as_bytes()).ok()?;
    // Size probe first (listxattr with size 0 returns needed length).
    let needed = unsafe { sys::llistxattr(c_path.as_ptr(), std::ptr::null_mut(), 0) };
    if needed < 0 {
        return None;
    }
    if needed == 0 {
        return Some(Vec::new());
    }
    let mut buf = vec![0u8; needed as usize];
    let written = unsafe {
        sys::llistxattr(
            c_path.as_ptr(),
            buf.as_mut_ptr() as *mut libc::c_char,
            buf.len(),
        )
    };
    if written < 0 {
        return None;
    }
    buf.truncate(written as usize);

    let mut pairs = Vec::new();
    for name in buf.split(|&b| b == 0).filter(|s| !s.is_empty()) {
        let Ok(name_str) = std::str::from_utf8(name) else {
            continue;
        };
        if !name_str.starts_with(USER_XATTR_PREFIX) || name_str == USER_XATTR_PREFIX {
            continue;
        }
        let Ok(c_name) = CString::new(name) else {
            continue;
        };
        let vlen =
            unsafe { sys::lgetxattr(c_path.as_ptr(), c_name.as_ptr(), std::ptr::null_mut(), 0) };
        if vlen < 0 {
            continue;
        }
        let mut value = vec![0u8; vlen as usize];
        let got = unsafe {
            sys::lgetxattr(
                c_path.as_ptr(),
                c_name.as_ptr(),
                value.as_mut_ptr() as *mut libc::c_void,
                value.len(),
            )
        };
        if got < 0 {
            continue;
        }
        value.truncate(got as usize);
        pairs.push(XattrPair::inline(name_str, value));
    }
    Some(pairs)
}

/// Apply sanitized xattrs to `path` (X.4). On ENOTSUP (X.5), returns
/// [`XattrApplyOutcome::Unsupported`] unless `fail_on_metadata_loss`.
///
/// Callers must invoke this on the **temp file before rename**.
pub fn apply_xattrs(
    path: &Path,
    pairs: &[XattrPair],
    fail_on_metadata_loss: bool,
) -> XattrApplyOutcome {
    let sanitized = match sanitize_xattrs_for_apply(pairs) {
        Ok(v) => v,
        Err(e) => {
            return XattrApplyOutcome::Failed {
                message: e.to_string(),
            };
        }
    };
    if sanitized.is_empty() {
        return XattrApplyOutcome::Applied { count: 0 };
    }

    #[cfg(unix)]
    {
        apply_xattrs_unix(path, &sanitized, fail_on_metadata_loss)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        if fail_on_metadata_loss {
            XattrApplyOutcome::Failed {
                message: "xattr apply is not supported on this platform".into(),
            }
        } else {
            XattrApplyOutcome::Unsupported {
                warnings: vec![
                    "xattr apply skipped: extended attributes are not supported on this platform"
                        .into(),
                ],
            }
        }
    }
}

#[cfg(unix)]
fn apply_xattrs_unix(
    path: &Path,
    pairs: &[(String, Vec<u8>)],
    fail_on_metadata_loss: bool,
) -> XattrApplyOutcome {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    if let Some(parent) = path.parent() {
        if !fs_supports_xattrs(parent) {
            let warning = format!(
                "destination filesystem does not support extended attributes ({}); \
                 skipping {} xattr(s) on {}",
                parent.display(),
                pairs.len(),
                path.display()
            );
            return if fail_on_metadata_loss {
                XattrApplyOutcome::Failed { message: warning }
            } else {
                XattrApplyOutcome::Unsupported {
                    warnings: vec![warning],
                }
            };
        }
    }

    let Ok(c_path) = CString::new(path.as_os_str().as_bytes()) else {
        return XattrApplyOutcome::Failed {
            message: format!(
                "xattr apply: path contains interior NUL: {}",
                path.display()
            ),
        };
    };

    let mut applied = 0usize;
    for (name, value) in pairs {
        let Ok(c_name) = CString::new(name.as_bytes()) else {
            return XattrApplyOutcome::Failed {
                message: format!("xattr apply: name contains interior NUL: {name:?}"),
            };
        };
        let rc = unsafe {
            sys::setxattr(
                c_path.as_ptr(),
                c_name.as_ptr(),
                value.as_ptr() as *const libc::c_void,
                value.len(),
            )
        };
        if rc != 0 {
            let err = std::io::Error::last_os_error();
            let enotsup = err.raw_os_error() == Some(libc::ENOTSUP)
                || err.raw_os_error() == Some(libc::EOPNOTSUPP);
            if enotsup {
                // Cache the negative probe so later files on the same fs skip.
                if let Some(parent) = path.parent() {
                    remember_xattr_support(parent, false);
                }
                let warning = format!(
                    "setxattr ENOTSUP on {} for {name:?}; destination does not support xattrs",
                    path.display()
                );
                return if fail_on_metadata_loss {
                    XattrApplyOutcome::Failed { message: warning }
                } else {
                    XattrApplyOutcome::Unsupported {
                        warnings: vec![warning],
                    }
                };
            }
            return XattrApplyOutcome::Failed {
                message: format!("setxattr({name:?}) on {}: {err}", path.display()),
            };
        }
        applied += 1;
    }
    XattrApplyOutcome::Applied { count: applied }
}

// --- capability probe cache (once per destination directory) ---------------

/// How many destination directories the probe cache may remember at once.
///
/// The cache used to be unbounded, so a long-lived process that synced into
/// many directories grew one entry per directory it ever saw, for the life of
/// the process. The cap turns that into a fixed cost; overflow clears the map
/// wholesale rather than evicting cleverly, because since R5 a re-probe is a
/// single read-only `listxattr` and is not worth an LRU.
const XATTR_CACHE_MAX_ENTRIES: usize = 256;

/// How long a cached answer stays trusted.
///
/// Entries used to live forever, which made the *negative* direction sticky: a
/// filesystem remounted with `user_xattr`, or a destination that moved to a
/// different device under the same path, kept being treated as unsupported
/// until the process restarted. (The positive direction always self-corrected,
/// because the real `setxattr` re-caches a negative on ENOTSUP.) A short expiry
/// bounds that staleness without giving up the per-file syscall saving.
const XATTR_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(60);

type XattrCacheEntry = (bool, std::time::Instant);

static XATTR_SUPPORT_CACHE: Mutex<Option<HashMap<PathBuf, XattrCacheEntry>>> = Mutex::new(None);

fn remember_xattr_support(dir: &Path, supported: bool) {
    let Ok(mut guard) = XATTR_SUPPORT_CACHE.lock() else {
        return;
    };
    let map = guard.get_or_insert_with(HashMap::new);
    if map.len() >= XATTR_CACHE_MAX_ENTRIES && !map.contains_key(dir) {
        map.clear();
    }
    map.insert(dir.to_path_buf(), (supported, std::time::Instant::now()));
}

/// Probe whether `dir`'s filesystem carries xattrs. Cached per path, bounded
/// by [`XATTR_CACHE_MAX_ENTRIES`] and expiring after [`XATTR_CACHE_TTL`].
#[cfg(unix)]
pub fn fs_supports_xattrs(dir: &Path) -> bool {
    if let Ok(guard) = XATTR_SUPPORT_CACHE.lock() {
        if let Some(map) = guard.as_ref() {
            if let Some(&(known, stamped)) = map.get(dir) {
                if stamped.elapsed() < XATTR_CACHE_TTL {
                    return known;
                }
            }
        }
    }
    let supported = probe_xattr_support(dir);
    remember_xattr_support(dir, supported);
    supported
}

#[cfg(not(unix))]
pub fn fs_supports_xattrs(_dir: &Path) -> bool {
    false
}

/// Probe whether `dir`'s filesystem carries extended attributes, **without
/// writing anything** (R5).
///
/// The earlier probe did `setxattr` + `removexattr` on the caller's
/// destination directory. That answered the question, but at the price of
/// touching a directory we were only asked to inspect: a crash between the two
/// calls left a stray `user.aeroftp.xattr_probe` behind on the user's own
/// folder. `listxattr` distinguishes ENOTSUP from "supported, currently empty"
/// just as well and cannot leave residue.
///
/// The probe is deliberately allowed to be optimistic. It is a fast path to
/// skip work, never the authority: the authority is the real `setxattr` in
/// [`apply_xattrs_unix`], which maps ENOTSUP to the soft warning path and
/// caches the negative. So every error other than ENOTSUP (EACCES on a
/// directory we may still write into, ENOENT on a racing path) answers
/// "supported" and lets the real call decide.
#[cfg(unix)]
fn probe_xattr_support(dir: &Path) -> bool {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let Ok(c_path) = CString::new(dir.as_os_str().as_bytes()) else {
        return true;
    };
    let rc = unsafe { sys::listxattr_following(c_path.as_ptr(), std::ptr::null_mut(), 0) };
    if rc >= 0 {
        // Includes rc == 0: the directory carries no attributes today, but the
        // filesystem accepts the call, which is what was asked.
        return true;
    }
    let err = std::io::Error::last_os_error();
    let enotsup =
        err.raw_os_error() == Some(libc::ENOTSUP) || err.raw_os_error() == Some(libc::EOPNOTSUPP);
    !enotsup
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_accepts_plain_user_attrs() {
        let pairs = vec![
            XattrPair::inline("user.a", b"v1".to_vec()),
            XattrPair::inline("user.b", vec![0u8, 1, 2]),
        ];
        let got = sanitize_xattrs_for_apply(&pairs).expect("ok");
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].0, "user.a");
        assert_eq!(got[1].1, vec![0, 1, 2]);
    }

    #[test]
    fn sanitize_rejects_empty_name() {
        let pairs = vec![XattrPair::inline("", b"v".to_vec())];
        assert_eq!(
            sanitize_xattrs_for_apply(&pairs),
            Err(XattrSanitizeError::EmptyName)
        );
    }

    #[test]
    fn sanitize_rejects_non_user_namespace() {
        for name in [
            "security.selinux",
            "trusted.foo",
            "system.posix_acl_access",
            "user.",
        ] {
            let pairs = vec![XattrPair::inline(name, b"v".to_vec())];
            match sanitize_xattrs_for_apply(&pairs) {
                Err(XattrSanitizeError::NamespaceNotAllowed { name: n }) => {
                    assert_eq!(n, name);
                }
                other => panic!("expected NamespaceNotAllowed for {name}, got {other:?}"),
            }
        }
    }

    #[test]
    fn sanitize_rejects_too_many_pairs() {
        let pairs: Vec<_> = (0..MAX_XATTR_PAIRS + 1)
            .map(|i| XattrPair::inline(format!("user.n{i}"), b"x".to_vec()))
            .collect();
        assert!(matches!(
            sanitize_xattrs_for_apply(&pairs),
            Err(XattrSanitizeError::TooManyPairs { .. })
        ));
    }

    #[test]
    fn sanitize_rejects_oversized_total() {
        let big = vec![b'Z'; MAX_XATTR_TOTAL_BYTES / 2 + 1];
        let pairs = vec![
            XattrPair::inline("user.a", big.clone()),
            XattrPair::inline("user.b", big),
        ];
        assert!(matches!(
            sanitize_xattrs_for_apply(&pairs),
            Err(XattrSanitizeError::TotalBytesTooLarge { .. })
        ));
    }

    #[test]
    fn sanitize_rejects_unresolved_deferred() {
        let pairs = vec![XattrPair {
            name: "user.big".into(),
            datum: XattrDatum::Deferred {
                len: 64,
                digest: [0u8; 16],
            },
        }];
        assert!(matches!(
            sanitize_xattrs_for_apply(&pairs),
            Err(XattrSanitizeError::DeferredUnresolved { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn read_and_apply_user_xattr_round_trip_on_tempfile() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("payload.bin");
        std::fs::write(&path, b"data").expect("write");

        // set via libc so we do not depend on xattr crate.
        {
            use std::ffi::CString;
            use std::os::unix::ffi::OsStrExt;
            let c_path = CString::new(path.as_os_str().as_bytes()).unwrap();
            let c_name = CString::new("user.aeroftp.test").unwrap();
            let val = b"hello-xattr";
            let rc = unsafe {
                sys::setxattr(
                    c_path.as_ptr(),
                    c_name.as_ptr(),
                    val.as_ptr() as *const libc::c_void,
                    val.len(),
                )
            };
            if rc != 0 {
                // Filesystem may not support xattrs (e.g. some CI mounts).
                eprintln!(
                    "skipping round-trip: setxattr unsupported: {}",
                    std::io::Error::last_os_error()
                );
                return;
            }
        }

        let pairs = read_user_xattrs(&path).expect("read");
        assert!(
            pairs.iter().any(|p| p.name == "user.aeroftp.test"
                && p.datum.bytes() == Some(b"hello-xattr".as_slice())),
            "expected user.aeroftp.test in {pairs:?}"
        );

        let dest = dir.path().join("dest.bin");
        std::fs::write(&dest, b"data").expect("write dest");
        match apply_xattrs(&dest, &pairs, false) {
            XattrApplyOutcome::Applied { count } => assert!(count >= 1),
            XattrApplyOutcome::Unsupported { warnings } => {
                eprintln!("apply unsupported: {warnings:?}");
            }
            XattrApplyOutcome::Failed { message } => panic!("apply failed: {message}"),
        }
    }

    /// R5: the capability probe inspects the destination directory, it does
    /// not write to it. The previous probe did `setxattr` + `removexattr`, so
    /// a crash between the two left `user.aeroftp.xattr_probe` behind on a
    /// folder the user owns. This pins the absence of any write at all: the
    /// directory's attribute set must be identical before and after, which
    /// also fails if someone reintroduces a write-then-clean-up probe (the
    /// probe name would appear in the middle) or forgets the cleanup.
    #[cfg(unix)]
    #[test]
    fn capability_probe_leaves_the_destination_directory_untouched() {
        let dir = tempfile::tempdir().expect("tempdir");

        let before = list_dir_xattr_names(dir.path());
        let supported = fs_supports_xattrs(dir.path());
        let after = list_dir_xattr_names(dir.path());

        assert_eq!(
            before, after,
            "the probe changed the directory's attributes (supported={supported})"
        );
        assert!(
            !after.iter().any(|n| n.contains("xattr_probe")),
            "probe residue left on the destination directory: {after:?}"
        );
    }

    /// R5: the probe cache is bounded. It used to be an unbounded process-wide
    /// map, so a long-lived sync into many directories grew one entry per
    /// directory for the life of the process.
    #[cfg(unix)]
    #[test]
    fn capability_probe_cache_stays_bounded() {
        let dir = tempfile::tempdir().expect("tempdir");
        for i in 0..(XATTR_CACHE_MAX_ENTRIES + 32) {
            let sub = dir.path().join(format!("d{i:04}"));
            std::fs::create_dir(&sub).expect("create subdir");
            let _ = fs_supports_xattrs(&sub);
        }
        let len = XATTR_SUPPORT_CACHE
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|m| m.len()))
            .expect("cache populated");
        assert!(
            len <= XATTR_CACHE_MAX_ENTRIES,
            "probe cache grew past its cap: {len} > {XATTR_CACHE_MAX_ENTRIES}"
        );
    }

    /// Names of the extended attributes on `dir`, sorted. Empty when the
    /// filesystem does not carry any (which is still a valid comparison:
    /// the assertion is that the set does not *change*).
    #[cfg(unix)]
    fn list_dir_xattr_names(dir: &Path) -> Vec<String> {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let Ok(c_path) = CString::new(dir.as_os_str().as_bytes()) else {
            return Vec::new();
        };
        let needed = unsafe { sys::listxattr_following(c_path.as_ptr(), std::ptr::null_mut(), 0) };
        if needed <= 0 {
            return Vec::new();
        }
        let mut buf = vec![0i8; needed as usize];
        let got = unsafe { sys::listxattr_following(c_path.as_ptr(), buf.as_mut_ptr(), buf.len()) };
        if got <= 0 {
            return Vec::new();
        }
        let bytes: Vec<u8> = buf[..got as usize].iter().map(|&c| c as u8).collect();
        let mut names: Vec<String> = bytes
            .split(|&b| b == 0)
            .filter(|s| !s.is_empty())
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .collect();
        names.sort();
        names
    }

    /// R2: reading a symlink must not report the target's attributes as the
    /// link's. Stock rsync reads the link itself, and on Linux a symlink
    /// cannot carry `user.*` at all, so the correct answer is "no attributes".
    /// Before the `l`-prefixed calls this test failed by finding
    /// `user.aeroftp.symlink_probe` on the link.
    #[cfg(unix)]
    #[test]
    fn read_user_xattrs_does_not_follow_a_symlink_to_its_target() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("target.bin");
        std::fs::write(&target, b"data").expect("write target");

        let c_target = CString::new(target.as_os_str().as_bytes()).unwrap();
        let c_name = CString::new("user.aeroftp.symlink_probe").unwrap();
        let val = b"TARGET_ATTR";
        let rc = unsafe {
            sys::setxattr(
                c_target.as_ptr(),
                c_name.as_ptr(),
                val.as_ptr() as *const libc::c_void,
                val.len(),
            )
        };
        if rc != 0 {
            // Filesystem without xattr support: nothing to pin here.
            eprintln!(
                "skipping symlink probe: setxattr unsupported: {}",
                std::io::Error::last_os_error()
            );
            return;
        }

        let link = dir.path().join("link.bin");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");

        // Control: the attribute really is on the target, so a failure below
        // means we followed the link rather than that the setxattr was lost.
        let on_target = read_user_xattrs(&target).expect("read target");
        assert!(
            on_target
                .iter()
                .any(|p| p.name == "user.aeroftp.symlink_probe"),
            "control failed: attribute missing on the target itself: {on_target:?}"
        );

        let on_link = read_user_xattrs(&link).unwrap_or_default();
        assert!(
            !on_link
                .iter()
                .any(|p| p.name == "user.aeroftp.symlink_probe"),
            "read through the symlink and picked up the target's attributes: {on_link:?}"
        );
    }
}
