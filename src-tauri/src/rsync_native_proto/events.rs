//! Out-of-band rsync mux event layer (Sinergia 8h).
//!
//! After Sinergie 8b–8c the demuxer surfaces every non-`MSG_DATA` frame as
//! a `(MuxTag, payload)` pair through `real_wire::ReassemblyReport`.
//! Sinergia 8h turns those pairs into a typed, classified
//! `NativeRsyncEvent` enum that downstream consumers (the future S8i
//! real_wire driver) can pattern-match on without re-doing byte
//! archaeology.
//!
//! # Source-of-truth references
//!
//! Every variant + every parsing rule is anchored in rsync 3.2.7.
//! Citations in the doc-comments use the `file:line` form so the next
//! reader can verify in one click. The mapping was validated against the
//! frozen byte oracle in `capture/artifacts_real/frozen/` (zero OOB on all
//! four streams, consistent with `MSG_STATS` being a generator-only pipe
//! signal — see `io.c:1507-1511`).
//!
//! # Severity policy
//!
//! Four tags are **terminal** (the consumer must abort the session):
//!
//! - `MuxTag::Error` (3)        — `log.c:251-253`, `FERROR`
//! - `MuxTag::ErrorXfer` (1)    — `log.c:251-253`, `FERROR_XFER`
//! - `MuxTag::ErrorSocket` (5)  — `log.c:281-282`, reroutes stderr
//! - `MuxTag::ErrorExit` (86)   — `io.c:1662-1700`, only when code != 0
//!
//! Everything else is **non-terminal**. Critical correction caught in S8h:
//!
//! - `MuxTag::IoError` (22)     — `io.c:1520-1528`, `io_error |= val`
//!   (flag merging, warning-level, NEVER bail)
//! - `MuxTag::IoTimeout` (33)   — `io.c:1529-1539`, client-side timeout
//!   refresh, NEVER bail
//! - `MuxTag::ErrorUtf8` (8)    — `log.c:362-395`, iconv warning, NEVER
//!   bail
//! - `MuxTag::Redo` (9)         — `receiver.c:958`, retry signal
//! - `MuxTag::Stats`/`Success`/`Deleted`/`NoSend`/`Noop`/`Log`/`Info`/
//!   `Warning`/`Client` — all state markers or display hints
//!
//! An early draft of the classifier put IoError/IoTimeout/ErrorUtf8 in the
//! terminal set. The S8h trust-but-verify pass against `io.c` rejected
//! that — a generator that bails on `io_error |= val` would treat every
//! permission error during file-list scan as a fatal session failure,
//! breaking real-world rsync semantics. Pinned by
//! `terminal_set_matches_io_c_policy` in tests below.
//!
//! # Payload-format pitfalls
//!
//! - All textual payloads (Info/Warning/Error/ErrorXfer/ErrorSocket/Log/
//!   Client/ErrorUtf8) are **UTF-8 with a trailing newline included**.
//!   `log.c:353` strips the newline at display time; we strip it at
//!   classification time so consumers see clean strings. Lossy decode
//!   (`from_utf8_lossy`) is used unconditionally — a malformed UTF-8
//!   sequence inside the payload is logged-from-remote-style data, not a
//!   protocol error.
//! - All integer payloads (Redo/IoError/IoTimeout/Success/NoSend/
//!   ErrorExit-with-code) are **little-endian u32** (4 bytes), per
//!   `io.c:1066` `SIVAL` + `io.c:read_int` family.
//! - `Stats` carries a 64-bit `total_read` (`io.c:1507-1511`). Pipe-only
//!   in current rsync, but we accept it on the wire so a future remote
//!   that tunnels generator stats (or a hand-crafted test) doesn't trip a
//!   panic.
//! - `ErrorExit` has TWO payload forms: 0 bytes (cleanup propagation,
//!   non-terminal — see `io.c:1668-1672`) or 4 bytes (binary exit code,
//!   terminal iff non-zero).
//! - `Deleted` carries a UTF-8 filename, optionally null-terminated for
//!   directories (`log.c:863`).
//! - `Noop` (42) has empty payload — keep-alive only.
//! - `Unknown(tag)` never panics; raw payload is preserved byte-for-byte
//!   so a future protocol bump doesn't silently corrupt data.

use crate::rsync_native_proto::real_wire::MuxTag;

