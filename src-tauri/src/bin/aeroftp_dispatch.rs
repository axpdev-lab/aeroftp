#[path = "../cli_dispatch.rs"]
mod cli_dispatch;

use cli_dispatch::{route_from_argv, DispatchRoute};
use std::env;
use std::ffi::OsString;
use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    let argv: Vec<OsString> = env::args_os().collect();
    let route = route_from_argv(&argv);
    match dispatch(argv, route) {
        Ok(code) => ExitCode::from(code),
        Err(err) => {
            eprintln!("aeroftp-dispatch: {err}");
            ExitCode::from(127)
        }
    }
}

fn dispatch(argv: Vec<OsString>, route: DispatchRoute) -> Result<u8, String> {
    let exe =
        env::current_exe().map_err(|err| format!("cannot resolve current executable: {err}"))?;
    let target = cli_dispatch::resolve_target(&exe, route).ok_or_else(|| {
        format!(
            "cannot find {} next to {} or under /usr/lib/aeroftp",
            route.target_name(),
            exe.display()
        )
    })?;

    let mut command = Command::new(&target);
    command.args(argv.iter().skip(1));

    #[cfg(target_os = "linux")]
    if route == DispatchRoute::Gui {
        use std::os::unix::process::CommandExt;
        // GTK derives the X11 WM_CLASS (and the Wayland app_id fallback)
        // from the basename of argv[0]. The real GUI payload lives at
        // /usr/lib/aeroftp/aeroftp.bin, so without this the window would
        // surface as WM_CLASS "aeroftp.bin", which does not match
        // StartupWMClass=aeroftp in AeroFTP.desktop: GNOME then shows a
        // generic icon and the "aeroftp.bin" process name in the dock
        // instead of the AeroFTP icon and name. Forcing argv[0] to
        // "aeroftp" binds the window back to the .desktop entry.
        command.arg0("aeroftp");
        command.env("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }

    exec_or_wait(command, route)
}

#[cfg(unix)]
fn exec_or_wait(mut command: Command, _route: DispatchRoute) -> Result<u8, String> {
    use std::os::unix::process::CommandExt;

    let err = command.exec();
    Err(format!("exec failed: {err}"))
}

#[cfg(windows)]
fn exec_or_wait(mut command: Command, route: DispatchRoute) -> Result<u8, String> {
    use std::os::windows::process::CommandExt;

    // DETACHED_PROCESS, CREATE_NEW_CONSOLE and CREATE_NO_WINDOW are
    // mutually exclusive in CreateProcessW. We only need DETACHED_PROCESS
    // for the GUI branch (the GUI binary is windows-subsystem so it
    // never allocates a console anyway). Hardcoded to keep the dispatcher
    // free of an extra crate dependency just for one integer.
    const DETACHED_PROCESS: u32 = 0x0000_0008;

    match route {
        DispatchRoute::Gui => {
            // The GUI binary is built with `windows_subsystem = "windows"`
            // (see main.rs:5) so it never allocates a console of its own.
            // Spawn it detached so:
            //   1. the cmd/PowerShell prompt returns immediately after
            //      `aeroftp` (the dispatcher fires the GUI and exits),
            //   2. when the dispatcher is launched from Explorer via a
            //      file association the brief console it inherits is not
            //      passed through to the GUI process.
            command.creation_flags(DETACHED_PROCESS);
            command
                .spawn()
                .map_err(|err| format!("GUI launch failed: {err}"))?;
            Ok(0)
        }
        DispatchRoute::Cli => {
            // CLI shares the dispatcher's console so output streams to
            // the parent shell and Ctrl+C reaches both processes via
            // the console control group. Install a no-op Ctrl+C handler
            // so the wait survives the signal long enough for the child
            // to perform its own shutdown and report an exit code.
            let _ = ctrlc::set_handler(|| {});
            let status = command
                .status()
                .map_err(|err| format!("CLI launch failed: {err}"))?;
            Ok(status.code().unwrap_or(1).try_into().unwrap_or(1))
        }
    }
}

#[cfg(not(any(unix, windows)))]
fn exec_or_wait(mut command: Command, _route: DispatchRoute) -> Result<u8, String> {
    let status = command
        .status()
        .map_err(|err| format!("process launch failed: {err}"))?;
    Ok(status.code().unwrap_or(1).try_into().unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::cli_dispatch::{route_from_argv_with_path_exists, DispatchRoute};
    use std::ffi::OsString;
    use std::path::Path;

    fn argv(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn dispatcher_routing_contract() {
        let existing = Path::new("/etc/hostname");
        let cases = [
            (vec!["aeroftp"], DispatchRoute::Gui),
            (vec!["aeroftp", "ls", "ftp://h"], DispatchRoute::Cli),
            (vec!["aeroftp", "--autostart"], DispatchRoute::Gui),
            (
                vec!["aeroftp", "--post-update-cleanup", "C:\\old.exe"],
                DispatchRoute::Gui,
            ),
            (vec!["aeroftp", "/tmp/x.aerovault"], DispatchRoute::Gui),
            (vec!["aeroftp", "/tmp/x.aerozip"], DispatchRoute::Gui),
            (vec!["aeroftp", "/etc/hostname"], DispatchRoute::Gui),
            (vec!["aftp", "ls"], DispatchRoute::Cli),
            (vec!["aeroftp-cli", "ls"], DispatchRoute::Cli),
            (vec!["aero", "ls"], DispatchRoute::Cli),
            (vec!["aeroftp", "mcp"], DispatchRoute::Cli),
            (
                vec!["aeroftp", "sottocomando-inesistente"],
                DispatchRoute::Gui,
            ),
        ];

        for (input, expected) in cases {
            let actual = route_from_argv_with_path_exists(&argv(&input), |path| path == existing);
            assert_eq!(actual, expected, "argv={input:?}");
        }
    }
}
