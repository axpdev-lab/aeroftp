//! Turning an AeroFTP connection profile into a module transport.
//!
//! `RsyncConfig` is an application type, so the module cannot read it:
//! this is where a profile becomes the crate's own `SshTransportConfig`.
//! A free function and not an inherent method on purpose: once the module
//! is a separate crate, an inherent block on a type from another crate is
//! not allowed, and this file would be the one to break.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use crate::aerorsync::delta_transport_impl::AerorsyncDeltaTransport;
use crate::aerorsync::ssh_transport::{SshHostKeyPolicy, SshTransportConfig};
use crate::aerorsync::transport::RemoteExecRequest;
use crate::rsync_over_ssh::{AuthMethod, RsyncConfig, RsyncError};

/// Convenience constructor that maps the production `RsyncConfig`
/// (used by `providers::sftp::delta_transport`) onto the prototype's
/// `SshTransportConfig`. `host_key_policy` is provided by the caller
/// so the factory (Zona B1) can honour whatever pinning the SFTP
/// session established during connect.
pub fn transport_from_rsync_config(
    cfg: &RsyncConfig,
    host_key_policy: SshHostKeyPolicy,
) -> Result<AerorsyncDeltaTransport, RsyncError> {
    // Z.4.5 R1 dispatch step (2026-05-14): the previous boundary
    // refusal `Err(PasswordAuthUnsupported)` was a placeholder while
    // the russh transport gained password auth. Now that
    // `RusshSessionTransport::connect` branches on
    // `SshTransportConfig::usable_password()`, the gate moves to
    // `RsyncConfig::validate_auth_material()` which enforces:
    //   - SshKey  → ssh_key_path required (else MissingKey)
    //   - Password → ssh_password required and non-empty (else MissingPassword)
    //   - Neither → HardRejection (integration bug, never silently retry)
    // Callers that want password-based delta sync can now construct
    // an `RsyncConfig { auth_method: Password, ssh_password: Some(_), .. }`
    // and the russh leg picks it up. Subprocess `rsync_over_ssh::build_ssh_e_arg`
    // still refuses Password upfront so the binary path never accidentally
    // shells out without auth material.
    cfg.validate_auth_material()?;

    // Password-only profiles legitimately have no key path. The
    // russh leg ignores `private_key_path` when `usable_password()`
    // is Some, so an empty placeholder is safe; it is never opened
    // or dereferenced. We MUST NOT default to `~/.ssh/id_rsa` or
    // any other concrete path: that would silently load credentials
    // the user did not opt into.
    let key_path = cfg.ssh_key_path.clone().unwrap_or_default();
    let ssh_config = SshTransportConfig {
        host: cfg.ssh_host.clone(),
        port: cfg.ssh_port.unwrap_or(22),
        username: cfg.ssh_user.clone(),
        private_key_path: key_path,
        connect_timeout_ms: 10_000,
        io_timeout_ms: 30_000,
        worker_idle_poll_ms: 250,
        max_frame_size: 1 << 20,
        host_key_policy,
        auth_password: cfg.ssh_password.clone(),
        // An Agent profile carries no key/password; the russh leg
        // resolves SSH_AUTH_SOCK at connect time. `prefers_russh_leg`
        // then routes probe + single-shot through russh (libssh2 is
        // pubkey-file-only).
        auth_agent: matches!(cfg.auth_method, AuthMethod::Agent),
        // B.1/B.4: probe stock `rsync --version` on the remote. The
        // parser in `parse_probe_protocol` extracts the numeric
        // protocol version from the multi-line banner. A missing
        // `rsync` binary surfaces as exit != 0 and is mapped to
        // `RsyncError::RemoteNotAvailable` (soft classic fallback);
        // only `HostKeyRejected` escalates to `HardRejection`.
        probe_request: RemoteExecRequest {
            program: "rsync".into(),
            args: vec!["--version".into()],
            environment: Vec::new(),
        },
    };
    Ok(AerorsyncDeltaTransport::new(ssh_config, cfg.min_file_size))
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::{ExposeSecret, SecretString};
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn fresh_tempdir() -> TempDir {
        TempDir::new().expect("tempdir")
    }

    /// Z.4.5 R1: when the production [`RsyncConfig`] carries an SSH
    /// password (a password-auth rsync-over-SSH profile), the
    /// constructor MUST
    /// propagate it onto [`SshTransportConfig::auth_password`] so the
    /// russh leg can pick it up. The propagation is independent of the
    /// `auth_method` discriminant: a profile may legitimately carry
    /// both a key and a password (e.g. for paranoid two-factor setups
    /// in the future); the actual selection happens inside
    /// `RusshSessionTransport::connect`.
    #[test]
    fn from_rsync_config_propagates_password_to_transport() {
        let dir = fresh_tempdir();
        let key_path = dir.path().join("id_dummy");
        std::fs::write(&key_path, b"-----BEGIN OPENSSH PRIVATE KEY-----\nfake\n").unwrap();

        let cfg = RsyncConfig {
            ssh_user: "tester".into(),
            ssh_host: "example.invalid".into(),
            ssh_port: Some(2222),
            ssh_key_path: Some(key_path.clone()),
            ssh_password: Some(SecretString::from("rsync-password".to_string())),
            auth_method: AuthMethod::SshKey,
            ..Default::default()
        };
        let transport = transport_from_rsync_config(&cfg, SshHostKeyPolicy::AcceptAny)
            .expect("from_rsync_config should accept SshKey method with both materials");
        assert_eq!(transport.ssh_config().host, "example.invalid");
        assert_eq!(transport.ssh_config().port, 2222);
        assert_eq!(transport.ssh_config().private_key_path, key_path);
        let propagated = transport
            .ssh_config()
            .auth_password
            .as_ref()
            .expect("ssh_password must be propagated");
        assert_eq!(propagated.expose_secret(), "rsync-password");
        // And the helper agrees:
        assert!(transport.ssh_config().usable_password().is_some());
        let _ = PathBuf::from("placeholder"); // silence unused import warning on some CI configs
    }

    /// Z.4.5 R1 dispatch step (2026-05-14): the boundary refusal of
    /// `auth_method=Password` is gone. A password-only `RsyncConfig`
    /// now produces a transport whose `auth_password` is set and whose
    /// `private_key_path` is the empty placeholder (the russh leg
    /// ignores the key path when `usable_password()` is Some).
    #[test]
    fn from_rsync_config_accepts_password_only_method() {
        let cfg = RsyncConfig {
            ssh_user: "tester".into(),
            ssh_host: "example.invalid".into(),
            ssh_password: Some(SecretString::from("rsync-password".to_string())),
            auth_method: AuthMethod::Password,
            ..Default::default()
        };
        let transport = transport_from_rsync_config(&cfg, SshHostKeyPolicy::AcceptAny)
            .expect("password-only RsyncConfig should now produce a transport");
        // Empty placeholder (NOT a default ~/.ssh path): russh leg ignores it.
        assert_eq!(
            transport.ssh_config().private_key_path,
            std::path::PathBuf::new(),
            "password-only profile must not silently inject a default key path"
        );
        let propagated = transport
            .ssh_config()
            .auth_password
            .as_ref()
            .expect("ssh_password must be propagated");
        assert_eq!(propagated.expose_secret(), "rsync-password");
        assert!(transport.ssh_config().usable_password().is_some());
    }

    /// SSH agent auth: a `RsyncConfig { auth_method: Agent }` with no key
    /// and no password must produce a transport whose `auth_agent` flag
    /// is set, no password propagated, and an empty key placeholder. The
    /// russh leg resolves SSH_AUTH_SOCK at connect time; nothing static
    /// is validated or injected here.
    #[test]
    fn from_rsync_config_agent_method_sets_auth_agent_flag() {
        let cfg = RsyncConfig {
            ssh_user: "tester".into(),
            ssh_host: "example.invalid".into(),
            ssh_key_path: None,
            ssh_password: None,
            auth_method: AuthMethod::Agent,
            ..Default::default()
        };
        let transport = transport_from_rsync_config(&cfg, SshHostKeyPolicy::AcceptAny)
            .expect("agent RsyncConfig should produce a transport");
        assert!(
            transport.ssh_config().auth_agent,
            "auth_agent must be set for AuthMethod::Agent"
        );
        assert!(
            transport.ssh_config().auth_password.is_none(),
            "agent profile must not carry a password"
        );
        assert_eq!(
            transport.ssh_config().private_key_path,
            std::path::PathBuf::new(),
            "agent profile must not inject a default key path"
        );
        assert!(
            transport.ssh_config().prefers_russh_leg(),
            "agent profile must route through the russh leg"
        );
    }

    /// Z.4.5 R1 dispatch step: `validate_auth_material()` now gates the
    /// boundary instead of the old hard refusal. A `Password` method
    /// without a non-empty password surfaces `MissingPassword`, NOT
    /// `PasswordAuthUnsupported` (which has been removed from the
    /// boundary as of this step).
    #[test]
    fn from_rsync_config_password_method_without_password_returns_missing_password() {
        let cfg = RsyncConfig {
            ssh_user: "tester".into(),
            ssh_host: "example.invalid".into(),
            ssh_password: None,
            ssh_key_path: Some(std::path::PathBuf::from("/tmp/key")),
            auth_method: AuthMethod::Password,
            ..Default::default()
        };
        match transport_from_rsync_config(&cfg, SshHostKeyPolicy::AcceptAny) {
            Err(RsyncError::MissingPassword) => {}
            Err(other) => {
                panic!("expected MissingPassword via validate_auth_material, got Err({other:?})")
            }
            Ok(_) => panic!("expected MissingPassword, got Ok(_)"),
        }
    }

    /// Z.4.5 R1 dispatch step: empty SecretString must be rejected by
    /// `validate_auth_material()` so a misconfigured profile cannot
    /// reach the russh leg with a zero-length password.
    #[test]
    fn from_rsync_config_password_method_with_empty_password_returns_missing_password() {
        use secrecy::SecretString;

        let cfg = RsyncConfig {
            ssh_user: "tester".into(),
            ssh_host: "example.invalid".into(),
            ssh_password: Some(SecretString::from(String::new())),
            auth_method: AuthMethod::Password,
            ..Default::default()
        };
        match transport_from_rsync_config(&cfg, SshHostKeyPolicy::AcceptAny) {
            Err(RsyncError::MissingPassword) => {}
            Err(other) => panic!("expected MissingPassword, got Err({other:?})"),
            Ok(_) => panic!("expected MissingPassword, got Ok(_)"),
        }
    }

    /// Z.4.5 R1 dispatch step: a config that carries neither key nor
    /// password is still rejected as `HardRejection`. This is the
    /// "integration bug" guard from `validate_auth_material()`: it is
    /// not a credential failure (which the user can fix with input) but
    /// a wiring bug (the call site forgot to attach material). The
    /// dispatch must not silently fall back to another transport.
    #[test]
    fn from_rsync_config_with_no_auth_material_is_hard_rejection() {
        let cfg = RsyncConfig {
            ssh_user: "tester".into(),
            ssh_host: "example.invalid".into(),
            ssh_password: None,
            ssh_key_path: None,
            auth_method: AuthMethod::SshKey,
            ..Default::default()
        };
        match transport_from_rsync_config(&cfg, SshHostKeyPolicy::AcceptAny) {
            Err(RsyncError::HardRejection(message)) => {
                assert!(message.contains("neither ssh_key_path nor ssh_password"));
            }
            Err(other) => panic!("expected HardRejection, got Err({other:?})"),
            Ok(_) => panic!("expected HardRejection, got Ok(_)"),
        }
    }
}
