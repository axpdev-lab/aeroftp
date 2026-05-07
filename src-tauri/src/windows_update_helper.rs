//! Windows auto-update helper.
//!
//! Implements the install + restart half of the auto-updater for Windows,
//! across three install formats (MSI, NSIS .exe, portable ZIP). Contract:
//!
//!   1. Caller has downloaded the artifact and verified its Sigstore bundle.
//!   2. Caller invokes `install_with_helper(&app, format, downloaded_path)`.
//!   3. This module:
//!      - For MSI/NSIS: writes a `.cmd` helper script in `%TEMP%` that runs
//!        the silent installer, relaunches the new exe, and self-deletes.
//!      - For portable: extracts the ZIP into `%TEMP%`, writes a `.cmd`
//!        helper that renames the running exe to `*.old`, moves the new
//!        exe into place, copies marker/README/LICENSE, and relaunches
//!        the new exe with `--post-update-cleanup <old-exe-path>`.
//!   4. Helper is spawned with `CREATE_NO_WINDOW | DETACHED_PROCESS`.
//!   5. AeroFTP exits cleanly. The helper waits 2s, runs the install,
//!      relaunches, and `del` self.
//!
//! On startup, `try_handle_post_update_cleanup_arg` (called from `main.rs`)
//! parses `--post-update-cleanup <path>` and, in a detached thread, deletes
//! the leftover `*.old` exe with retry-and-backoff. Used only on the
//! portable path.

#![cfg(windows)]

use rand::Rng;
use std::path::{Path, PathBuf};

/// Generate a random hex suffix for temp filenames. Avoids races between
/// two AeroFTP instances trying to update at the same time.
fn random_suffix() -> String {
    let n: u64 = rand::thread_rng().r#gen();
    format!("{n:016x}")
}

/// Path to the per-user temp directory for staging update helpers.
fn temp_dir() -> PathBuf {
    std::env::temp_dir()
}

/// Convert a Rust path into a CMD-friendly quoted string. Doubles any
/// embedded `"` (rare in real paths) and wraps in `"..."`.
fn quote_for_cmd(path: &Path) -> String {
    let s = path.to_string_lossy().replace('"', "\"\"");
    format!("\"{s}\"")
}

/// Install-format dispatch. The `install_with_helper` entrypoint takes the
/// format as a string from the existing updater code; we centralise the
/// parsing here.
#[derive(Debug, Clone, Copy)]
enum InstallFormat {
    Msi,
    Nsis,
    Portable,
}

impl InstallFormat {
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "msi" => Ok(Self::Msi),
            "exe" => Ok(Self::Nsis),
            "portable" => Ok(Self::Portable),
            other => Err(format!("Unsupported Windows install format: {other}")),
        }
    }
}

/// Write a `.cmd` helper script to `%TEMP%\aeroftp-update-<rand>.cmd`,
/// spawn it detached, and return so the caller can exit. The helper handles
/// the rest of the upgrade.
///
/// `downloaded_path` for MSI/NSIS points at the installer artifact; for
/// portable it points at the downloaded ZIP, which this function extracts
/// into a sibling temp directory before scripting the swap.
pub fn install_with_helper(
    app: &tauri::AppHandle,
    format: &str,
    downloaded_path: &Path,
) -> Result<(), String> {
    let format = InstallFormat::from_str(format)?;

    if !downloaded_path.exists() {
        return Err(format!(
            "Downloaded file not found: {}",
            downloaded_path.display()
        ));
    }

    let exe_path = std::env::current_exe()
        .map_err(|e| format!("Cannot resolve current exe path: {e}"))?;

    let script = match format {
        InstallFormat::Msi => write_msi_helper(downloaded_path, &exe_path)?,
        InstallFormat::Nsis => write_nsis_helper(downloaded_path, &exe_path)?,
        InstallFormat::Portable => {
            let staged = stage_portable_artifact(downloaded_path)?;
            write_portable_helper(&staged, &exe_path)?
        }
    };

    log::info!(
        "Spawning Windows update helper: {} (format={:?})",
        script.display(),
        format
    );

    spawn_detached(&script)?;

    // Notify frontend before we drop the AppHandle. Same phase emitted by
    // the Linux paths so the UI state machine stays uniform.
    let _ = tauri::Emitter::emit(app, "update_install_phase", "restart");

    Ok(())
}

