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
//!      - For portable: extracts the ZIP into `%TEMP%`, copies the new app
//!        and CLI exes next to the installed ones as `*.new`, and writes a
//!        `.cmd` helper that replaces the app exe with its copy in one
//!        rename once the old process has exited, then the CLI exes, copies
//!        marker/README/LICENSE, and relaunches the new exe with
//!        `--post-update-cleanup <old-exe-path>`.
//!   4. Helper is spawned with `CREATE_NO_WINDOW | DETACHED_PROCESS`.
//!   5. AeroFTP exits cleanly. The helper waits 2s, runs the install,
//!      relaunches, and `del` self.
//!
//! On startup, `try_handle_post_update_cleanup_arg` (called from `main.rs`)
//! parses `--post-update-cleanup <path>` and, in a detached thread, deletes
//! the leftover `*.old` exe with retry-and-backoff. Used only on the
//! portable path: the helper no longer makes a `*.old`, but the one an
//! earlier version's helper left is still removed this way.

#![cfg(windows)]

use rand::Rng;
use std::ffi::OsStr;
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

    let exe_path =
        std::env::current_exe().map_err(|e| format!("Cannot resolve current exe path: {e}"))?;

    let script = match format {
        InstallFormat::Msi => write_msi_helper(downloaded_path, &exe_path)?,
        InstallFormat::Nsis => write_nsis_helper(downloaded_path, &exe_path)?,
        InstallFormat::Portable => {
            let app_exe_name = exe_path
                .file_name()
                .ok_or_else(|| "Cannot resolve current exe file name".to_string())?;
            let staged = stage_portable_artifact(downloaded_path, app_exe_name)?;
            let cli = stage_update(&staged, &exe_path)?;
            let log_file = crate::portable::log_dir().map(|dir| dir.join(APP_LOG_FILE));
            write_portable_helper(
                &staged,
                &exe_path,
                &cli,
                log_file.as_deref(),
                downloaded_path,
            )
            .inspect_err(|_| discard_staged(&exe_path, &cli))?
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
/// extracted from the downloaded ZIP, plus the stage dir itself so the
/// cmd helper can wipe it after the swap (issue #176).
struct StagedPortable {
    new_exe: PathBuf,
    new_marker: Option<PathBuf>,
    new_readme: Option<PathBuf>,
    new_license: Option<PathBuf>,
    stage_dir: PathBuf,
}

/// Extract the portable ZIP into `%TEMP%\aeroftp-update-stage-<rand>\`
/// and locate the inner files. The CI ZIP layout is flat:
///   AeroFTP.exe
///   aeroftp-cli.exe
///   aeroftp-dispatch.exe
///   portable.marker
///   README.txt
///   LICENSE.txt
///
/// `app_exe_name` is the file name of the running exe: the new app exe is
/// the entry with that name.
fn stage_portable_artifact(
    zip_path: &Path,
    app_exe_name: &OsStr,
) -> Result<StagedPortable, String> {
    let stage_dir = temp_dir().join(format!("aeroftp-update-stage-{}", random_suffix()));
    std::fs::create_dir_all(&stage_dir).map_err(|e| format!("Failed to create stage dir: {e}"))?;

    let file =
        std::fs::File::open(zip_path).map_err(|e| format!("Failed to open portable ZIP: {e}"))?;
    let mut zip =
        zip::ZipArchive::new(file).map_err(|e| format!("Failed to read portable ZIP: {e}"))?;

    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .map_err(|e| format!("Failed to read ZIP entry {i}: {e}"))?;

        // Defensive: reject path traversal in the ZIP. The portable ZIP we
        // ship has flat entries only — anything with `..` or absolute paths
        // is a tampered artifact.
        let name = entry
            .name()
            .map_err(|e| format!("Failed to read ZIP entry {i} name: {e}"))?
            .into_owned();
        if name.contains("..") || name.starts_with('/') || name.starts_with('\\') {
            return Err(format!("ZIP entry rejects traversal: {name}"));
        }

        let target = stage_dir.join(&name);
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

    // Locate the app exe at the top of the stage directory by name. The ZIP
    // also holds the CLI exes, and taking the first `.exe` took
    // `aeroftp-cli.exe`, which NTFS lists before `AeroFTP.exe`.
    let new_exe = match find_exe_named(&stage_dir, app_exe_name) {
        Ok(path) => path,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&stage_dir);
            return Err(e);
        }
    };

    let pick_optional = |name: &str| -> Option<PathBuf> {
        let p = stage_dir.join(name);
        p.is_file().then_some(p)
    };

    Ok(StagedPortable {
        new_exe,
        new_marker: pick_optional("portable.marker"),
        new_readme: pick_optional("README.txt"),
        new_license: pick_optional("LICENSE.txt"),
        stage_dir,
    })
}

