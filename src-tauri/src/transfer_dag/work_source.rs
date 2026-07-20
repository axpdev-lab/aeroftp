// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! DAG-P2-04: streaming `WorkSource` frontier for multi-file transfers.
//!
//! ## The problem this solves
//!
//! Before this slice every multi-file job built one static
//! [`TransferDag`](super::graph::TransferDag) for the *entire* entry list up
//! front (`from_batch_shaped` / `from_sync_plan_shaped`): ~7 nodes per file,
//! multiplied further by multipart pre-expansion. For millions of files that is
//! O(total_files) structure resident in RAM before the first byte moves. The
//! bounded [`DEFAULT_DISPATCH_WINDOW`](super::executor::DEFAULT_DISPATCH_WINDOW)
//! caps resident *tasks*, not the materialized *graph*.
//!
//! ## The streaming model (owner decision 2026-07-20, Option A)
//!
//! The semantic DAG stays: acquire -> transfer/parts -> verify -> preserve ->
//! commit -> emit remains real for every *active* file. What changes is when a
//! file's subgraph exists. A [`WorkSource`] emits [`TransferWorkItem`] values as
//! listing / compare makes them available; a bounded backlog holds pending
//! items (and pauses the source when full — backpressure); and only files
//! admitted from the backlog into a bounded *active set* materialize their node
//! subgraph. A completed file's subgraph is dropped immediately, so peak graph
//! size is `O(active_file_cap * nodes_per_template)`, independent of the total
//! job size.
//!
//! ## What this module owns and does not own
//!
//! This is the streaming *frontier* only: the source, the bounded backlog, the
//! bounded active set, and the peak-graph meter. It does **not** replace the
//! executor: each admitted file is still executed by
//! [`execute_dag`](super::executor::execute_dag) on its own template subgraph,
//! so resource permits, AIMD dispatch gates, governor leases, typed errors,
//! fail-fast, and observers all remain on real nodes exactly as before. The
//! per-file execution is a caller-supplied closure so the same frontier serves
//! both the batch and sync production paths (and a synthetic scale proof)
//! without a second scheduler.

use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use futures_util::StreamExt;
use tokio::sync::mpsc;

use super::builder::TransferDirection;

/// Default bounded backlog of pending work items. Aligned with the CLI
/// `--max-backlog` default so the documented knob and the engine agree.
pub const DEFAULT_ENGINE_MAX_BACKLOG: usize = 10_000;

/// Ceiling for file subgraphs materialized and executing at once (the
/// active-set residency cap). Production callers set `active_file_cap` to
/// `file_slots + small pipeline headroom`, then clamp by this ceiling (or by
/// `file_slots` when higher). That keeps peak nodes near
/// `O(file_slots * nodes_per_template)` instead of forcing a wide idle window
/// of multipart graphs. Same spirit as
/// [`DEFAULT_DISPATCH_WINDOW`](super::executor::DEFAULT_DISPATCH_WINDOW).
pub const DEFAULT_ACTIVE_FILE_WINDOW: usize = 64;

/// One unit of streaming transfer work.
///
/// Carries the minimum the frontier and a per-file runner need: a stable `key`
/// (diagnostics / ordering), the runner-binding `index` into the caller's
/// entry or plan vector (the whole file subgraph binds to this one item), the
/// object `size` (drives multipart shaping), and the `direction`. Sync
/// upload/download actions map onto [`TransferDirection`]; sync skip/delete
/// entries never become work items.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferWorkItem {
    pub key: String,
    pub index: usize,
    pub size: u64,
    pub direction: TransferDirection,
}

impl TransferWorkItem {
    pub fn new(
        key: impl Into<String>,
        index: usize,
        size: u64,
        direction: TransferDirection,
    ) -> Self {
        Self {
            key: key.into(),
            index,
            size,
            direction,
        }
    }
}

