//! Shared, byte-true progress for local archive compress/extract and Cryptomator
//! decrypt operations.
//!
//! One mechanism feeds the flagship `TransferProgressBar` for every local archive
//! operation: a throttled emitter (`ArchiveProgress`) plus a `Read` adapter
//! (`ProgressReader`) that counts bytes as a stream is consumed.
//!
//! Honesty rules (see `docs/dev/handoffs/HANDOFF-2026-06-22_progress-bar-compressed-formats.md`
//! section 3.6): every emitted byte reflects real work; below
//! `PROGRESS_THRESHOLD_BYTES` nothing is emitted (the op is effectively instant and a
//! bar would only flicker); the terminal 100% frame is forced past the throttle so it
//! is never dropped. RAR extract is opaque (the `unrar` crate extracts in a single
//! blocking call), so it uses an honest *indeterminate* state rather than a faked
//! percentage.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use std::io::{self, Read};
use std::time::Instant;

/// Operations whose total payload is below this are treated as instant: no event is
/// emitted and the GUI shows no bar. 10 MiB, locked with the owner (HANDOFF section 3.2).
pub const PROGRESS_THRESHOLD_BYTES: u64 = 10 * 1024 * 1024;

/// The single Tauri event every local archive / plaintext-vault / cryptomator op
/// emits on. The three modal surfaces (Compress, Archive browse, Cryptomator) are
/// mutually exclusive, so one event name with a `phase` discriminator is enough.
pub const ARCHIVE_PROGRESS_EVENT: &str = "archive_progress";

/// Phase label keys, mapped to i18n `progress.*` on the frontend.
pub mod phase {
    pub const SCANNING: &str = "scanning";
    pub const COMPRESSING: &str = "compressing";
    pub const EXTRACTING: &str = "extracting";
    pub const DECRYPTING: &str = "decrypting";
}

/// A single progress frame handed to the emit sink.
#[derive(Debug, Clone, Copy)]
pub struct ProgressFrame {
    pub phase: &'static str,
    pub percentage: i64,
    pub transferred: u64,
    pub total: u64,
    pub indeterminate: bool,
}

/// Sink that delivers a frame to its destination (a Tauri event in the GUI, a Vec in
/// tests, an `indicatif` bar in a future CLI use). Boxed `FnMut + Send` so it can be
/// held across the `async` command body (Tauri command futures must be `Send`).
pub type ProgressEmit = Box<dyn FnMut(&ProgressFrame) + Send>;

/// Build an emit sink that forwards frames to the GUI as `archive_progress` events.
pub fn tauri_emitter(app: tauri::AppHandle) -> ProgressEmit {
    use tauri::Emitter;
    Box::new(move |f: &ProgressFrame| {
        let _ = app.emit(
            ARCHIVE_PROGRESS_EVENT,
            serde_json::json!({
                "phase": f.phase,
                "percentage": f.percentage,
                "transferred": f.transferred,
                "total": f.total,
                "indeterminate": f.indeterminate,
            }),
        );
    })
}

/// Throttled byte-progress emitter for a single archive operation.
///
/// `total` is the operation denominator in bytes: the sum of input file sizes for a
/// compress, the entry's uncompressed size for an extract, or the plaintext size for
/// a decrypt. Emission is gated on `total >= PROGRESS_THRESHOLD_BYTES`; throttled to
/// at most one event per ~150 ms or per 2% step; `finish()` forces a terminal 100%
/// frame so the last frame is never dropped by the throttle.
pub struct ArchiveProgress {
    emit: ProgressEmit,
    phase: &'static str,
    total: u64,
    transferred: u64,
    last_emit: Instant,
    last_pct: i64,
    enabled: bool,
    indeterminate: bool,
    finished: bool,
}

impl ArchiveProgress {
    /// Determinate progress. Emits an immediate 0% frame when above threshold so the
    /// bar appears at the very start of a long operation.
    pub fn new(phase: &'static str, total: u64, emit: ProgressEmit) -> Self {
        Self::build(phase, total, emit, false)
    }