/// Severity gradient. Richer than a single `is_terminal` bool — lets the
/// driver progress events route to different sinks (status bar vs log
/// pane vs error toast) without switching on the variant in two places.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventSeverity {
    /// Informational chatter (`Info`, `Log`, `Client`, state markers).
    Info,
    /// Warning-level (`Warning`, `IoError`, `IoTimeout`, `ErrorUtf8`).
    /// The session continues but the consumer should surface to the user.
    Warning,
    /// Soft error — emitted but the session may still continue
    /// (`ErrorExit` with code 0). Reserved for future use.
    Error,
    /// Terminal — the consumer MUST abort. See the `is_terminal`
    /// list in the module-level doc.
    Terminal,
}

/// Classified out-of-band mux event. One variant per `MuxTag` minus
/// `Data` (which is the app stream and never an event). `Unknown`
/// preserves the raw tag + payload so future protocol bumps surface
/// cleanly instead of silently corrupting state.
///
/// Variants ordered to mirror `MuxTag::from_code` for cross-reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeRsyncEvent {
    /// `MSG_ERROR_XFER` (tag 1) — file-transfer error reported by remote.
    /// `log.c:251-253` (FERROR_XFER), text payload, terminal.
    ErrorXfer { message: String },

    /// `MSG_INFO` (tag 2) — info-level message. `log.c:251-253` (FINFO).
    Info { message: String },

    /// `MSG_ERROR` (tag 3) — error-level message. `log.c:251-253`
    /// (FERROR). Terminal.
    Error { message: String },

    /// `MSG_WARNING` (tag 4) — warning-level message. `log.c:251-253`
    /// (FWARNING). Non-terminal.
    Warning { message: String },

    /// `MSG_ERROR_SOCKET` (tag 5) — socket-level error. `log.c:281-282`
    /// reroutes to stderr. Terminal.
    ErrorSocket { message: String },

    /// `MSG_LOG` (tag 6) — local log message. `log.c:304-307` shows it
    /// is rarely on the wire but we accept it.
    Log { message: String },

    /// `MSG_CLIENT` (tag 7) — message addressed to the client. `log.c:288`
    /// converts to FINFO at display time.
    Client { message: String },

    /// `MSG_ERROR_UTF8` (tag 8) — UTF-8 decoding warning, typically a
    /// filename outside the active locale. `log.c:362-395` runs iconv;
    /// non-terminal.
    ErrorUtf8 { message: String },

    /// `MSG_REDO` (tag 9) — file-list-index retry signal from receiver
    /// to generator. `receiver.c:958`. Pipe-internal in current rsync;
    /// non-terminal.
    Redo { flist_index: u32 },

    /// `MSG_STATS` (tag 10) — pipe-only stats from sender/receiver to
    /// generator. `io.c:1507-1511`, `!am_generator => goto invalid_msg`.
    /// Surfaces here so a hand-crafted test or a future tunneled
    /// generator-feed doesn't panic.
    Stats { total_read: u64 },

    /// `MSG_IO_ERROR` (tag 22) — receiver-side io_error flag merge.
    /// `io.c:1520-1528`, `io_error |= val`. **Non-terminal**.
    IoError { flags: u32 },

    /// `MSG_IO_TIMEOUT` (tag 33) — client-side io_timeout refresh.
    /// `io.c:1529-1539`. Non-terminal.
    IoTimeout { seconds: u32 },

    /// `MSG_NOOP` (tag 42) — keep-alive. Empty payload. Non-terminal.
    Noop,

    /// `MSG_ERROR_EXIT` (tag 86) — propagated exit-code. `io.c:1662-1700`.
    /// Empty payload encodes code 0 (cleanup propagation, non-terminal);
    /// 4-byte payload carries a binary code, terminal iff non-zero.
    ErrorExit { code: Option<u32> },

    /// `MSG_SUCCESS` (tag 100) — file-list-index success marker.
    /// `io.c:1601-1615`. Non-terminal state push.
    Success { flist_index: u32 },

    /// `MSG_DELETED` (tag 101) — `--delete` notification from generator.
    /// `log.c:863`. Filename UTF-8, optionally null-terminated for dirs.
    Deleted { path: String },

    /// `MSG_NO_SEND` (tag 102) — receiver could not open file for sending.
    /// `io.c:1617-1625`. Non-terminal state push.
    NoSend { flist_index: u32 },

    /// Tag we do not know yet. Never panics — payload preserved raw.
    /// Always treated as non-terminal so a future opcode that the
    /// receiver does not recognise does not bring the session down.
    Unknown { tag: u8, payload: Vec<u8> },
}

