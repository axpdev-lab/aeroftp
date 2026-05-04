//! aerorsync: AeroFTP's native Rust implementation of the rsync wire
//! protocol 31. Clean-room module born as Strada C, now promoted to
//! first-class component of the Aero family.
//!
//! The Cargo feature `aerorsync` is compiled by default. Runtime dispatch
//! is gated by `settings::load_native_rsync_enabled()` (default OFF, TOML
//! key retains the legacy `native_rsync_enabled` name for backward
//! compatibility of persisted user settings). See `README.md` for the
//! current product status, scope, and the open stock-rsync interop track.
//!
//! Useful local checks:
//!
//! ```text
//! cargo check --features aerorsync
//! cargo test  --features aerorsync --lib aerorsync
//! cargo clippy --all-targets --features aerorsync -- -D warnings
//! ```

// The module intentionally keeps protocol helpers and test fixtures available
// next to the production bridge. Several are exercised only by regression
// tests or live-test lanes, so dead-code warnings here would be noise.
#![allow(dead_code)]

pub mod delta_transport_impl;
pub mod driver;
pub mod engine_adapter;
pub mod events;
pub mod fallback_policy;
pub mod fixtures;
pub mod frame_io;
pub mod live_tests;
pub mod mock;
pub mod native_driver;
pub mod planner;
pub mod protocol;
pub mod real_wire;
pub mod remote_command;
pub mod rsync_event_bridge;
pub mod russh_session_transport;
pub mod server;
pub mod session;
pub mod shell_escape;
pub mod ssh_transport;
pub mod streaming_writer;
pub mod tests;
pub mod transport;
pub mod types;

pub const CURRENT_PROTOCOL_VERSION: u32 = 31;
