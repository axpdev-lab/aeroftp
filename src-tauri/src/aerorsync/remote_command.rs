//! Remote command builder for rsync remote-shell mode.
//!
//! Production uses the integrated product profile (`-ltp...`): mode, mtime,
//! symlinks, optional xattrs and Linux ACLs. It does not request owner,
//! group, or devices. Historical capture literals (`-logDtp...`) stay
//! byte-identical for frozen oracles and are reachable only through an
//! explicit test-only constructor.
//!
//!   product upload   : `rsync --server -ltprcze.iLsfxCIvu --stats . <target>`
//!   capture upload   : `rsync --server -logDtprcze.iLsfxCIvu --stats . /workspace/upload/target.bin`
//!
//! See `fixtures::UPLOAD_REMOTE_COMMAND` / `DOWNLOAD_REMOTE_COMMAND` for the
//! historical capture command lines.
//!
//! Conventions:
//!   - upload   → remote runs as Receiver (no `--sender`) and the wrapper
//!     enables `--stats` on the remote command line
//!   - download → remote runs as Sender (`--sender`) without `--stats`

use crate::aerorsync::transport::RemoteExecRequest;
use crate::aerorsync::types::SessionRole;

/// Historical capture literals. Frozen oracles and explicit capture-profile
/// tests keep these byte-identical. Production never selects them.
pub const OBSERVED_COMPACT_FLAGS: &str = "-logDtprcze.iLsfxCIvu";
pub const OBSERVED_COMPACT_FLAGS_XATTR: &str = "-logDtpXrcze.iLsfxCIvu";
pub const OBSERVED_COMPACT_FLAGS_ACL: &str = "-logDtpArcze.iLsfxCIvu";
pub const OBSERVED_COMPACT_FLAGS_ACL_XATTR: &str = "-logDtpAXrcze.iLsfxCIvu";

/// Integrated product literals, measured against rsync 3.2.7 on 2026-08-27
/// through a fake remote shell at protocol 31:
///
/// ```text
/// -ltprcz   -> -ltprcze.iLsfxCIvu
/// -ltprczX  -> -ltpXrcze.iLsfxCIvu
/// -ltprczA  -> -ltpArcze.iLsfxCIvu
/// -ltprczAX -> -ltpAXrcze.iLsfxCIvu
/// ```
///
/// `A`/`X` sit after `-ltp` and before `r`, the same slot as the historical
/// capture after `-logDtp`. They are never a suffix.
pub const PRODUCT_COMPACT_FLAGS: &str = "-ltprcze.iLsfxCIvu";
pub const PRODUCT_COMPACT_FLAGS_XATTR: &str = "-ltpXrcze.iLsfxCIvu";
pub const PRODUCT_COMPACT_FLAGS_ACL: &str = "-ltpArcze.iLsfxCIvu";
pub const PRODUCT_COMPACT_FLAGS_ACL_XATTR: &str = "-ltpAXrcze.iLsfxCIvu";

#[cfg(test)]
const CAPTURE_FLAG_ANCHOR: &str = "-logDtp";
#[cfg(test)]
const PRODUCT_FLAG_ANCHOR: &str = "-ltp";

/// Which compact-flag family a session emits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompactFlagProfile {
    /// Integrated product: no owner/group/devices.
    Product,
    /// Historical stock-rsync capture with `-o -g -D`. Test-only.
    #[cfg_attr(not(test), allow(dead_code))]
    Capture,
}

/// Metadata bits read from the compact bundle that will actually be sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EffectiveMetadataFlags {
    pub preserve_owner: bool,
    pub preserve_group: bool,
    pub preserve_devices: bool,
    pub preserve_acls: bool,
    pub preserve_xattrs: bool,
}

