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
        command.env("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }

    exec_or_wait(command)
}

#[cfg(unix)]
fn exec_or_wait(mut command: Command) -> Result<u8, String> {
    use std::os::unix::process::CommandExt;

    let err = command.exec();
    Err(format!("exec failed: {err}"))
}

#[cfg(not(unix))]
fn exec_or_wait(mut command: Command) -> Result<u8, String> {
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