    /// Indeterminate progress for opaque operations (RAR extract): the bar animates
    /// client-side without a faked percentage. `add()` is a no-op; only the start and
    /// `finish()` frames are emitted.
    pub fn new_indeterminate(phase: &'static str, total: u64, emit: ProgressEmit) -> Self {
        Self::build(phase, total, emit, true)
    }

    /// Convenience: a determinate emitter wired to the GUI event sink.
    pub fn for_app(app: tauri::AppHandle, phase: &'static str, total: u64) -> Self {
        Self::new(phase, total, tauri_emitter(app))
    }

    /// A determinate emitter that emits to the GUI when an `AppHandle` is present and
    /// is otherwise a silent no-op. Lets one implementation serve both the GUI command
    /// path (`Some(app)`) and the headless CLI / AI-tool `_core` path (`None`).
    pub fn for_optional_app(
        app: Option<tauri::AppHandle>,
        phase: &'static str,
        total: u64,
    ) -> Self {
        match app {
            Some(a) => Self::for_app(a, phase, total),
            None => Self::new(phase, total, Box::new(|_| {})),
        }
    }

    /// Convenience: an indeterminate emitter wired to the GUI event sink.
    pub fn indeterminate_for_app(app: tauri::AppHandle, phase: &'static str, total: u64) -> Self {
        Self::new_indeterminate(phase, total, tauri_emitter(app))
    }

    fn build(phase: &'static str, total: u64, emit: ProgressEmit, indeterminate: bool) -> Self {
        let enabled = total >= PROGRESS_THRESHOLD_BYTES;
        let mut p = Self {
            emit,
            phase,
            total,
            transferred: 0,
            last_emit: Instant::now(),
            last_pct: -1,
            enabled,
            indeterminate,
            finished: false,
        };
        if enabled {
            p.emit_frame(0);
        }
        p
    }

    /// Whether this operation is large enough to surface a bar. Callers can skip the
    /// extra wrapping work entirely when this is false.
    pub fn is_active(&self) -> bool {
        self.enabled
    }

    /// Record `n` more bytes of real work and emit if the throttle allows. No-op for
    /// indeterminate or sub-threshold operations.
    pub fn add(&mut self, n: u64) {
        if !self.enabled || self.indeterminate {
            return;
        }
        self.transferred = self.transferred.saturating_add(n).min(self.total);
        let pct = self.pct();
        let should =
            self.last_emit.elapsed().as_millis() >= 150 || pct.saturating_sub(self.last_pct) >= 2;
        if should {
            self.emit_frame(pct);
            self.last_emit = Instant::now();
            self.last_pct = pct;
        }
    }

    /// Force the terminal frame: 100% at `total` bytes, bypassing the throttle. Safe to
    /// call more than once (subsequent calls are ignored).
    pub fn finish(&mut self) {
        if !self.enabled || self.finished {
            return;
        }
        self.finished = true;
        self.transferred = self.total;
        self.emit_frame(100);
    }

    fn pct(&self) -> i64 {
        if self.total == 0 {
            return 100;
        }
        // saturating_mul guards >40 GiB totals; u64 throughout, no f64 rounding drift.
        (self.transferred.saturating_mul(100) / self.total) as i64
    }

    fn emit_frame(&mut self, percentage: i64) {
        let frame = ProgressFrame {
            phase: self.phase,
            percentage,
            transferred: self.transferred,
            total: self.total,
            indeterminate: self.indeterminate,
        };
        (self.emit)(&frame);
    }
}

/// `Read` adapter that counts bytes pulled through it into an `ArchiveProgress`.
///
/// Wrap the *source* of a compress (a `File`) or the entry reader of an extract; the
/// byte count is therefore the true uncompressed payload moving through the stream.
pub struct ProgressReader<'a, R: Read> {
    inner: R,
    progress: &'a mut ArchiveProgress,
}

impl<'a, R: Read> ProgressReader<'a, R> {
    pub fn new(inner: R, progress: &'a mut ArchiveProgress) -> Self {
        Self { inner, progress }
    }
}

