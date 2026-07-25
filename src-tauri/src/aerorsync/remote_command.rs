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

/// Same bundle with `X` (preserve xattrs) enabled.
///
/// Measured against rsync 3.2.7 on 2026-07-25 by handing `rsync -e` a fake
/// remote shell that prints its argv:
///
/// ```text
/// rsync -logDtprcz   ...  ->  rsync --server -logDtprcze.iLsfxCIvu  . /tmp/
/// rsync -logDtprczX  ...  ->  rsync --server -logDtpXrcze.iLsfxCIvu . /tmp/
/// rsync -logDtprczAX ...  ->  rsync --server -logDtpAXrcze.iLsfxCIvu . /tmp/
/// ```
///
/// The first line reproduces `OBSERVED_COMPACT_FLAGS` byte for byte, which is
/// what validates the capture method.
///
/// **`X` is not a suffix.** Stock rsync inserts it after the `-logDtp` block and
/// *before* `r`. Appending it to the end of the bundle would produce a server
/// command line that stock rsync never emits, which is exactly the divergence
/// this pinned constant exists to prevent. ACL, when it lands, goes in as `AX`
/// at the same position.
pub const OBSERVED_COMPACT_FLAGS_XATTR: &str = "-logDtpXrcze.iLsfxCIvu";

/// The prefix `X` is inserted after. Test-only on purpose: production selects
/// between the two measured literals rather than deriving one from the other,
/// because the literals are the capture oracles. This constant exists so the
/// pin test can assert the *position* of `X`, not merely the resulting string.
#[cfg(test)]
const XATTR_FLAG_ANCHOR: &str = "-logDtp";

/// Pick the compact flag bundle for a session.
///
/// `-X` is sent **only** when the session actually intends to carry xattrs.
/// Sending it unconditionally would change the byte-pinned server command line
/// for every transfer, including the ones the frozen oracles are pinned against.
pub fn compact_flags_for(preserve_xattrs: bool) -> &'static str {
    if preserve_xattrs {
        OBSERVED_COMPACT_FLAGS_XATTR
    } else {
        OBSERVED_COMPACT_FLAGS
    }
}

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
    /// Whether this session carries extended attributes, i.e. whether `X` goes
    /// into the compact flag bundle. Defaults to `false` on every constructor,
    /// so the emitted command line is unchanged until a caller opts in via
    /// [`RemoteCommandSpec::with_xattrs`].
    pub preserve_xattrs: bool,
}

impl RemoteCommandSpec {
    pub fn upload(remote_target: impl Into<String>) -> Self {
        Self {
            remote_role: SessionRole::Receiver,
            remote_target: remote_target.into(),
            emit_stats: true,
            flavor: RemoteCommandFlavor::WrapperParity,
            preserve_xattrs: false,
        }
    }

    pub fn download(remote_target: impl Into<String>) -> Self {
        Self {
            remote_role: SessionRole::Sender,
            remote_target: remote_target.into(),
            emit_stats: false,
            flavor: RemoteCommandFlavor::WrapperParity,
            preserve_xattrs: false,
        }
    }

    pub fn aerorsync_upload(remote_target: impl Into<String>) -> Self {
        Self {
            remote_role: SessionRole::Receiver,
            remote_target: remote_target.into(),
            emit_stats: true,
            flavor: RemoteCommandFlavor::AerorsyncServe,
            preserve_xattrs: false,
        }
    }

    pub fn aerorsync_download(remote_target: impl Into<String>) -> Self {
        Self {
            remote_role: SessionRole::Sender,
            remote_target: remote_target.into(),
            emit_stats: false,
            flavor: RemoteCommandFlavor::AerorsyncServe,
            preserve_xattrs: false,
        }
    }