/// The file in `dir` named `name`, compared without case as Windows does.
fn find_exe_named(dir: &Path, name: &OsStr) -> Result<PathBuf, String> {
    let wanted = name.to_string_lossy();
    for entry in std::fs::read_dir(dir)
        .map_err(|e| format!("Failed to read stage dir: {e}"))?
        .flatten()
    {
        let path = entry.path();
        if path.is_file()
            && entry
                .file_name()
                .to_string_lossy()
                .eq_ignore_ascii_case(&wanted)
        {
            return Ok(path);
        }
    }
    Err(format!("The portable update has no {wanted}"))
}

/// The CLI exes the portable ZIP ships next to the app (#340).
const PORTABLE_CLI_EXES: &[&str] = &["aeroftp-cli.exe", "aeroftp-dispatch.exe"];

/// The file the log plugin writes in `portable::log_dir()`, named after the
/// product. The helper appends to it when it keeps an installed CLI exe.
const APP_LOG_FILE: &str = "AeroFTP.log";

/// Stages every exe of the update next to the one it replaces: the app exe
/// and each CLI exe the ZIP holds. Staging touches none of the installed
/// exes. Any failure removes what was staged and stops the update, so it
/// installs the three exes of one version or none. Returns the CLI exes
/// staged, which the helper replaces once the app exe is the new one.
fn stage_update(staged: &StagedPortable, current_exe: &Path) -> Result<Vec<&'static str>, String> {
    let exe_dir = current_exe
        .parent()
        .ok_or_else(|| "Cannot resolve current exe parent dir".to_string())?;
    stage_beside(&staged.new_exe, current_exe)?;
    let mut cli = Vec::new();
    for &name in PORTABLE_CLI_EXES {
        let source = staged.stage_dir.join(name);
        if !source.is_file() {
            continue;
        }
        if let Err(e) = stage_beside(&source, &exe_dir.join(name)) {
            discard_staged(current_exe, &cli);
            return Err(format!("{name}: {e}"));
        }
        cli.push(name);
    }
    Ok(cli)
}

/// Removes the copies `stage_update` put next to the installed exes.
fn discard_staged(current_exe: &Path, cli: &[&str]) {
    let _ = std::fs::remove_file(beside_path(current_exe));
    if let Some(exe_dir) = current_exe.parent() {
        for name in cli {
            let _ = std::fs::remove_file(beside_path(&exe_dir.join(name)));
        }
    }
}

/// Where the new exe waits next to the running one until the helper puts it
/// in place: same folder, so the same volume, so that last step is a rename.
fn beside_path(current_exe: &Path) -> PathBuf {
    current_exe.with_extension("exe.new")
}

/// Copies the new exe from the stage dir to `beside_path(current_exe)`,
/// flushes it and checks that every byte arrived. The stage dir is in
/// `%TEMP%`, possibly on another volume, where a move is a copy: done by the
/// helper over the launch path, a stop halfway left a partial exe there, or
/// none. Done here, the exe at the launch path is not touched at all.
///
/// The staged name is fixed and sits in the exe's folder. Whatever is there
/// already (a copy a stopped update left, or a link) is removed, and the copy
/// goes only into a file this call creates. Any failure removes the staged
/// copy.
fn stage_beside(new_exe: &Path, current_exe: &Path) -> Result<PathBuf, String> {
    let staged = beside_path(current_exe);
    let copied = (|| {
        match std::fs::remove_file(&staged) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("Failed to clear a stale staged exe: {e}")),
        }
        let mut source =
            std::fs::File::open(new_exe).map_err(|e| format!("Failed to read the new exe: {e}"))?;
        let expected = source
            .metadata()
            .map_err(|e| format!("Failed to read the new exe: {e}"))?
            .len();
        let mut out = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staged)
            .map_err(|e| format!("Failed to stage the new exe: {e}"))?;
        std::io::copy(&mut source, &mut out)
            .map_err(|e| format!("Failed to stage the new exe: {e}"))?;
        out.sync_all()
            .map_err(|e| format!("Failed to flush the staged exe: {e}"))?;
        let written = out
            .metadata()
            .map_err(|e| format!("Failed to check the staged exe: {e}"))?
            .len();
        if written != expected {
            return Err(format!(
                "Staged exe is {written} bytes, the update has {expected}"
            ));
        }
        Ok(())
    })();
    if let Err(e) = copied {
        let _ = std::fs::remove_file(&staged);
        return Err(e);
    }
    Ok(staged)
}