impl EffectiveMetadataFlags {
    pub(crate) fn product(preserve_acls: bool, preserve_xattrs: bool) -> Self {
        Self {
            preserve_owner: false,
            preserve_group: false,
            preserve_devices: false,
            preserve_acls,
            preserve_xattrs,
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn capture(preserve_acls: bool, preserve_xattrs: bool) -> Self {
        Self {
            preserve_owner: true,
            preserve_group: true,
            preserve_devices: true,
            preserve_acls,
            preserve_xattrs,
        }
    }
}

/// Pick the compact flag bundle for the integrated product profile.
pub fn compact_flags_for(preserve_acls: bool, preserve_xattrs: bool) -> &'static str {
    compact_flags_for_profile(CompactFlagProfile::Product, preserve_acls, preserve_xattrs)
}

fn compact_flags_for_profile(
    profile: CompactFlagProfile,
    preserve_acls: bool,
    preserve_xattrs: bool,
) -> &'static str {
    match (profile, preserve_acls, preserve_xattrs) {
        (CompactFlagProfile::Product, false, false) => PRODUCT_COMPACT_FLAGS,
        (CompactFlagProfile::Product, false, true) => PRODUCT_COMPACT_FLAGS_XATTR,
        (CompactFlagProfile::Product, true, false) => PRODUCT_COMPACT_FLAGS_ACL,
        (CompactFlagProfile::Product, true, true) => PRODUCT_COMPACT_FLAGS_ACL_XATTR,
        (CompactFlagProfile::Capture, false, false) => OBSERVED_COMPACT_FLAGS,
        (CompactFlagProfile::Capture, false, true) => OBSERVED_COMPACT_FLAGS_XATTR,
        (CompactFlagProfile::Capture, true, false) => OBSERVED_COMPACT_FLAGS_ACL,
        (CompactFlagProfile::Capture, true, true) => OBSERVED_COMPACT_FLAGS_ACL_XATTR,
    }
}

/// Read owner/group/devices/A/X from the compact bundle on the completed argv.
///
/// `AEROFTP_RSYNC_SERVER_FLAGS` can replace the measured bundle, so the
/// driver must parse the argv that will actually be sent.
pub(crate) fn metadata_flags_from_args(args: &[String]) -> Option<EffectiveMetadataFlags> {
    let bundle = args
        .iter()
        .find(|arg| arg.starts_with('-') && !arg.starts_with("--") && arg.len() > 1)?;
    Some(EffectiveMetadataFlags {
        preserve_owner: bundle.contains('o'),
        preserve_group: bundle.contains('g'),
        preserve_devices: bundle.contains('D'),
        preserve_acls: bundle.contains('A'),
        preserve_xattrs: bundle.contains('X'),
    })
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
    /// Whether this session carries POSIX ACLs, i.e. whether `A` goes into
    /// the compact flag bundle. Defaults to `false` on every constructor,
    /// so the emitted command line is unchanged until a caller opts in via
    /// [`RemoteCommandSpec::with_acls`].
    pub preserve_acls: bool,
    /// Whether this session carries extended attributes, i.e. whether `X` goes
    /// into the compact flag bundle. Defaults to `false` on every constructor,
    /// so the emitted command line is unchanged until a caller opts in via
    /// [`RemoteCommandSpec::with_xattrs`].
    pub preserve_xattrs: bool,
    /// Compact-flag family. Production constructors pin [`CompactFlagProfile::Product`].
    flag_profile: CompactFlagProfile,
}

impl RemoteCommandSpec {
    pub fn upload(remote_target: impl Into<String>) -> Self {
        Self {
            remote_role: SessionRole::Receiver,
            remote_target: remote_target.into(),
            emit_stats: true,
            flavor: RemoteCommandFlavor::WrapperParity,
            preserve_acls: false,
            preserve_xattrs: false,
            flag_profile: CompactFlagProfile::Product,
        }
    }

    pub fn download(remote_target: impl Into<String>) -> Self {
        Self {
            remote_role: SessionRole::Sender,
            remote_target: remote_target.into(),
            emit_stats: false,
            flavor: RemoteCommandFlavor::WrapperParity,
            preserve_acls: false,
            preserve_xattrs: false,
            flag_profile: CompactFlagProfile::Product,
        }
    }

    /// Historical capture profile with `-o -g -D`. Frozen-oracle tests only.
    /// Not a production builder.
    #[cfg(test)]
    pub fn capture_upload(remote_target: impl Into<String>) -> Self {
        let mut spec = Self::upload(remote_target);
        spec.flag_profile = CompactFlagProfile::Capture;
        spec
    }

    /// Historical capture profile with `-o -g -D`. Frozen-oracle tests only.
    #[cfg(test)]
    pub fn capture_download(remote_target: impl Into<String>) -> Self {
        let mut spec = Self::download(remote_target);
        spec.flag_profile = CompactFlagProfile::Capture;
        spec
    }

    pub fn aerorsync_upload(remote_target: impl Into<String>) -> Self {
        Self {
            remote_role: SessionRole::Receiver,
            remote_target: remote_target.into(),
            emit_stats: true,
            flavor: RemoteCommandFlavor::AerorsyncServe,
            preserve_acls: false,
            preserve_xattrs: false,
            flag_profile: CompactFlagProfile::Product,
        }
    }