impl NativeRsyncEvent {
    /// True iff the consumer MUST abort the session on this event.
    /// Single source of truth for the bail policy. Pinned by
    /// `terminal_set_matches_io_c_policy` in tests.
    pub fn is_terminal(&self) -> bool {
        matches!(self.severity(), EventSeverity::Terminal)
    }

    /// Severity gradient. See `EventSeverity` doc.
    pub fn severity(&self) -> EventSeverity {
        match self {
            NativeRsyncEvent::Error { .. }
            | NativeRsyncEvent::ErrorXfer { .. }
            | NativeRsyncEvent::ErrorSocket { .. } => EventSeverity::Terminal,
            NativeRsyncEvent::ErrorExit { code } => match code {
                Some(0) | None => EventSeverity::Info,
                Some(_) => EventSeverity::Terminal,
            },
            NativeRsyncEvent::Warning { .. }
            | NativeRsyncEvent::IoError { .. }
            | NativeRsyncEvent::IoTimeout { .. }
            | NativeRsyncEvent::ErrorUtf8 { .. } => EventSeverity::Warning,
            NativeRsyncEvent::Info { .. }
            | NativeRsyncEvent::Log { .. }
            | NativeRsyncEvent::Client { .. }
            | NativeRsyncEvent::Redo { .. }
            | NativeRsyncEvent::Stats { .. }
            | NativeRsyncEvent::Noop
            | NativeRsyncEvent::Success { .. }
            | NativeRsyncEvent::Deleted { .. }
            | NativeRsyncEvent::NoSend { .. }
            | NativeRsyncEvent::Unknown { .. } => EventSeverity::Info,
        }
    }

    /// The mux tag this event was decoded from. `Unknown` returns the
    /// raw tag byte preserved at classification time.
    pub fn tag(&self) -> MuxTag {
        match self {
            NativeRsyncEvent::ErrorXfer { .. } => MuxTag::ErrorXfer,
            NativeRsyncEvent::Info { .. } => MuxTag::Info,
            NativeRsyncEvent::Error { .. } => MuxTag::Error,
            NativeRsyncEvent::Warning { .. } => MuxTag::Warning,
            NativeRsyncEvent::ErrorSocket { .. } => MuxTag::ErrorSocket,
            NativeRsyncEvent::Log { .. } => MuxTag::Log,
            NativeRsyncEvent::Client { .. } => MuxTag::Client,
            NativeRsyncEvent::ErrorUtf8 { .. } => MuxTag::ErrorUtf8,
            NativeRsyncEvent::Redo { .. } => MuxTag::Redo,
            NativeRsyncEvent::Stats { .. } => MuxTag::Stats,
            NativeRsyncEvent::IoError { .. } => MuxTag::IoError,
            NativeRsyncEvent::IoTimeout { .. } => MuxTag::IoTimeout,
            NativeRsyncEvent::Noop => MuxTag::Noop,
            NativeRsyncEvent::ErrorExit { .. } => MuxTag::ErrorExit,
            NativeRsyncEvent::Success { .. } => MuxTag::Success,
            NativeRsyncEvent::Deleted { .. } => MuxTag::Deleted,
            NativeRsyncEvent::NoSend { .. } => MuxTag::NoSend,
            NativeRsyncEvent::Unknown { tag, .. } => MuxTag::from_code(*tag),
        }
    }

    /// Best-effort textual rendering for log/UI use. Empty for purely
    /// numeric / state-marker variants.
    pub fn message(&self) -> Option<&str> {
        match self {
            NativeRsyncEvent::ErrorXfer { message }
            | NativeRsyncEvent::Info { message }
            | NativeRsyncEvent::Error { message }
            | NativeRsyncEvent::Warning { message }
            | NativeRsyncEvent::ErrorSocket { message }
            | NativeRsyncEvent::Log { message }
            | NativeRsyncEvent::Client { message }
            | NativeRsyncEvent::ErrorUtf8 { message } => Some(message.as_str()),
            NativeRsyncEvent::Deleted { path } => Some(path.as_str()),
            _ => None,
        }
    }
}

