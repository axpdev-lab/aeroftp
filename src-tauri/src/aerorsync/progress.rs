//! Progress reporting contract of the aerorsync driver.
//!
//! The driver calls the sink with `(transferred_wire_bytes, total_hint)` as
//! the transfer makes network progress. For an upload `total_hint` is the
//! full delta payload size; for a download it is the remote file size hint
//! (wire bytes may be fewer than the file on a real delta hit, so a caller
//! drawing a bar may see it under-fill and complete at reconstruction).
//! The driver throttles the calls (`native_driver::report_wire_progress`),
//! so the boxed closure fires at most about once per percent of movement.
//! `None` costs nothing per chunk: a single `is_none()` check.
//!
//! The application keeps its own structurally identical alias, because
//! the module that holds it also compiles with the `aerorsync` feature
//! off; the two must stay the same type, which the application-side test
//! `delta_transport::tests::aerorsync_delta_progress_sink_is_the_crate_progress_sink`
//! pins at compile time. This module never names the application path,
//! comments included.

#![cfg(feature = "aerorsync")]

/// Optional per-byte progress callback for a delta transfer.
pub type ProgressSink = Box<dyn FnMut(u64, u64) + Send>;