    pub fn aerorsync_download(remote_target: impl Into<String>) -> Self {
        Self {
            remote_role: SessionRole::Sender,
            remote_target: remote_target.into(),
            emit_stats: false,
            flavor: RemoteCommandFlavor::AerorsyncServe,
            preserve_acls: false,
            preserve_xattrs: false,
            flag_profile: CompactFlagProfile::Product,
        }
    }

    /// Opt this session into POSIX ACLs, which puts `A` into the compact
    /// flag bundle. Off by default: see `compact_flags_for`.
    pub fn with_acls(mut self, preserve_acls: bool) -> Self {
        self.preserve_acls = preserve_acls;
        self
    }

    /// Opt this session into extended attributes, which puts `X` into the
    /// compact flag bundle. Off by default: see `compact_flags_for`.
    pub fn with_xattrs(mut self, preserve_xattrs: bool) -> Self {
        self.preserve_xattrs = preserve_xattrs;
        self
    }

    pub(crate) fn requested_metadata_flags(&self) -> EffectiveMetadataFlags {
        match self.flag_profile {
            CompactFlagProfile::Product => {
                EffectiveMetadataFlags::product(self.preserve_acls, self.preserve_xattrs)
            }
            CompactFlagProfile::Capture => {
                EffectiveMetadataFlags::capture(self.preserve_acls, self.preserve_xattrs)
            }
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
                // Live-tuning escape hatch: `AEROFTP_RSYNC_SERVER_FLAGS`
                // overrides the byte-pinned compact flag string so a
                // non-stock remote `rsync --server` wrapper whose
                // ForceCommand whitelists a different argv can be probed
                // live without a rebuild per attempt. No-op when unset,
                // so the default stays byte-pinned against rsync 3.2.7.
                let compact_flags = std::env::var("AEROFTP_RSYNC_SERVER_FLAGS")
                    .ok()
                    .filter(|v| !v.trim().is_empty())
                    .unwrap_or_else(|| {
                        compact_flags_for_profile(
                            self.flag_profile,
                            self.preserve_acls,
                            self.preserve_xattrs,
                        )
                        .to_string()
                    });
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
            PRODUCT_COMPACT_FLAGS.starts_with("-l"),
            "product server flag string must keep -l (preserve links): {}",
            PRODUCT_COMPACT_FLAGS
        );
        assert!(
            OBSERVED_COMPACT_FLAGS.starts_with("-l"),
            "capture server flag string must keep -l (preserve links): {}",
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
            .strip_prefix(CAPTURE_FLAG_ANCHOR)
            .expect("base bundle must start with the -logDtp block");
        let xattr_head = OBSERVED_COMPACT_FLAGS_XATTR
            .strip_prefix(CAPTURE_FLAG_ANCHOR)
            .expect("xattr bundle must start with the same -logDtp block");

        assert_eq!(
            xattr_head,
            format!("X{base_head}"),
            "X must be inserted right after {CAPTURE_FLAG_ANCHOR} and before the \
             rest of the bundle; got {OBSERVED_COMPACT_FLAGS_XATTR}"
        );
        assert!(
            !OBSERVED_COMPACT_FLAGS_XATTR.ends_with('X'),
            "X is not a suffix: stock rsync never emits it at the end of the bundle"
        );
        assert_eq!(
            OBSERVED_COMPACT_FLAGS_XATTR, "-logDtpXrcze.iLsfxCIvu",
            "captured from rsync 3.2.7 on 2026-07-25; re-measure before changing"
        );
    }

    #[test]
    fn compact_flags_pin_acl_position_before_xattr() {
        let base_head = OBSERVED_COMPACT_FLAGS
            .strip_prefix(CAPTURE_FLAG_ANCHOR)
            .expect("base bundle must start with the -logDtp block");
        let acl_head = OBSERVED_COMPACT_FLAGS_ACL
            .strip_prefix(CAPTURE_FLAG_ANCHOR)
            .expect("acl bundle must start with the same -logDtp block");
        let ax_head = OBSERVED_COMPACT_FLAGS_ACL_XATTR
            .strip_prefix(CAPTURE_FLAG_ANCHOR)
            .expect("acl+xattr bundle must start with the same -logDtp block");

        assert_eq!(
            acl_head,
            format!("A{base_head}"),
            "A must be inserted right after {CAPTURE_FLAG_ANCHOR} and before the \
             rest of the bundle; got {OBSERVED_COMPACT_FLAGS_ACL}"
        );
        assert_eq!(
            ax_head,
            format!("AX{base_head}"),
            "A must sit immediately before X after {CAPTURE_FLAG_ANCHOR}; got \
             {OBSERVED_COMPACT_FLAGS_ACL_XATTR}"
        );
        assert_eq!(
            OBSERVED_COMPACT_FLAGS_ACL, "-logDtpArcze.iLsfxCIvu",
            "captured from rsync 3.2.7 on 2026-08-27; re-measure before changing"
        );
        assert_eq!(
            OBSERVED_COMPACT_FLAGS_ACL_XATTR, "-logDtpAXrcze.iLsfxCIvu",
            "captured from rsync 3.2.7 on 2026-08-27; re-measure before changing"
        );
    }

    #[test]
    fn product_compact_flag_literals_are_exact() {
        assert_eq!(PRODUCT_COMPACT_FLAGS, "-ltprcze.iLsfxCIvu");
        assert_eq!(PRODUCT_COMPACT_FLAGS_XATTR, "-ltpXrcze.iLsfxCIvu");
        assert_eq!(PRODUCT_COMPACT_FLAGS_ACL, "-ltpArcze.iLsfxCIvu");
        assert_eq!(PRODUCT_COMPACT_FLAGS_ACL_XATTR, "-ltpAXrcze.iLsfxCIvu");
        let base_head = PRODUCT_COMPACT_FLAGS
            .strip_prefix(PRODUCT_FLAG_ANCHOR)
            .expect("product bundle must start with -ltp");
        assert_eq!(
            PRODUCT_COMPACT_FLAGS_XATTR
                .strip_prefix(PRODUCT_FLAG_ANCHOR)
                .expect("xattr product bundle"),
            format!("X{base_head}")
        );
        assert_eq!(
            PRODUCT_COMPACT_FLAGS_ACL
                .strip_prefix(PRODUCT_FLAG_ANCHOR)
                .expect("acl product bundle"),
            format!("A{base_head}")
        );
        assert_eq!(
            PRODUCT_COMPACT_FLAGS_ACL_XATTR
                .strip_prefix(PRODUCT_FLAG_ANCHOR)
                .expect("ax product bundle"),
            format!("AX{base_head}")
        );
    }

    #[test]
    fn historical_capture_literals_remain_exact() {
        assert_eq!(OBSERVED_COMPACT_FLAGS, "-logDtprcze.iLsfxCIvu");
        assert_eq!(OBSERVED_COMPACT_FLAGS_XATTR, "-logDtpXrcze.iLsfxCIvu");
        assert_eq!(OBSERVED_COMPACT_FLAGS_ACL, "-logDtpArcze.iLsfxCIvu");
        assert_eq!(OBSERVED_COMPACT_FLAGS_ACL_XATTR, "-logDtpAXrcze.iLsfxCIvu");
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
            assert!(!spec.preserve_acls, "acls must default to off");
            assert!(
                spec.to_args().iter().any(|a| a == PRODUCT_COMPACT_FLAGS),
                "default product spec must emit the product bundle: {:?}",
                spec.to_args()
            );
            assert!(
                spec.to_args().iter().all(|a| a != OBSERVED_COMPACT_FLAGS),
                "production must not emit the historical capture bundle"
            );
        }

        let with_x = RemoteCommandSpec::upload("/workdir/t.bin").with_xattrs(true);
        assert!(
            with_x
                .to_args()
                .iter()
                .any(|a| a == PRODUCT_COMPACT_FLAGS_XATTR),
            "with_xattrs(true) must emit the product xattr bundle: {:?}",
            with_x.to_args()
        );
        assert!(
            with_x.to_args().iter().all(|a| a != PRODUCT_COMPACT_FLAGS),
            "the two product bundles are mutually exclusive"
        );
    }

