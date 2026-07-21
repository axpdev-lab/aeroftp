// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! DAG-P2-07 (block C): honest time-to-first-byte sampling.
//!
//! A TTFB sample is recorded only where a provider call boundary exposes a
//! real first-byte moment. For HTTP that is the resolution of the reqwest
//! `execute` future: response headers have arrived, so request-start to
//! headers-received is an honest first-byte measurement for single GET/PUT,
//! multipart part, and ranged segment calls that flow through
//! `providers::http_retry::send_with_retry`. Paths without such a moment
//! record NOTHING here; their latency is already covered by
//! `run_nanos_total`.
//!
//! Plumbing: a choke point cannot receive a per-run handle (provider trait
//! signatures are shared by 25+ providers), so samples are published to a
//! process-global registry of active recorders. Each DAG run installs one
//! [`TtfbRecorder`] guard at executor start and folds its totals into the
//! run metrics at the executor's single finalize, the same point the
//! byte/timing attestations land. Recorders are scoped by
//! [`tokio::runtime::Id`]: a sample lands only in the recorder installed
//! on the runtime it was recorded on, so a foreign runtime (a parallel test's
//! fixture traffic, or any non-transfer runtime) can never pollute a run's
//! attribution.
//!
//! ## Job-level attribution (one owning guard per runtime)
//!
//! Attribution is job-level, not per-subgraph: at most ONE recorder is
//! registered per runtime. [`TtfbRecorder::install`] on a runtime that already
//! has an active recorder returns a NESTED guard which owns nothing and folds
//! zero; only the outermost (owning) guard accumulates samples. The batch and
//! sync streaming frontiers install the owning guard around the whole job, so
//! every per-file `execute_dag` subgraph nests inside it and each real HTTP
//! sample is counted exactly once per job (per-file subgraph metrics honestly
//! report `ttfb_samples == 0`). A single-file or copy run is itself outermost,
//! so it owns the recorder and folds exactly as before. A second independent
//! top-level job started on the SAME runtime while one is active also nests
//! and reports zero TTFB: the samples are attributed to the first job.
//! Concurrent unrelated `send_with_retry` traffic on the same runtime during
//! a job is likewise attributed to that job; that shared-runtime attribution
//! is documented, never relabeled.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// Per-run accumulator of honest first-byte samples. Created only through
/// [`TtfbRecorder::install`], which binds it to the current runtime.
#[derive(Debug)]
pub struct TtfbRecorder {
    nanos_total: AtomicU64,
    samples: AtomicU64,
    runtime: tokio::runtime::Id,
}

impl TtfbRecorder {
    /// Register a fresh recorder for the current runtime and return its
    /// guard, unless the runtime already has an active recorder: then the
    /// returned guard is NESTED, registers nothing, and folds zero, so each
    /// sample is counted exactly once, by the outermost (owning) guard.
    /// Samples recorded on this runtime while the owning guard is alive fold
    /// into that recorder; dropping it detaches it.
    ///
    /// Panics when called outside a Tokio runtime context. The executor only
    /// runs inside one.
    pub fn install() -> TtfbGuard {
        let runtime = tokio::runtime::Handle::current().id();
        let mut registry = active().lock().expect("ttfb registry poisoned");
        if registry.iter().any(|recorder| recorder.runtime == runtime) {
            return TtfbGuard { recorder: None };
        }
        let recorder = Arc::new(Self {
            nanos_total: AtomicU64::new(0),
            samples: AtomicU64::new(0),
            runtime,
        });
        registry.push(Arc::clone(&recorder));
        TtfbGuard {
            recorder: Some(recorder),
        }
    }

    fn record(&self, nanos: u64) {
        self.nanos_total.fetch_add(nanos, Ordering::Relaxed);
        self.samples.fetch_add(1, Ordering::Relaxed);
    }

    /// `(ttfb_nanos_total, ttfb_samples)` accumulated so far.
    pub fn totals(&self) -> (u64, u64) {
        (
            self.nanos_total.load(Ordering::Relaxed),
            self.samples.load(Ordering::Relaxed),
        )
    }
}

/// Keeps the owning [`TtfbRecorder`] registered for its runtime; detaches on
/// drop. A nested guard (a recorder was already active on the runtime) owns
/// nothing: it folds zero and its drop is a no-op.
#[derive(Debug)]
pub struct TtfbGuard {
    /// `Some` for the outermost (owning) guard on its runtime, `None` for a
    /// nested one.
    recorder: Option<Arc<TtfbRecorder>>,
}

impl TtfbGuard {
    /// `(ttfb_nanos_total, ttfb_samples)` accumulated so far. Always `(0, 0)`
    /// for a nested guard: attribution lives at the outermost run.
    pub fn totals(&self) -> (u64, u64) {
        self.recorder
            .as_ref()
            .map_or((0, 0), |recorder| recorder.totals())
    }

    /// True when this guard owns the runtime's active recorder.
    pub fn is_owning(&self) -> bool {
        self.recorder.is_some()
    }
}

impl Drop for TtfbGuard {
    fn drop(&mut self) {
        let Some(recorder) = self.recorder.take() else {
            return;
        };
        // Graceful on a poisoned registry: a mutex poisoned during unwind
        // must not abort the process. Skipping the detach only leaves a
        // stale recorder whose runtime id can never match a fresh runtime.
        if let Ok(mut registry) = active().lock() {
            registry.retain(|candidate| !Arc::ptr_eq(candidate, &recorder));
        }
    }
}

fn active() -> &'static Mutex<Vec<Arc<TtfbRecorder>>> {
    static ACTIVE: OnceLock<Mutex<Vec<Arc<TtfbRecorder>>>> = OnceLock::new();
    ACTIVE.get_or_init(|| Mutex::new(Vec::new()))
}

