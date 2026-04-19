//! Bridge layer: `NativeRsyncEvent` (from `events.rs`) → `RsyncEvent`
//! (from `crate::rsync_output`).
//!
//! # Why a dedicated module
//!
//! The production delta stack (`rsync_over_ssh` wrapper + the subprocess
//! `DeltaTransport`) already emits `RsyncEvent` by parsing rsync CLI
//! stdout. When the native driver replaces the subprocess, the UI + logging
//! stack MUST see identical event semantics so the user cannot tell which
//! backend produced them. Any divergence is a UX bug.
//!
//! The mapping is non-trivial (dual payload of `ErrorExit`, dropped
//! pipe-internal events, `IoError` vs `Warning` severity) and has to live
//! in a single source of truth. A scattered mapping would guarantee drift
//! the moment one codepath gets a new variant.
//!
//! # Scope of the bridge
//!
//! This module ONLY handles out-of-band (`NativeRsyncEvent`) traffic.
//! App-stream signals — `Progress` (byte-level transfer counter),
//! `FileStart` (emerges from FileListEntry parsing), and `Summary` (emitted
//! from the `SummaryFrame` decoded by `real_wire`) — are produced by the
//! native driver directly, NOT routed through the bridge. Keeping those
//! separate avoids a synthetic coupling between OOB classification and
//! app-stream accounting.
//!
//! # Dropped vs mapped events
//!
//! Several `NativeRsyncEvent` variants map to `None`:
//!
//! - Internal state markers (`Redo`, `Success`, `NoSend`) — pipe-internal
//!   signals between generator/receiver that would be noise in the UI.
//! - Keep-alive (`Noop`).
//! - Soft-exit propagation (`ErrorExit` with `None` or `Some(0)`) — mirrors
//!   `io.c:1668-1672` cleanup signaling, non-terminal by design.
//! - `Unknown` — a future protocol bump we do not recognise. We preserve
//!   the raw tag in `first_terminal` debug but do not surface to `RsyncEvent`
//!   which has no equivalent.
//! - Pipe-only `Stats` — surfaced on the app-stream side by the driver
//!   when it decodes the authoritative `SummaryFrame`.
//! - `Log`/`Client`/`Info` — rsync-internal chatter with no UI value in
//!   the production wrapper either (the stdout parser ignores these too).
//!
//! Mapped events:
//!
//! | `NativeRsyncEvent`                        | `RsyncEvent`                   |
//! |-------------------------------------------|--------------------------------|
//! | `Warning { message }`                     | `Warning { message }`          |
//! | `Error { message }` (terminal)            | `Error { message }` (terminal) |
//! | `ErrorXfer { message }` (terminal)        | `Error { message }` (terminal) |
//! | `ErrorSocket { message }` (terminal)      | `Error { message }` (terminal) |
//! | `ErrorUtf8 { message }`                   | `Warning { message }`          |
//! | `ErrorExit { Some(code!=0) }` (terminal)  | `Error { "remote exit N" }` (terminal) |
//! | `IoError { flags }`                       | `Warning { "io_error flags 0x…" }` |
//! | `IoTimeout { seconds }`                   | `Warning { "io_timeout Ns" }`  |
//! | `Deleted { path }`                        | `Warning { "deleted: path" }`  |
//!
//! # Terminal bookkeeping
//!
//! The bridge captures the FIRST terminal event for post-mortem access via
//! `first_terminal()`. Subsequent terminal events are mapped and forwarded
//! normally — we do not silently swallow trailing errors.

use crate::rsync_native_proto::events::{EventSink, NativeRsyncEvent};
use crate::rsync_output::RsyncEvent;

/// `EventSink` adapter that translates typed native events into the
/// existing `RsyncEvent` stream consumed by the production UI layer.
///
/// Parameterised on a sink callback `F: FnMut(RsyncEvent)` so the caller
/// decides where the mapped events go — a channel, a logger, a direct
/// progress bus. The bridge itself is transport-agnostic.
pub struct RsyncEventBridge<F: FnMut(RsyncEvent)> {
    sink: F,
    first_terminal: Option<NativeRsyncEvent>,
    events_forwarded: u64,
    events_dropped: u64,
}