/// MSI helper: silent upgrade with progress UI, relaunch.
///
/// `/qb` (basic UI) is the recommended setting for app-driven upgrades:
/// the user sees the progress dialog so they know something is happening,
/// but no input is required. `/norestart` + `REBOOT=ReallySuppress`
/// blocks the OS reboot Windows Installer can request.
fn write_msi_helper(msi_path: &Path, exe_path: &Path) -> Result<PathBuf, String> {
    let script_path = temp_dir().join(format!("aeroftp-update-{}.cmd", random_suffix()));

    let body = format!(
        r#"@echo off
setlocal
rem AeroFTP MSI auto-update helper. Self-deletes on completion.

rem Wait for the parent AeroFTP process to release file locks.
ping 127.0.0.1 -n 3 >nul

rem Run the MSI silently with basic UI (progress dialog, no prompts).
msiexec.exe /i {msi} /qb /norestart REBOOT=ReallySuppress

if errorlevel 1 (
    rem Install failed: leave the helper visible in TEMP for diagnosis
    rem and skip the restart.
    exit /b %errorlevel%
)

rem Relaunch the new AeroFTP. msiexec installs over the existing path,
rem so the same exe location is now updated.
start "" {exe}

rem Self-delete the helper script.
del /F /Q "%~f0"
endlocal
"#,
        msi = quote_for_cmd(msi_path),
        exe = quote_for_cmd(exe_path),
    );

    std::fs::write(&script_path, body)
        .map_err(|e| format!("Failed to write MSI helper script: {e}"))?;
    Ok(script_path)
}

/// NSIS helper: silent install, relaunch.
///
/// Tauri's NSIS template + our `installer/hooks.nsh` already handle silent
/// mode correctly (`IfSilent` skips data-cleanup prompts). `/S` is the
/// stock silent flag.
fn write_nsis_helper(setup_path: &Path, exe_path: &Path) -> Result<PathBuf, String> {
    let script_path = temp_dir().join(format!("aeroftp-update-{}.cmd", random_suffix()));

    let body = format!(
        r#"@echo off
setlocal
rem AeroFTP NSIS auto-update helper. Self-deletes on completion.

ping 127.0.0.1 -n 3 >nul

rem /S = silent. NSIS hooks (PATH, .aerovault assoc, VC++ runtime) still run.
{setup} /S

if errorlevel 1 (
    exit /b %errorlevel%
)

rem Relaunch from the same install location (NSIS overwrites in place).
start "" {exe}

del /F /Q "%~f0"
endlocal
"#,
        setup = quote_for_cmd(setup_path),
        exe = quote_for_cmd(exe_path),
    );

    std::fs::write(&script_path, body)
        .map_err(|e| format!("Failed to write NSIS helper script: {e}"))?;
    Ok(script_path)
}

/// Staged portable artifact: paths to the new exe + ancillary files
/// extracted from the downloaded ZIP.
struct StagedPortable {
    new_exe: PathBuf,
    new_marker: Option<PathBuf>,
    new_readme: Option<PathBuf>,
    new_license: Option<PathBuf>,
}

