//! Local POSIX.1e access-ACL I/O for AeroRsync B2.
//!
//! Wire codec lives in [`crate::aerorsync::real_wire`]. This module is the
//! filesystem side: read an access ACL from an open file descriptor,
//! strip inferable object bits the way rsync 3.2.7 does, reconstruct a
//! received literal from the file mode, and apply the result onto the
//! still-open temp fd before rename.
//!
//! B2 is Linux-only. Other targets compile this module without libacl
//! and refuse an ACL opt-in before the remote session opens.

#![cfg(feature = "aerorsync")]

// `io` is read only by the Linux implementation below, and `AclPrincipal` by
// that implementation and by the tests. Ungated, both are unused imports on
// Windows and macOS, which the standalone lane compiles with `-D warnings`.
#[cfg(target_os = "linux")]
use std::io;

#[cfg(any(target_os = "linux", test))]
use crate::aerorsync::real_wire::AclPrincipal;
use crate::aerorsync::real_wire::{
    AclNamedEntry, AclWireEntry, FileListAcls, RsyncAcl, MAX_ACL_NAMED_ENTRIES,
};

/// Outcome of applying an access ACL to an open file descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AclApplyOutcome {
    Applied,
    /// Destination filesystem does not support POSIX ACLs (ENOTSUP).
    Unsupported {
        warning: String,
    },
    Failed {
        message: String,
    },
}

/// Why an ACL conversion or I/O step failed. Platform
/// [`AclFsError::Unsupported`] is always hard. Destination-FS ENOTSUP
/// is surfaced as [`AclApplyOutcome::Unsupported`] and may become a
/// warning when `fail_on_metadata_loss` is off.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AclFsError {
    Unsupported,
    UnresolvedReference { index: u32 },
    Invalid { reason: String },
    Io { message: String },
}

impl std::fmt::Display for AclFsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported => {
                write!(f, "POSIX ACLs are only supported on Linux in this release")
            }
            Self::UnresolvedReference { index } => {
                write!(
                    f,
                    "ACL wire reference {index} cannot be resolved in a single-file session"
                )
            }
            Self::Invalid { reason } => write!(f, "invalid ACL: {reason}"),
            Self::Io { message } => write!(f, "{message}"),
        }
    }
}

/// Refuse an ACL opt-in on any platform that is not Linux, before the
/// remote stream is opened.
pub fn ensure_linux_acl_support() -> Result<(), AclFsError> {
    #[cfg(target_os = "linux")]
    {
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err(AclFsError::Unsupported)
    }
}

/// Extract the access ACL literal, rejecting unresolved references.
pub fn access_literal(acls: &FileListAcls) -> Result<&RsyncAcl, AclFsError> {
    match &acls.access {
        AclWireEntry::Literal(acl) => Ok(acl),
        AclWireEntry::Reference(index) => Err(AclFsError::UnresolvedReference { index: *index }),
    }
}

/// Validate an outgoing file-list entry before the wire opens.
pub fn validate_outgoing_acls(
    preserve_acls: bool,
    entry: &crate::aerorsync::real_wire::FileListEntry,
) -> Result<(), AclFsError> {
    if !preserve_acls {
        return Ok(());
    }
    ensure_linux_acl_support()?;
    if crate::aerorsync::real_wire::is_symlink_mode(entry.mode) {
        if entry.acls.is_some() {
            return Err(AclFsError::Invalid {
                reason: "symlink entries never carry ACL bytes".into(),
            });
        }
        return Ok(());
    }
    let acls = entry.acls.as_ref().ok_or_else(|| AclFsError::Invalid {
        reason: "session negotiated -A but the file-list entry has no access ACL".into(),
    })?;
    access_literal(acls)?;
    Ok(())
}

fn mode_user(mode: u32) -> u8 {
    ((mode >> 6) & 7) as u8
}
fn mode_group(mode: u32) -> u8 {
    ((mode >> 3) & 7) as u8
}
fn mode_other(mode: u32) -> u8 {
    (mode & 7) as u8
}

fn perm_in_range(perm: u8, field: &str) -> Result<u8, AclFsError> {
    if perm > 7 {
        return Err(AclFsError::Invalid {
            reason: format!("{field} permission {perm} is outside 0..=7"),
        });
    }
    Ok(perm)
}