impl<F: FnMut(RsyncEvent)> RsyncEventBridge<F> {
    pub fn new(sink: F) -> Self {
        Self {
            sink,
            first_terminal: None,
            events_forwarded: 0,
            events_dropped: 0,
        }
    }

    /// The first `NativeRsyncEvent` classified as terminal, if any.
    /// Preserves the full typed payload (including the raw `ErrorExit` code
    /// or the `tag` of an `Unknown` that happened to bail) so post-mortem
    /// logging / fallback classification can see exactly why the session
    /// ended.
    pub fn first_terminal(&self) -> Option<&NativeRsyncEvent> {
        self.first_terminal.as_ref()
    }

    pub fn events_forwarded(&self) -> u64 {
        self.events_forwarded
    }

    pub fn events_dropped(&self) -> u64 {
        self.events_dropped
    }

    pub fn bailed(&self) -> bool {
        self.first_terminal.is_some()
    }
}

impl<F: FnMut(RsyncEvent) + Send> EventSink for RsyncEventBridge<F> {
    fn handle(&mut self, event: NativeRsyncEvent) {
        if event.is_terminal() && self.first_terminal.is_none() {
            self.first_terminal = Some(event.clone());
        }
        match map_native_to_rsync_event(&event) {
            Some(mapped) => {
                (self.sink)(mapped);
                self.events_forwarded += 1;
            }
            None => {
                self.events_dropped += 1;
            }
        }
    }
}

