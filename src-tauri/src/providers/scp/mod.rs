//! SCP: the engine that reaches SSH servers with no SFTP subsystem.
//!
//! A stock OpenWrt router (Dropbear, no `sftp-server`) and other BusyBox or
//! minimal systems ship `scp` but no SFTP. WinSCP reaches them by falling back
//! to SCP. This module is the engine for that: SCP is a protocol of its own,
//! and the plan is for an SFTP connection to such a server to continue in SCP
//! mode on the same session: [`provider::ScpProvider`] is both, the explicit
//! protocol and the provider `SftpProvider` hands its connection to when the
//! server refuses the `sftp` subsystem or has nothing behind it.
//! Transfers run `scp -t` / `scp -f` over exec channels of the already
//! authenticated russh handle (never a second connection: see the v3.0.5
//! libssh2 path removed in #73), and everything SCP does not do (listing,
//! stat, mkdir, delete, rename) runs as POSIX shell commands.
//!
//! Design and measurements: `docs/dev/roadmap/APPENDIX-SCP-PROTOCOL/`.

pub mod codec;
pub mod listing;
pub mod provider;
pub mod session;
