// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Windows-specific utilities: ACL permissions + reserved filename validation.
//!
//! [`restrict_to_owner`] is the Windows equivalent of `chmod 0600` / `0700` on
//! the vault files. It used to shell out to
//! `icacls <path> /inheritance:r /grant:r %USERNAME%:F /T`, which had three
//! independent ways to lock the user out of their own data:
//!
//!   1. `/inheritance:r` was applied *before* the grant, so anything going
//!      wrong afterwards left the object with no usable ACE at all.
//!   2. `%USERNAME%` is an unqualified account name. `LookupAccountName`
//!      searches the local SAM before the domain, so on a domain-joined
//!      machine (`CORP\Phil`) a local account with the same name wins and the
//!      grant lands on a different SID than the interactive user.
//!   3. Removing inheritance also drops SYSTEM and Administrators, so not even
//!      an elevated Explorer could repair the object: the user had to
//!      `takeown` a portable install before being able to delete it.
//!
//! The result was a vault.key / vault.db the user owned but could not read,
//! write or delete — and, because it is an ACL and not a lock, rebooting did
//! not help.
//!
//! The current implementation:
//!
//!   - builds the DACL from the *process token's* user SID, which is correct
//!     by construction on domain- and Entra-joined machines and under any UI
//!     language, with no name resolution and no child process;
//!   - keeps SYSTEM and BUILTIN\Administrators at full control, so the object
//!     always stays repairable by an administrator;
//!   - applies the whole DACL in a single `SetNamedSecurityInfoW` call, so
//!     there is no window in which inheritance is already gone and the grant
//!     is not yet in place;
//!   - verifies the outcome and rolls back to inheritance if the current user
//!     can no longer reach the object. Hardening is a best-effort defence; it
//!     must never be the reason a user loses access to their own vault.
//!
//! [`reset_to_inherited`] is the recovery counterpart: it drops the explicit
//! DACL and re-enables inheritance from the parent directory (the equivalent
//! of `icacls <path> /reset`). The object owner keeps `WRITE_DAC` implicitly
//! even when the DACL grants nothing, so this repairs an already-locked file
//! from inside the app, without `takeown` and without elevation.

#![allow(dead_code)]

use std::path::Path;

/// Restrict file/directory access to the current user, SYSTEM and
/// Administrators. Best-effort: failures are logged and never propagated,
/// because an object that could not be hardened is still usable, while an
/// object that was hardened wrongly is not.
#[cfg(windows)]
pub fn restrict_to_owner(path: &Path) {
    // `is_dir` is sampled before the DACL changes: once the object is
    // protected, a metadata query is one more thing that can fail.
    let is_dir = path.is_dir();
    match win::apply_owner_only_dacl(path, is_dir) {
        Ok(()) => {
            if win::can_still_access(path, is_dir) {
                return;
            }
            tracing::warn!(
                "ACL hardening left {} unreachable for the current user; restoring inheritance",
                path.display()
            );
            if let Err(e) = win::set_inherited_dacl(path) {
                tracing::error!("Failed to restore inheritance on {}: {e}", path.display());
            }
        }
        Err(e) => {
            tracing::warn!("ACL hardening skipped for {}: {e}", path.display());
        }
    }
}

/// No-op on non-Windows platforms
#[cfg(not(windows))]
pub fn restrict_to_owner(_path: &Path) {}

/// Drop the explicit DACL and re-enable inheritance from the parent
/// (equivalent to `icacls <path> /reset`).
///
/// Used both as the rollback path of [`restrict_to_owner`] and as the repair
/// path for objects that an earlier release already locked out.
#[cfg(windows)]
pub fn reset_to_inherited(path: &Path) -> Result<(), String> {
    win::set_inherited_dacl(path)
}

/// No-op on non-Windows platforms
#[cfg(not(windows))]
pub fn reset_to_inherited(_path: &Path) -> Result<(), String> {
    Ok(())
}

/// Recovery helper: if `path` exists but the current user cannot reach it,
/// restore inheritance and report whether that fixed it.
///
/// Returns `Ok(true)` when a repair was performed and access is back,
/// `Ok(false)` when nothing had to be repaired.
#[cfg(windows)]
pub fn repair_access_if_locked(path: &Path) -> Result<bool, String> {
    if !path.exists() {
        return Ok(false);
    }
    let is_dir = path.is_dir();
    if win::can_still_access(path, is_dir) {
        return Ok(false);
    }
    win::set_inherited_dacl(path)?;
    if win::can_still_access(path, is_dir) {
        tracing::info!("Restored inherited permissions on {}", path.display());
        Ok(true)
    } else {
        Err(format!(
            "{} is still unreachable after restoring inheritance",
            path.display()
        ))
    }
}