/// A source of transfer work.
///
/// May be **finite** (a fully known batch entry list) or **live** (a sync plan
/// streamed as compare resolves it). The contract is that the first item can be
/// emitted without the whole job graph existing, so transfer can begin before
/// listing finishes.
#[async_trait]
pub trait WorkSource: Send {
    /// The next work item, or `None` once the source is exhausted. May await
    /// (a live source blocks until the next item is produced).
    async fn next_item(&mut self) -> Option<TransferWorkItem>;

    /// Total item count when the inventory is fully known up front; `None`
    /// while a live source is still producing. Callers use this for honest
    /// "known so far" progress without materializing the graph.
    fn known_total(&self) -> Option<usize> {
        None
    }
}

/// A finite [`WorkSource`] backed by a fully known item list.
///
/// Used by the batch path (the entry list is known) and the sync path (the
/// transfer plan is precomputed from the pre-transfer scan — a plan iterator,
/// not a full executable node graph).
pub struct SliceWorkSource {
    items: std::vec::IntoIter<TransferWorkItem>,
    total: usize,
}

impl SliceWorkSource {
    pub fn new(items: Vec<TransferWorkItem>) -> Self {
        let total = items.len();
        Self {
            items: items.into_iter(),
            total,
        }
    }
}

#[async_trait]
impl WorkSource for SliceWorkSource {
    async fn next_item(&mut self) -> Option<TransferWorkItem> {
        self.items.next()
    }

    fn known_total(&self) -> Option<usize> {
        Some(self.total)
    }
}

/// A live [`WorkSource`] backed by a channel a producer fills as work becomes
/// available. Emits items in receive order; the total is unknown until the
/// sender is dropped, so [`WorkSource::known_total`] stays `None`.
pub struct ChannelWorkSource {
    rx: mpsc::Receiver<TransferWorkItem>,
}

impl ChannelWorkSource {
    pub fn new(rx: mpsc::Receiver<TransferWorkItem>) -> Self {
        Self { rx }
    }
}

#[async_trait]
impl WorkSource for ChannelWorkSource {
    async fn next_item(&mut self) -> Option<TransferWorkItem> {
        self.rx.recv().await
    }
}

/// Streaming frontier configuration: the two bounds that keep the resident
/// graph `O(active_file_cap * nodes_per_template)` regardless of job size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamingConfig {
    /// Bounded backlog of work items pulled from the source but not yet
    /// admitted. When full the source pauses (backpressure) instead of growing
    /// without limit. Maps from the CLI `--max-backlog` knob.
    pub backlog_cap: usize,
    /// Maximum file subgraphs materialized and executing concurrently.
    pub active_file_cap: usize,
}

impl Default for StreamingConfig {
    fn default() -> Self {
        Self {
            backlog_cap: DEFAULT_ENGINE_MAX_BACKLOG,
            active_file_cap: DEFAULT_ACTIVE_FILE_WINDOW,
        }
    }
}

impl StreamingConfig {
    /// Clamp both caps to at least 1, and the backlog to at least the active
    /// set so the active window can always be filled from the backlog.
    pub fn normalized(self) -> Self {
        let active_file_cap = self.active_file_cap.max(1);
        let backlog_cap = self.backlog_cap.max(1).max(active_file_cap);
        Self {
            backlog_cap,
            active_file_cap,
        }
    }

    pub fn with_backlog(mut self, backlog_cap: usize) -> Self {
        self.backlog_cap = backlog_cap;
        self
    }

    pub fn with_active_files(mut self, active_file_cap: usize) -> Self {
        self.active_file_cap = active_file_cap;
        self
    }

    /// Production multi-file policy shared by batch and sync.
    ///
    /// Active set = `file_slots + 2` pipeline headroom (structural prefix can
    /// start while a transfer holds a slot), never below `file_slots`, never
    /// above `DEFAULT_ACTIVE_FILE_WINDOW` unless slots themselves exceed it.
    /// Backlog comes from the CLI/engine knob (`--max-backlog` / config).
    pub fn for_file_slots(file_slots: usize, backlog_cap: usize) -> Self {
        let file_slots = file_slots.max(1);
        let pipeline = file_slots.saturating_add(2);
        let ceiling = DEFAULT_ACTIVE_FILE_WINDOW.max(file_slots);
        let active_file_cap = pipeline.min(ceiling).max(file_slots);
        Self {
            backlog_cap,
            active_file_cap,
        }
        .normalized()
    }
}

