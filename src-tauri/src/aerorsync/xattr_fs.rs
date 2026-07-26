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
//! - capability probe once per destination directory, not per file

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

#[cfg(unix)]
fn read_user_xattrs_unix(path: &Path) -> Option<Vec<XattrPair>> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let c_path = CString::new(path.as_os_str().as_bytes()).ok()?;
    // Size probe first (listxattr with size 0 returns needed length).
    let needed = unsafe { libc::listxattr(c_path.as_ptr(), std::ptr::null_mut(), 0) };
    if needed < 0 {
        return None;
    }
    if needed == 0 {
        return Some(Vec::new());
    }
    let mut buf = vec![0u8; needed as usize];
    let written = unsafe {
        libc::listxattr(
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
            unsafe { libc::getxattr(c_path.as_ptr(), c_name.as_ptr(), std::ptr::null_mut(), 0) };
        if vlen < 0 {
            continue;
        }
        let mut value = vec![0u8; vlen as usize];
        let got = unsafe {
            libc::getxattr(
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
            libc::setxattr(
                c_path.as_ptr(),
                c_name.as_ptr(),
                value.as_ptr() as *const libc::c_void,
                value.len(),
                0,
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

static XATTR_SUPPORT_CACHE: Mutex<Option<HashMap<PathBuf, bool>>> = Mutex::new(None);

fn remember_xattr_support(dir: &Path, supported: bool) {
    let Ok(mut guard) = XATTR_SUPPORT_CACHE.lock() else {
        return;
    };
    let map = guard.get_or_insert_with(HashMap::new);
    map.insert(dir.to_path_buf(), supported);
}

/// Probe whether `dir`'s filesystem accepts user xattrs. Cached per path.
#[cfg(unix)]
pub fn fs_supports_xattrs(dir: &Path) -> bool {
    if let Ok(guard) = XATTR_SUPPORT_CACHE.lock() {
        if let Some(map) = guard.as_ref() {
            if let Some(&known) = map.get(dir) {
                return known;
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

#[cfg(unix)]
fn probe_xattr_support(dir: &Path) -> bool {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    // Prefer probing the directory itself with a throwaway user xattr.
    // If the directory is not writable / not an xattr carrier, fall back
    // to "supported" optimistically so a later setxattr on the temp file
    // can still try (and map ENOTSUP to the soft path).
    let Ok(c_path) = CString::new(dir.as_os_str().as_bytes()) else {
        return true;
    };
    let Ok(c_name) = CString::new("user.aeroftp.xattr_probe") else {
        return true;
    };
    let probe = b"1";
    let rc = unsafe {
        libc::setxattr(
            c_path.as_ptr(),
            c_name.as_ptr(),
            probe.as_ptr() as *const libc::c_void,
            probe.len(),
            0,
        )
    };
    if rc == 0 {
        let _ = unsafe { libc::removexattr(c_path.as_ptr(), c_name.as_ptr()) };
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
                libc::setxattr(
                    c_path.as_ptr(),
                    c_name.as_ptr(),
                    val.as_ptr() as *const libc::c_void,
                    val.len(),
                    0,
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
}