    #[test]
    fn compact_flags_for_selects_the_four_product_constants() {
        assert_eq!(compact_flags_for(false, false), PRODUCT_COMPACT_FLAGS);
        assert_eq!(compact_flags_for(false, true), PRODUCT_COMPACT_FLAGS_XATTR);
        assert_eq!(compact_flags_for(true, false), PRODUCT_COMPACT_FLAGS_ACL);
        assert_eq!(
            compact_flags_for(true, true),
            PRODUCT_COMPACT_FLAGS_ACL_XATTR
        );
    }

    #[test]
    fn capture_profile_selects_the_four_historical_literals() {
        assert_eq!(
            compact_flags_for_profile(CompactFlagProfile::Capture, false, false),
            OBSERVED_COMPACT_FLAGS
        );
        assert_eq!(
            compact_flags_for_profile(CompactFlagProfile::Capture, false, true),
            OBSERVED_COMPACT_FLAGS_XATTR
        );
        assert_eq!(
            compact_flags_for_profile(CompactFlagProfile::Capture, true, false),
            OBSERVED_COMPACT_FLAGS_ACL
        );
        assert_eq!(
            compact_flags_for_profile(CompactFlagProfile::Capture, true, true),
            OBSERVED_COMPACT_FLAGS_ACL_XATTR
        );
        let spec = RemoteCommandSpec::capture_upload("/workdir/t.bin");
        assert!(
            spec.to_args().iter().any(|a| a == OBSERVED_COMPACT_FLAGS),
            "capture constructor must emit the historical bundle: {:?}",
            spec.to_args()
        );
    }