/// Decode a single OOB frame `(tag, payload)` into a typed event.
///
/// Pure function. Never panics. Never returns `Result` — every malformed
/// payload is folded into `Unknown` (for an unrecognised tag) or a
/// best-effort decode (e.g. truncated 4-byte int yields `Some(0)` for
/// missing slots). Rationale: rsync receivers tolerate slightly
/// malformed OOB rather than aborting; we mirror that semantics.
///
/// `Data` (tag 0) is a programming error — that frame is the app
/// stream, not an event. We classify it as `Unknown` rather than panic
/// so a misuse surfaces in tests instead of crashing prod.
pub fn classify_oob_frame(tag: MuxTag, payload: &[u8]) -> NativeRsyncEvent {
    match tag {
        MuxTag::Data => NativeRsyncEvent::Unknown {
            tag: tag.code(),
            payload: payload.to_vec(),
        },

        // Textual payloads: UTF-8 lossy + strip trailing \r\n.
        MuxTag::ErrorXfer => NativeRsyncEvent::ErrorXfer {
            message: decode_text(payload),
        },
        MuxTag::Info => NativeRsyncEvent::Info {
            message: decode_text(payload),
        },
        MuxTag::Error => NativeRsyncEvent::Error {
            message: decode_text(payload),
        },
        MuxTag::Warning => NativeRsyncEvent::Warning {
            message: decode_text(payload),
        },
        MuxTag::ErrorSocket => NativeRsyncEvent::ErrorSocket {
            message: decode_text(payload),
        },
        MuxTag::Log => NativeRsyncEvent::Log {
            message: decode_text(payload),
        },
        MuxTag::Client => NativeRsyncEvent::Client {
            message: decode_text(payload),
        },
        MuxTag::ErrorUtf8 => NativeRsyncEvent::ErrorUtf8 {
            message: decode_text(payload),
        },
        MuxTag::Deleted => NativeRsyncEvent::Deleted {
            // `log.c:863` may include a trailing null for directory
            // markers. Strip both null and newline trailers.
            path: decode_text(strip_trailing_null(payload)),
        },

        // Integer payloads (4 bytes LE, SIVAL).
        MuxTag::Redo => NativeRsyncEvent::Redo {
            flist_index: read_u32_le_or_zero(payload),
        },
        MuxTag::IoError => NativeRsyncEvent::IoError {
            flags: read_u32_le_or_zero(payload),
        },
        MuxTag::IoTimeout => NativeRsyncEvent::IoTimeout {
            seconds: read_u32_le_or_zero(payload),
        },
        MuxTag::Success => NativeRsyncEvent::Success {
            flist_index: read_u32_le_or_zero(payload),
        },
        MuxTag::NoSend => NativeRsyncEvent::NoSend {
            flist_index: read_u32_le_or_zero(payload),
        },

        // 64-bit binary (Stats — pipe-only in real rsync, accepted here
        // for hand-crafted tests / future tunneling).
        MuxTag::Stats => NativeRsyncEvent::Stats {
            total_read: read_u64_le_or_zero(payload),
        },

        // Empty payload only.
        MuxTag::Noop => NativeRsyncEvent::Noop,

        // Two payload forms — see `io.c:1668-1672`.
        MuxTag::ErrorExit => NativeRsyncEvent::ErrorExit {
            code: classify_error_exit_payload(payload),
        },

        MuxTag::Unknown(raw) => NativeRsyncEvent::Unknown {
            tag: raw,
            payload: payload.to_vec(),
        },
    }
}

/// `io.c:1668-1672` semantics: 0-byte payload encodes exit code 0
/// (cleanup propagation), 4-byte payload carries a binary code,
/// anything else is malformed and we fold to `None` (treat as
/// cleanup-style).
fn classify_error_exit_payload(payload: &[u8]) -> Option<u32> {
    match payload.len() {
        0 => None,
        4 => Some(read_u32_le_or_zero(payload)),
        _ => None,
    }
}

/// UTF-8 lossy decode + strip trailing `\r` / `\n`. Mirrors
/// `log.c:353` `trailing_CR_or_NL` + the implicit lossy path that
/// `rwrite` falls back to when iconv is unavailable.
fn decode_text(payload: &[u8]) -> String {
    let s = String::from_utf8_lossy(payload);
    s.trim_end_matches(['\n', '\r']).to_string()
}

fn strip_trailing_null(payload: &[u8]) -> &[u8] {
    if let Some(last) = payload.last() {
        if *last == 0 {
            return &payload[..payload.len() - 1];
        }
    }
    payload
}

fn read_u32_le_or_zero(payload: &[u8]) -> u32 {
    if payload.len() < 4 {
        return 0;
    }
    let mut arr = [0u8; 4];
    arr.copy_from_slice(&payload[..4]);
    u32::from_le_bytes(arr)
}

