//! Fail-closed OS launch boundary for untrusted outbound MCP STDIO peers.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

#[cfg(any(test, target_os = "linux"))]
use std::path::Path;
#[cfg(target_os = "linux")]
use std::path::PathBuf;
use tokio::process::Command;

use crate::mcp_client_config::McpServerConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SandboxError {
    Unavailable,
    InvalidPath,
    DirectoryChanged,
    IntegrityFailure,
}

#[cfg(any(test, target_os = "linux"))]
fn test_fixture(config: &McpServerConfig) -> bool {
    #[cfg(test)]
    {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mcp_stdio_fixture.mjs");
        config
            .args
            .first()
            .is_some_and(|arg| arg == &path.to_string_lossy().into_owned())
    }
    #[cfg(not(test))]
    {
        let _ = config;
        false
    }
}

/// Test-only transport fixtures can run without an OS sandbox on CI hosts.
/// Production builds never compile this fallback.
#[cfg(test)]
fn fixture_command(config: &McpServerConfig) -> Result<Command, SandboxError> {
    if !test_fixture(config) {
        return Err(SandboxError::Unavailable);
    }
    let mut command = Command::new(&config.command);
    command.args(&config.args);
    Ok(command)
}

/// Clear inherited secrets. Windows' Node fixture needs its OS directory for
/// libuv initialization; production non-Linux launches still fail closed.
pub(crate) fn clear_peer_environment(command: &mut Command) {
    command.env_clear();
    #[cfg(all(test, target_os = "windows"))]
    if let Some(root) = std::env::var_os("SystemRoot") {
        command.env("SystemRoot", root);
    }
}

/// A binary on disk does not prove its required flags/user namespaces work.
#[cfg(all(test, target_os = "linux"))]
pub(crate) fn fixture_sandbox_available() -> bool {
    static AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        let config = McpServerConfig {
            id: "sandbox_probe".into(),
            command: "/usr/bin/true".into(),
            args: Vec::new(),
            env: Default::default(),
            enabled: true,
            revision: 1,
            sandbox: Default::default(),
        };
        let Ok(mut command) = linux_command(&config) else {
            return false;
        };
        command
            .as_std_mut()
            .env_clear()
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    })
}

#[cfg(target_os = "linux")]
fn system_path(path: &Path) -> bool {
    ["/usr", "/lib", "/lib64", "/bin", "/sbin"]
        .iter()
        .any(|base| path.starts_with(base))
}

#[cfg(target_os = "linux")]
fn bind_explicit_file(command: &mut Command, path: &Path) -> Result<(), SandboxError> {
    let canonical = path.canonicalize().map_err(|_| SandboxError::InvalidPath)?;
    if !canonical.is_file() {
        return Err(SandboxError::InvalidPath);
    }
    if !system_path(&canonical) {
        command.arg("--ro-bind").arg(&canonical).arg(&canonical);
    }
    if path != canonical && !system_path(path) {
        command.arg("--ro-bind").arg(&canonical).arg(path);
    }
    Ok(())
}