/// Live counters for the streaming frontier's materialized graph, so the
/// bounded peak is observable and testable. Each admitted file subgraph
/// brackets its node count with [`ActiveGraphMeter::add_nodes`] /
/// [`ActiveGraphMeter::sub_nodes`]; peak node and file counts never exceed
/// `active_file_cap * nodes_per_template` and `active_file_cap` respectively.
#[derive(Debug, Default)]
pub struct ActiveGraphMeter {
    current_nodes: AtomicUsize,
    peak_nodes: AtomicUsize,
    current_files: AtomicUsize,
    peak_files: AtomicUsize,
    admitted: AtomicUsize,
}

impl ActiveGraphMeter {
    fn enter_file(&self) {
        let now = self.current_files.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak_files.fetch_max(now, Ordering::SeqCst);
        self.admitted.fetch_add(1, Ordering::SeqCst);
    }

    fn exit_file(&self) {
        self.current_files.fetch_sub(1, Ordering::SeqCst);
    }

    fn add_nodes(&self, n: usize) {
        let now = self.current_nodes.fetch_add(n, Ordering::SeqCst) + n;
        self.peak_nodes.fetch_max(now, Ordering::SeqCst);
    }

    fn sub_nodes(&self, n: usize) {
        self.current_nodes.fetch_sub(n, Ordering::SeqCst);
    }

    pub fn peak_nodes(&self) -> usize {
        self.peak_nodes.load(Ordering::SeqCst)
    }

    pub fn peak_files(&self) -> usize {
        self.peak_files.load(Ordering::SeqCst)
    }

    pub fn admitted(&self) -> usize {
        self.admitted.load(Ordering::SeqCst)
    }
}

/// Handle given to the per-file executor when a work item is admitted from the
/// backlog into the active set.
///
/// It represents the file's active-set slot and, on drop, retires the file's
/// materialized nodes and its slot. The per-file executor calls
/// [`FileAdmission::materialize_nodes`] once, right after it builds the file
/// subgraph, so the meter's peak reflects the real resident node count.
pub struct FileAdmission {
    meter: Arc<ActiveGraphMeter>,
    nodes: usize,
}

impl FileAdmission {
    fn new(meter: Arc<ActiveGraphMeter>) -> Self {
        meter.enter_file();
        Self { meter, nodes: 0 }
    }

    /// Record that this admitted file materialized `node_count` graph nodes.
    /// Idempotent-safe to call once per file; additional calls accumulate.
    pub fn materialize_nodes(&mut self, node_count: usize) {
        self.meter.add_nodes(node_count);
        self.nodes += node_count;
    }
}

impl Drop for FileAdmission {
    fn drop(&mut self) {
        if self.nodes > 0 {
            self.meter.sub_nodes(self.nodes);
        }
        self.meter.exit_file();
    }
}

/// Outcome of a streaming multi-file run: how many items were admitted and the
/// proven peak resident graph size. The per-file transfer results are recorded
/// through the caller's own shared state (progress snapshot, sync report), not
/// here; this summary is purely the frontier's bookkeeping. `metrics` is the
/// job-level DAG total: each admitted file's per-subgraph
/// [`TransferDagMetrics`](super::metrics::TransferDagMetrics) (executor timing
/// plus runner-attested bytes/retries) folded in via
/// [`TransferDagMetrics::absorb`](super::metrics::TransferDagMetrics::absorb),
/// so the totals accumulate across the whole job, not just the last subgraph.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StreamingSummary {
    pub items_admitted: usize,
    pub peak_active_files: usize,
    pub peak_active_nodes: usize,
    pub backlog_cap: usize,
    pub active_file_cap: usize,
    pub metrics: super::metrics::TransferDagMetrics,
}