    #[test]
    fn product_effective_metadata_has_no_owner_group_or_devices() {
        for (acls, xattrs, bundle) in [
            (false, false, PRODUCT_COMPACT_FLAGS),
            (false, true, PRODUCT_COMPACT_FLAGS_XATTR),
            (true, false, PRODUCT_COMPACT_FLAGS_ACL),
            (true, true, PRODUCT_COMPACT_FLAGS_ACL_XATTR),
        ] {
            let spec = RemoteCommandSpec::upload("/workdir/t.bin")
                .with_acls(acls)
                .with_xattrs(xattrs);
            assert_eq!(
                spec.requested_metadata_flags(),
                EffectiveMetadataFlags::product(acls, xattrs)
            );
            assert_eq!(
                metadata_flags_from_args(&spec.to_args()),
                Some(EffectiveMetadataFlags::product(acls, xattrs))
            );
            assert!(spec.to_args().iter().any(|a| a == bundle));
        }
    }

    #[test]
    fn metadata_flags_are_read_from_the_effective_argv_bundle() {
        for (bundle, expected) in [
            (
                OBSERVED_COMPACT_FLAGS,
                EffectiveMetadataFlags::capture(false, false),
            ),
            (
                OBSERVED_COMPACT_FLAGS_XATTR,
                EffectiveMetadataFlags::capture(false, true),
            ),
            (
                OBSERVED_COMPACT_FLAGS_ACL,
                EffectiveMetadataFlags::capture(true, false),
            ),
            (
                OBSERVED_COMPACT_FLAGS_ACL_XATTR,
                EffectiveMetadataFlags::capture(true, true),
            ),
            (
                PRODUCT_COMPACT_FLAGS,
                EffectiveMetadataFlags::product(false, false),
            ),
            (
                PRODUCT_COMPACT_FLAGS_ACL_XATTR,
                EffectiveMetadataFlags::product(true, true),
            ),
        ] {
            let args = vec!["--server".to_string(), bundle.to_string()];
            assert_eq!(metadata_flags_from_args(&args), Some(expected));
        }
        let alternate = vec!["--server".to_string(), "-rlptgoDAXzc".to_string()];
        assert_eq!(
            metadata_flags_from_args(&alternate),
            Some(EffectiveMetadataFlags {
                preserve_owner: true,
                preserve_group: true,
                preserve_devices: true,
                preserve_acls: true,
                preserve_xattrs: true,
            })
        );
        assert_eq!(metadata_flags_from_args(&["--mode".to_string()]), None);
    }

    #[test]
    fn acl_flag_is_opt_in_and_sits_before_x_when_both_are_on() {
        let with_a = RemoteCommandSpec::upload("/workdir/t.bin").with_acls(true);
        assert!(
            with_a
                .to_args()
                .iter()
                .any(|a| a == PRODUCT_COMPACT_FLAGS_ACL),
            "with_acls(true) must emit the product ACL bundle: {:?}",
            with_a.to_args()
        );

        let with_ax = RemoteCommandSpec::upload("/workdir/t.bin")
            .with_acls(true)
            .with_xattrs(true);
        assert!(
            with_ax
                .to_args()
                .iter()
                .any(|a| a == PRODUCT_COMPACT_FLAGS_ACL_XATTR),
            "with_acls+with_xattrs must emit the product AX bundle: {:?}",
            with_ax.to_args()
        );
        let ax = PRODUCT_COMPACT_FLAGS_ACL_XATTR;
        let a_at = ax.find('A').expect("A present");
        let x_at = ax.find('X').expect("X present");
        assert!(a_at < x_at, "A must precede X in the compact flag bundle");
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
