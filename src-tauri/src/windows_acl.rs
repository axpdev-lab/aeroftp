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

/// Restrict file/directory access to the current user, SYSTEM and
/// Administrators, failing closed if the owner-only DACL cannot be applied or
/// verified. Used before writing secret-bearing export payloads into a temp
/// file so an export directory with a permissive parent DACL never exposes
/// secret bytes on disk.
///
/// On Windows the DACL is applied to `file`'s open handle, and `path` must
/// still identify that same file object (volume serial + file index) before
/// and after: while the staging handle allows `FILE_SHARE_DELETE`, another
/// account with delete-child rights in the directory could rename the path,
/// and a path-only check could then harden (or verify) a different file than
/// the one the payload is written to.
#[cfg(windows)]
pub fn restrict_to_owner_checked(file: &std::fs::File, path: &Path) -> std::io::Result<()> {
    win::restrict_owner_only_by_handle(file, path)
}

/// No-op on non-Windows platforms (Unix enforces `0600` on the open handle).
#[cfg(not(windows))]
pub fn restrict_to_owner_checked(_file: &std::fs::File, _path: &Path) -> std::io::Result<()> {
    Ok(())
}

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
        SetEntriesInAclW, SetNamedSecurityInfoW, SetSecurityInfo, EXPLICIT_ACCESS_W,
        NO_MULTIPLE_TRUSTEE, SET_ACCESS, SE_FILE_OBJECT, TRUSTEE_IS_SID, TRUSTEE_IS_USER,
        TRUSTEE_IS_WELL_KNOWN_GROUP, TRUSTEE_TYPE, TRUSTEE_W,
    };
    use windows::Win32::Security::{
        CreateWellKnownSid, GetTokenInformation, InitializeAcl, TokenUser,
        WinBuiltinAdministratorsSid, WinLocalSystemSid, ACL, ACL_REVISION,
        DACL_SECURITY_INFORMATION, NO_INHERITANCE, PROTECTED_DACL_SECURITY_INFORMATION,
        PSECURITY_DESCRIPTOR, PSID, SECURITY_MAX_SID_SIZE, SUB_CONTAINERS_AND_OBJECTS_INHERIT,
        TOKEN_QUERY, TOKEN_USER, UNPROTECTED_DACL_SECURITY_INFORMATION, WELL_KNOWN_SID_TYPE,
    };
    use windows::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, FILE_ALL_ACCESS,
    };
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

    /// A DACL granting full control to the current user, SYSTEM and
    /// Administrators, and nothing to anybody else. A NULL result would mean
    /// "everyone, full control", so it is refused by the caller.
    fn owner_only_acl(is_dir: bool) -> Result<LocalAcl, String> {
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
        Ok(acl)
    }

    /// Replace the object's DACL with one that grants full control to the
    /// current user, SYSTEM and Administrators, and nothing to anybody else.
    pub(super) fn apply_owner_only_dacl(path: &Path, is_dir: bool) -> Result<(), String> {
        let acl = owner_only_acl(is_dir)?;
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

    /// The file's identity (volume serial + file index) as seen through an
    /// open handle.
    fn file_identity(handle: HANDLE) -> Result<(u32, u32, u32), String> {
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        unsafe {
            GetFileInformationByHandle(handle, &mut info)
                .map_err(|e| format!("GetFileInformationByHandle failed: {e}"))?;
        }
        Ok((
            info.dwVolumeSerialNumber,
            info.nFileIndexHigh,
            info.nFileIndexLow,
        ))
    }

    /// Open `path` for metadata/ACL rights only, share-all: those rights do
    /// not conflict with a live `FILE_SHARE_DELETE`-only staging handle.
    fn open_path_metadata(path: &Path) -> std::io::Result<std::fs::File> {
        use std::os::windows::fs::OpenOptionsExt;
        // READ_CONTROL (0x0002_0000) | WRITE_DAC (0x0004_0000) | FILE_READ_ATTRIBUTES (0x0080)
        const METADATA_ACL_ACCESS: u32 = 0x0006_0080;
        const SHARE_ALL: u32 = 0x0000_0007;
        std::fs::OpenOptions::new()
            .access_mode(METADATA_ACL_ACCESS)
            .share_mode(SHARE_ALL)
            .open(path)
    }

    /// Handle-based owner-only hardening for the staged export temp file.
    /// The DACL is set on the OPEN HANDLE's object (`SetSecurityInfo`), never
    /// on the mutable path, and `path` must identify that same file object
    /// (volume serial + file index) before and after the change. While the
    /// staging handle allows `FILE_SHARE_DELETE`, another account with
    /// delete-child rights in the export directory can rename the path; this
    /// check fails closed instead of hardening (or verifying) a different
    /// file than the one the payload is about to be written to.
    pub(super) fn restrict_owner_only_by_handle(
        file: &std::fs::File,
        path: &Path,
    ) -> std::io::Result<()> {
        use std::os::windows::io::AsRawHandle;
        let other = |e: String| std::io::Error::new(std::io::ErrorKind::Other, e);
        let denied =
            |msg: &str| std::io::Error::new(std::io::ErrorKind::PermissionDenied, msg.to_string());
        let staging_handle = HANDLE(file.as_raw_handle() as *mut core::ffi::c_void);
        let staging = file_identity(staging_handle).map_err(other)?;
        let path_still_staged = || -> std::io::Result<()> {
            let path_file = open_path_metadata(path)?;
            let via_path =
                file_identity(HANDLE(path_file.as_raw_handle() as *mut core::ffi::c_void))
                    .map_err(other)?;
            if via_path != staging {
                return Err(denied("the staged path no longer refers to the open file"));
            }
            Ok(())
        };
        // Identity first: harden only if the path is still our staged file.
        path_still_staged()?;
        let acl = owner_only_acl(false).map_err(other)?;
        unsafe {
            check(
                SetSecurityInfo(
                    staging_handle,
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                    NULL_SID,
                    NULL_SID,
                    Some(acl.0 as *const ACL),
                    None,
                ),
                "SetSecurityInfo",
            )
            .map_err(other)?;
        }
        // Verify on the SAME HANDLE: read the DACL back through it and require
        // the owner-only DACL. A path-based probe here could verify a
        // different file after a rename under FILE_SHARE_DELETE. On failure,
        // roll back through the handle too: no path-based ACL write after the
        // handle is open.
        if let Err(reason) = verify_owner_only_dacl_by_handle(staging_handle) {
            let _ = restore_inherited_dacl_by_handle(staging_handle);
            return Err(denied(&format!(
                "owner-only ACL verification failed on the staging handle: {reason}"
            )));
        }
        Ok(())
    }

    /// Free a `GetSecurityInfo` security descriptor.
    struct PsdGuard(PSECURITY_DESCRIPTOR);

    impl Drop for PsdGuard {
        fn drop(&mut self) {
            if !self.0 .0.is_null() {
                unsafe {
                    let _ = LocalFree(HLOCAL(self.0 .0 as *mut core::ffi::c_void));
                }
            }
        }
    }

    /// Read the DACL back through `handle` (never the path) and require
    /// exactly the owner-only DACL: present, protected, and every ACE an
    /// allow-full-control ACE for the current user, SYSTEM or Administrators.
    pub(super) fn verify_owner_only_dacl_by_handle(handle: HANDLE) -> Result<(), String> {
        use windows::Win32::Security::Authorization::GetSecurityInfo;
        use windows::Win32::Security::{
            AclSizeInformation, EqualSid, GetAce, GetAclInformation, GetSecurityDescriptorControl,
            GetSecurityDescriptorDacl, ACCESS_ALLOWED_ACE, ACE_HEADER, ACL_SIZE_INFORMATION,
            SE_DACL_PROTECTED,
        };
        use windows::Win32::System::SystemServices::ACCESS_ALLOWED_ACE_TYPE;
        let user = TokenUserSid::query()?;
        let system = WellKnownSid::new(WinLocalSystemSid)?;
        let admins = WellKnownSid::new(WinBuiltinAdministratorsSid)?;
        let expected = [user.psid(), system.psid(), admins.psid()];
        unsafe {
            let mut psd = PSECURITY_DESCRIPTOR(core::ptr::null_mut());
            check(
                GetSecurityInfo(
                    handle,
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION,
                    None,
                    None,
                    None,
                    None,
                    Some(&mut psd),
                ),
                "GetSecurityInfo",
            )?;
            let psd = PsdGuard(psd);
            let mut present = windows::Win32::Foundation::BOOL::default();
            let mut dacl: *mut ACL = core::ptr::null_mut();
            let mut defaulted = windows::Win32::Foundation::BOOL::default();
            GetSecurityDescriptorDacl(psd.0, &mut present, &mut dacl, &mut defaulted)
                .map_err(|e| format!("GetSecurityDescriptorDacl failed: {e}"))?;
            if !present.as_bool() || dacl.is_null() {
                return Err("the handle has no DACL".to_string());
            }
            let mut control = 0u16;
            let mut revision = 0u32;
            GetSecurityDescriptorControl(psd.0, &mut control, &mut revision)
                .map_err(|e| format!("GetSecurityDescriptorControl failed: {e}"))?;
            if control & SE_DACL_PROTECTED.0 == 0 {
                return Err("the DACL is not protected".to_string());
            }
            let mut size = ACL_SIZE_INFORMATION::default();
            GetAclInformation(
                dacl,
                &mut size as *mut ACL_SIZE_INFORMATION as *mut core::ffi::c_void,
                core::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
                AclSizeInformation,
            )
            .map_err(|e| format!("GetAclInformation failed: {e}"))?;
            if size.AceCount as usize != expected.len() {
                return Err(format!(
                    "the DACL has {} ACEs, expected {}",
                    size.AceCount,
                    expected.len()
                ));
            }
            for index in 0..size.AceCount {
                let mut ace: *mut core::ffi::c_void = core::ptr::null_mut();
                GetAce(dacl, index, &mut ace).map_err(|e| format!("GetAce failed: {e}"))?;
                let header = &*(ace as *const ACE_HEADER);
                if u32::from(header.AceType) != ACCESS_ALLOWED_ACE_TYPE {
                    return Err("the DACL holds a non-allow ACE".to_string());
                }
                let allow = &*(ace as *const ACCESS_ALLOWED_ACE);
                if allow.Mask != FILE_ALL_ACCESS.0 {
                    return Err("a DACL ACE does not grant full control".to_string());
                }
                let sid = PSID(
                    (ace as *const u8).add(core::mem::offset_of!(ACCESS_ALLOWED_ACE, SidStart))
                        as *mut core::ffi::c_void,
                );
                if !expected.iter().any(|e| EqualSid(sid, *e).is_ok()) {
                    return Err("a DACL ACE names another trustee".to_string());
                }
            }
        }
        Ok(())
    }

    /// Roll the handle's DACL back to inheritance (empty explicit DACL plus
    /// UNPROTECTED_DACL_SECURITY_INFORMATION) through the SAME handle: never
    /// through the path, which a rename under FILE_SHARE_DELETE can have
    /// pointed at another file.
    pub(super) fn restore_inherited_dacl_by_handle(handle: HANDLE) -> Result<(), String> {
        let acl = EmptyAcl::new()?;
        unsafe {
            check(
                SetSecurityInfo(
                    handle,
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION | UNPROTECTED_DACL_SECURITY_INFORMATION,
                    NULL_SID,
                    NULL_SID,
                    Some(acl.as_ptr()),
                    None,
                ),
                "SetSecurityInfo (rollback)",
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
    // Windows reserves `COM¹` and `LPT³` as it does `COM1` and `LPT3`.
    let upper = stem
        .to_uppercase()
        .replace('¹', "1")
        .replace('²', "2")
        .replace('³', "3");
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

    #[test]
    fn restrict_to_owner_checked_verifies_live_staging_handle_and_rejects_missing_path() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = scratch("checked");
        let file = dir.join("staged.tmp");
        // GENERIC_READ | GENERIC_WRITE | WRITE_DAC, the rights the production
        // staging open requests (SetSecurityInfo on the handle needs WRITE_DAC).
        const STAGING_ACCESS: u32 = 0x8000_0000 | 0x4000_0000 | 0x0004_0000;
        const FILE_SHARE_DELETE: u32 = 0x0000_0004;
        let live_handle = std::fs::OpenOptions::new()
            .create_new(true)
            .access_mode(STAGING_ACCESS)
            .share_mode(FILE_SHARE_DELETE)
            .open(&file)
            .expect("create staged handle with FILE_SHARE_DELETE");

        restrict_to_owner_checked(&live_handle, &file)
            .expect("checked owner-only ACL must verify cleanly against live staging handle");
        assert!(
            restrict_to_owner_checked(&live_handle, &dir.join("missing.tmp")).is_err(),
            "checked ACL must fail closed when the path no longer refers to the open file"
        );
        drop(live_handle);
        let _ = std::fs::remove_file(&file);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Structural pin for the handle binding: after the path is renamed away
    /// and a different file is planted at the old path, the verify and the
    /// rollback helpers still act on the OPEN HANDLE's file object. A
    /// path-based verify/rollback would have inspected or rewritten the
    /// planted file instead.
    #[test]
    fn verification_and_rollback_are_bound_to_the_staging_handle_not_the_path() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = scratch("handle-bound");
        let file = dir.join("staged.tmp");
        const STAGING_ACCESS: u32 = 0x8000_0000 | 0x4000_0000 | 0x0004_0000;
        const FILE_SHARE_DELETE: u32 = 0x0000_0004;
        let live_handle = std::fs::OpenOptions::new()
            .create_new(true)
            .access_mode(STAGING_ACCESS)
            .share_mode(FILE_SHARE_DELETE)
            .open(&file)
            .expect("create staged handle");

        restrict_to_owner_checked(&live_handle, &file).expect("hardening must verify cleanly");

        // Another account renames the staged path away and plants a different
        // file at the old path.
        let renamed = dir.join("swapped.tmp");
        std::fs::rename(&file, &renamed).expect("FILE_SHARE_DELETE allows the rename");
        let planted = dir.join("staged.tmp");
        std::fs::write(&planted, b"attacker").unwrap();

        // Verify through the HANDLE: the handle's object is the hardened one,
        // so verification succeeds even though the path now names the planted
        // file (a path-based verify would read the planted file's DACL).
        assert!(
            win::verify_owner_only_dacl_by_handle(windows::Win32::Foundation::HANDLE(
                std::os::windows::io::AsRawHandle::as_raw_handle(&live_handle) as *mut _
            ))
            .is_ok(),
            "verification must read the DACL back through the staging handle"
        );

        // Roll back through the HANDLE: the handle's object loses the
        // owner-only DACL (verify now fails), while the planted file at the
        // old path is untouched (still plainly readable/writable).
        win::restore_inherited_dacl_by_handle(windows::Win32::Foundation::HANDLE(
            std::os::windows::io::AsRawHandle::as_raw_handle(&live_handle) as *mut _,
        ))
        .expect("rollback must succeed through the handle");
        assert!(
            win::verify_owner_only_dacl_by_handle(windows::Win32::Foundation::HANDLE(
                std::os::windows::io::AsRawHandle::as_raw_handle(&live_handle) as *mut _
            ))
            .is_err(),
            "after the handle rollback the handle's object no longer has the owner-only DACL"
        );
        assert_eq!(
            std::fs::read(&planted).unwrap(),
            b"attacker",
            "the planted file at the old path must be untouched"
        );
        assert!(
            std::fs::OpenOptions::new()
                .write(true)
                .open(&planted)
                .is_ok(),
            "the planted file's ACL must be unchanged: no path-based ACL write happened"
        );
        drop(live_handle);
        let _ = std::fs::remove_file(&planted);
        let _ = std::fs::remove_file(&renamed);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn restrict_to_owner_checked_fails_closed_when_the_path_was_renamed_away() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = scratch("renamed");
        let file = dir.join("staged.tmp");
        // GENERIC_READ | GENERIC_WRITE | WRITE_DAC, the rights the production
        // staging open requests (SetSecurityInfo on the handle needs WRITE_DAC).
        const STAGING_ACCESS: u32 = 0x8000_0000 | 0x4000_0000 | 0x0004_0000;
        const FILE_SHARE_DELETE: u32 = 0x0000_0004;
        let live_handle = std::fs::OpenOptions::new()
            .create_new(true)
            .access_mode(STAGING_ACCESS)
            .share_mode(FILE_SHARE_DELETE)
            .open(&file)
            .expect("create staged handle with FILE_SHARE_DELETE");

        // Another account with delete-child rights renames the staged path
        // while our handle stays open on the original file object.
        let renamed = dir.join("swapped.tmp");
        std::fs::rename(&file, &renamed).expect("FILE_SHARE_DELETE allows the rename");
        // ...and plants a different file at the original path.
        std::fs::write(&file, b"attacker").unwrap();

        assert!(
            restrict_to_owner_checked(&live_handle, &file).is_err(),
            "the path now identifies a different file object: the ACL check must fail closed"
        );
        drop(live_handle);
        let _ = std::fs::remove_file(&file);
        let _ = std::fs::remove_file(&renamed);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