/// Drive a [`WorkSource`] through a bounded backlog and a bounded active set,
/// invoking `per_file` for each admitted work item.
///
/// - **Backlog / backpressure**: the source is pumped into a channel of
///   capacity `backlog_cap` by a dedicated producer task; when the channel is
///   full the producer awaits on `send`, so the source pauses rather than
///   growing without bound.
/// - **Active set**: at most `active_file_cap` `per_file` futures run
///   concurrently (via `for_each_concurrent`, so there is no task spawn per
///   file — only the work the per-file closure itself spawns). Each file gets a
///   [`FileAdmission`] it uses to record its materialized node count; the file
///   subgraph is retired when that future completes and drops the admission.
/// - **Metrics**: each `per_file` future returns its file's
///   [`TransferDagMetrics`](super::metrics::TransferDagMetrics) (zero when the
///   file never reached a graph); the frontier folds them into the job-level
///   [`StreamingSummary::metrics`] total.
///
/// The whole job graph is never materialized: peak resident nodes stay
/// `<= active_file_cap * max_nodes_per_template`.
pub async fn run_streaming<S, F, Fut>(
    mut source: S,
    config: StreamingConfig,
    per_file: F,
) -> StreamingSummary
where
    S: WorkSource + 'static,
    F: Fn(TransferWorkItem, FileAdmission) -> Fut + Send + Sync,
    Fut: Future<Output = super::metrics::TransferDagMetrics> + Send,
{
    let config = config.normalized();
    let meter = Arc::new(ActiveGraphMeter::default());
    // Concurrent per-file futures fold their metrics here; the updates are
    // short and synchronous, so a plain mutex suffices.
    let metrics_total = Arc::new(std::sync::Mutex::new(
        super::metrics::TransferDagMetrics::default(),
    ));

    // Bounded backlog: a producer task pulls from the source and sends into a
    // channel of capacity `backlog_cap`. `send` awaits when the channel is
    // full, so the source is paused — the backpressure contract.
    let (tx, rx) = mpsc::channel::<TransferWorkItem>(config.backlog_cap);
    let producer = tokio::spawn(async move {
        while let Some(item) = source.next_item().await {
            if tx.send(item).await.is_err() {
                // The consumer side is gone; stop producing.
                break;
            }
        }
    });

    // Admission: consume the backlog, running up to `active_file_cap` per-file
    // futures at once. `unfold` turns the receiver into a stream without
    // pulling more than the concurrency limit needs.
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    });
    {
        let meter = &meter;
        let per_file = &per_file;
        let metrics_total = &metrics_total;
        stream
            .for_each_concurrent(config.active_file_cap, move |item| {
                let admission = FileAdmission::new(Arc::clone(meter));
                let metrics_total = Arc::clone(metrics_total);
                async move {
                    let file_metrics = per_file(item, admission).await;
                    metrics_total
                        .lock()
                        .expect("streaming metrics total poisoned")
                        .absorb(&file_metrics);
                }
            })
            .await;
    }

    // The stream drained: the channel closed (producer finished) or every item
    // was consumed. Join the producer so its task never outlives the run.
    let _ = producer.await;

    let metrics = std::mem::take(
        &mut *metrics_total
            .lock()
            .expect("streaming metrics total poisoned"),
    );
    StreamingSummary {
        items_admitted: meter.admitted(),
        peak_active_files: meter.peak_files(),
        peak_active_nodes: meter.peak_nodes(),
        backlog_cap: config.backlog_cap,
        active_file_cap: config.active_file_cap,
        metrics,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;
    use tokio::sync::Semaphore;

    /// A synthetic finite source that yields `n` empty-keyed items without
    /// allocating an N-length vector, so million/ten-million-scale proofs stay
    /// cheap. Counts how many items it has actually produced.
    struct CountingWorkSource {
        remaining: usize,
        produced: Arc<AtomicUsize>,
    }

    impl CountingWorkSource {
        fn new(n: usize, produced: Arc<AtomicUsize>) -> Self {
            Self {
                remaining: n,
                produced,
            }
        }
    }

    #[async_trait]
    impl WorkSource for CountingWorkSource {
        async fn next_item(&mut self) -> Option<TransferWorkItem> {
            if self.remaining == 0 {
                return None;
            }
            self.remaining -= 1;
            let index = self.produced.fetch_add(1, Ordering::SeqCst);
            // Empty key => no allocation; this is a structural scale proof.
            Some(TransferWorkItem::new(
                String::new(),
                index,
                0,
                TransferDirection::Upload,
            ))
        }
    }

    #[test]
    fn streaming_config_normalizes_bounds() {
        let cfg = StreamingConfig {
            backlog_cap: 0,
            active_file_cap: 0,
        }
        .normalized();
        assert_eq!(cfg.active_file_cap, 1);
        assert_eq!(cfg.backlog_cap, 1);

        // Backlog is raised to at least the active set.
        let cfg = StreamingConfig {
            backlog_cap: 4,
            active_file_cap: 32,
        }
        .normalized();
        assert_eq!(cfg.active_file_cap, 32);
        assert_eq!(cfg.backlog_cap, 32);
    }

    #[test]
    fn streaming_config_for_file_slots_tracks_budget_not_a_fixed_floor() {
        let serial = StreamingConfig::for_file_slots(1, 1_000);
        assert_eq!(serial.active_file_cap, 3);
        assert_eq!(serial.backlog_cap, 1_000);

        let mid = StreamingConfig::for_file_slots(4, 5_000);
        assert_eq!(mid.active_file_cap, 6);
        assert_eq!(mid.backlog_cap, 5_000);

        let wide = StreamingConfig::for_file_slots(32, 10_000);
        assert_eq!(
            wide.active_file_cap,
            34.min(DEFAULT_ACTIVE_FILE_WINDOW),
            "32 slots + 2 headroom, capped by default window"
        );
        // Backlog never below active after normalize.
        assert!(wide.backlog_cap >= wide.active_file_cap);
    }

    #[test]
    fn active_graph_meter_tracks_peaks() {
        let meter = ActiveGraphMeter::default();
        meter.enter_file();
        meter.add_nodes(7);
        meter.enter_file();
        meter.add_nodes(7);
        assert_eq!(meter.peak_nodes(), 14);
        assert_eq!(meter.peak_files(), 2);
        meter.sub_nodes(7);
        meter.exit_file();
        // Peak is a high-water mark; it does not decrease.
        assert_eq!(meter.peak_nodes(), 14);
        assert_eq!(meter.admitted(), 2);
    }

    /// Peak-graph bound: streaming a huge source materializes at most
    /// `active_file_cap * template_nodes` at once, never `total * template`.
    /// This is the proof that fails on the old "build the full dag first"
    /// approach (whose peak would be `N * template`).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn streaming_peak_graph_bounded_over_ten_million_items() {
        const N: usize = 10_000_000;
        const ACTIVE: usize = 64;
        const TEMPLATE_NODES: usize = 7; // single-file shaped chain

        let produced = Arc::new(AtomicUsize::new(0));
        let source = CountingWorkSource::new(N, Arc::clone(&produced));
        let config = StreamingConfig {
            backlog_cap: 10_000,
            active_file_cap: ACTIVE,
        };

        let summary = run_streaming(source, config, |_item, mut admission| async move {
            // Simulate expand-on-admit: materialize the template's node count.
            admission.materialize_nodes(TEMPLATE_NODES);
            // Instant runner: no node execution, this proves the graph bound.
            crate::transfer_dag::TransferDagMetrics::default()
        })
        .await;

        assert_eq!(summary.items_admitted, N, "every item must be admitted");
        assert_eq!(produced.load(Ordering::SeqCst), N);
        assert!(
            summary.peak_active_nodes <= ACTIVE * TEMPLATE_NODES,
            "peak nodes {} exceeded active bound {}",
            summary.peak_active_nodes,
            ACTIVE * TEMPLATE_NODES
        );
        assert!(
            summary.peak_active_files <= ACTIVE,
            "peak files {} exceeded active_file_cap {ACTIVE}",
            summary.peak_active_files
        );
    }

    /// Parametric proof: peak resident graph is a function of `active_file_cap`
    /// and template size, independent of the total item count.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn streaming_peak_graph_is_independent_of_total() {
        const ACTIVE: usize = 16;
        const TEMPLATE_NODES: usize = 7;

        async fn peak_for(n: usize) -> usize {
            let produced = Arc::new(AtomicUsize::new(0));
            let source = CountingWorkSource::new(n, produced);
            let config = StreamingConfig {
                backlog_cap: 1_000,
                active_file_cap: ACTIVE,
            };
            run_streaming(source, config, |_item, mut admission| async move {
                admission.materialize_nodes(TEMPLATE_NODES);
                tokio::task::yield_now().await;
                crate::transfer_dag::TransferDagMetrics::default()
            })
            .await
            .peak_active_nodes
        }

        let small = peak_for(50_000).await;
        let large = peak_for(500_000).await;
        // Both bounded identically; growth is not O(total).
        assert!(small <= ACTIVE * TEMPLATE_NODES);
        assert!(large <= ACTIVE * TEMPLATE_NODES);
    }

    /// Backpressure: with the active set and the backlog both full, the source
    /// is paused — it never runs unboundedly ahead of the consumers. When the
    /// consumers drain, production resumes to completion.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn streaming_backpressure_pauses_source_when_backlog_full() {
        const N: usize = 1_000;
        const ACTIVE: usize = 2;
        const BACKLOG: usize = 4;

        let produced = Arc::new(AtomicUsize::new(0));
        let source = CountingWorkSource::new(N, Arc::clone(&produced));
        // A gate with no permits: every admitted file blocks until released.
        let gate = Arc::new(Semaphore::new(0));

        let gate_run = Arc::clone(&gate);
        let run = tokio::spawn(async move {
            run_streaming(
                source,
                StreamingConfig {
                    backlog_cap: BACKLOG,
                    active_file_cap: ACTIVE,
                },
                move |_item, _admission| {
                    let gate = Arc::clone(&gate_run);
                    async move {
                        // Hold the active slot until released.
                        let _permit = gate.acquire().await.expect("gate open");
                        crate::transfer_dag::TransferDagMetrics::default()
                    }
                },
            )
            .await
        });

        // Let the producer fill the backlog and the active set, then stall.
        tokio::time::sleep(Duration::from_millis(150)).await;
        let observed = produced.load(Ordering::SeqCst);
        assert!(
            observed <= ACTIVE + BACKLOG + 2,
            "source ran ahead unbounded: produced {observed} with active={ACTIVE} backlog={BACKLOG}"
        );
        assert!(observed < N, "source must not have produced everything yet");

        // Drain: release every file. Production resumes and the run completes.
        gate.add_permits(N);
        let summary = run.await.expect("streaming run");
        assert_eq!(summary.items_admitted, N);
        assert_eq!(produced.load(Ordering::SeqCst), N);
    }

    /// Topology preserved on active files: an admitted file's shaped subgraph
    /// runs `acquire -> transfer -> verify -> preserve -> commit -> emit` in
    /// order, and a verify failure blocks commit (the P2-03 Verified-before-
    /// Committed gate) even when the file is executed through the streaming
    /// frontier. Uses a small mock runner over the real production subgraph.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn streaming_active_file_preserves_transfer_topology() {
        use crate::transfer_dag::builder::{TransferDagBuilder, TransferDirection};
        use crate::transfer_dag::capabilities::TransferCapabilities;
        use crate::transfer_dag::error::TransferError;
        use crate::transfer_dag::executor::{execute_dag, DagNodeRunner, NodeFuture, NodeOutcome};
        use crate::transfer_dag::graph::{TransferNode, TransferNodeKind};
        use crate::transfer_dag::observer::NoopDagObserver;
        use crate::transfer_dag::resources::{TransferBudget, TransferResourceManager};
        use std::sync::Mutex as StdMutex;

        // Records (file index, node kind) as each node executes.
        type Trace = Arc<StdMutex<Vec<(usize, TransferNodeKind)>>>;
        let trace: Trace = Arc::new(StdMutex::new(Vec::new()));

        let manager = Arc::new(TransferResourceManager::new(
            TransferBudget::from_file_slots(2),
        ));
        let caps = TransferCapabilities::default();

        // Two files: index 0 succeeds, index 1 fails at VerifyChecksum.
        let source = SliceWorkSource::new(vec![
            TransferWorkItem::new("ok.bin", 0, 0, TransferDirection::Upload),
            TransferWorkItem::new("bad.bin", 1, 0, TransferDirection::Upload),
        ]);

        let trace_run = Arc::clone(&trace);
        let manager_run = Arc::clone(&manager);
        let summary = run_streaming(
            source,
            StreamingConfig {
                backlog_cap: 8,
                active_file_cap: 2,
            },
            move |item, mut admission| {
                let trace = Arc::clone(&trace_run);
                let manager = Arc::clone(&manager_run);
                let caps = caps.clone();
                async move {
                    let shaped = TransferDagBuilder::shaped_file(item.direction, &caps, item.size);
                    admission.materialize_nodes(shaped.dag.nodes().len());
                    let index = item.index;
                    let runner: Arc<dyn DagNodeRunner> =
                        Arc::new(move |node: TransferNode| -> NodeFuture {
                            let trace = Arc::clone(&trace);
                            Box::pin(async move {
                                trace.lock().unwrap().push((index, node.kind));
                                // File 1 fails verification: a fatal Failed does
                                // not release dependents, so commit cannot run.
                                if index == 1 && node.kind == TransferNodeKind::VerifyChecksum {
                                    return NodeOutcome::Failed(TransferError::from_message(
                                        "verify failed",
                                    ));
                                }
                                NodeOutcome::Completed
                            })
                        });
                    let observer: Arc<dyn crate::transfer_dag::observer::DagObserver> =
                        Arc::new(NoopDagObserver);
                    let _ = execute_dag(&shaped.dag, &manager, runner, observer, None).await;
                    crate::transfer_dag::TransferDagMetrics::default()
                }
            },
        )
        .await;

        assert_eq!(summary.items_admitted, 2);
        assert!(summary.peak_active_nodes <= 2 * 7);

        let trace = trace.lock().unwrap();
        let file0: Vec<TransferNodeKind> = trace
            .iter()
            .filter(|(i, _)| *i == 0)
            .map(|(_, k)| *k)
            .collect();
        assert_eq!(
            file0,
            vec![
                TransferNodeKind::DiscoverLocal,
                TransferNodeKind::AcquireResource,
                TransferNodeKind::UploadFile,
                TransferNodeKind::VerifyChecksum,
                TransferNodeKind::PreserveMetadata,
                TransferNodeKind::CommitTemp,
                TransferNodeKind::EmitProgress,
            ],
            "an admitted file keeps the acquire->transfer->verify->preserve->commit->emit order"
        );

        let file1: Vec<TransferNodeKind> = trace
            .iter()
            .filter(|(i, _)| *i == 1)
            .map(|(_, k)| *k)
            .collect();
        assert!(
            file1.contains(&TransferNodeKind::VerifyChecksum),
            "verify ran for the failing file"
        );
        assert!(
            !file1.contains(&TransferNodeKind::CommitTemp),
            "a verify failure must block commit (Verified before Committed)"
        );
        assert!(
            !file1.contains(&TransferNodeKind::EmitProgress),
            "no terminal emit after a blocked commit"
        );
    }
}
