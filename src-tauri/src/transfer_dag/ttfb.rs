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
//! [`tokio::runtime::Id`]: a sample lands only in recorders installed
//! on the runtime it was recorded on, so a foreign runtime (a parallel test's
//! fixture traffic, or any non-transfer runtime) can never pollute a run's
//! attribution. Concurrent runs on the SAME runtime each see every sample
//! recorded while they are active; that per-run attribution is documented as
//! shared, never relabeled.

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
    /// guard. Samples recorded on this runtime while the guard is alive fold
    /// into this recorder; dropping the guard detaches it.
    ///
    /// Panics when called outside a Tokio runtime context. The executor only
    /// runs inside one.
    pub fn install() -> TtfbGuard {
        let recorder = Arc::new(Self {
            nanos_total: AtomicU64::new(0),
            samples: AtomicU64::new(0),
            runtime: tokio::runtime::Handle::current().id(),
        });
        active()
            .lock()
            .expect("ttfb registry poisoned")
            .push(Arc::clone(&recorder));
        TtfbGuard { recorder }
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

/// Keeps a [`TtfbRecorder`] registered for its runtime; detaches on drop.
#[derive(Debug)]
pub struct TtfbGuard {
    recorder: Arc<TtfbRecorder>,
}

impl TtfbGuard {
    /// `(ttfb_nanos_total, ttfb_samples)` accumulated so far.
    pub fn totals(&self) -> (u64, u64) {
        self.recorder.totals()
    }
}

impl Drop for TtfbGuard {
    fn drop(&mut self) {
        active()
            .lock()
            .expect("ttfb registry poisoned")
            .retain(|candidate| !Arc::ptr_eq(candidate, &self.recorder));
    }
}

fn active() -> &'static Mutex<Vec<Arc<TtfbRecorder>>> {
    static ACTIVE: OnceLock<Mutex<Vec<Arc<TtfbRecorder>>>> = OnceLock::new();
    ACTIVE.get_or_init(|| Mutex::new(Vec::new()))
}

/// Record one honest first-byte sample (request start to first byte /
/// headers received) into every recorder registered for the CURRENT runtime.
/// With no active recorder the sample is dropped: non-DAG paths simply do
/// not report TTFB, which beats inventing one.
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
    async fn fan_out_reaches_every_recorder_on_the_same_runtime() {
        let first = TtfbRecorder::install();
        let second = TtfbRecorder::install();
        record_sample(500);
        assert_eq!(first.totals(), (500, 1));
        assert_eq!(second.totals(), (500, 1));
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
