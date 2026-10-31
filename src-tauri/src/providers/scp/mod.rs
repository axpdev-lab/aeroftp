//! SCP: the engine that reaches SSH servers with no SFTP subsystem.
//!
//! A stock OpenWrt router (Dropbear, no `sftp-server`) and other BusyBox or
//! minimal systems ship `scp` but no SFTP. WinSCP reaches them by falling back
//! to SCP, and AeroFTP does the same: SCP is a protocol of its own, and an SFTP
//! connection to such a server continues in SCP mode on the same session.
//! Transfers run `scp -t` / `scp -f` over exec channels of the already
//! authenticated russh handle (never a second connection: see the v3.0.5
//! libssh2 path removed in #73), and everything SCP does not do (listing,
//! stat, mkdir, delete, rename) runs as POSIX shell commands.
//!
//! Design and measurements: `docs/dev/roadmap/APPENDIX-SCP-PROTOCOL/`.

pub mod codec;
pub mod listing;
pub mod session;