/// Record one honest first-byte sample (request start to first byte /
/// headers received) into the recorder registered for the CURRENT runtime.
/// At most one recorder exists per runtime (nested guards register nothing),
/// so a sample is counted exactly once, by the outermost run. With no active
/// recorder the sample is dropped: non-DAG paths simply do not report TTFB,
/// which beats inventing one.
pub fn record_sample(nanos: u64) {
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return;
    };
    let registry = active().lock().expect("ttfb registry poisoned");
    for recorder in registry.iter() {
        if recorder.runtime == handle.id() {
            recorder.record(nanos);
        }
    }
}

#[cfg(test)]
pub(crate) mod test_fixture {
    //! Minimal delayed-first-byte HTTP/1.1 fixture shared by the TTFB tests.
    //! The server reads the request head, sleeps `delay` (so the measured
    //! TTFB has a known floor), then answers with the caller's response and
    //! `Connection: close`, so a retry opens a fresh connection.

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    pub(crate) struct FixtureResponse {
        pub(crate) status: String,
        pub(crate) headers: Vec<(String, String)>,
        pub(crate) body: Vec<u8>,
    }

    impl FixtureResponse {
        pub(crate) fn ok(body: &str) -> Self {
            Self {
                status: "200 OK".to_string(),
                headers: Vec::new(),
                body: body.as_bytes().to_vec(),
            }
        }
    }

    /// Serve `connections` sequential connections, answering each with
    /// `respond(request_head)` after `delay`. Returns the bound address.
    pub(crate) async fn spawn_delayed_http_fixture<F>(
        delay: std::time::Duration,
        connections: usize,
        respond: F,
    ) -> std::net::SocketAddr
    where
        F: Fn(&str) -> FixtureResponse + Send + Sync + 'static,
    {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fixture bind");
        let addr = listener.local_addr().expect("fixture addr");
        let respond = std::sync::Arc::new(respond);
        tokio::spawn(async move {
            for _ in 0..connections {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let respond = std::sync::Arc::clone(&respond);
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 8192];
                    let Ok(read) = socket.read(&mut buf).await else {
                        return;
                    };
                    let head = String::from_utf8_lossy(&buf[..read]).into_owned();
                    tokio::time::sleep(delay).await;
                    let response = respond(&head);
                    let mut head_out = format!(
                        "HTTP/1.1 {}\r\nContent-Length: {}\r\nConnection: close\r\n",
                        response.status,
                        response.body.len()
                    );
                    for (name, value) in &response.headers {
                        head_out.push_str(&format!("{}: {}\r\n", name, value));
                    }
                    head_out.push_str("\r\n");
                    let mut wire = head_out.into_bytes();
                    wire.extend_from_slice(&response.body);
                    let _ = socket.write_all(&wire).await;
                });
            }
        });
        addr
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn recorder_counts_each_sample_once() {
        let guard = TtfbRecorder::install();
        record_sample(1_000);
        record_sample(2_000);
        assert_eq!(guard.totals(), (3_000, 2));
    }

    #[tokio::test]
    async fn nested_install_folds_zero_and_the_owner_sees_every_sample() {
        let outer = TtfbRecorder::install();
        let inner = TtfbRecorder::install();
        assert!(outer.is_owning(), "the first install owns the recorder");
        assert!(
            !inner.is_owning(),
            "a concurrent install on one runtime nests"
        );
        record_sample(1_000);
        record_sample(2_000);
        assert_eq!(
            inner.totals(),
            (0, 0),
            "a nested guard folds zero TTFB into its per-run metrics"
        );
        assert_eq!(
            outer.totals(),
            (3_000, 2),
            "the outermost guard counts each sample exactly once"
        );
    }

    #[tokio::test]
    async fn a_second_guard_on_the_same_runtime_nests_instead_of_fanning_out() {
        let first = TtfbRecorder::install();
        let second = TtfbRecorder::install();
        record_sample(500);
        assert_eq!(first.totals(), (500, 1));
        assert_eq!(
            second.totals(),
            (0, 0),
            "no fan-out: the nested guard reports zero"
        );
        drop(second);
        drop(first);
        // With the owner detached, a fresh install owns again.
        let third = TtfbRecorder::install();
        assert!(third.is_owning());
        record_sample(500);
        assert_eq!(third.totals(), (500, 1));
    }

    #[tokio::test]
    async fn guard_drop_detaches_the_recorder() {
        let guard = TtfbRecorder::install();
        record_sample(100);
        assert_eq!(guard.totals(), (100, 1));
        let totals_before = guard.totals();
        drop(guard);
        // The recorder is detached: later samples cannot reach it. Proven
        // indirectly through a fresh recorder seeing only its own window.
        let fresh = TtfbRecorder::install();
        record_sample(100);
        assert_eq!(fresh.totals(), (100, 1));
        assert_eq!(totals_before, (100, 1));
    }

    #[tokio::test]
    async fn samples_from_other_runtimes_never_cross_over() {
        let guard = TtfbRecorder::install();
        record_sample(100);
        assert_eq!(guard.totals(), (100, 1));

        // The foreign runtime drives block_on from its own thread: tokio
        // refuses block_on from inside this test's runtime context.
        std::thread::spawn(|| {
            let other = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("second runtime");
            other.block_on(async {
                record_sample(10_000);
            });
        })
        .join()
        .expect("foreign runtime thread panicked");

        assert_eq!(
            guard.totals(),
            (100, 1),
            "a sample recorded on a foreign runtime must not cross over"
        );
    }

    #[test]
    fn no_active_recorder_drops_the_sample() {
        // No runtime context at all: the call must be a silent no-op.
        record_sample(42);
    }
}