/// Strip object permissions inferable from `mode`, matching
/// `rsync_acl_strip_perms` in rsync 3.2.7 `acls.c`.
pub fn strip_perms_for_wire(acl: &RsyncAcl, mode: u32) -> Result<RsyncAcl, AclFsError> {
    validate_named_entries(&acl.names)?;
    let group_bits = mode_group(mode);
    let has_names = !acl.names.is_empty();
    let group_obj = if acl.mask_obj.is_none() || acl.group_obj == Some(group_bits) {
        None
    } else {
        acl.group_obj
            .map(|p| perm_in_range(p, "group_obj"))
            .transpose()?
    };
    let mask_obj = if has_names && acl.mask_obj == Some(group_bits) {
        None
    } else {
        acl.mask_obj
            .map(|p| perm_in_range(p, "mask_obj"))
            .transpose()?
    };
    Ok(RsyncAcl {
        user_obj: None,
        group_obj,
        mask_obj,
        other_obj: None,
        names: acl.names.clone(),
    })
}

/// Reconstruct omitted object fields from `mode`, matching
/// `rsync_acl_fake_perms` / `change_sacl_perms`. Does not call
/// `fix_mask()`: an explicit mask on the wire is kept verbatim.
pub fn reconstruct_from_wire(acl: &RsyncAcl, mode: u32) -> Result<RsyncAcl, AclFsError> {
    validate_named_entries(&acl.names)?;
    let user_obj = perm_in_range(acl.user_obj.unwrap_or(mode_user(mode)), "user_obj")?;
    let other_obj = perm_in_range(acl.other_obj.unwrap_or(mode_other(mode)), "other_obj")?;
    let group_obj = perm_in_range(acl.group_obj.unwrap_or(mode_group(mode)), "group_obj")?;
    let mask_obj = match acl.mask_obj {
        Some(mask) => Some(perm_in_range(mask, "mask_obj")?),
        None if !acl.names.is_empty() => Some(mode_group(mode)),
        None => None,
    };
    Ok(RsyncAcl {
        user_obj: Some(user_obj),
        group_obj: Some(group_obj),
        mask_obj,
        other_obj: Some(other_obj),
        names: acl.names.clone(),
    })
}

fn validate_named_entries(names: &[AclNamedEntry]) -> Result<(), AclFsError> {
    if names.len() > MAX_ACL_NAMED_ENTRIES {
        return Err(AclFsError::Invalid {
            reason: format!(
                "named-entry count {} exceeds {MAX_ACL_NAMED_ENTRIES}",
                names.len()
            ),
        });
    }
    for (i, entry) in names.iter().enumerate() {
        perm_in_range(entry.access, "named access")?;
        for other in &names[..i] {
            if other.principal == entry.principal && other.id == entry.id {
                return Err(AclFsError::Invalid {
                    reason: format!("duplicate named {:?} id {}", entry.principal, entry.id),
                });
            }
        }
    }
    Ok(())
}

/// Convert a full filesystem ACL (every object field present) into the
/// stripped wire literal plus the access slot used on the file list.
pub fn filesystem_acl_to_wire(acl: RsyncAcl, mode: u32) -> Result<FileListAcls, AclFsError> {
    let stripped = strip_perms_for_wire(&acl, mode)?;
    Ok(FileListAcls {
        access: AclWireEntry::Literal(stripped),
        default: None,
    })
}