impl<R: Read> Read for ProgressReader<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.progress.add(n as u64);
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// Collect every emitted frame into a shared Vec for assertions.
    fn recorder() -> (ProgressEmit, Arc<Mutex<Vec<ProgressFrame>>>) {
        let frames = Arc::new(Mutex::new(Vec::new()));
        let sink = frames.clone();
        let emit: ProgressEmit = Box::new(move |f: &ProgressFrame| {
            sink.lock().unwrap().push(*f);
        });
        (emit, frames)
    }

    #[test]
    fn sub_threshold_emits_nothing() {
        let (emit, frames) = recorder();
        let mut p = ArchiveProgress::new(phase::COMPRESSING, 1024, emit);
        p.add(512);
        p.add(512);
        p.finish();
        assert!(
            frames.lock().unwrap().is_empty(),
            "tiny ops must stay silent"
        );
    }

    #[test]
    fn above_threshold_emits_initial_zero_and_final_hundred() {
        let total = 50 * 1024 * 1024;
        let (emit, frames) = recorder();
        let mut p = ArchiveProgress::new(phase::EXTRACTING, total, emit);
        // First add is below the 2% / 150ms throttle, so only the initial 0% shows yet.
        p.add(1);
        p.finish();
        let f = frames.lock().unwrap();
        assert_eq!(f.first().unwrap().percentage, 0, "must open at 0%");
        let last = f.last().unwrap();
        assert_eq!(last.percentage, 100, "must close at exactly 100%");
        assert_eq!(
            last.transferred, total,
            "final transferred must equal total"
        );
    }

    #[test]
    fn never_overshoots_total() {
        let total = 20 * 1024 * 1024;
        let (emit, frames) = recorder();
        let mut p = ArchiveProgress::new(phase::COMPRESSING, total, emit);
        // Feed more than `total` (e.g. a file that grew between scan and read).
        p.add(total);
        p.add(total);
        p.finish();
        for fr in frames.lock().unwrap().iter() {
            assert!(
                fr.transferred <= total,
                "transferred must never exceed total"
            );
            assert!(fr.percentage <= 100, "percentage must never exceed 100");
        }
    }

    #[test]
    fn finish_is_idempotent() {
        let total = 30 * 1024 * 1024;
        let (emit, frames) = recorder();
        let mut p = ArchiveProgress::new(phase::DECRYPTING, total, emit);
        p.finish();
        p.finish();
        let hundreds = frames
            .lock()
            .unwrap()
            .iter()
            .filter(|f| f.percentage == 100)
            .count();
        assert_eq!(hundreds, 1, "the 100% frame must be emitted exactly once");
    }

    #[test]
    fn progress_reader_counts_exact_bytes() {
        let total = 12 * 1024 * 1024;
        let (emit, frames) = recorder();
        let mut p = ArchiveProgress::new(phase::COMPRESSING, total, emit);
        let data = vec![0u8; total as usize];
        let mut sink = Vec::new();
        {
            let mut reader = ProgressReader::new(&data[..], &mut p);
            io::copy(&mut reader, &mut sink).unwrap();
        }
        p.finish();
        let f = frames.lock().unwrap();
        let last = f.last().unwrap();
        assert_eq!(last.transferred, total);
        assert_eq!(last.percentage, 100);
        assert_eq!(sink.len(), total as usize, "copy must move every byte");
    }

    #[test]
    fn indeterminate_add_is_noop_but_brackets_emit() {
        let total = 40 * 1024 * 1024;
        let (emit, frames) = recorder();
        let mut p = ArchiveProgress::new_indeterminate(phase::EXTRACTING, total, emit);
        p.add(1024);
        p.add(1024);
        p.finish();
        let f = frames.lock().unwrap();
        assert!(
            f.iter().all(|fr| fr.indeterminate),
            "frames stay indeterminate"
        );
        assert_eq!(f.first().unwrap().percentage, 0);
        assert_eq!(f.last().unwrap().percentage, 100);
        // add() must not have produced extra frames for an opaque op.
        assert_eq!(f.len(), 2, "only the start and finish frames");
    }
}