/// No-op on non-Windows platforms
#[cfg(not(windows))]
pub fn repair_access_if_locked(_path: &Path) -> Result<bool, String> {
    Ok(false)
}

#[cfg(windows)]
mod win {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::Foundation::{
        CloseHandle, LocalFree, ERROR_ACCESS_DENIED, ERROR_SUCCESS, HANDLE, HLOCAL, WIN32_ERROR,
    };
    use windows::Win32::Security::Authorization::{
        SetEntriesInAclW, SetNamedSecurityInfoW, EXPLICIT_ACCESS_W, NO_MULTIPLE_TRUSTEE,
        SET_ACCESS, SE_FILE_OBJECT, TRUSTEE_IS_SID, TRUSTEE_IS_USER, TRUSTEE_IS_WELL_KNOWN_GROUP,
        TRUSTEE_TYPE, TRUSTEE_W,
    };
    use windows::Win32::Security::{
        CreateWellKnownSid, GetTokenInformation, InitializeAcl, TokenUser,
        WinBuiltinAdministratorsSid, WinLocalSystemSid, ACL, ACL_REVISION,
        DACL_SECURITY_INFORMATION, NO_INHERITANCE, PROTECTED_DACL_SECURITY_INFORMATION, PSID,
        SECURITY_MAX_SID_SIZE, SUB_CONTAINERS_AND_OBJECTS_INHERIT, TOKEN_QUERY, TOKEN_USER,
        UNPROTECTED_DACL_SECURITY_INFORMATION, WELL_KNOWN_SID_TYPE,
    };
    use windows::Win32::Storage::FileSystem::FILE_ALL_ACCESS;
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    /// `windows` 0.58 does not derive `Default` on `PSID`.
    const NULL_SID: PSID = PSID(core::ptr::null_mut());

    /// Owner of a `LocalAlloc`'d ACL returned by `SetEntriesInAclW`.
    struct LocalAcl(*mut ACL);

