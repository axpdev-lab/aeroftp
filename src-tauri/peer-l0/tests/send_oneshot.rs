//! S5 gate: end-to-end "Send file to user" one-shot over real iroh endpoints on
//! localhost. Exercises the WHOLE path the app uses: an identity-seeded receiver
//! (`run_receiver`) dialed BY AFID by a sender (`run_send_file`), through
//! seal-to-recipient + offer + Accept + encrypted blob + BLAKE3 verify + inbox
//! write, plus the explicit-decline path.
//!
//! `#[ignore]` because it binds real endpoints and relies on iroh discovery
//! (n0/DHT + staging relay) to resolve the receiver's NodeId, so it needs the
//! network and is not hermetic. Run it manually:
//!   cargo test -p aeroftp-peer-l0 --test send_oneshot -- --ignored --nocapture

use aeroftp_peer_l0::endpoint::PeerEndpointConfig;
use aeroftp_peer_l0::send::{run_receiver, run_send_file, IncomingOffer, ReceiveEvent};
use aeroftp_peer_l0::Identity;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn cfg() -> PeerEndpointConfig {
    PeerEndpointConfig::default()
}

/// Spawn a receiver that, per offer, calls `decide` and records every
/// ReceiveEvent into the returned shared vec. Returns (task, events, my_afid).
fn spawn_receiver<F>(
    secret: Vec<u8>,
    decide: F,
) -> (tokio::task::JoinHandle<()>, Arc<Mutex<Vec<ReceiveEvent>>>)
where
    F: Fn(IncomingOffer) -> Option<PathBuf> + Send + Sync + 'static,
{
    let events = Arc::new(Mutex::new(Vec::new()));
    let events2 = events.clone();
    let decide = Arc::new(decide);
    let task = tokio::spawn(async move {
        let decide = decide.clone();
        let _ = run_receiver(
            &secret,
            cfg(),
            move |offer: IncomingOffer| {
                let decide = decide.clone();
                async move { decide(offer) }
            },
            move |ev: ReceiveEvent| {
                events2.lock().unwrap().push(ev);
            },
        )
        .await;
    });
    (task, events)
}

#[tokio::test]
#[ignore]
async fn one_shot_accept_delivers_file_byte_identical() {
    let bob = Identity::generate();
    let bob_secret = bob.to_secret_bytes().to_vec();
    let bob_afid = bob.public().to_aeroftp_id();

    let alice = Identity::generate();
    let alice_secret = alice.to_secret_bytes().to_vec();

    let inbox = tempfile::tempdir().expect("inbox");
    let inbox_dir = inbox.path().to_path_buf();

    let dest = inbox_dir.clone();
    let (recv_task, events) = spawn_receiver(bob_secret, move |_offer| Some(dest.clone()));

    // Give Bob's endpoint time to bind + publish to discovery.
    tokio::time::sleep(Duration::from_secs(3)).await;

    let payload = b"hello from alice - one-shot E2EE send".to_vec();
    let src = tempfile::NamedTempFile::new().expect("src");
    std::fs::write(src.path(), &payload).expect("write src");

    let sent = run_send_file(
        &bob_afid,
        &alice_secret,
        &src.path().to_string_lossy(),
        cfg(),
    )
    .await;
    assert!(sent.is_ok(), "send must succeed: {sent:?}");

    // Let the receiver finish writing + emit Completed.
    tokio::time::sleep(Duration::from_secs(1)).await;

    let evs = events.lock().unwrap().clone();
    let completed = evs.iter().find_map(|e| match e {
        ReceiveEvent::Completed { path, .. } => Some(path.clone()),
        _ => None,
    });
    let path = completed.expect("a Completed event with the written path");
    let got = std::fs::read(&path).expect("read received file");
    assert_eq!(got, payload, "received bytes must equal sent bytes");

    recv_task.abort();
}

#[tokio::test]
#[ignore]
async fn one_shot_decline_is_reported_to_sender() {
    let bob = Identity::generate();
    let bob_secret = bob.to_secret_bytes().to_vec();
    let bob_afid = bob.public().to_aeroftp_id();

    let alice = Identity::generate();
    let alice_secret = alice.to_secret_bytes().to_vec();

    // Decline everything.
    let (recv_task, events) = spawn_receiver(bob_secret, move |_offer| None);

    tokio::time::sleep(Duration::from_secs(3)).await;

    let src = tempfile::NamedTempFile::new().expect("src");
    std::fs::write(src.path(), b"nope").expect("write src");

    let sent = run_send_file(
        &bob_afid,
        &alice_secret,
        &src.path().to_string_lossy(),
        cfg(),
    )
    .await;
    assert!(sent.is_err(), "a declined send must surface as an error to the sender");

    tokio::time::sleep(Duration::from_millis(500)).await;
    let evs = events.lock().unwrap().clone();
    assert!(
        evs.iter().any(|e| matches!(e, ReceiveEvent::Declined { .. })),
        "the receiver must report a Declined event"
    );

    recv_task.abort();
}
