// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! AIMD backpressure integration tests (DAG-ENGINE phase 3, F3-T07).
//!
//! These drive the real `execute_dag` scheduler with a wired `AimdController`
//! against congestion "flapper" runners. They cover the three congestion
//! classes the controller throttles on (429, 503, request timeout), confirm a
//! non-congestion failure does not throttle, and prove a congestion shrink
//! carries into the following run as a genuine concurrency throttle.
//!
//! The congestion signal is delivered as the provider error string the real
//! `congestion_from_error` classifier parses; a mock HTTP server would only
//! produce the same strings. The live rate-limited validation against a real
//! provider (Google Drive) is F3-T08, run in the phase-4 validation window.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ftp_client_gui_lib::transfer_dag::executor::{
    execute_dag, DagNodeRunner, NodeFuture, NodeOutcome,
};
use ftp_client_gui_lib::transfer_dag::graph::{TransferDag, TransferNode, TransferNodeKind};
use ftp_client_gui_lib::transfer_dag::observer::CollectingDagObserver;
use ftp_client_gui_lib::transfer_dag::{
    AimdConfig, AimdController, DagObserver, NoopDagObserver, ResourceRequest, TransferBudget,
    TransferResourceManager,
};

/// A one-node graph: a single `DownloadFile` reserving one file slot.
fn one_file_dag() -> TransferDag {
    let mut dag = TransferDag::default();
    dag.add_node(
        TransferNodeKind::DownloadFile,
        vec![],
        ResourceRequest::file_transfer(),
    );
    dag
}

/// A runner whose every node fails with a fixed message.
fn failing_runner(message: &'static str) -> Arc<dyn DagNodeRunner> {
    Arc::new(move |_node: TransferNode| -> NodeFuture {
        Box::pin(async move { NodeOutcome::Failed(message.to_string()) })
    })
}

/// Run a one-file graph whose transfer fails with `message`, with an AIMD
/// controller wired, and return the recorded `backpressure_events` count.
async fn backpressure_events_for(message: &'static str) -> u32 {
    let dag = one_file_dag();
    let manager = TransferResourceManager::new(TransferBudget::from_file_slots(8));
    let controller = Arc::new(AimdController::from_budget(
        &manager.budget(),
        AimdConfig::default(),
    ));
    let observer = Arc::new(CollectingDagObserver::default());

    let result = execute_dag(
        &dag,
        &manager,
        failing_runner(message),
        Arc::clone(&observer) as Arc<dyn DagObserver>,
        Some(controller),
    )
    .await;

    assert!(
        result.is_err(),
        "a failed transfer node must surface as a graph error"
    );
    observer.metrics().backpressure_events
}

#[tokio::test]
async fn aimd_429_flap_records_backpressure() {
    assert_eq!(
        backpressure_events_for("HTTP 429 Too Many Requests").await,
        1,
        "a 429 is a congestion signal and must record one backpressure event"
    );
}

#[tokio::test]
async fn aimd_503_flap_records_backpressure() {
    assert_eq!(
        backpressure_events_for("503 Service Unavailable").await,
        1,
        "a 503 is a congestion signal and must record one backpressure event"
    );
}

#[tokio::test]
async fn aimd_timeout_flap_records_backpressure() {
    assert_eq!(
        backpressure_events_for("operation timed out after 30s").await,
        1,
        "a request timeout is a congestion signal and must record one event"
    );
}

#[tokio::test]
async fn aimd_non_congestion_failure_records_no_backpressure() {
    // A not-found is a hard error, not congestion: the run still fails, but
    // AIMD must not throttle on it.
    assert_eq!(
        backpressure_events_for("404 not found").await,
        0,
        "a non-congestion failure must not record a backpressure event"
    );
    assert_eq!(backpressure_events_for("permission denied").await, 0);
}

#[tokio::test]
async fn aimd_shrink_from_a_congested_run_throttles_the_next_run() {
    // A long-window config so a healthy run after congestion cannot regrow
    // the target inside the test: the shrink is observed as a hard throttle.
    let cfg = AimdConfig {
        cooldown: Duration::from_secs(3600),
        healthy_window: Duration::from_secs(3600),
        recovery_window: Duration::from_secs(3600),
    };
    let budget = TransferBudget::from_file_slots(8);
    let controller = Arc::new(AimdController::from_budget(&budget, cfg));

    // Run 1: one DownloadFile fails with a 429. The shared controller's File
    // class halves its dispatch target 8 -> 4.
    let observer = Arc::new(CollectingDagObserver::default());
    let run1 = execute_dag(
        &one_file_dag(),
        &TransferResourceManager::new(budget),
        failing_runner("HTTP 429 Too Many Requests"),
        Arc::clone(&observer) as Arc<dyn DagObserver>,
        Some(Arc::clone(&controller)),
    )
    .await;
    assert!(run1.is_err());
    assert_eq!(observer.metrics().backpressure_events, 1);

    // Run 2: eight independent DownloadFile nodes all succeed slowly. The
    // resource manager budget still allows 8 in flight, but the shrunk
    // controller caps the File dispatch target at 4 — peak in-flight is 4.
    let mut dag2 = TransferDag::default();
    for _ in 0..8 {
        dag2.add_node(
            TransferNodeKind::DownloadFile,
            vec![],
            ResourceRequest::file_transfer(),
        );
    }
    let in_flight = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let runner: Arc<dyn DagNodeRunner> = {
        let in_flight = Arc::clone(&in_flight);
        let peak = Arc::clone(&peak);
        Arc::new(move |_node: TransferNode| -> NodeFuture {
            let in_flight = Arc::clone(&in_flight);
            let peak = Arc::clone(&peak);
            Box::pin(async move {
                let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(40)).await;
                in_flight.fetch_sub(1, Ordering::SeqCst);
                NodeOutcome::Completed
            })
        })
    };

    let summary = execute_dag(
        &dag2,
        &TransferResourceManager::new(budget),
        runner,
        Arc::new(NoopDagObserver) as Arc<dyn DagObserver>,
        Some(Arc::clone(&controller)),
    )
    .await
    .expect("the healthy second run must complete");

    assert_eq!(summary.nodes_completed, 8);
    assert_eq!(
        peak.load(Ordering::SeqCst),
        4,
        "the congestion shrink from run 1 throttles run 2 to 4 transfers in flight"
    );
}