/// Tries the helper gives a staged CLI exe. The app exe gets 30, because it
/// waits for the old process to exit, which takes seconds. A CLI exe still in
/// use is a mount or a sync that will not end in that time, and every try
/// delays the relaunch by a second.
const CLI_ATTEMPTS: u32 = 5;

/// Portable helper: replace the exe with the copy `stage_update` put next to
/// it, then the CLI exes the same way, copy marker and ancillary files,
/// relaunch with --post-update-cleanup, and wipe the staged ZIP + extraction
/// dir so nothing leaks into Downloads/TEMP after the swap (issue #176:
/// portable contract is "no-trace").
fn write_portable_helper(
    staged: &StagedPortable,
    current_exe: &Path,
    cli: &[&str],
    log_file: Option<&Path>,
    downloaded_zip: &Path,
) -> Result<PathBuf, String> {
    let script_path = temp_dir().join(format!("aeroftp-update-{}.cmd", random_suffix()));
    let body = portable_helper_script(staged, current_exe, cli, log_file, downloaded_zip)?;
    std::fs::write(&script_path, body)
        .map_err(|e| format!("Failed to write portable helper script: {e}"))?;
    Ok(script_path)
}

fn portable_helper_script(
    staged: &StagedPortable,
    current_exe: &Path,
    cli: &[&str],
    log_file: Option<&Path>,
    downloaded_zip: &Path,
) -> Result<String, String> {
    let exe_dir = current_exe
        .parent()
        .ok_or_else(|| "Cannot resolve current exe parent dir".to_string())?;
    let old_exe = current_exe.with_extension("exe.old");

    // Every staged copy goes when the app exe cannot be replaced: then the
    // installed exes all stay as they were.
    let mut discard_block = format!(
        "del /F /Q {} >nul 2>&1\n",
        quote_for_cmd(&beside_path(current_exe))
    );
    // Each CLI exe is replaced only after the app exe, so a failed app swap
    // leaves all three at the installed version. One still in use keeps the
    // installed exe, and the app log says so.
    let mut cli_block = String::new();
    for (i, name) in cli.iter().enumerate() {
        let target = exe_dir.join(name);
        let new = quote_for_cmd(&beside_path(&target));
        discard_block.push_str(&format!("del /F /Q {new} >nul 2>&1\n"));
        // A log the restarted app or another instance holds refuses the
        // append; the outer group swallows cmd's error, and the helper goes
        // on with its exit code unchanged.
        let log_line = match log_file {
            Some(log) => format!(
                "if exist {dir} ((echo [windows_update_helper][WARN] portable update: {name} is in use, kept the installed one)>>{log}) 2>nul\n",
                dir = quote_for_cmd(log.parent().unwrap_or(exe_dir)),
                log = quote_for_cmd(log),
            ),
            None => String::new(),
        };
        cli_block.push_str(&format!(
            r#"set RETRY=0
:cli_{i}
move /Y {new} {target} >nul 2>&1
if not errorlevel 1 goto cli_{i}_done
set /a RETRY+=1
if %RETRY% GEQ {CLI_ATTEMPTS} goto cli_{i}_busy
ping 127.0.0.1 -n 2 >nul
goto cli_{i}
:cli_{i}_busy
del /F /Q {new} >nul 2>&1
{log_line}:cli_{i}_done
"#,
            target = quote_for_cmd(&target),
        ));
    }

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
rem AeroFTP portable in-place update helper. The updater has already
rem copied the new exes, whole, next to the installed ones. This helper
rem replaces the app exe with its copy in one rename on the same volume,
rem so the launch path names the old exe or the new one at every instant,
rem then the CLI exes the same way, copies the new marker/README/LICENSE,
rem and launches the new exe. The cleanup argument removes a .old an
rem earlier version's helper may have left. After the swap the staged ZIP
rem and extraction dir are also wiped so the portable update leaves
rem nothing behind (issue #176).

ping 127.0.0.1 -n 3 >nul

if not exist {new_exe} goto replace_failed

rem Windows refuses to replace an exe a process still runs from, so retry
rem the replace, about once a second for 30 seconds, until the old
rem AeroFTP has exited. `move /Y` within one folder is a single rename that
rem replaces the target.
set RETRY=0
:replace_loop
move /Y {new_exe} {current} >nul 2>&1
if not errorlevel 1 goto replace_done
set /a RETRY+=1
if %RETRY% GEQ 30 goto replace_failed
ping 127.0.0.1 -n 2 >nul
goto replace_loop

:replace_failed
rem The old exe never left the launch path: drop the staged copies, the
rem CLI exes stay at the same version as the app.
{discard_block}exit /b 1

:replace_done
rem The app exe is the new one: replace the CLI exes.
{cli_block}
rem Copy ancillary files (marker, README, LICENSE) over the old ones.
{copy_block}
rem Launch the new exe with cleanup arg pointing at the .old file.
start "" {current} --post-update-cleanup {old}

rem Wipe staged artifacts. Best-effort: a leftover here is invisible to
rem the user (cache dir + temp dir), but tidy is part of the portable
rem promise. `del` and `rmdir` swallow errors when the path is missing.
rem The sigstore sidecar (`<zip>.sigstore.json`) is normally removed by
rem download_update post-verification, but if the flow is interrupted
rem between download and removal the sidecar can survive. Clean it
rem here too so the staging dir reaches steady state == empty.
del /F /Q {zip} >nul 2>&1
del /F /Q {sigstore} >nul 2>&1
rmdir /S /Q {stage} >nul 2>&1

del /F /Q "%~f0"
endlocal
"#,
        current = quote_for_cmd(current_exe),
        old = quote_for_cmd(&old_exe),
        new_exe = quote_for_cmd(&beside_path(current_exe)),
        discard_block = discard_block,
        cli_block = cli_block,
        copy_block = copy_block,
        zip = quote_for_cmd(downloaded_zip),
        sigstore = quote_for_cmd(&downloaded_zip.with_file_name(format!(
            "{}.sigstore.json",
            downloaded_zip.file_name().and_then(|n| n.to_str()).unwrap_or("")
        ))),
        stage = quote_for_cmd(&staged.stage_dir),
    );
    // CRLF: cmd finds a `goto` label by scanning the file, and in an LF-only
    // file it misses one at some offsets ("cannot find the batch label"),
    // which stopped this helper after the app swap with the CLI exes not
    // replaced. Measured on this script under %TEMP%: 7 of 150 folder-name
    // lengths with LF, none with CRLF.
    Ok(body.replace('\n', "\r\n"))
}