/// Extract the portable ZIP into `%TEMP%\aeroftp-update-stage-<rand>\`
/// and locate the inner files. The CI ZIP layout is flat:
///   AeroFTP.exe
///   portable.marker
///   README.txt
///   LICENSE.txt
fn stage_portable_artifact(zip_path: &Path) -> Result<StagedPortable, String> {
    let stage_dir = temp_dir().join(format!("aeroftp-update-stage-{}", random_suffix()));
    std::fs::create_dir_all(&stage_dir)
        .map_err(|e| format!("Failed to create stage dir: {e}"))?;

    let file = std::fs::File::open(zip_path)
        .map_err(|e| format!("Failed to open portable ZIP: {e}"))?;
    let mut zip = zip::ZipArchive::new(file)
        .map_err(|e| format!("Failed to read portable ZIP: {e}"))?;

    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .map_err(|e| format!("Failed to read ZIP entry {i}: {e}"))?;

        // Defensive: reject path traversal in the ZIP. The portable ZIP we
        // ship has flat entries only — anything with `..` or absolute paths
        // is a tampered artifact.
        let name = entry.name();
        if name.contains("..") || name.starts_with('/') || name.starts_with('\\') {
            return Err(format!("ZIP entry rejects traversal: {name}"));
        }

        let target = stage_dir.join(name);
        if entry.is_dir() {
            std::fs::create_dir_all(&target)
                .map_err(|e| format!("Failed to create dir from ZIP: {e}"))?;
            continue;
        }

        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create parent dir: {e}"))?;
        }
        let mut out = std::fs::File::create(&target)
            .map_err(|e| format!("Failed to create staged file: {e}"))?;
        std::io::copy(&mut entry, &mut out)
            .map_err(|e| format!("Failed to extract ZIP entry: {e}"))?;
    }

    // Locate AeroFTP.exe at the top of the stage directory. The CI ZIP is
    // flat, but we walk one level just in case future builds nest under a
    // version folder.
    let new_exe = find_first_exe(&stage_dir)?;

    let pick_optional = |name: &str| -> Option<PathBuf> {
        let p = stage_dir.join(name);
        p.is_file().then_some(p)
    };

    Ok(StagedPortable {
        new_exe,
        new_marker: pick_optional("portable.marker"),
        new_readme: pick_optional("README.txt"),
        new_license: pick_optional("LICENSE.txt"),
    })
}

fn find_first_exe(dir: &Path) -> Result<PathBuf, String> {
    for entry in std::fs::read_dir(dir)
        .map_err(|e| format!("Failed to read stage dir: {e}"))?
        .flatten()
    {
        let path = entry.path();
        if path.is_file()
            && path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("exe"))
                .unwrap_or(false)
        {
            return Ok(path);
        }
    }
    Err("No .exe found in extracted portable ZIP".to_string())
}

/// Portable helper: rename old exe, move new exe into place, copy marker
/// and ancillary files, relaunch with --post-update-cleanup.
fn write_portable_helper(staged: &StagedPortable, current_exe: &Path) -> Result<PathBuf, String> {
    let script_path = temp_dir().join(format!("aeroftp-update-{}.cmd", random_suffix()));

    let exe_dir = current_exe
        .parent()
        .ok_or_else(|| "Cannot resolve current exe parent dir".to_string())?;
    let old_exe = current_exe.with_extension("exe.old");

    let mut copy_block = String::new();
    if let Some(m) = &staged.new_marker {
        copy_block.push_str(&format!(
            "copy /Y {src} {dst} >nul\n",
            src = quote_for_cmd(m),
            dst = quote_for_cmd(&exe_dir.join("portable.marker"))
        ));
    }
    if let Some(r) = &staged.new_readme {
        copy_block.push_str(&format!(
            "copy /Y {src} {dst} >nul\n",
            src = quote_for_cmd(r),
            dst = quote_for_cmd(&exe_dir.join("README.txt"))
        ));
    }
    if let Some(l) = &staged.new_license {
        copy_block.push_str(&format!(
            "copy /Y {src} {dst} >nul\n",
            src = quote_for_cmd(l),
            dst = quote_for_cmd(&exe_dir.join("LICENSE.txt"))
        ));
    }

    let body = format!(
        r#"@echo off
setlocal enableextensions
rem AeroFTP portable in-place update helper. Renames the running exe to
rem .old (Windows allows rename of a running exe but not delete), moves
rem the new exe into place, copies the new marker/README/LICENSE, then
rem launches the new exe with --post-update-cleanup pointing at the .old
rem so the new process can delete it once the old process is fully gone.

ping 127.0.0.1 -n 3 >nul

rem Retry the rename up to 5 times with backoff: another file lock can
rem race us briefly even after the parent process exits.
set RETRY=0
:rename_loop
move /Y {current} {old} >nul 2>&1
if not errorlevel 1 goto rename_done
set /a RETRY+=1
if %RETRY% GEQ 5 goto rename_failed
ping 127.0.0.1 -n 2 >nul
goto rename_loop

:rename_failed
rem Rename never succeeded: leave everything as-is, nothing was modified.
exit /b 1

:rename_done
rem Move the new exe into place.
move /Y {new_exe} {current} >nul 2>&1
if errorlevel 1 (
    rem Restore the old exe so the user has a working binary.
    move /Y {old} {current} >nul 2>&1
    exit /b 1
)

rem Copy ancillary files (marker, README, LICENSE) over the old ones.
{copy_block}
rem Launch the new exe with cleanup arg pointing at the .old file.
start "" {current} --post-update-cleanup {old}

del /F /Q "%~f0"
endlocal
"#,
        current = quote_for_cmd(current_exe),
        old = quote_for_cmd(&old_exe),
        new_exe = quote_for_cmd(&staged.new_exe),
        copy_block = copy_block,
    );

    std::fs::write(&script_path, body)
        .map_err(|e| format!("Failed to write portable helper script: {e}"))?;
    Ok(script_path)
}

