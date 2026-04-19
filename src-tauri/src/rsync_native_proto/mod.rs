//! Strada C — native rsync prototype.
//!
//! This module tree is gated behind the `proto_native_rsync` Cargo feature
//! (off by default). It never compiles in the shipped production build.
//! Enable with:
//!
//! ```text
//! cargo check --features proto_native_rsync
//! cargo test  --features proto_native_rsync rsync_native_proto
//! cargo clippy --all-targets --features proto_native_rsync -- -D warnings
//! ```
//!
//! Scope is intentionally narrow:
//!   - protocol 31
//!   - remote-shell mode
//!   - single-file
//!   - explicit sender/receiver role split
//!   - no live integration with AeroSync
//!
//! The production path remains the Phase 1 wrapper at `rsync_over_ssh.rs`.

// This is a scaffold: many types are currently only exercised by tests and by
// the mock transport. Dead-code warnings here would be pure noise until the
// native session flow is wired up in a later Strada C step.
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
pub mod server;
pub mod session;
pub mod ssh_transport;
pub mod tests;
pub mod transport;
pub mod types;

pub const CURRENT_PROTOCOL_VERSION: u32 = 31;