fn read_u64_le_or_zero(payload: &[u8]) -> u64 {
    if payload.len() < 8 {
        return 0;
    }
    let mut arr = [0u8; 8];
    arr.copy_from_slice(&payload[..8]);
    u64::from_le_bytes(arr)
}

// ============================================================================
// Sinks — the consumer-side abstraction.
// ============================================================================

/// Callback trait for OOB events. Sync-only by design: events arrive
/// after reassembly, fully buffered in memory; an async sink would add
/// no value and would couple this layer to a runtime choice that
/// belongs in the driver.
///
/// Implementors choose how to handle individual events. The default
/// `handle` dispatcher routes to per-severity hooks (`on_info`,
/// `on_warning`, `on_terminal`) so simple consumers only override
/// what they care about.
///
/// The `Send` super-bound is required so drivers that hold
/// `&mut dyn EventSink` across an `.await` point can be embedded in
/// `Send` futures (needed by `DeltaTransport` impls — see
/// `delta_transport_impl.rs`). All existing `impl EventSink` consumers
/// are already `Send` (simple owned structs or closures that capture
/// `Send` state), so the bound is purely additive.
pub trait EventSink: Send {
    fn handle(&mut self, event: NativeRsyncEvent) {
        match event.severity() {
            EventSeverity::Info => self.on_info(event),
            EventSeverity::Warning => self.on_warning(event),
            EventSeverity::Error => self.on_error(event),
            EventSeverity::Terminal => self.on_terminal(event),
        }
    }

    fn on_info(&mut self, _event: NativeRsyncEvent) {}
    fn on_warning(&mut self, _event: NativeRsyncEvent) {}
    fn on_error(&mut self, _event: NativeRsyncEvent) {}
    fn on_terminal(&mut self, _event: NativeRsyncEvent) {}
}

/// Test sink that accumulates every event in encounter order.
#[derive(Debug, Default, Clone)]
pub struct CollectingSink {
    pub events: Vec<NativeRsyncEvent>,
}

impl EventSink for CollectingSink {
    fn handle(&mut self, event: NativeRsyncEvent) {
        self.events.push(event);
    }
}

/// Test sink that captures the FIRST terminal event and drops the rest
/// after that point. Subsequent non-terminal events ARE still recorded
/// in `trailing` to surface accidental "after the terminal" emissions.
///
/// Use `first_terminal()` after consumption to see whether the stream
/// bailed and on which event.
#[derive(Debug, Default, Clone)]
pub struct BailingSink {
    pub before_terminal: Vec<NativeRsyncEvent>,
    pub terminal: Option<NativeRsyncEvent>,
    pub trailing: Vec<NativeRsyncEvent>,
}

impl BailingSink {
    pub fn first_terminal(&self) -> Option<&NativeRsyncEvent> {
        self.terminal.as_ref()
    }

    pub fn bailed(&self) -> bool {
        self.terminal.is_some()
    }
}