/// Pure mapping. `None` means "drop this event silently" (not an error —
/// the native protocol carries many signals that have no UI analog).
///
/// Single source of truth for the mapping. Pinned exhaustively by tests
/// below. If `NativeRsyncEvent` grows a variant, the exhaustive `match`
/// forces the author to decide a mapping (fail-to-compile > silent drift).
pub fn map_native_to_rsync_event(event: &NativeRsyncEvent) -> Option<RsyncEvent> {
    match event {
        // Terminal textual → Error
        NativeRsyncEvent::Error { message } => Some(RsyncEvent::Error {
            message: message.clone(),
        }),
        NativeRsyncEvent::ErrorXfer { message } => Some(RsyncEvent::Error {
            message: message.clone(),
        }),
        NativeRsyncEvent::ErrorSocket { message } => Some(RsyncEvent::Error {
            message: message.clone(),
        }),

        // ErrorExit has dual semantics: None / Some(0) = cleanup, non-terminal,
        // drop. Some(code!=0) = terminal, render as Error with code.
        NativeRsyncEvent::ErrorExit { code } => match code {
            None | Some(0) => None,
            Some(c) => Some(RsyncEvent::Error {
                message: format!("remote rsync exited with code {}", c),
            }),
        },

        // Non-terminal warnings
        NativeRsyncEvent::Warning { message } => Some(RsyncEvent::Warning {
            message: message.clone(),
        }),
        NativeRsyncEvent::ErrorUtf8 { message } => Some(RsyncEvent::Warning {
            message: format!("utf-8 decode warning: {}", message),
        }),
        NativeRsyncEvent::IoError { flags } => Some(RsyncEvent::Warning {
            message: format!("io_error flags: 0x{:08X}", flags),
        }),
        NativeRsyncEvent::IoTimeout { seconds } => Some(RsyncEvent::Warning {
            message: format!("io_timeout refresh: {}s", seconds),
        }),
        NativeRsyncEvent::Deleted { path } => Some(RsyncEvent::Warning {
            message: format!("deleted: {}", path),
        }),

        // Pipe-internal + state markers — no UI value, mirror the
        // production wrapper's stdout parser which also ignores these.
        NativeRsyncEvent::Info { .. }
        | NativeRsyncEvent::Log { .. }
        | NativeRsyncEvent::Client { .. }
        | NativeRsyncEvent::Redo { .. }
        | NativeRsyncEvent::Stats { .. }
        | NativeRsyncEvent::Noop
        | NativeRsyncEvent::Success { .. }
        | NativeRsyncEvent::NoSend { .. }
        | NativeRsyncEvent::Unknown { .. } => None,
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rsync_native_proto::events::classify_oob_frame;
    use crate::rsync_native_proto::real_wire::MuxTag;

    // --- mapping pins --------------------------------------------------------

    #[test]
    fn error_maps_to_rsync_error_terminal() {
        let native = NativeRsyncEvent::Error {
            message: "remote kaboom".to_string(),
        };
        let out = map_native_to_rsync_event(&native);
        assert!(matches!(
            out,
            Some(RsyncEvent::Error { ref message }) if message == "remote kaboom"
        ));
    }

    #[test]
    fn error_xfer_maps_to_rsync_error() {
        let native = NativeRsyncEvent::ErrorXfer {
            message: "xfer failed".to_string(),
        };
        assert!(matches!(
            map_native_to_rsync_event(&native),
            Some(RsyncEvent::Error { .. })
        ));
    }

    #[test]
    fn error_socket_maps_to_rsync_error() {
        let native = NativeRsyncEvent::ErrorSocket {
            message: "socket broken".to_string(),
        };
        assert!(matches!(
            map_native_to_rsync_event(&native),
            Some(RsyncEvent::Error { .. })
        ));
    }

    #[test]
    fn error_exit_none_is_dropped() {
        let native = NativeRsyncEvent::ErrorExit { code: None };
        assert_eq!(map_native_to_rsync_event(&native), None);
    }

    #[test]
    fn error_exit_zero_is_dropped() {
        let native = NativeRsyncEvent::ErrorExit { code: Some(0) };
        assert_eq!(map_native_to_rsync_event(&native), None);
    }

    #[test]
    fn error_exit_nonzero_maps_to_error_with_code_in_message() {
        let native = NativeRsyncEvent::ErrorExit { code: Some(11) };
        match map_native_to_rsync_event(&native) {
            Some(RsyncEvent::Error { message }) => {
                assert!(message.contains("11"), "missing code: {message}");
            }
            other => panic!("expected Error with code, got {other:?}"),
        }
    }

    #[test]
    fn warning_passes_through_verbatim() {
        let native = NativeRsyncEvent::Warning {
            message: "the thing".to_string(),
        };
        assert_eq!(
            map_native_to_rsync_event(&native),
            Some(RsyncEvent::Warning {
                message: "the thing".to_string()
            })
        );
    }

    #[test]
    fn error_utf8_maps_to_warning_with_prefix() {
        let native = NativeRsyncEvent::ErrorUtf8 {
            message: "bad filename".to_string(),
        };
        match map_native_to_rsync_event(&native) {
            Some(RsyncEvent::Warning { message }) => {
                assert!(message.starts_with("utf-8"), "prefix missing: {message}");
                assert!(message.contains("bad filename"));
            }
            other => panic!("expected Warning, got {other:?}"),
        }
    }

    #[test]
    fn io_error_maps_to_warning_with_hex_flags() {
        let native = NativeRsyncEvent::IoError { flags: 0xDEAD_BEEF };
        match map_native_to_rsync_event(&native) {
            Some(RsyncEvent::Warning { message }) => {
                assert!(message.contains("0xDEADBEEF"), "hex missing: {message}");
            }
            other => panic!("expected Warning, got {other:?}"),
        }
    }

    #[test]
    fn io_timeout_maps_to_warning_with_seconds() {
        let native = NativeRsyncEvent::IoTimeout { seconds: 60 };
        match map_native_to_rsync_event(&native) {
            Some(RsyncEvent::Warning { message }) => {
                assert!(message.contains("60"));
                assert!(message.contains("s"));
            }
            other => panic!("expected Warning, got {other:?}"),
        }
    }

    #[test]
    fn deleted_maps_to_warning_with_prefix() {
        let native = NativeRsyncEvent::Deleted {
            path: "foo/bar.txt".to_string(),
        };
        match map_native_to_rsync_event(&native) {
            Some(RsyncEvent::Warning { message }) => {
                assert!(message.starts_with("deleted"), "missing prefix: {message}");
                assert!(message.contains("foo/bar.txt"));
            }
            other => panic!("expected Warning, got {other:?}"),
        }
    }

    // --- dropped events (pipe-internal + state markers) ---------------------

    #[test]
    fn info_is_dropped() {
        assert!(map_native_to_rsync_event(&NativeRsyncEvent::Info {
            message: "x".to_string(),
        })
        .is_none());
    }

    #[test]
    fn log_is_dropped() {
        assert!(map_native_to_rsync_event(&NativeRsyncEvent::Log {
            message: "x".to_string(),
        })
        .is_none());
    }

    #[test]
    fn client_is_dropped() {
        assert!(map_native_to_rsync_event(&NativeRsyncEvent::Client {
            message: "x".to_string(),
        })
        .is_none());
    }

    #[test]
    fn redo_is_dropped() {
        assert!(map_native_to_rsync_event(&NativeRsyncEvent::Redo { flist_index: 7 }).is_none());
    }

    #[test]
    fn stats_is_dropped() {
        assert!(map_native_to_rsync_event(&NativeRsyncEvent::Stats { total_read: 4096 }).is_none());
    }

    #[test]
    fn noop_is_dropped() {
        assert!(map_native_to_rsync_event(&NativeRsyncEvent::Noop).is_none());
    }

    #[test]
    fn success_is_dropped() {
        assert!(map_native_to_rsync_event(&NativeRsyncEvent::Success { flist_index: 5 }).is_none());
    }

    #[test]
    fn no_send_is_dropped() {
        assert!(map_native_to_rsync_event(&NativeRsyncEvent::NoSend { flist_index: 9 }).is_none());
    }

    #[test]
    fn unknown_is_dropped() {
        assert!(map_native_to_rsync_event(&NativeRsyncEvent::Unknown {
            tag: 77,
            payload: vec![1, 2, 3],
        })
        .is_none());
    }

    // --- exhaustive policy: terminality preserved ---------------------------

    #[test]
    fn every_terminal_native_event_maps_to_rsync_error() {
        // For every OOB frame that `events.rs` classifies as terminal, the
        // bridge MUST produce an `RsyncEvent::Error`. This pins the
        // cross-layer invariant "terminal ⇒ Error" so a future refactor of
        // either side cannot silently break it.
        let terminal_cases: Vec<NativeRsyncEvent> = vec![
            classify_oob_frame(MuxTag::Error, b"boom"),
            classify_oob_frame(MuxTag::ErrorXfer, b"xfer"),
            classify_oob_frame(MuxTag::ErrorSocket, b"sock"),
            classify_oob_frame(MuxTag::ErrorExit, &[0x05, 0x00, 0x00, 0x00]),
        ];
        for e in terminal_cases {
            assert!(e.is_terminal(), "{e:?} must be terminal per events.rs");
            match map_native_to_rsync_event(&e) {
                Some(RsyncEvent::Error { .. }) => {}
                other => panic!("terminal {e:?} → {other:?}, expected Error"),
            }
        }
    }

    #[test]
    fn non_terminal_native_event_never_maps_to_error() {
        // Symmetric pin: no non-terminal OOB frame may produce an Error.
        // Any drift here would misclassify recoverable warnings as fatal.
        let cases: Vec<NativeRsyncEvent> = vec![
            NativeRsyncEvent::Warning {
                message: "w".into(),
            },
            NativeRsyncEvent::ErrorUtf8 {
                message: "u".into(),
            },
            NativeRsyncEvent::IoError { flags: 1 },
            NativeRsyncEvent::IoTimeout { seconds: 30 },
            NativeRsyncEvent::ErrorExit { code: None },
            NativeRsyncEvent::ErrorExit { code: Some(0) },
            NativeRsyncEvent::Info {
                message: "i".into(),
            },
            NativeRsyncEvent::Log {
                message: "l".into(),
            },
            NativeRsyncEvent::Client {
                message: "c".into(),
            },
            NativeRsyncEvent::Redo { flist_index: 0 },
            NativeRsyncEvent::Stats { total_read: 0 },
            NativeRsyncEvent::Noop,
            NativeRsyncEvent::Success { flist_index: 0 },
            NativeRsyncEvent::Deleted { path: "p".into() },
            NativeRsyncEvent::NoSend { flist_index: 0 },
            NativeRsyncEvent::Unknown {
                tag: 77,
                payload: vec![],
            },
        ];
        for e in cases {
            assert!(!e.is_terminal(), "{e:?} must NOT be terminal per events.rs");
            if let Some(RsyncEvent::Error { .. }) = map_native_to_rsync_event(&e) {
                panic!("non-terminal {e:?} mapped to Error — severity drift");
            }
        }
    }

    // --- bridge sink behaviour ---------------------------------------------

    #[test]
    fn bridge_forwards_mapped_events_via_sink_callback() {
        // `EventSink: Send` means the sink closure must capture `Send`
        // state — a `Mutex` rather than a `RefCell`. Same semantics,
        // compile-time guarantee that the bridge works in `Send` futures.
        let collected = std::sync::Arc::new(std::sync::Mutex::new(Vec::<RsyncEvent>::new()));
        let collected_for_closure = collected.clone();
        let mut bridge =
            RsyncEventBridge::new(move |evt| collected_for_closure.lock().unwrap().push(evt));
        bridge.handle(NativeRsyncEvent::Warning {
            message: "w".into(),
        });
        bridge.handle(NativeRsyncEvent::Info {
            message: "i".into(),
        });
        bridge.handle(NativeRsyncEvent::Error {
            message: "e".into(),
        });
        let log = collected.lock().unwrap();
        assert_eq!(log.len(), 2, "Info must be dropped");
        assert!(matches!(log[0], RsyncEvent::Warning { .. }));
        assert!(matches!(log[1], RsyncEvent::Error { .. }));
    }

    #[test]
    fn bridge_captures_first_terminal_and_forwards_subsequent() {
        let collected = std::sync::Arc::new(std::sync::Mutex::new(Vec::<RsyncEvent>::new()));
        let collected_for_closure = collected.clone();
        let mut bridge =
            RsyncEventBridge::new(move |evt| collected_for_closure.lock().unwrap().push(evt));
        bridge.handle(NativeRsyncEvent::Warning {
            message: "pre".into(),
        });
        bridge.handle(NativeRsyncEvent::Error {
            message: "first".into(),
        });
        bridge.handle(NativeRsyncEvent::Error {
            message: "second".into(),
        });
        // First terminal pinned, but subsequent Errors still flow to the
        // sink — callers may need the trailing context (often an ExitCode).
        assert!(bridge.bailed());
        match bridge.first_terminal() {
            Some(NativeRsyncEvent::Error { message }) => assert_eq!(message, "first"),
            other => panic!("expected Error first_terminal, got {other:?}"),
        }
        let log = collected.lock().unwrap();
        assert_eq!(log.len(), 3);
    }

    #[test]
    fn bridge_counters_track_forwarded_vs_dropped() {
        let mut bridge = RsyncEventBridge::new(|_| ());
        bridge.handle(NativeRsyncEvent::Info {
            message: "i".into(),
        });
        bridge.handle(NativeRsyncEvent::Noop);
        bridge.handle(NativeRsyncEvent::Warning {
            message: "w".into(),
        });
        bridge.handle(NativeRsyncEvent::Error {
            message: "e".into(),
        });
        assert_eq!(bridge.events_forwarded(), 2);
        assert_eq!(bridge.events_dropped(), 2);
    }

    #[test]
    fn bridge_with_no_terminal_does_not_flag_bailed() {
        let mut bridge = RsyncEventBridge::new(|_| ());
        bridge.handle(NativeRsyncEvent::Warning {
            message: "w".into(),
        });
        bridge.handle(NativeRsyncEvent::IoError { flags: 1 });
        assert!(!bridge.bailed());
        assert!(bridge.first_terminal().is_none());
    }

    #[test]
    fn bridge_preserves_full_native_payload_in_first_terminal() {
        // HARDENING: the bridge's `first_terminal()` must return the
        // unmodified NativeRsyncEvent (not the lossy RsyncEvent mapping).
        // A future fallback policy classifier needs the raw ErrorExit code
        // / Unknown tag to make an informed decision.
        let mut bridge = RsyncEventBridge::new(|_| ());
        bridge.handle(NativeRsyncEvent::ErrorExit { code: Some(23) });
        match bridge.first_terminal() {
            Some(NativeRsyncEvent::ErrorExit { code: Some(23) }) => {}
            other => panic!("payload lost in terminal capture: {other:?}"),
        }
    }
}