/// Convert a received file-list ACL into a reconstructed filesystem ACL.
pub fn wire_acl_to_filesystem(acls: &FileListAcls, mode: u32) -> Result<RsyncAcl, AclFsError> {
    reconstruct_from_wire(access_literal(acls)?, mode)
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::os::unix::io::RawFd;

    use posix_acl::{ACLEntry, PosixACL, Qualifier, ACL_EXECUTE, ACL_READ, ACL_WRITE};

    fn perm_from_posix(perm: u32, field: &str) -> Result<u8, AclFsError> {
        let allowed = ACL_READ | ACL_WRITE | ACL_EXECUTE;
        if perm & !allowed != 0 {
            return Err(AclFsError::Invalid {
                reason: format!("{field} has bits outside rwx: {perm:#x}"),
            });
        }
        perm_in_range(perm as u8, field)
    }

    fn perm_to_posix(perm: u8) -> u32 {
        let mut bits = 0u32;
        if perm & 4 != 0 {
            bits |= ACL_READ;
        }
        if perm & 2 != 0 {
            bits |= ACL_WRITE;
        }
        if perm & 1 != 0 {
            bits |= ACL_EXECUTE;
        }
        bits
    }

    fn posix_from_entries(acl: &RsyncACLFull) -> Result<PosixACL, AclFsError> {
        let mut posix = PosixACL::empty();
        posix.set(Qualifier::UserObj, perm_to_posix(acl.user_obj));
        posix.set(Qualifier::GroupObj, perm_to_posix(acl.group_obj));
        posix.set(Qualifier::Other, perm_to_posix(acl.other_obj));
        if let Some(mask) = acl.mask_obj {
            posix.set(Qualifier::Mask, perm_to_posix(mask));
        }
        for named in &acl.names {
            let qual = match named.principal {
                AclPrincipal::User => Qualifier::User(named.id),
                AclPrincipal::Group => Qualifier::Group(named.id),
            };
            posix.set(qual, perm_to_posix(named.access));
        }
        posix.validate().map_err(|err| AclFsError::Invalid {
            reason: format!("libacl rejected the ACL: {err}"),
        })?;
        Ok(posix)
    }

    struct RsyncACLFull {
        user_obj: u8,
        group_obj: u8,
        mask_obj: Option<u8>,
        other_obj: u8,
        names: Vec<AclNamedEntry>,
    }

    fn full_from_reconstructed(acl: RsyncAcl) -> Result<RsyncACLFull, AclFsError> {
        Ok(RsyncACLFull {
            user_obj: acl.user_obj.ok_or_else(|| AclFsError::Invalid {
                reason: "reconstructed ACL missing user_obj".into(),
            })?,
            group_obj: acl.group_obj.ok_or_else(|| AclFsError::Invalid {
                reason: "reconstructed ACL missing group_obj".into(),
            })?,
            mask_obj: acl.mask_obj,
            other_obj: acl.other_obj.ok_or_else(|| AclFsError::Invalid {
                reason: "reconstructed ACL missing other_obj".into(),
            })?,
            names: acl.names,
        })
    }

    fn entries_to_rsync(entries: &[ACLEntry]) -> Result<RsyncAcl, AclFsError> {
        let mut user_obj = None;
        let mut group_obj = None;
        let mut mask_obj = None;
        let mut other_obj = None;
        let mut names = Vec::new();
        for entry in entries {
            match entry.qual {
                Qualifier::Undefined => {
                    return Err(AclFsError::Invalid {
                        reason: "filesystem ACL contains an Undefined qualifier".into(),
                    });
                }
                Qualifier::UserObj => {
                    if user_obj.is_some() {
                        return Err(AclFsError::Invalid {
                            reason: "duplicate UserObj".into(),
                        });
                    }
                    user_obj = Some(perm_from_posix(entry.perm, "user_obj")?);
                }
                Qualifier::GroupObj => {
                    if group_obj.is_some() {
                        return Err(AclFsError::Invalid {
                            reason: "duplicate GroupObj".into(),
                        });
                    }
                    group_obj = Some(perm_from_posix(entry.perm, "group_obj")?);
                }
                Qualifier::Mask => {
                    if mask_obj.is_some() {
                        return Err(AclFsError::Invalid {
                            reason: "duplicate Mask".into(),
                        });
                    }
                    mask_obj = Some(perm_from_posix(entry.perm, "mask_obj")?);
                }
                Qualifier::Other => {
                    if other_obj.is_some() {
                        return Err(AclFsError::Invalid {
                            reason: "duplicate Other".into(),
                        });
                    }
                    other_obj = Some(perm_from_posix(entry.perm, "other_obj")?);
                }
                Qualifier::User(id) => names.push(AclNamedEntry {
                    id,
                    principal: AclPrincipal::User,
                    access: perm_from_posix(entry.perm, "named user")?,
                    name: None,
                }),
                Qualifier::Group(id) => names.push(AclNamedEntry {
                    id,
                    principal: AclPrincipal::Group,
                    access: perm_from_posix(entry.perm, "named group")?,
                    name: None,
                }),
            }
        }
        if user_obj.is_none() || group_obj.is_none() || other_obj.is_none() {
            return Err(AclFsError::Invalid {
                reason: "filesystem ACL is missing a required object entry".into(),
            });
        }
        validate_named_entries(&names)?;
        Ok(RsyncAcl {
            user_obj,
            group_obj,
            mask_obj,
            other_obj,
            names,
        })
    }

    /// Take ownership of a raw `acl_t`. NULL is an I/O failure, never a
    /// panic: libacl signals errors that way.
    fn take_acl(raw: acl_sys::acl_t, op: &str) -> Result<PosixACL, AclFsError> {
        if raw.is_null() {
            let err = io::Error::last_os_error();
            return Err(AclFsError::Io {
                message: format!("{op}: {err}"),
            });
        }
        // SAFETY: `raw` is a non-NULL acl_t from libacl. PosixACL becomes
        // the unique owner and frees it on drop.
        Ok(unsafe { PosixACL::from_raw(raw) })
    }

    pub fn read_access_acl_fd(fd: RawFd, mode: u32) -> Result<FileListAcls, AclFsError> {
        // SAFETY: `fd` is a live descriptor; libacl returns NULL on error.
        let raw = unsafe { acl_sys::acl_get_fd(fd) };
        let posix = take_acl(raw, "acl_get_fd")?;
        let full = entries_to_rsync(&posix.entries())?;
        filesystem_acl_to_wire(full, mode)
    }

    pub fn apply_access_acl_fd(
        fd: RawFd,
        acls: &FileListAcls,
        mode: u32,
        fail_on_metadata_loss: bool,
    ) -> AclApplyOutcome {
        #[cfg(test)]
        if forced_enotsup() {
            let warning = "acl_set_fd ENOTSUP (test seam); destination does not support POSIX ACLs"
                .to_string();
            return if fail_on_metadata_loss {
                AclApplyOutcome::Failed { message: warning }
            } else {
                AclApplyOutcome::Unsupported { warning }
            };
        }
        let reconstructed = match wire_acl_to_filesystem(acls, mode) {
            Ok(acl) => acl,
            Err(err) => {
                return AclApplyOutcome::Failed {
                    message: err.to_string(),
                };
            }
        };
        let full = match full_from_reconstructed(reconstructed) {
            Ok(full) => full,
            Err(err) => {
                return AclApplyOutcome::Failed {
                    message: err.to_string(),
                };
            }
        };
        let posix = match posix_from_entries(&full) {
            Ok(posix) => posix,
            Err(err) => {
                return AclApplyOutcome::Failed {
                    message: err.to_string(),
                };
            }
        };
        let raw = posix.into_raw();
        // SAFETY: `raw` is the unique acl_t just yielded by into_raw; fd is live.
        let rc = unsafe { acl_sys::acl_set_fd(fd, raw) };
        let os_err = (rc != 0).then(io::Error::last_os_error);
        // Re-wrap so Drop still owns the only acl_t, success or failure.
        // SAFETY: same unique non-NULL acl_t; this is the only owner.
        let _owner = unsafe { PosixACL::from_raw(raw) };
        if let Some(err) = os_err {
            let enotsup = err.raw_os_error() == Some(libc::ENOTSUP)
                || err.raw_os_error() == Some(libc::EOPNOTSUPP);
            let message = format!("acl_set_fd: {err}");
            if enotsup {
                return if fail_on_metadata_loss {
                    AclApplyOutcome::Failed { message }
                } else {
                    AclApplyOutcome::Unsupported { warning: message }
                };
            }
            return AclApplyOutcome::Failed { message };
        }
        AclApplyOutcome::Applied
    }

    pub fn read_access_acl_model_fd(fd: RawFd) -> Result<RsyncAcl, AclFsError> {
        // SAFETY: `fd` is a live descriptor; libacl returns NULL on error.
        let raw = unsafe { acl_sys::acl_get_fd(fd) };
        let posix = take_acl(raw, "acl_get_fd")?;
        entries_to_rsync(&posix.entries())
    }
}