/// Mount a private verified snapshot by fd. Holding it in the Command closes the
/// pathname race and keeps the tree alive through bwrap setup and peer lifetime.
#[cfg(target_os = "linux")]
fn bind_explicit_directory(
    command: &mut Command,
    grant: &crate::mcp_client_config::McpDirectoryGrant,
    expected_hash: Option<&str>,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<(), SandboxError> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;
    let (snapshot, hash) = crate::mcp_client_install_paths::snapshot(grant, cancel)
        .map_err(|_| SandboxError::DirectoryChanged)?;
    if expected_hash.is_some_and(|expected| expected != hash) {
        return Err(SandboxError::IntegrityFailure);
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(snapshot.path())
        .map_err(|_| SandboxError::InvalidPath)?;
    let fd = file.as_raw_fd();
    command
        .arg("--ro-bind-fd")
        .arg(fd.to_string())
        .arg(&grant.path);
    // SAFETY: only async-signal-safe fcntl runs after fork. Captured resources
    // belong to the parent Command; Rust cleanup never runs in the child hook.
    unsafe {
        command.pre_exec(move || {
            let _keep_alive = (&file, &snapshot);
            if libc::fcntl(fd, libc::F_SETFD, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(())
}

/// File grants and separately consented, verified directory snapshots.
#[cfg(target_os = "linux")]
#[cfg(test)]
fn linux_command(config: &McpServerConfig) -> Result<Command, SandboxError> {
    linux_command_with_cancel(config, &tokio_util::sync::CancellationToken::new())
}
#[cfg(target_os = "linux")]
fn linux_command_with_cancel(
    config: &McpServerConfig,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<Command, SandboxError> {
    let bwrap = Path::new("/usr/bin/bwrap");
    if !bwrap.is_file() {
        return Err(SandboxError::Unavailable);
    }
    let executable = PathBuf::from(&config.command)
        .canonicalize()
        .map_err(|_| SandboxError::InvalidPath)?;
    if !executable.is_file() {
        return Err(SandboxError::InvalidPath);
    }
    let mut command = Command::new(bwrap);
    command
        .arg("--unshare-all")
        .arg("--unshare-user")
        .arg("--disable-userns")
        .arg("--die-with-parent")
        .arg("--new-session")
        .arg("--ro-bind")
        .arg("/usr")
        .arg("/usr")
        .arg("--ro-bind-try")
        .arg("/lib")
        .arg("/lib")
        .arg("--ro-bind-try")
        .arg("/lib64")
        .arg("/lib64")
        .arg("--ro-bind-try")
        .arg("/bin")
        .arg("/bin")
        .arg("--ro-bind-try")
        .arg("/sbin")
        .arg("/sbin")
        .arg("--proc")
        .arg("/proc")
        .arg("--dev")
        .arg("/dev")
        .arg("--tmpfs")
        .arg("/tmp");
    if config.sandbox.network_consent {
        command.arg("--share-net");
        // Resolver and CA files are the only added host system configuration.
        for path in ["/etc/resolv.conf", "/etc/hosts", "/etc/ssl/certs"] {
            command.arg("--ro-bind").arg(path).arg(path);
        }
    }
    for grant in &config.sandbox.directories {
        if config.sandbox.managed.is_none() {
            crate::mcp_client_install_paths::custom_path(Path::new(&grant.path))
                .map_err(|_| SandboxError::InvalidPath)?;
        }
        bind_explicit_directory(
            &mut command,
            grant,
            config
                .sandbox
                .managed
                .as_ref()
                .map(|i| i.tree_sha256.as_str()),
            cancel,
        )?;
    }
    if !config
        .sandbox
        .directories
        .iter()
        .any(|g| Path::new(&config.command).starts_with(&g.path))
    {
        bind_explicit_file(&mut command, Path::new(&config.command))?;
    }
    for arg in &config.args {
        let path = Path::new(arg);
        if path.is_absolute()
            && path.exists()
            && !config
                .sandbox
                .directories
                .iter()
                .any(|g| path.starts_with(&g.path))
        {
            bind_explicit_file(&mut command, path)?;
        }
    }
    command
        .arg("--chdir")
        .arg("/tmp")
        .arg("--")
        .arg(executable)
        .args(&config.args);
    Ok(command)
}

#[cfg(test)]
pub(crate) fn peer_command(config: &McpServerConfig) -> Result<Command, SandboxError> {
    peer_command_with_cancel(config, &tokio_util::sync::CancellationToken::new())
}

pub(crate) fn peer_command_with_cancel(
    config: &McpServerConfig,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<Command, SandboxError> {
    #[cfg(not(target_os = "linux"))]
    let _ = cancel;
    config.validate().map_err(|_| SandboxError::InvalidPath)?;
    #[cfg(all(test, target_os = "linux"))]
    if test_fixture(config) && !fixture_sandbox_available() {
        return fixture_command(config);
    }
    #[cfg(target_os = "linux")]
    {
        match linux_command_with_cancel(config, cancel) {
            Ok(command) => Ok(command),
            Err(SandboxError::Unavailable) if test_fixture(config) => {
                #[cfg(test)]
                {
                    fixture_command(config)
                }
                #[cfg(not(test))]
                {
                    Err(SandboxError::Unavailable)
                }
            }
            Err(error) => Err(error),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        #[cfg(test)]
        if test_fixture(config) {
            return fixture_command(config);
        }
        Err(SandboxError::Unavailable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn fixture(args: Vec<String>) -> McpServerConfig {
        McpServerConfig {
            id: "sandbox".into(),
            command: std::env::current_exe()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            args,
            env: BTreeMap::new(),
            enabled: true,
            revision: 1,
            sandbox: Default::default(),
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_sandbox_has_no_ambient_home_or_network() {
        if !Path::new("/usr/bin/bwrap").is_file() {
            return;
        }
        let config = fixture(Vec::new());
        let command = peer_command(&config).unwrap();
        let view = format!("{command:?}");
        assert!(view.contains("--unshare-all"));
        assert!(view.contains("--tmpfs"));
        assert!(!view.contains("--share-net"));
        assert!(!view.contains("--bind /home"));
    }

    #[tokio::test]
    async fn unsandboxed_test_fixture_receives_its_script_and_literal_arguments() {
        let executable = if cfg!(windows) { "node.exe" } else { "node" };
        let node = std::env::split_paths(&std::env::var_os("PATH").expect("PATH"))
            .map(|dir| dir.join(executable))
            .find(|path| path.is_file())
            .expect("Node runtime");
        let script =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mcp_stdio_fixture.mjs");
        let mut config = fixture(vec![script.to_string_lossy().into_owned(), "exit".into()]);
        config.command = node.to_string_lossy().into_owned();
        let mut command = fixture_command(&config).unwrap();
        clear_peer_environment(&mut command);
        let output = command.output().await.unwrap();
        assert_eq!(
            output.status.code(),
            Some(17),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        config.args[0] = "untrusted-other-script.mjs".into();
        assert_eq!(
            fixture_command(&config).err(),
            Some(SandboxError::Unavailable)
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn multifile_directory_is_read_only_private_and_network_denied() {
        if !fixture_sandbox_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            dir.path().join("dependency.js"),
            "module.exports='original'",
        )
        .unwrap();
        std::fs::write(dir.path().join("main.js"), r#"
            const fs=require('fs'),net=require('net');
            let readonly=false;try{fs.writeFileSync(__dirname+'/write','bad')}catch{readonly=true}
            fs.writeFileSync('/tmp/private-probe','ok');
            const output={value:require('./dependency'),readonly,hidden:!fs.existsSync(process.env.OUTSIDE),tmp:fs.readFileSync('/tmp/private-probe','utf8')};
            net.connect(+process.env.PORT,'127.0.0.1').on('error',()=>{output.network=false;console.log(JSON.stringify(output))}).on('connect',()=>{output.network=true;console.log(JSON.stringify(output));process.exit(0)});
        "#).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut config = fixture(vec![dir
            .path()
            .join("main.js")
            .to_string_lossy()
            .into_owned()]);
        config.command = "/usr/bin/node".into();
        let grant = crate::mcp_client_install_paths::directory_grant(dir.path()).unwrap();
        config.sandbox.directories = vec![grant];
        let mut command = peer_command(&config).unwrap();
        // The launch owns a private snapshot, not a racy mount of the source.
        std::fs::write(
            dir.path().join("dependency.js"),
            "module.exports='replaced'",
        )
        .unwrap();
        clear_peer_environment(&mut command);
        command
            .env("OUTSIDE", outside.path())
            .env("PORT", listener.local_addr().unwrap().port().to_string());
        let output = command.output().await.unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            result,
            serde_json::json!({"value":"original","readonly":true,"hidden":true,"tmp":"ok","network":false})
        );
        assert!(!dir.path().join("write").exists());
        assert!(!Path::new("/tmp/private-probe").exists());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn managed_tree_tampering_and_replacement_fail_and_network_requires_consent() {
        if !fixture_sandbox_available() {
            return;
        }
        let parent = tempfile::tempdir().unwrap();
        let dir = parent.path().join("package");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("main.js"), "const net=require('net');net.connect(+process.env.PORT,'127.0.0.1').on('connect',()=>{console.log('connected');process.exit(0)}).on('error',()=>process.exit(2))").unwrap();
        let grant = crate::mcp_client_install_paths::directory_grant(&dir).unwrap();
        let (_, hash) = crate::mcp_client_install_paths::snapshot(
            &grant,
            &tokio_util::sync::CancellationToken::new(),
        )
        .unwrap();
        let mut config = fixture(vec![dir.join("main.js").to_string_lossy().into_owned()]);
        config.command = "/usr/bin/node".into();
        config.sandbox.directories = vec![grant];
        config.sandbox.managed = Some(crate::mcp_client_config::McpManagedInstall {
            manifest_id: "fixture".into(),
            version: "1.0.0".into(),
            archive_sha256: "a".repeat(64),
            tree_sha256: hash,
            network_declared: true,
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut denied = peer_command(&config).unwrap();
        clear_peer_environment(&mut denied);
        denied.env("PORT", listener.local_addr().unwrap().port().to_string());
        assert_eq!(denied.output().await.unwrap().status.code(), Some(2));
        config.sandbox.network_consent = true;
        let mut consented = peer_command(&config).unwrap();
        clear_peer_environment(&mut consented);
        consented.env("PORT", listener.local_addr().unwrap().port().to_string());
        let output = consented.output().await.unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"connected\n");
        std::fs::write(dir.join("main.js"), "tampered").unwrap();
        assert_eq!(
            peer_command(&config).err(),
            Some(SandboxError::IntegrityFailure)
        );
        std::fs::rename(&dir, parent.path().join("old")).unwrap();
        std::fs::create_dir(&dir).unwrap();
        assert_eq!(
            peer_command(&config).err(),
            Some(SandboxError::DirectoryChanged)
        );
        let implicit = fixture(vec![parent.path().to_string_lossy().into_owned()]);
        assert_eq!(
            peer_command(&implicit).err(),
            Some(SandboxError::InvalidPath)
        );
    }

    #[test]
    fn invalid_config_cannot_form_a_command() {
        let config = fixture(Vec::new());
        let invalid = McpServerConfig {
            command: "relative".into(),
            ..config
        };
        assert_eq!(
            peer_command(&invalid).err(),
            Some(SandboxError::InvalidPath)
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn linux_sandbox_hides_unlisted_host_files() {
        if !fixture_sandbox_available() {
            eprintln!("Sandbox integration unavailable: required bubblewrap flags or user namespaces unsupported");
            return;
        }
        let host_file = tempfile::NamedTempFile::new().unwrap();
        let node = std::env::split_paths(&std::env::var_os("PATH").expect("PATH"))
            .map(|dir| dir.join("node"))
            .find(|path| path.is_file())
            .expect("Node fixture runtime");
        let mut config = fixture(vec![
            "-e".into(),
            "const fs=require('fs');process.stdout.write(fs.existsSync(process.env.PROBE_PATH)?'visible':'hidden')".into(),
        ]);
        config.command = node.to_string_lossy().into_owned();
        let mut command = peer_command(&config).unwrap();
        command.env_clear().env("PROBE_PATH", host_file.path());
        let output = command.output().await.unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"hidden");
    }
}