/// Spawn the helper script detached: no console window, no parent wait.
/// `CREATE_NO_WINDOW` (0x08000000) suppresses the console; `DETACHED_PROCESS`
/// (0x00000008) detaches from the parent's console group.
fn spawn_detached(script: &Path) -> Result<(), String> {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x08000000;
    const DETACHED_PROCESS: u32 = 0x00000008;

    std::process::Command::new("cmd.exe")
        .args(["/C", "call"])
        .arg(script)
        .creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("Failed to spawn helper: {e}"))
}

// ===========================================================================
// Post-update cleanup (called from main.rs on startup)
// ===========================================================================

const CLEANUP_FLAG: &str = "--post-update-cleanup";

/// Walk argv looking for `--post-update-cleanup <path>`. If present, spawn
/// a detached thread that retries `remove_file` for up to 30 seconds with
/// 2-second intervals, then schedules `MoveFileEx(MOVEFILE_DELAY_UNTIL_REBOOT)`
/// as a last resort.
///
/// Called from `main.rs` before `run()` so the cleanup runs in the
/// background while Tauri starts up.
pub fn try_handle_post_update_cleanup_arg() {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == CLEANUP_FLAG {
            if let Some(target) = args.next() {
                let target = PathBuf::from(target);
                std::thread::spawn(move || cleanup_old_exe(&target));
            }
            return;
        }
    }
}

/// Retry-with-backoff delete of the `.old` exe left behind by the portable
/// update helper. Final fallback: schedule deletion at next reboot.
fn cleanup_old_exe(path: &Path) {
    if !path.exists() {
        return;
    }

    // Wait 2s for the cmd helper to fully release any handles.
    std::thread::sleep(std::time::Duration::from_secs(2));

    for attempt in 1..=15 {
        match std::fs::remove_file(path) {
            Ok(_) => {
                log::info!("post-update cleanup: removed {}", path.display());
                return;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(e) => {
                log::debug!(
                    "post-update cleanup: attempt {} failed for {}: {}",
                    attempt,
                    path.display(),
                    e
                );
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
        }
    }

    // Last resort: schedule deletion at next reboot via Win32 API.
    schedule_deletion_at_reboot(path);
}

/// `MoveFileExW(target, NULL, MOVEFILE_DELAY_UNTIL_REBOOT)` queues the
/// file for deletion at the next OS startup. Used when we can't get a
/// delete handle within 30 seconds (rare; usually means the user's AV
/// is still scanning the .old).
fn schedule_deletion_at_reboot(path: &Path) {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_DELAY_UNTIL_REBOOT};

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let ok = unsafe {
        MoveFileExW(
            PCWSTR::from_raw(wide.as_ptr()),
            PCWSTR::null(),
            MOVEFILE_DELAY_UNTIL_REBOOT,
        )
    };
    match ok {
        Ok(()) => log::warn!(
            "post-update cleanup: scheduled deletion at next reboot for {}",
            path.display()
        ),
        Err(e) => log::error!(
            "post-update cleanup: schedule-at-reboot failed for {}: {:?}",
            path.display(),
            e
        ),
    }
}