#[cfg(target_os = "linux")]
pub use linux::{apply_access_acl_fd, read_access_acl_fd, read_access_acl_model_fd};

#[cfg(not(target_os = "linux"))]
pub fn read_access_acl_fd(_fd: i32, _mode: u32) -> Result<FileListAcls, AclFsError> {
    Err(AclFsError::Unsupported)
}

#[cfg(not(target_os = "linux"))]
pub fn apply_access_acl_fd(
    _fd: i32,
    _acls: &FileListAcls,
    _mode: u32,
    fail_on_metadata_loss: bool,
) -> AclApplyOutcome {
    // Platform unsupported is not destination-FS ENOTSUP: fail closed even
    // when `fail_on_metadata_loss` is off. Production callers must have
    // refused the opt-in in `ensure_linux_acl_support` before the wire.
    let _ = fail_on_metadata_loss;
    AclApplyOutcome::Failed {
        message: AclFsError::Unsupported.to_string(),
    }
}

// Test-only seam lives after production so the unsafe-surface pin, which
// stops at the first column-0 `#[cfg(test)]`, still counts the libacl FFI.
#[cfg(test)]
use std::cell::Cell;

#[cfg(test)]
thread_local! {
    static FORCE_ENOTSUP: Cell<bool> = const { Cell::new(false) };
}

