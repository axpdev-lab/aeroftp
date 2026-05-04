//! Remote command builder for rsync remote-shell mode.
//!
//! Goal: produce the exact same remote command line that the current wrapper
//! capture observes. The captured forms are:
//!
//!   upload   : `rsync --server -logDtprcze.iLsfxCIvu --stats . /workspace/upload/target.bin`
//!   download : `rsync --server --sender -logDtprcze.iLsfxCIvu . /workspace/download/target.bin`
//!
//! See `fixtures::UPLOAD_REMOTE_COMMAND` / `DOWNLOAD_REMOTE_COMMAND`.
//!
//! Conventions:
//!   - upload   → remote runs as Receiver (no `--sender`) and the wrapper
//!     enables `--stats` on the remote command line
//!   - download → remote runs as Sender (`--sender`) without `--stats`
//!
//! Flag order is fixed to match the captured shape.

use crate::aerorsync::transport::RemoteExecRequest;
use crate::aerorsync::types::SessionRole;

/// The compact flag bundle observed in both captures.
/// Spelled out: log, gid, Devices, times, perms, recursion, z (compress request),
/// extended attribute chars `.iLsfxCIvu` (incremental + extras).
pub const OBSERVED_COMPACT_FLAGS: &str = "-logDtprcze.iLsfxCIvu";
pub const AERORSYNC_SERVER_PROGRAM: &str = "/opt/aerorsync/bin/aerorsync_serve";

/// Working directory placeholder passed to `rsync --server`.
/// In remote-shell mode rsync uses `.` as the source in the remote command.
pub const REMOTE_WORKDIR_PLACEHOLDER: &str = ".";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteCommandFlavor {
    WrapperParity,
    AerorsyncServe,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteCommandSpec {
    /// The remote role. For upload this is `Receiver`; for download `Sender`.
    pub remote_role: SessionRole,
    /// Absolute remote target path.
    pub remote_target: String,
    /// Whether to include `--stats` on the remote command line (matches the
    /// upload capture).
    pub emit_stats: bool,
    /// Which remote command shape should be emitted.
    pub flavor: RemoteCommandFlavor,
}

impl RemoteCommandSpec {
    pub fn upload(remote_target: impl Into<String>) -> Self {
        Self {
            remote_role: SessionRole::Receiver,
            remote_target: remote_target.into(),
            emit_stats: true,
            flavor: RemoteCommandFlavor::WrapperParity,
        }
    }

    pub fn download(remote_target: impl Into<String>) -> Self {
        Self {
            remote_role: SessionRole::Sender,
            remote_target: remote_target.into(),
            emit_stats: false,
            flavor: RemoteCommandFlavor::WrapperParity,
        }
    }

    pub fn aerorsync_upload(remote_target: impl Into<String>) -> Self {
        Self {
            remote_role: SessionRole::Receiver,
            remote_target: remote_target.into(),
            emit_stats: true,
            flavor: RemoteCommandFlavor::AerorsyncServe,
        }
    }

    pub fn aerorsync_download(remote_target: impl Into<String>) -> Self {
        Self {
            remote_role: SessionRole::Sender,
            remote_target: remote_target.into(),
            emit_stats: false,
            flavor: RemoteCommandFlavor::AerorsyncServe,
        }
    }

    /// Produce the argv for `rsync --server [--sender] <flags> [--stats] . <target>`
    /// in the exact order observed in the capture.
    pub fn to_args(&self) -> Vec<String> {
        match self.flavor {
            RemoteCommandFlavor::WrapperParity => {
                let mut args: Vec<String> = Vec::with_capacity(6);
                args.push("--server".to_string());
                if self.remote_role == SessionRole::Sender {
                    args.push("--sender".to_string());
                }
                args.push(OBSERVED_COMPACT_FLAGS.to_string());
                if self.emit_stats {
                    args.push("--stats".to_string());
                }
                args.push(REMOTE_WORKDIR_PLACEHOLDER.to_string());
                args.push(self.remote_target.clone());
                args
            }
            RemoteCommandFlavor::AerorsyncServe => {
                let mut args = vec![
                    "--mode".to_string(),
                    match self.remote_role {
                        SessionRole::Receiver => "upload".to_string(),
                        SessionRole::Sender => "download".to_string(),
                    },
                    "--target".to_string(),
                    self.remote_target.clone(),
                    "--protocol".to_string(),
                    "31".to_string(),
                ];
                if self.emit_stats {
                    args.push("--stats".to_string());
                }
                args
            }
        }
    }

    /// Produce a full `RemoteExecRequest` suitable for the transport layer.
    pub fn to_exec_request(&self) -> RemoteExecRequest {
        RemoteExecRequest {
            program: match self.flavor {
                RemoteCommandFlavor::WrapperParity => "rsync".to_string(),
                RemoteCommandFlavor::AerorsyncServe => AERORSYNC_SERVER_PROGRAM.to_string(),
            },
            args: self.to_args(),
            environment: Vec::new(),
        }
    }

    /// String representation matching the captured single-line form.
    pub fn to_command_line(&self) -> String {
        self.to_exec_request().full_command_line()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // B.5 pin: production dispatch (`AerorsyncDeltaTransport::upload` and
    // `::download` in `delta_transport_impl.rs`) calls `RemoteCommandSpec::
    // upload` / `RemoteCommandSpec::download`. These MUST stay locked on
    // `WrapperParity` forever: the `AerorsyncServe` flavor is a dev-only
    // helper kept alive exclusively for the in-process mock tests
    // (`driver_finish_session_aerorsync_serve_upload_*`) and the gated
    // `live_tests.rs` lane. If these constructors ever regress to
    // `AerorsyncServe`, production will try to exec
    // `/opt/aerorsync/bin/aerorsync_serve` against stock rsync servers -
    // the exact failure mode that Blocco B.1 ended.
    #[test]
    fn upload_spec_is_always_wrapper_parity_for_production() {
        let spec = RemoteCommandSpec::upload("/workdir/anything.bin");
        assert_eq!(
            spec.flavor,
            RemoteCommandFlavor::WrapperParity,
            "RemoteCommandSpec::upload is the production dispatch call site; \
             it must pin to WrapperParity so AerorsyncDeltaTransport never \
             exec's the dev-only aerorsync_serve binary in production"
        );
        let argv = spec.to_args();
        assert_eq!(argv.first().map(String::as_str), Some("--server"));
        assert!(
            argv.iter().all(|a| a != "--mode"),
            "--mode is the aerorsync_serve CLI shape; must never appear in \
             the stock rsync wire command"
        );
    }

    #[test]
    fn download_spec_is_always_wrapper_parity_for_production() {
        let spec = RemoteCommandSpec::download("/workdir/anything.bin");
        assert_eq!(
            spec.flavor,
            RemoteCommandFlavor::WrapperParity,
            "RemoteCommandSpec::download mirrors the upload pin: production \
             dispatch uses only stock rsync --server --sender"
        );
        let argv = spec.to_args();
        assert_eq!(argv.first().map(String::as_str), Some("--server"));
        assert_eq!(argv.get(1).map(String::as_str), Some("--sender"));
    }
}
