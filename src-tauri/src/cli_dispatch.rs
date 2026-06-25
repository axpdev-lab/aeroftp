use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub const CLI_SUBCOMMANDS: &[&str] = &[
    "connect",
    "ls",
    "get",
    "pget",
    "put",
    "mkdir",
    "rm",
    "mv",
    "cp",
    "link",
    "edit",
    "cat",
    "head",
    "tail",
    "peek",
    "touch",
    "hashsum",
    "check",
    "cryptcheck",
    "reconcile",
    "stat",
    "find",
    "df",
    "size",
    "lsd",
    "lsl",
    "lsf",
    "lsjson",
    "purge",
    "rmdir",
    "rmdirs",
    "about",
    "speed",
    "speed-compare",
    "benchmark",
    "cleanup",
    "dedupe",
    "sync",
    "sync-doctor",
    "tree",
    "ncdu",
    "mount",
    "mount-vault",
    "transfer",
    "transfer-doctor",
    "batch",
    "rcat",
    "serve",
    "alias",
    "alias-toggle",
    "agent",
    "mcp",
    "completions",
    "catalog",
    "tui",
    "profiles",
    "groups",
    "users",
    "profile-add",
    "profile-duplicate",
    "profile-duplicate-mode",
    "profile-convert-mode",
    "profile-delete",
    "profile-copy-user",
    "profile-move-user",
    "profile-set-password",
    "ai-models",
    "agent-bootstrap",
    "agent-info",
    "agent-connect",
    "daemon",
    "jobs",
    "versions",
    "crypt",
    "rclone-crypt",
    "audit",
    "import",
    "export",
    "keystore",
    "aerorsync",
    "vault",
    "correct",
    "compress",
    "extract",
    "archive",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DispatchRoute {
    Gui,
    Cli,
}

impl DispatchRoute {
    pub fn target_name(self) -> &'static str {
        match self {
            // Windows requires the `.exe` suffix because Rust's
            // Command::new(<absolute-path>) calls CreateProcessW with
            // lpApplicationName set, which does not append PATHEXT.
            // The GUI is renamed at packaging time from the default
            // `aeroftp.exe` produced by cargo to `aeroftp-gui.exe` so
            // the dispatcher itself can take over the canonical
            // `aeroftp.exe` slot in the install directory.
            DispatchRoute::Gui => {
                if cfg!(windows) {
                    "aeroftp-gui.exe"
                } else {
                    "aeroftp.bin"
                }
            }
            DispatchRoute::Cli => {
                if cfg!(windows) {
                    "aeroftp-cli.exe"
                } else {
                    "aeroftp-cli"
                }
            }
        }
    }
}

pub fn route_from_argv(argv: &[OsString]) -> DispatchRoute {
    route_from_argv_with_path_exists(argv, |path| path.exists())
}

pub fn route_from_argv_with_path_exists<F>(argv: &[OsString], path_exists: F) -> DispatchRoute
where
    F: Fn(&Path) -> bool,
{
    let Some(argv0) = argv.first() else {
        return DispatchRoute::Gui;
    };

    if executable_stem(argv0) != Some("aeroftp".to_string()) {
        return DispatchRoute::Cli;
    }

    let Some(first_arg) = argv.get(1).and_then(|arg| arg.to_str()) else {
        return DispatchRoute::Gui;
    };

    if first_arg.starts_with('-') {
        return DispatchRoute::Gui;
    }

    if is_gui_file_suffix(first_arg) {
        return DispatchRoute::Gui;
    }

    if CLI_SUBCOMMANDS.contains(&first_arg) {
        return DispatchRoute::Cli;
    }

    if path_exists(Path::new(first_arg)) {
        return DispatchRoute::Gui;
    }

    DispatchRoute::Gui
}

fn executable_stem(argv0: &OsString) -> Option<String> {
    Path::new(argv0)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(|stem| stem.to_string())
}