#[cfg(test)]
fn forced_enotsup() -> bool {
    FORCE_ENOTSUP.with(Cell::get)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_forced_enotsup<T>(f: impl FnOnce() -> T) -> T {
        FORCE_ENOTSUP.with(|flag| flag.set(true));
        let result = f();
        FORCE_ENOTSUP.with(|flag| flag.set(false));
        result
    }

    fn named_user(id: u32, access: u8) -> AclNamedEntry {
        AclNamedEntry {
            id,
            principal: AclPrincipal::User,
            access,
            name: None,
        }
    }

    fn full_acl(mask: Option<u8>, names: Vec<AclNamedEntry>) -> RsyncAcl {
        RsyncAcl {
            user_obj: Some(6),
            group_obj: Some(4),
            mask_obj: mask,
            other_obj: Some(4),
            names,
        }
    }

    #[test]
    fn strip_named_user_and_mask_matches_rsync() {
        // mode 0644: user 6, group 4, other 4. Mask 6 differs from group,
        // so rsync keeps the mask and drops group/user/other.
        let src = full_acl(Some(6), vec![named_user(1000, 4)]);
        let wire = strip_perms_for_wire(&src, 0o100_644).expect("strip");
        assert_eq!(wire.user_obj, None);
        assert_eq!(wire.other_obj, None);
        assert_eq!(wire.group_obj, None);
        assert_eq!(wire.mask_obj, Some(6));
        assert_eq!(wire.names, vec![named_user(1000, 4)]);
    }

    #[test]
    fn strip_keeps_group_obj_when_it_differs_from_mode_group_bits() {
        // mode 0644 group bits 4, explicit group_obj 5, mask 6: rsync
        // 3.2.7 keeps both because neither equals the mode group bits.
        let src = RsyncAcl {
            user_obj: Some(6),
            group_obj: Some(5),
            mask_obj: Some(6),
            other_obj: Some(4),
            names: vec![named_user(1000, 4)],
        };
        let wire = strip_perms_for_wire(&src, 0o100_644).expect("strip");
        assert_eq!(wire.user_obj, None);
        assert_eq!(wire.other_obj, None);
        assert_eq!(wire.group_obj, Some(5));
        assert_eq!(wire.mask_obj, Some(6));
    }

    #[test]
    fn omitted_mask_without_named_entries_stays_absent() {
        let wire = RsyncAcl {
            user_obj: None,
            group_obj: None,
            mask_obj: None,
            other_obj: None,
            names: Vec::new(),
        };
        let full = reconstruct_from_wire(&wire, 0o100_755).expect("fake");
        assert_eq!(full.user_obj, Some(7));
        assert_eq!(full.group_obj, Some(5));
        assert_eq!(full.other_obj, Some(5));
        assert_eq!(full.mask_obj, None);
    }

    #[test]
    fn reconstruct_fills_omitted_object_fields_from_mode() {
        let wire = RsyncAcl {
            user_obj: None,
            group_obj: None,
            mask_obj: Some(6),
            other_obj: None,
            names: vec![named_user(1000, 4)],
        };
        let full = reconstruct_from_wire(&wire, 0o100_644).expect("fake");
        assert_eq!(full.user_obj, Some(6));
        assert_eq!(full.group_obj, Some(4));
        assert_eq!(full.other_obj, Some(4));
        assert_eq!(full.mask_obj, Some(6));
    }

    #[test]
    fn explicit_mask_is_not_recalculated() {
        let wire = RsyncAcl {
            user_obj: None,
            group_obj: None,
            mask_obj: Some(4),
            other_obj: None,
            names: vec![named_user(1000, 6)],
        };
        let full = reconstruct_from_wire(&wire, 0o100_755).expect("fake");
        assert_eq!(
            full.mask_obj,
            Some(4),
            "an explicit mask must not be widened to the named-user bits"
        );
    }

    #[test]
    fn omitted_mask_with_named_entries_uses_group_mode_bits() {
        let wire = RsyncAcl {
            user_obj: None,
            group_obj: None,
            mask_obj: None,
            other_obj: None,
            names: vec![named_user(1000, 4)],
        };
        let full = reconstruct_from_wire(&wire, 0o100_640).expect("fake");
        assert_eq!(full.mask_obj, Some(4));
        assert_eq!(full.group_obj, Some(4));
    }

    #[test]
    fn named_ids_preserve_full_u32() {
        let id = u32::MAX;
        let src = full_acl(Some(6), vec![named_user(id, 4)]);
        let wire = strip_perms_for_wire(&src, 0o100_644).expect("strip");
        assert_eq!(wire.names[0].id, id);
        let full = reconstruct_from_wire(&wire, 0o100_644).expect("fake");
        assert_eq!(full.names[0].id, id);
    }

    #[test]
    fn hostile_acl_shapes_fail_closed() {
        let dup = full_acl(Some(6), vec![named_user(7, 4), named_user(7, 5)]);
        assert!(matches!(
            strip_perms_for_wire(&dup, 0o644),
            Err(AclFsError::Invalid { .. })
        ));

        let mut bad_perm = full_acl(Some(6), vec![named_user(1, 8)]);
        assert!(matches!(
            strip_perms_for_wire(&bad_perm, 0o644),
            Err(AclFsError::Invalid { .. })
        ));
        bad_perm.names.clear();
        bad_perm.user_obj = Some(9);
        assert!(matches!(
            reconstruct_from_wire(&bad_perm, 0o644),
            Err(AclFsError::Invalid { .. })
        ));

        let too_many = vec![named_user(1, 4); MAX_ACL_NAMED_ENTRIES + 1];
        assert!(matches!(
            strip_perms_for_wire(&full_acl(Some(6), too_many), 0o644),
            Err(AclFsError::Invalid { .. })
        ));

        let referenced = FileListAcls {
            access: AclWireEntry::Reference(2),
            default: None,
        };
        assert!(matches!(
            access_literal(&referenced),
            Err(AclFsError::UnresolvedReference { index: 2 })
        ));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn enotsup_seam_honours_fail_on_metadata_loss() {
        let acls = filesystem_acl_to_wire(full_acl(Some(6), vec![named_user(1000, 4)]), 0o100_644)
            .expect("wire");
        with_forced_enotsup(|| {
            match apply_access_acl_fd(0, &acls, 0o100_644, false) {
                AclApplyOutcome::Unsupported { .. } => {}
                other => panic!("soft ENOTSUP expected, got {other:?}"),
            }
            match apply_access_acl_fd(0, &acls, 0o100_644, true) {
                AclApplyOutcome::Failed { .. } => {}
                other => panic!("hard ENOTSUP expected, got {other:?}"),
            }
        });
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn fd_bound_round_trip_on_tempfile() {
        use std::os::unix::io::AsRawFd;

        let file = tempfile::tempfile().expect("tempfile");
        let mode = 0o100_644;
        let src = full_acl(Some(6), vec![named_user(65534, 4)]);
        let acls = filesystem_acl_to_wire(src, mode).expect("wire");
        match apply_access_acl_fd(file.as_raw_fd(), &acls, mode, true) {
            AclApplyOutcome::Applied => {}
            AclApplyOutcome::Unsupported { warning } => {
                eprintln!("skipping fd-bound ACL round-trip: {warning}");
                return;
            }
            AclApplyOutcome::Failed { message } => {
                if message.contains("Operation not supported") || message.contains("ENOTSUP") {
                    eprintln!("skipping fd-bound ACL round-trip: {message}");
                    return;
                }
                panic!("apply failed: {message}");
            }
        }
        let reread = read_access_acl_model_fd(file.as_raw_fd()).expect("reread");
        assert_eq!(reread.mask_obj, Some(6));
        assert!(
            reread
                .names
                .iter()
                .any(|n| n.principal == AclPrincipal::User && n.id == 65534 && n.access == 4),
            "named nobody/r-- must survive an fd-bound apply: {reread:?}"
        );
    }

    #[test]
    fn linux_support_gate_matches_compile_target() {
        let result = ensure_linux_acl_support();
        #[cfg(target_os = "linux")]
        result.expect("Linux must accept the ACL opt-in");
        #[cfg(not(target_os = "linux"))]
        assert!(matches!(result, Err(AclFsError::Unsupported)));
    }
}