    impl Drop for LocalAcl {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    let _ = LocalFree(HLOCAL(self.0 as *mut core::ffi::c_void));
                }
            }
        }
    }

    /// An ACL with no ACEs, built by hand.
    ///
    /// `SetEntriesInAclW` with zero entries is documented to return an ACL
    /// with no ACEs, but it is also allowed to hand back a NULL pointer — and
    /// a NULL DACL does not mean "no access", it means *everyone, full
    /// control*. Passing that to `SetNamedSecurityInfoW` would turn a reset
    /// into a wide-open vault, so the empty ACL is built deterministically
    /// instead.
    struct EmptyAcl {
        // `u32` storage rather than `u8`: Windows requires ACLs to be
        // DWORD-aligned.
        buffer: [u32; 16],
    }

    impl EmptyAcl {
        fn new() -> Result<Self, String> {
            let mut acl = Self { buffer: [0u32; 16] };
            let size = core::mem::size_of_val(&acl.buffer) as u32;
            unsafe {
                InitializeAcl(acl.buffer.as_mut_ptr() as *mut ACL, size, ACL_REVISION)
                    .map_err(|e| format!("InitializeAcl failed: {e}"))?;
            }
            Ok(acl)
        }

        fn as_ptr(&self) -> *const ACL {
            self.buffer.as_ptr() as *const ACL
        }
    }

    /// Owner of a process token handle.
    struct TokenHandle(HANDLE);

    impl Drop for TokenHandle {
        fn drop(&mut self) {
            if !self.0.is_invalid() {
                unsafe {
                    let _ = CloseHandle(self.0);
                }
            }
        }
    }

    /// The current process token's user SID, kept alive by the buffer it
    /// points into.
    struct TokenUserSid {
        buffer: Vec<u8>,
    }

    impl TokenUserSid {
        fn query() -> Result<Self, String> {
            unsafe {
                let mut raw = HANDLE(core::ptr::null_mut());
                OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw)
                    .map_err(|e| format!("OpenProcessToken failed: {e}"))?;
                let token = TokenHandle(raw);

                // The first call only sizes the buffer: it is expected to fail
                // with ERROR_INSUFFICIENT_BUFFER, so only `needed` is read.
                let mut needed: u32 = 0;
                let _ = GetTokenInformation(token.0, TokenUser, None, 0, &mut needed);
                if (needed as usize) < core::mem::size_of::<TOKEN_USER>() {
                    return Err(format!(
                        "GetTokenInformation reported an implausible TOKEN_USER size ({needed})"
                    ));
                }

                let mut buffer = vec![0u8; needed as usize];
                GetTokenInformation(
                    token.0,
                    TokenUser,
                    Some(buffer.as_mut_ptr() as *mut core::ffi::c_void),
                    needed,
                    &mut needed,
                )
                .map_err(|e| format!("GetTokenInformation(TokenUser) failed: {e}"))?;

                Ok(Self { buffer })
            }
        }

        /// SID pointer into the owned buffer; valid as long as `self` is.
        fn psid(&self) -> PSID {
            // SAFETY: the buffer was filled by GetTokenInformation(TokenUser)
            // and its size checked, so it starts with a TOKEN_USER whose Sid
            // points inside it.
            unsafe { (*(self.buffer.as_ptr() as *const TOKEN_USER)).User.Sid }
        }
    }

    /// A well-known SID (SYSTEM, Administrators) materialised in a local buffer.
    struct WellKnownSid {
        buffer: Vec<u8>,
    }

    impl WellKnownSid {
        fn new(kind: WELL_KNOWN_SID_TYPE) -> Result<Self, String> {
            let mut buffer = vec![0u8; SECURITY_MAX_SID_SIZE as usize];
            let mut size = SECURITY_MAX_SID_SIZE;
            unsafe {
                CreateWellKnownSid(
                    kind,
                    NULL_SID,
                    PSID(buffer.as_mut_ptr() as *mut core::ffi::c_void),
                    &mut size,
                )
                .map_err(|e| format!("CreateWellKnownSid({}) failed: {e}", kind.0))?;
            }
            buffer.truncate(size as usize);
            Ok(Self { buffer })
        }

        fn psid(&self) -> PSID {
            PSID(self.buffer.as_ptr() as *mut core::ffi::c_void)
        }
    }

    fn trustee(sid: PSID, kind: TRUSTEE_TYPE) -> TRUSTEE_W {
        TRUSTEE_W {
            pMultipleTrustee: core::ptr::null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: kind,
            // Documented contract of TRUSTEE_IS_SID: ptstrName carries the SID
            // pointer, not a string.
            ptstrName: PWSTR(sid.0 as *mut u16),
        }
    }

    fn full_control(sid: PSID, kind: TRUSTEE_TYPE, is_dir: bool) -> EXPLICIT_ACCESS_W {
        EXPLICIT_ACCESS_W {
            grfAccessPermissions: FILE_ALL_ACCESS.0,
            grfAccessMode: SET_ACCESS,
            // Files take no inheritance flags; directories propagate to both
            // child files and child directories, so vault data written later
            // is protected the same way.
            grfInheritance: if is_dir {
                SUB_CONTAINERS_AND_OBJECTS_INHERIT
            } else {
                NO_INHERITANCE
            },
            Trustee: trustee(sid, kind),
        }
    }

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    fn check(rc: WIN32_ERROR, what: &str) -> Result<(), String> {
        if rc == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(format!("{what} failed with Win32 error {}", rc.0))
        }
    }

    /// Replace the object's DACL with one that grants full control to the
    /// current user, SYSTEM and Administrators, and nothing to anybody else.
    pub(super) fn apply_owner_only_dacl(path: &Path, is_dir: bool) -> Result<(), String> {
        let user = TokenUserSid::query()?;
        let system = WellKnownSid::new(WinLocalSystemSid)?;
        let admins = WellKnownSid::new(WinBuiltinAdministratorsSid)?;

        let entries = [
            full_control(user.psid(), TRUSTEE_IS_USER, is_dir),
            full_control(system.psid(), TRUSTEE_IS_WELL_KNOWN_GROUP, is_dir),
            full_control(admins.psid(), TRUSTEE_IS_WELL_KNOWN_GROUP, is_dir),
        ];

        let mut raw_acl: *mut ACL = core::ptr::null_mut();
        let acl = unsafe {
            check(
                SetEntriesInAclW(Some(&entries), None, &mut raw_acl),
                "SetEntriesInAclW",
            )?;
            LocalAcl(raw_acl)
        };
        if acl.0.is_null() {
            // A NULL DACL grants everyone full control. Refuse rather than
            // "harden" the vault into being world-writable.
            return Err("SetEntriesInAclW returned a NULL DACL".to_string());
        }

        let path_w = wide(path);
        unsafe {
            check(
                SetNamedSecurityInfoW(
                    PCWSTR(path_w.as_ptr()),
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                    NULL_SID,
                    NULL_SID,
                    Some(acl.0 as *const ACL),
                    None,
                ),
                "SetNamedSecurityInfoW",
            )
        }
    }

    /// Remove the explicit DACL and re-enable inheritance from the parent.
    ///
    /// An empty explicit DACL combined with `UNPROTECTED_DACL_SECURITY_INFORMATION`
    /// is what lets the parent's inheritable ACEs flow back in — the same net
    /// effect as `icacls <path> /reset`.
    pub(super) fn set_inherited_dacl(path: &Path) -> Result<(), String> {
        let acl = EmptyAcl::new()?;
        let path_w = wide(path);
        unsafe {
            check(
                SetNamedSecurityInfoW(
                    PCWSTR(path_w.as_ptr()),
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION | UNPROTECTED_DACL_SECURITY_INFORMATION,
                    NULL_SID,
                    NULL_SID,
                    Some(acl.as_ptr()),
                    None,
                ),
                "SetNamedSecurityInfoW (reset)",
            )
        }
    }

    /// Whether the current user can still reach the object. Deliberately a
    /// real open rather than an effective-rights computation: what matters is
    /// the answer the kernel gives the app and Explorer, not what the ACL
    /// looks like on paper.
    ///
    /// Only `ERROR_ACCESS_DENIED` counts as locked out. A sharing violation
    /// surfaces as `PermissionDenied` too, and treating it as a lockout would
    /// silently roll the hardening back whenever another handle happens to be
    /// open. (`std::fs` itself opens with all three share flags, so this never
    /// conflicts with a handle AeroFTP holds.)
    pub(super) fn can_still_access(path: &Path, is_dir: bool) -> bool {
        let probe = if is_dir {
            std::fs::read_dir(path).map(|_| ())
        } else {
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)
                .map(|_| ())
        };
        match probe {
            Ok(()) => true,
            Err(e) => e.raw_os_error() != Some(ERROR_ACCESS_DENIED.0 as i32),
        }
    }
}