/// Spawn the helper script with the console suppressed.
///
/// Prior versions combined `CREATE_NO_WINDOW | DETACHED_PROCESS`, which on a
/// console-subsystem child (cmd.exe) produced a race in console allocation:
/// the window flashed for a frame before being destroyed, so the user saw a
/// black box appear and disappear during the auto-update. Microsoft's process-
/// creation-flags doc treats the two flags as effectively conflicting for
/// console children.
///
/// Candidate A: keep `CREATE_NO_WINDOW`, drop `DETACHED_PROCESS`, and pin
/// stdio to null so the helper does not inherit the parent's console handles.
/// The parent (AeroFTP) exits within milliseconds anyway, so true detachment
/// is not required: the child outlives the parent once the parent's console
/// handle is closed.
fn spawn_detached(script: &Path) -> Result<(), String> {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x08000000;

    std::process::Command::new("cmd.exe")
        .args(["/C", "call"])
        .arg(script)
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
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

    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
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

#[cfg(test)]
mod tests {
    use super::*;

    const OLD_EXE: &[u8] = b"old exe";
    const NEW_EXE: &[u8] = b"new exe, longer";

    /// A portable install and a staged update, laid out as the updater finds
    /// them: the exe in its folder, the extracted ZIP in a stage folder.
    struct Fixture {
        root: tempfile::TempDir,
        app: PathBuf,
        current: PathBuf,
        staged: StagedPortable,
        zip: PathBuf,
        log: PathBuf,
    }

    fn fixture() -> Fixture {
        let root = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir(root.path().join("logs")).expect("log dir");
        let log = root.path().join("logs").join(APP_LOG_FILE);
        let app = root.path().join("app");
        std::fs::create_dir(&app).expect("app dir");
        let current = app.join("AeroFTP.exe");
        std::fs::write(&current, OLD_EXE).expect("old exe");
        let stage_dir = root.path().join("stage");
        std::fs::create_dir(&stage_dir).expect("stage dir");
        let new_exe = stage_dir.join("AeroFTP.exe");
        std::fs::write(&new_exe, NEW_EXE).expect("new exe");
        let marker = stage_dir.join("portable.marker");
        std::fs::write(&marker, b"portable").expect("marker");
        let zip = root.path().join("AeroFTP_portable.zip");
        std::fs::write(&zip, b"zip").expect("zip");
        Fixture {
            root,
            app,
            current,
            staged: StagedPortable {
                new_exe,
                new_marker: Some(marker),
                new_readme: None,
                new_license: None,
                stage_dir,
            },
            zip,
            log,
        }
    }

    /// The entries of the portable ZIP the CI builds, in its order (build.yml,
    /// "Create Windows portable ZIP").
    const CI_ZIP_ENTRIES: [(&str, &[u8]); 6] = [
        ("AeroFTP.exe", NEW_EXE),
        ("aeroftp-cli.exe", b"new cli"),
        ("aeroftp-dispatch.exe", b"new dispatch"),
        ("portable.marker", b"portable"),
        ("README.txt", b"readme"),
        ("LICENSE.txt", b"license"),
    ];

    fn portable_zip(dir: &Path, entries: &[(&str, &[u8])]) -> PathBuf {
        use std::io::Write;
        let path = dir.join(format!("portable-{}.zip", entries.len()));
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).expect("zip"));
        for (name, bytes) in entries {
            zip.start_file(*name, zip::write::SimpleFileOptions::default())
                .expect("entry");
            zip.write_all(bytes).expect("write entry");
        }
        zip.finish().expect("finish zip");
        path
    }

    /// Each CLI exe with its installed and its updated content.
    const CLI_FILES: [(&str, &[u8], &[u8]); 2] = [
        ("aeroftp-cli.exe", b"old cli", b"new cli"),
        ("aeroftp-dispatch.exe", b"old dispatch", b"new dispatch"),
    ];

    /// Installed CLI exes next to the app, and the update's in the stage.
    fn with_cli(fx: &Fixture) {
        for (name, old, new) in CLI_FILES {
            std::fs::write(fx.app.join(name), old).expect("installed cli");
            std::fs::write(fx.staged.stage_dir.join(name), new).expect("staged cli");
        }
    }

    /// What `install_with_helper` does before it spawns the helper: stage the
    /// new exes next to the installed ones, then write the script.
    fn script_for(fx: &Fixture) -> String {
        let cli = stage_update(&fx.staged, &fx.current).expect("staged");
        portable_helper_script(&fx.staged, &fx.current, &cli, Some(&fx.log), &fx.zip)
            .expect("script")
    }

    fn read(fx: &Fixture, name: &str) -> Vec<u8> {
        std::fs::read(fx.app.join(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
    }

    const THREE_EXES: [&str; 3] = ["AeroFTP.exe", "aeroftp-cli.exe", "aeroftp-dispatch.exe"];

    /// The script as the helper runs it, minus the waits (`ping`) and the
    /// relaunch (`start`), which would launch the test's fake exe.
    fn runnable(script: &str) -> Vec<String> {
        script
            .split_inclusive('\n')
            .map(|line| {
                let trimmed = line.trim_start();
                if trimmed.starts_with("ping ") || trimmed.starts_with("start ") {
                    "rem\r\n".to_string()
                } else {
                    line.to_string()
                }
            })
            .collect()
    }

    fn run(fx: &Fixture, lines: &[String]) -> std::process::ExitStatus {
        let script = fx.root.path().join("helper.cmd");
        std::fs::write(&script, lines.concat()).expect("write script");
        std::process::Command::new("cmd.exe")
            .args(["/D", "/C", "call"])
            .arg(&script)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("run cmd.exe")
    }

    fn names_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .expect("read dir")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }

    /// The helper can be stopped between any two of its lines (killed, logged
    /// off, power lost). Each run below stops it after one more line. At every
    /// point the launch path has to hold a whole exe, the old one or the new
    /// one, and so does each CLI exe; and while the app exe is the old one the
    /// CLI exes are too. The helper used to move the running exe to `.old`
    /// first and then move the new one in from %TEMP%: stopped in between,
    /// there was no exe to start.
    #[test]
    fn the_launch_path_holds_a_whole_exe_wherever_the_helper_stops() {
        let first = fixture();
        with_cli(&first);
        let lines = runnable(&script_for(&first));
        for stop_after in 0..=lines.len() {
            let fx = fixture();
            with_cli(&fx);
            let lines = runnable(&script_for(&fx));
            run(&fx, &lines[..stop_after]);
            let at = |line: usize| {
                format!(
                    "stopped after line {line}: {:?}",
                    lines.get(line.wrapping_sub(1))
                )
            };
            let app = std::fs::read(&fx.current)
                .unwrap_or_else(|_| panic!("no exe at the launch path, {}", at(stop_after)));
            assert!(
                app == OLD_EXE || app == NEW_EXE,
                "a partial exe at the launch path, {}",
                at(stop_after)
            );
            for (name, old, new) in CLI_FILES {
                let cli = read(&fx, name);
                assert!(
                    cli == old || cli == new,
                    "{name} partial, {}",
                    at(stop_after)
                );
                if app == OLD_EXE {
                    assert_eq!(cli, old, "{name} newer than the app, {}", at(stop_after));
                }
            }
        }
    }

    /// Run to the end, the update leaves the three new exes and the new marker
    /// in the app folder and nothing else, and removes the download and the
    /// stage.
    #[test]
    fn a_complete_run_installs_the_new_exes_and_leaves_nothing_beside_them() {
        let fx = fixture();
        with_cli(&fx);
        let script = script_for(&fx);
        assert!(
            script.contains(&format!(
                "start \"\" {} --post-update-cleanup",
                quote_for_cmd(&fx.current)
            )),
            "the helper relaunches the exe at the launch path"
        );
        run(&fx, &runnable(&script));
        assert_eq!(read(&fx, "AeroFTP.exe"), NEW_EXE);
        assert_eq!(read(&fx, "aeroftp-cli.exe"), b"new cli");
        assert_eq!(read(&fx, "aeroftp-dispatch.exe"), b"new dispatch");
        let mut expected: Vec<String> = THREE_EXES.iter().map(|n| n.to_string()).collect();
        expected.push("portable.marker".to_string());
        assert_eq!(names_in(&fx.app), expected);
        assert!(!fx.zip.exists());
        assert!(!fx.staged.stage_dir.exists());
        assert!(!fx.log.exists(), "nothing to log");
    }

    /// Every line of the helper ends in CRLF. With LF only, cmd missed the
    /// `replace_done` label for some folder-name lengths, the tempfile one
    /// among them, and stopped after the app swap.
    #[test]
    fn the_helper_script_ends_every_line_with_crlf() {
        let fx = fixture();
        with_cli(&fx);
        let script = script_for(&fx);
        let bare_lf = script
            .match_indices('\n')
            .filter(|(i, _)| *i == 0 || script.as_bytes()[i - 1] != b'\r')
            .count();
        assert_eq!(bare_lf, 0);
        assert!(script.ends_with("\r\n"));
    }

    /// Holds a file the way a running exe is held for this purpose: a handle
    /// that does not share delete refuses a replace, as a mapped image does.
    fn hold(path: &Path) -> std::fs::File {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 0x1;
        std::fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(path)
            .expect("hold")
    }

    /// The old process never lets go of its exe. The helper gives up with the
    /// old app exe where it was, leaves the CLI exes at the same old version,
    /// and removes every staged copy.
    #[test]
    fn a_failed_app_swap_leaves_every_exe_as_it_was() {
        let fx = fixture();
        with_cli(&fx);
        let script = script_for(&fx);
        let held = hold(&fx.current);
        run(&fx, &runnable(&script));
        drop(held);
        assert_eq!(read(&fx, "AeroFTP.exe"), OLD_EXE);
        assert_eq!(read(&fx, "aeroftp-cli.exe"), b"old cli");
        assert_eq!(read(&fx, "aeroftp-dispatch.exe"), b"old dispatch");
        assert_eq!(names_in(&fx.app), THREE_EXES.map(String::from).to_vec());
    }

    /// The staged copy is whole, sits next to the exe, and staging does not
    /// touch the exe at the launch path. A copy a stopped update left at the
    /// staged name is replaced, not appended to or reused.
    #[test]
    fn staging_copies_the_whole_exe_beside_the_old_one() {
        let fx = fixture();
        std::fs::write(beside_path(&fx.current), b"a much longer stale copy").expect("stale");
        let staged = stage_beside(&fx.staged.new_exe, &fx.current).expect("staged");
        assert_eq!(staged, fx.app.join("AeroFTP.exe.new"));
        assert_eq!(std::fs::read(&staged).expect("staged"), NEW_EXE);
        assert_eq!(std::fs::read(&fx.current).expect("exe"), OLD_EXE);
    }

    /// The portable ZIP the CI builds (build.yml, "Create Windows portable
    /// ZIP") holds the app and, since #340, the two CLI exes. NTFS lists a
    /// folder in upcased name order and `-` sorts before `.`, so
    /// `aeroftp-cli.exe` comes before `AeroFTP.exe`.
    #[test]
    fn the_update_takes_the_app_exe_from_a_zip_that_also_holds_the_cli() {
        let root = tempfile::tempdir().expect("temp dir");
        let zip_path = portable_zip(root.path(), &CI_ZIP_ENTRIES);
        let staged = stage_portable_artifact(&zip_path, OsStr::new("AeroFTP.exe")).expect("staged");
        let _ = std::fs::remove_dir_all(&staged.stage_dir);
        assert_eq!(
            staged.new_exe.file_name().and_then(|n| n.to_str()),
            Some("AeroFTP.exe")
        );
    }

    /// The running exe's name is matched without case, as Windows does.
    /// A ZIP without it is refused, and its extraction is removed.
    #[test]
    fn the_app_exe_is_found_by_the_running_name_or_the_update_is_refused() {
        let root = tempfile::tempdir().expect("temp dir");
        let zip_path = portable_zip(root.path(), &CI_ZIP_ENTRIES);
        let staged = stage_portable_artifact(&zip_path, OsStr::new("aeroftp.EXE")).expect("staged");
        let new_exe = std::fs::read(&staged.new_exe).expect("new exe");
        let _ = std::fs::remove_dir_all(&staged.stage_dir);
        assert_eq!(new_exe, NEW_EXE);

        let without_app = portable_zip(root.path(), &CI_ZIP_ENTRIES[1..]);
        let refused = stage_portable_artifact(&without_app, OsStr::new("AeroFTP.exe"))
            .err()
            .expect("refused");
        assert!(refused.contains("AeroFTP.exe"), "{refused}");
    }

    /// A CLI exe held open (a mount still running) keeps the installed one,
    /// whole, with no staged copy beside it, and the app log says so. The app
    /// exe and the other CLI exe are still replaced.
    #[test]
    fn a_cli_exe_in_use_keeps_the_installed_one_and_the_log_says_so() {
        let fx = fixture();
        with_cli(&fx);
        let script = script_for(&fx);
        let held = hold(&fx.app.join("aeroftp-cli.exe"));
        run(&fx, &runnable(&script));
        drop(held);
        assert_eq!(read(&fx, "AeroFTP.exe"), NEW_EXE);
        assert_eq!(read(&fx, "aeroftp-cli.exe"), b"old cli");
        assert_eq!(read(&fx, "aeroftp-dispatch.exe"), b"new dispatch");
        assert!(!fx.app.join("aeroftp-cli.exe.new").exists());
        let log = std::fs::read_to_string(&fx.log).expect("log");
        assert_eq!(
            log.trim_end(),
            "[windows_update_helper][WARN] portable update: aeroftp-cli.exe is in use, kept the installed one"
        );
    }

    /// The app log held without sharing refuses the helper's line. The helper
    /// still runs to its end, with the exit code of the same run with the log
    /// free: the other exes are replaced, the marker copied and the stage
    /// removed.
    #[test]
    fn a_log_that_cannot_be_written_does_not_stop_the_helper() {
        use std::os::windows::fs::OpenOptionsExt;
        let free = fixture();
        with_cli(&free);
        let script = script_for(&free);
        let held_cli = hold(&free.app.join("aeroftp-cli.exe"));
        let free_status = run(&free, &runnable(&script));
        drop(held_cli);
        assert!(std::fs::read_to_string(&free.log)
            .expect("log")
            .contains("aeroftp-cli.exe is in use"));

        let fx = fixture();
        with_cli(&fx);
        std::fs::write(&fx.log, b"").expect("log");
        let script = script_for(&fx);
        let held_cli = hold(&fx.app.join("aeroftp-cli.exe"));
        let held_log = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(0)
            .open(&fx.log)
            .expect("hold the log");
        let status = run(&fx, &runnable(&script));
        drop(held_log);
        drop(held_cli);
        assert_eq!(status.code(), free_status.code());
        assert_eq!(read(&fx, "aeroftp-cli.exe"), b"old cli");
        assert_eq!(read(&fx, "aeroftp-dispatch.exe"), b"new dispatch");
        assert_eq!(read(&fx, "portable.marker"), b"portable");
        assert!(!fx.staged.stage_dir.exists());
        assert_eq!(std::fs::read(&fx.log).expect("log"), b"");
    }

    /// An update without the CLI exes leaves the installed ones alone.
    #[test]
    fn an_update_without_the_cli_leaves_the_installed_cli() {
        let fx = fixture();
        with_cli(&fx);
        for (name, _, _) in CLI_FILES {
            std::fs::remove_file(fx.staged.stage_dir.join(name)).expect("rm");
        }
        let script = script_for(&fx);
        assert!(!script.contains("aeroftp-cli"), "{script}");
        run(&fx, &runnable(&script));
        assert_eq!(read(&fx, "AeroFTP.exe"), NEW_EXE);
        assert_eq!(read(&fx, "aeroftp-cli.exe"), b"old cli");
        assert_eq!(read(&fx, "aeroftp-dispatch.exe"), b"old dispatch");
    }

    /// A CLI exe that cannot be staged stops the update before the helper
    /// exists, with every copy staged so far removed: one version or none.
    #[test]
    fn a_cli_exe_that_cannot_be_staged_stops_the_update() {
        let fx = fixture();
        with_cli(&fx);
        // A folder at the staged name: it cannot be cleared or created.
        std::fs::create_dir(fx.app.join("aeroftp-dispatch.exe.new")).expect("blocker");
        let refused = stage_update(&fx.staged, &fx.current).expect_err("refused");
        assert!(refused.contains("aeroftp-dispatch.exe"), "{refused}");
        std::fs::remove_dir(fx.app.join("aeroftp-dispatch.exe.new")).expect("blocker");
        assert_eq!(names_in(&fx.app), THREE_EXES.map(String::from).to_vec());
        assert_eq!(read(&fx, "AeroFTP.exe"), OLD_EXE);
    }

    #[test]
    fn a_failed_stage_leaves_the_old_exe_and_no_staged_copy() {
        let fx = fixture();
        let missing = fx.staged.stage_dir.join("not-extracted.exe");
        assert!(stage_beside(&missing, &fx.current).is_err());
        assert_eq!(std::fs::read(&fx.current).expect("exe"), OLD_EXE);
        assert_eq!(names_in(&fx.app), vec!["AeroFTP.exe".to_string()]);
    }
}