fn is_gui_file_suffix(arg: &str) -> bool {
    arg.ends_with(".aerovault")
        || arg.ends_with(".aerozip")
        || arg.ends_with(".aeroftp")
        || arg.ends_with(".aeroftp-keystore")
}

pub fn resolve_target(exe_path: &Path, route: DispatchRoute) -> Option<PathBuf> {
    let exe_dir = exe_path.parent()?;
    let target_name = route.target_name();
    candidate_targets(exe_dir, target_name)
        .into_iter()
        .find(|candidate| candidate.exists())
}

pub fn candidate_targets(exe_dir: &Path, target_name: &str) -> Vec<PathBuf> {
    vec![
        exe_dir.join("../lib/aeroftp").join(target_name),
        exe_dir.join(target_name),
        PathBuf::from("/usr/lib/aeroftp").join(target_name),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn argv(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn routes_table_cases() {
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

    #[test]
    fn resolves_targets_in_packaging_order() {
        let exe = Path::new("/usr/bin/aeroftp");
        let candidates = candidate_targets(exe.parent().unwrap(), "aeroftp-cli");
        assert_eq!(
            candidates[0],
            PathBuf::from("/usr/bin/../lib/aeroftp/aeroftp-cli")
        );
        assert_eq!(candidates[1], PathBuf::from("/usr/bin/aeroftp-cli"));
        assert_eq!(candidates[2], PathBuf::from("/usr/lib/aeroftp/aeroftp-cli"));
    }

    #[test]
    fn target_names_are_platform_appropriate() {
        if cfg!(windows) {
            assert_eq!(DispatchRoute::Gui.target_name(), "aeroftp-gui.exe");
            assert_eq!(DispatchRoute::Cli.target_name(), "aeroftp-cli.exe");
        } else {
            assert_eq!(DispatchRoute::Gui.target_name(), "aeroftp.bin");
            assert_eq!(DispatchRoute::Cli.target_name(), "aeroftp-cli");
        }
    }

    // Windows-only: argv[0] often carries a full backslash path and the
    // `.exe` extension. Both must be stripped before the stem comparison
    // routes the invocation. Running this case under `cfg(windows)` is
    // load-bearing: on Unix the Path parser treats `C:\foo\bar.exe` as
    // a single filename and the assertions would not exercise the real
    // Windows behavior.
    #[cfg(windows)]
    #[test]
    fn windows_argv0_with_full_path_and_extension_routes_correctly() {
        let cases = [
            (
                vec!["C:\\Program Files\\AeroFTP\\aeroftp.exe", "ls", "ftp://h"],
                DispatchRoute::Cli,
            ),
            (
                vec!["C:\\Program Files\\AeroFTP\\aftp.exe", "ls"],
                DispatchRoute::Cli,
            ),
            (
                vec!["C:\\Program Files\\AeroFTP\\aeroftp-cli.exe", "ls"],
                DispatchRoute::Cli,
            ),
            (
                vec!["C:\\Program Files\\AeroFTP\\aero.exe", "ls"],
                DispatchRoute::Cli,
            ),
            (
                vec!["C:\\Program Files\\AeroFTP\\aeroftp.exe"],
                DispatchRoute::Gui,
            ),
            (
                vec![
                    "C:\\Program Files\\AeroFTP\\aeroftp.exe",
                    "C:\\Users\\me\\vault.aerovault",
                ],
                DispatchRoute::Gui,
            ),
            (
                vec![
                    "C:\\Program Files\\AeroFTP\\aeroftp.exe",
                    "--post-update-cleanup",
                    "C:\\Users\\me\\AppData\\Local\\AeroFTP\\aeroftp.exe.old",
                ],
                DispatchRoute::Gui,
            ),
        ];

        for (input, expected) in cases {
            let actual = route_from_argv_with_path_exists(&argv(&input), |_| false);
            assert_eq!(actual, expected, "argv={input:?}");
        }
    }
}