impl EventSink for BailingSink {
    fn handle(&mut self, event: NativeRsyncEvent) {
        if self.terminal.is_some() {
            self.trailing.push(event);
            return;
        }
        if event.is_terminal() {
            self.terminal = Some(event);
        } else {
            self.before_terminal.push(event);
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn all_known_mux_tags() -> Vec<MuxTag> {
        vec![
            MuxTag::Data,
            MuxTag::ErrorXfer,
            MuxTag::Info,
            MuxTag::Error,
            MuxTag::Warning,
            MuxTag::ErrorSocket,
            MuxTag::Log,
            MuxTag::Client,
            MuxTag::ErrorUtf8,
            MuxTag::Redo,
            MuxTag::Stats,
            MuxTag::IoError,
            MuxTag::IoTimeout,
            MuxTag::Noop,
            MuxTag::ErrorExit,
            MuxTag::Success,
            MuxTag::Deleted,
            MuxTag::NoSend,
        ]
    }

    // -------------------------------------------------------------------------
    // Textual classification
    // -------------------------------------------------------------------------

    #[test]
    fn classify_info_strips_trailing_newline() {
        let event = classify_oob_frame(MuxTag::Info, b"hello\n");
        assert_eq!(
            event,
            NativeRsyncEvent::Info {
                message: "hello".to_string()
            }
        );
    }

    #[test]
    fn classify_info_strips_crlf() {
        let event = classify_oob_frame(MuxTag::Info, b"hello\r\n");
        assert_eq!(
            event,
            NativeRsyncEvent::Info {
                message: "hello".to_string()
            }
        );
    }

    #[test]
    fn classify_warning_preserves_inner_newlines_and_strips_only_trailing() {
        let event = classify_oob_frame(MuxTag::Warning, b"line1\nline2\n");
        let NativeRsyncEvent::Warning { message } = event else {
            panic!("expected Warning");
        };
        assert_eq!(message, "line1\nline2");
    }

    #[test]
    fn classify_error_with_invalid_utf8_uses_lossy_decode_no_panic() {
        let event = classify_oob_frame(MuxTag::Error, &[0x66, 0x6F, 0xFF, 0xFE, 0x6F]);
        let NativeRsyncEvent::Error { message } = event else {
            panic!("expected Error");
        };
        assert!(message.contains('f'));
        assert!(message.contains('o'));
    }

    #[test]
    fn classify_error_empty_payload_is_empty_string_no_panic() {
        let event = classify_oob_frame(MuxTag::Error, &[]);
        assert_eq!(
            event,
            NativeRsyncEvent::Error {
                message: String::new()
            }
        );
    }

    // -------------------------------------------------------------------------
    // Integer classification
    // -------------------------------------------------------------------------

    #[test]
    fn classify_redo_decodes_le_u32() {
        let event = classify_oob_frame(MuxTag::Redo, &[0x2A, 0x00, 0x00, 0x00]);
        assert_eq!(event, NativeRsyncEvent::Redo { flist_index: 42 });
    }

    #[test]
    fn classify_io_error_decodes_le_u32() {
        // io.c:1525 io_error |= val pattern. 0xDEADBEEF as a flag set.
        let event = classify_oob_frame(MuxTag::IoError, &[0xEF, 0xBE, 0xAD, 0xDE]);
        assert_eq!(event, NativeRsyncEvent::IoError { flags: 0xDEAD_BEEF });
    }

    #[test]
    fn classify_io_timeout_decodes_seconds() {
        let event = classify_oob_frame(MuxTag::IoTimeout, &[0x3C, 0x00, 0x00, 0x00]);
        assert_eq!(event, NativeRsyncEvent::IoTimeout { seconds: 60 });
    }

    #[test]
    fn classify_truncated_int_payload_yields_zero_no_panic() {
        let event = classify_oob_frame(MuxTag::Redo, &[0x01, 0x02]);
        assert_eq!(event, NativeRsyncEvent::Redo { flist_index: 0 });
    }

    #[test]
    fn classify_stats_decodes_le_u64() {
        let event = classify_oob_frame(
            MuxTag::Stats,
            &[0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
        );
        assert_eq!(event, NativeRsyncEvent::Stats { total_read: 4096 });
    }

    // -------------------------------------------------------------------------
    // ErrorExit dual payload
    // -------------------------------------------------------------------------

    #[test]
    fn classify_error_exit_empty_payload_is_none_non_terminal() {
        // io.c:1668-1672: 0-byte payload -> exit code 0 (cleanup signal).
        let event = classify_oob_frame(MuxTag::ErrorExit, &[]);
        assert_eq!(event, NativeRsyncEvent::ErrorExit { code: None });
        assert!(!event.is_terminal());
    }

    #[test]
    fn classify_error_exit_zero_code_payload_is_some_zero_non_terminal() {
        let event = classify_oob_frame(MuxTag::ErrorExit, &[0x00, 0x00, 0x00, 0x00]);
        assert_eq!(event, NativeRsyncEvent::ErrorExit { code: Some(0) });
        assert!(!event.is_terminal());
    }

    #[test]
    fn classify_error_exit_nonzero_code_payload_is_terminal() {
        // RERR_FILEIO=11, see errcode.h:24-64.
        let event = classify_oob_frame(MuxTag::ErrorExit, &[0x0B, 0x00, 0x00, 0x00]);
        assert_eq!(event, NativeRsyncEvent::ErrorExit { code: Some(11) });
        assert!(event.is_terminal());
    }

    #[test]
    fn classify_error_exit_malformed_length_folds_to_none() {
        let event = classify_oob_frame(MuxTag::ErrorExit, &[0x01, 0x02, 0x03]);
        assert_eq!(event, NativeRsyncEvent::ErrorExit { code: None });
        assert!(!event.is_terminal());
    }

    // -------------------------------------------------------------------------
    // Deleted (filename + null trailer)
    // -------------------------------------------------------------------------

    #[test]
    fn classify_deleted_strips_trailing_null_for_dir_marker() {
        let event = classify_oob_frame(MuxTag::Deleted, b"some/dir\0");
        assert_eq!(
            event,
            NativeRsyncEvent::Deleted {
                path: "some/dir".to_string()
            }
        );
    }

    #[test]
    fn classify_deleted_keeps_filename_without_null() {
        let event = classify_oob_frame(MuxTag::Deleted, b"some/file.txt");
        assert_eq!(
            event,
            NativeRsyncEvent::Deleted {
                path: "some/file.txt".to_string()
            }
        );
    }

    // -------------------------------------------------------------------------
    // Noop / Unknown / Data misuse
    // -------------------------------------------------------------------------

    #[test]
    fn classify_noop_ignores_payload_and_yields_unit_variant() {
        let event = classify_oob_frame(MuxTag::Noop, &[1, 2, 3]);
        assert_eq!(event, NativeRsyncEvent::Noop);
    }

    #[test]
    fn classify_unknown_preserves_raw_tag_and_payload_byte_for_byte() {
        let event = classify_oob_frame(MuxTag::Unknown(77), &[0xAA, 0xBB, 0xCC]);
        assert_eq!(
            event,
            NativeRsyncEvent::Unknown {
                tag: 77,
                payload: vec![0xAA, 0xBB, 0xCC],
            }
        );
        assert!(!event.is_terminal());
    }

    #[test]
    fn classify_data_tag_misuse_folds_to_unknown_without_panic() {
        // Programmer error — Data is the app stream, never an event.
        // We surface it as Unknown(0) so a misuse trips a test rather
        // than a prod panic.
        let event = classify_oob_frame(MuxTag::Data, &[0x42]);
        assert_eq!(
            event,
            NativeRsyncEvent::Unknown {
                tag: 0,
                payload: vec![0x42],
            }
        );
    }

    // -------------------------------------------------------------------------
    // Terminal-set policy (HARDENING — pinned against io.c)
    // -------------------------------------------------------------------------

    #[test]
    fn terminal_set_matches_io_c_policy() {
        // The S8h trust-but-verify pass against rsync 3.2.7 io.c +
        // log.c yielded exactly four terminal mux tags. Any drift is
        // a regression — either io.c semantics changed (re-verify) or
        // someone moved a tag class without updating the doc.
        for tag in all_known_mux_tags() {
            let payload: &[u8] = match tag {
                // Empty payload triggers the Some(0)/None branches that
                // are deliberately non-terminal.
                MuxTag::ErrorExit => &[0x01, 0x00, 0x00, 0x00], // code=1 => terminal
                _ => b"x\n",
            };
            let event = classify_oob_frame(tag, payload);
            let expected_terminal = matches!(
                tag,
                MuxTag::Error | MuxTag::ErrorXfer | MuxTag::ErrorSocket | MuxTag::ErrorExit
            );
            assert_eq!(
                event.is_terminal(),
                expected_terminal,
                "tag={:?} unexpected terminality (got {}, expected {})",
                tag,
                event.is_terminal(),
                expected_terminal
            );
        }
    }

    #[test]
    fn severity_gradient_matches_terminal_for_terminal_events() {
        // Cross-check is_terminal() vs severity() Terminal — they MUST
        // agree (single source of truth). If a future refactor splits
        // these, this test catches drift.
        for tag in all_known_mux_tags() {
            let payload: &[u8] = match tag {
                MuxTag::ErrorExit => &[0x07, 0x00, 0x00, 0x00],
                _ => b"x",
            };
            let event = classify_oob_frame(tag, payload);
            assert_eq!(
                event.is_terminal(),
                event.severity() == EventSeverity::Terminal,
                "is_terminal/severity drift for {:?}",
                tag
            );
        }
    }

    #[test]
    fn tag_round_trip_for_every_known_event_variant() {
        // For every known MuxTag, the classified event reports the
        // same tag back via `event.tag()`. Pins the reverse mapping.
        for tag in all_known_mux_tags() {
            if matches!(tag, MuxTag::Data) {
                continue; // misuse path — surfaces as Unknown(0)
            }
            let event = classify_oob_frame(tag, &[]);
            assert_eq!(
                event.tag(),
                tag,
                "tag round-trip failed for {:?} (got {:?})",
                tag,
                event.tag()
            );
        }
    }

    #[test]
    fn message_helper_returns_text_for_textual_variants_only() {
        let cases: &[(MuxTag, &[u8], bool)] = &[
            (MuxTag::Info, b"hi", true),
            (MuxTag::Warning, b"x", true),
            (MuxTag::Error, b"e", true),
            (MuxTag::ErrorXfer, b"e", true),
            (MuxTag::ErrorSocket, b"e", true),
            (MuxTag::Log, b"l", true),
            (MuxTag::Client, b"c", true),
            (MuxTag::ErrorUtf8, b"u", true),
            (MuxTag::Deleted, b"f", true),
            (MuxTag::Redo, &[0u8; 4], false),
            (MuxTag::Noop, &[], false),
            (MuxTag::ErrorExit, &[], false),
            (MuxTag::Success, &[0u8; 4], false),
        ];
        for (tag, payload, expects_msg) in cases {
            let event = classify_oob_frame(*tag, payload);
            assert_eq!(
                event.message().is_some(),
                *expects_msg,
                "message() presence wrong for {:?}",
                tag
            );
        }
    }

    // -------------------------------------------------------------------------
    // Sinks
    // -------------------------------------------------------------------------

    #[test]
    fn collecting_sink_accumulates_all_events_in_order() {
        let mut sink = CollectingSink::default();
        sink.handle(classify_oob_frame(MuxTag::Info, b"a"));
        sink.handle(classify_oob_frame(MuxTag::Warning, b"b"));
        sink.handle(classify_oob_frame(MuxTag::Error, b"c"));
        assert_eq!(sink.events.len(), 3);
        assert!(matches!(&sink.events[0], NativeRsyncEvent::Info { .. }));
        assert!(matches!(&sink.events[1], NativeRsyncEvent::Warning { .. }));
        assert!(matches!(&sink.events[2], NativeRsyncEvent::Error { .. }));
    }

    #[test]
    fn bailing_sink_captures_first_terminal_only() {
        let mut sink = BailingSink::default();
        sink.handle(classify_oob_frame(MuxTag::Info, b"start"));
        sink.handle(classify_oob_frame(MuxTag::Warning, b"watch"));
        sink.handle(classify_oob_frame(MuxTag::Error, b"BOOM"));
        // Subsequent events go to `trailing`, not silently swallowed.
        sink.handle(classify_oob_frame(MuxTag::Error, b"second-not-captured"));
        sink.handle(classify_oob_frame(MuxTag::Info, b"trailing-info"));

        assert!(sink.bailed());
        assert_eq!(sink.before_terminal.len(), 2);
        assert!(matches!(
            sink.first_terminal().unwrap(),
            NativeRsyncEvent::Error { message } if message == "BOOM"
        ));
        // HARDENING: trailing events are PRESERVED, not dropped.
        // A driver post-mortem may want to see what the remote sent
        // after the failure (often the ErrorExit code).
        assert_eq!(sink.trailing.len(), 2);
    }

    #[test]
    fn bailing_sink_with_no_terminal_records_everything_in_before_terminal() {
        let mut sink = BailingSink::default();
        sink.handle(classify_oob_frame(MuxTag::Info, b"a"));
        sink.handle(classify_oob_frame(MuxTag::Warning, b"b"));
        assert!(!sink.bailed());
        assert_eq!(sink.before_terminal.len(), 2);
        assert!(sink.trailing.is_empty());
    }

    #[test]
    fn default_event_sink_dispatches_to_per_severity_hooks() {
        // Custom sink that tags each event by which hook it landed in.
        #[derive(Default)]
        struct TaggingSink {
            seen: Vec<&'static str>,
        }
        impl EventSink for TaggingSink {
            fn on_info(&mut self, _e: NativeRsyncEvent) {
                self.seen.push("info");
            }
            fn on_warning(&mut self, _e: NativeRsyncEvent) {
                self.seen.push("warning");
            }
            fn on_error(&mut self, _e: NativeRsyncEvent) {
                self.seen.push("error");
            }
            fn on_terminal(&mut self, _e: NativeRsyncEvent) {
                self.seen.push("terminal");
            }
        }

        let mut sink = TaggingSink::default();
        sink.handle(classify_oob_frame(MuxTag::Info, b"i"));
        sink.handle(classify_oob_frame(MuxTag::Warning, b"w"));
        sink.handle(classify_oob_frame(MuxTag::Error, b"e"));
        sink.handle(classify_oob_frame(MuxTag::ErrorExit, &[5, 0, 0, 0]));

        assert_eq!(sink.seen, vec!["info", "warning", "terminal", "terminal"]);
    }
}