/// Check if a filename is reserved on Windows (CON, PRN, AUX, NUL, COM1-9, LPT1-9).
/// Returns Some(reserved_name) if reserved, None if safe.
pub fn check_windows_reserved(filename: &str) -> Option<&'static str> {
    let stem = filename.split('.').next().unwrap_or(filename);
    let upper = stem.to_uppercase();
    match upper.as_str() {
        "CON" => Some("CON"),
        "PRN" => Some("PRN"),
        "AUX" => Some("AUX"),
        "NUL" => Some("NUL"),
        "COM1" => Some("COM1"),
        "COM2" => Some("COM2"),
        "COM3" => Some("COM3"),
        "COM4" => Some("COM4"),
        "COM5" => Some("COM5"),
        "COM6" => Some("COM6"),
        "COM7" => Some("COM7"),
        "COM8" => Some("COM8"),
        "COM9" => Some("COM9"),
        "LPT1" => Some("LPT1"),
        "LPT2" => Some("LPT2"),
        "LPT3" => Some("LPT3"),
        "LPT4" => Some("LPT4"),
        "LPT5" => Some("LPT5"),
        "LPT6" => Some("LPT6"),
        "LPT7" => Some("LPT7"),
        "LPT8" => Some("LPT8"),
        "LPT9" => Some("LPT9"),
        _ => None,
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("aeroftp-acl-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The whole point of the rewrite: after hardening, the user who owns the
    /// file must still be able to read, write and — critically — delete it.
    /// A portable install has to stay deletable from plain Explorer.
    #[test]
    fn hardened_file_stays_deletable_by_its_owner() {
        let dir = scratch("file");
        let file = dir.join("vault.key");
        std::fs::write(&file, b"AEROVKEY\x02\x02\x00\x00").unwrap();

        restrict_to_owner(&file);

        assert!(std::fs::read(&file).is_ok(), "owner must still read");
        assert!(
            std::fs::OpenOptions::new().write(true).open(&file).is_ok(),
            "owner must still write"
        );
        assert!(
            std::fs::remove_file(&file).is_ok(),
            "owner must still delete"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Directory hardening is inheritable, so files created afterwards must
    /// stay reachable too — this is what happens to the portable data dir.
    #[test]
    fn hardened_directory_stays_removable() {
        let dir = scratch("dir");

        restrict_to_owner(&dir);

        let child = dir.join("vault.db");
        std::fs::write(&child, b"{}").unwrap();
        assert!(std::fs::read(&child).is_ok());
        assert!(std::fs::remove_file(&child).is_ok());
        assert!(std::fs::remove_dir_all(&dir).is_ok());
    }

    #[test]
    fn reset_to_inherited_recovers_a_hardened_file() {
        let dir = scratch("rec");
        let file = dir.join("vault.db");
        std::fs::write(&file, b"{}").unwrap();

        restrict_to_owner(&file);
        reset_to_inherited(&file).expect("owner keeps WRITE_DAC, so reset must succeed");

        assert!(std::fs::remove_file(&file).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn repair_reports_nothing_to_do_on_a_healthy_file() {
        let dir = scratch("ok");
        let file = dir.join("plain.txt");
        std::fs::write(&file, b"x").unwrap();

        assert_eq!(repair_access_if_locked(&file), Ok(false));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn repair_is_a_no_op_on_a_missing_path() {
        let dir = scratch("missing");
        assert_eq!(repair_access_if_locked(&dir.join("nope")), Ok(false));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