    /// Opt this session into extended attributes, which puts `X` into the
    /// compact flag bundle. Off by default: see `compact_flags_for`.
    pub fn with_xattrs(mut self, preserve_xattrs: bool) -> Self {
        self.preserve_xattrs = preserve_xattrs;
        self
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
                // Live-tuning escape hatch: `AEROFTP_RSYNC_SERVER_FLAGS`
                // overrides the byte-pinned compact flag string so a
                // non-stock remote `rsync --server` wrapper whose
                // ForceCommand whitelists a different argv can be probed
                // live without a rebuild per attempt. No-op when unset,
                // so the default stays byte-pinned against rsync 3.2.7.
                let compact_flags = std::env::var("AEROFTP_RSYNC_SERVER_FLAGS")
                    .ok()
                    .filter(|v| !v.trim().is_empty())
                    .unwrap_or_else(|| compact_flags_for(self.preserve_xattrs).to_string());
                args.push(compact_flags);
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

    // Y-RSC.4 pin: `-l` (preserve links) must stay in the compact flag
    // bundle sent to stock `rsync --server`. The roadmap item assumed the
    // flag had to be ADDED; the captured shape already carries it as the
    // first short option (`-l ogDtprcz...`), because stock clients enable
    // it via `-a`. Without `-l` the remote side would follow / skip
    // symlinks instead of preserving them, breaking the symlink
    // end-to-end path in both directions.
    #[test]
    fn compact_flags_pin_preserve_links() {
        assert!(
            OBSERVED_COMPACT_FLAGS.starts_with("-l"),
            "server flag string must keep -l (preserve links) as captured: {}",
            OBSERVED_COMPACT_FLAGS
        );
    }

    // X.1 pin: `X` (preserve xattrs) goes INSIDE the bundle, after the
    // `-logDtp` block and before `r`. Measured against rsync 3.2.7 on
    // 2026-07-25: `rsync -logDtprczX` emits `--server -logDtpXrcze.iLsfxCIvu`.
    // Appending `X` to the end of the bundle is the intuitive mistake and it
    // would emit a command line stock rsync never produces. This test exists
    // to make that mistake impossible to land.
    #[test]
    fn compact_flags_pin_xattr_position() {
        let base_head = OBSERVED_COMPACT_FLAGS
            .strip_prefix(XATTR_FLAG_ANCHOR)
            .expect("base bundle must start with the -logDtp block");
        let xattr_head = OBSERVED_COMPACT_FLAGS_XATTR
            .strip_prefix(XATTR_FLAG_ANCHOR)
            .expect("xattr bundle must start with the same -logDtp block");

        assert_eq!(
            xattr_head,
            format!("X{base_head}"),
            "X must be inserted right after {XATTR_FLAG_ANCHOR} and before the \
             rest of the bundle; got {OBSERVED_COMPACT_FLAGS_XATTR}"
        );
        assert!(
            !OBSERVED_COMPACT_FLAGS_XATTR.ends_with('X'),
            "X is not a suffix: stock rsync never emits it at the end of the bundle"
        );
        // The measured literal is the oracle: the derivation above must agree
        // with what rsync actually printed, not merely be self-consistent.
        assert_eq!(
            OBSERVED_COMPACT_FLAGS_XATTR, "-logDtpXrcze.iLsfxCIvu",
            "captured from rsync 3.2.7 on 2026-07-25; re-measure before changing"
        );
    }

    // X.1: opting into xattrs must be the ONLY thing that changes the emitted
    // flag bundle. Every existing call site keeps the byte-pinned string, which
    // is what keeps the frozen oracles and the live lanes valid.
    #[test]
    fn xattr_flag_is_opt_in_and_changes_nothing_by_default() {
        for spec in [
            RemoteCommandSpec::upload("/workdir/t.bin"),
            RemoteCommandSpec::download("/workdir/t.bin"),
        ] {
            assert!(!spec.preserve_xattrs, "xattrs must default to off");
            assert!(
                spec.to_args().iter().any(|a| a == OBSERVED_COMPACT_FLAGS),
                "default spec must still emit the byte-pinned bundle: {:?}",
                spec.to_args()
            );
        }

        let with_x = RemoteCommandSpec::upload("/workdir/t.bin").with_xattrs(true);
        assert!(
            with_x
                .to_args()
                .iter()
                .any(|a| a == OBSERVED_COMPACT_FLAGS_XATTR),
            "with_xattrs(true) must emit the xattr bundle: {:?}",
            with_x.to_args()
        );
        assert!(
            with_x.to_args().iter().all(|a| a != OBSERVED_COMPACT_FLAGS),
            "the two bundles are mutually exclusive"
        );
    }

    #[test]
    fn compact_flags_for_selects_the_measured_constants() {
        assert_eq!(compact_flags_for(false), OBSERVED_COMPACT_FLAGS);
        assert_eq!(compact_flags_for(true), OBSERVED_COMPACT_FLAGS_XATTR);
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
