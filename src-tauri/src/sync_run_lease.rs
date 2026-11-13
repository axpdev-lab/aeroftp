// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Keep a folder pair owned until every native command of its run settles.
//! OS locks exclude other windows/processes and release on process exit.
//! Lock files are never unlinked while in use.

use std::collections::HashMap;
use std::fs::File;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

/// Native commands a sync run may leave in flight that are still running.
/// A target pin covers only a command that was given an expected target;
/// a local-local run gives none, so this count is what tells a lost page's
/// cleanup that its copies, deletes and index writes are over.
static NATIVE_WORK: AtomicUsize = AtomicUsize::new(0);

/// Held for the whole body of such a command. Dropping it ends the work.
#[must_use = "the work counts only while the guard is alive"]
pub(crate) struct NativeWork;

impl Drop for NativeWork {
    fn drop(&mut self) {
        NATIVE_WORK.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Admit one native command of a sync run and count it as in flight until the
/// guard drops.
///
/// A run passes its lease token. The token must still name a live lease: one
/// released, or orphaned when its page was lost (even while the orphan still
/// keeps the pair locked for work already running), is refused. The check and
/// the count happen under the registry lock [`orphan_window`] takes, so a
/// command is either counted before its page is orphaned, and the cleanup
/// waits for it, or refused after. A command called outside a run passes
/// `None` and is only counted.
pub(crate) fn admit(token: Option<&str>) -> Result<NativeWork, String> {
    let registry = REGISTRY
        .lock()
        .map_err(|_| "Sync lease registry unavailable")?;
    if let Some(token) = token {
        if !registry.leases.contains_key(token) {
            return Err(
                "SYNC_RUN_BUSY: this run no longer owns the folder pair; compare again".into(),
            );
        }
    }
    NATIVE_WORK.fetch_add(1, Ordering::SeqCst);
    drop(registry);
    Ok(NativeWork)
}

/// No native command of a sync run is in flight.
pub(crate) fn native_work_settled() -> bool {
    NATIVE_WORK.load(Ordering::SeqCst) == 0
}

struct Lease {
    pair: String,
    file: Arc<File>,
    /// Label of the window whose page began the run.
    window: String,
}

/// Every sync lease, under one lock: the live ones, the orphans whose page is
/// gone, and how many times each window's page was replaced. Admission
/// ([`admit`]), registration ([`acquire`]) and page loss ([`orphan_window`])
/// all decide under it, so none of them can slip between the others.
#[derive(Default)]
struct Registry {
    leases: HashMap<String, Lease>,
    /// Leases whose page is gone. Their tokens no longer open the intent
    /// writer or admit a command, and the folder pair stays locked until
    /// [`release_orphans`].
    orphans: HashMap<String, Lease>,
    /// Bumped each time a window's page is replaced.
    page_epochs: HashMap<String, u64>,
}

static REGISTRY: LazyLock<Mutex<Registry>> = LazyLock::new(|| Mutex::new(Registry::default()));

/// The page generation of `window`, read when a command of its page begins.
/// [`acquire`] refuses to register a lease for a generation that is over.
pub(crate) fn page_epoch(window: &str) -> Result<u64, String> {
    let registry = REGISTRY
        .lock()
        .map_err(|_| "Sync lease registry unavailable")?;
    Ok(registry.page_epochs.get(window).copied().unwrap_or(0))
}

fn acquire_at(dir: &Path, pair: String, window: &str, epoch: u64) -> Result<String, String> {
    let mut registry = REGISTRY
        .lock()
        .map_err(|_| "Sync lease registry unavailable")?;
    // The page that asked was replaced while the request waited for this
    // worker: no script is left to end the run, and its page loss has already
    // been handled, so nothing would orphan this lease. Refuse before any lock.
    if registry.page_epochs.get(window).copied().unwrap_or(0) != epoch {
        return Err("SYNC_RUN_PAGE_REPLACED: the page that began this sync was reloaded".into());
    }
    if registry.leases.len() >= 256 {
        return Err("Too many active sync runs; finish them or restart the application".into());
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options
        .open(dir.join(&pair).with_extension("run-lock"))
        .map_err(|_| "Cannot open sync run lock")?;
    file.try_lock().map_err(|_| "SYNC_RUN_BUSY: a sync still owns this folder pair; wait for it to finish or restart the application")?;
    let token = uuid::Uuid::new_v4().to_string();
    registry.leases.insert(
        token.clone(),
        Lease {
            pair,
            file: Arc::new(file),
            window: window.to_owned(),
        },
    );
    Ok(token)
}

/// Register a lease for the page of `window` whose generation was `epoch`
/// when its command began (see [`page_epoch`]).
pub(crate) fn acquire(
    local: &str,
    remote: &str,
    window: &str,
    epoch: u64,
) -> Result<String, String> {
    acquire_at(
        &crate::sync::sync_index_dir()?,
        crate::sync::index_filename(local, remote),
        window,
        epoch,
    )
}

/// The script `on_page_load` evaluates in a document that finished loading:
/// it gives that document the window's page generation, once, read-only.
/// A document keeps the first value it receives; one the window replaced
/// never receives a later one, and its script cannot write its own.
pub(crate) fn page_epoch_script(epoch: u64) -> String {
    format!(
        "if (!Object.prototype.hasOwnProperty.call(window, '{PAGE_EPOCH_GLOBAL}')) {{ \
         Object.defineProperty(window, '{PAGE_EPOCH_GLOBAL}', {{ value: {epoch}, writable: false, configurable: false }}); }}"
    )
}

/// The document global [`page_epoch_script`] sets; the runner reads it.
pub(crate) const PAGE_EPOCH_GLOBAL: &str = "__AEROFTP_SYNC_PAGE_EPOCH__";

/// [`acquire`] for a begin request from a page, with the generation that page
/// read at its start. A request that names none is refused: reading the
/// generation now could give the page that replaced it.
pub(crate) fn acquire_for_page(
    local: &str,
    remote: &str,
    window: &str,
    epoch: Option<u64>,
) -> Result<String, String> {
    let epoch = epoch.ok_or(
        "SYNC_RUN_PAGE_REPLACED: the page did not name its generation; reload it and compare again",
    )?;
    acquire(local, remote, window, epoch)
}

/// The page of `window` was replaced (a reload, or a new web process after a
/// crash), so the script that would end its runs no longer exists. Bump the
/// window's page generation, take its leases out of the registry without
/// unlocking the pairs, and return the tokens to hand to [`release_orphans`].
pub(crate) fn orphan_window(window: &str) -> Vec<String> {
    let Ok(mut registry) = REGISTRY.lock() else {
        return Vec::new();
    };
    *registry.page_epochs.entry(window.to_owned()).or_insert(0) += 1;
    let tokens: Vec<String> = registry
        .leases
        .iter()
        .filter(|(_, lease)| lease.window == window)
        .map(|(token, _)| token.clone())
        .collect();
    for token in &tokens {
        if let Some(lease) = registry.leases.remove(token) {
            registry.orphans.insert(token.clone(), lease);
        }
    }
    tokens
}

pub(crate) fn release_orphans(tokens: &[String]) {
    if let Ok(mut registry) = REGISTRY.lock() {
        for token in tokens {
            registry.orphans.remove(token);
        }
    }
}

/// Unlock orphaned pairs once `settled` says the native work the lost page
/// had started is over (no target pin and no [`admit`] guard). Until then a
/// new run of the same pair is refused.
pub(crate) async fn release_orphans_once_settled(tokens: Vec<String>, settled: impl Fn() -> bool) {
    while !settled() {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    release_orphans(&tokens);
}

pub(crate) fn guard(local: &str, remote: &str, token: &str) -> Result<Arc<File>, String> {
    let pair = crate::sync::index_filename(local, remote);
    let registry = REGISTRY
        .lock()
        .map_err(|_| "Sync lease registry unavailable")?;
    let lease = registry
        .leases
        .get(token)
        .filter(|lease| lease.pair == pair)
        .ok_or("SYNC_RUN_BUSY: this run no longer owns the folder pair")?;
    Ok(lease.file.clone())
}

pub(crate) fn release(token: &str) -> Result<(), String> {
    REGISTRY
        .lock()
        .map_err(|_| "Sync lease registry unavailable")?
        .leases
        .remove(token);
    Ok(())
}

/// Validate before calling the native intent writer, keeping the lock through it.
pub(crate) fn with_guard<T>(
    local: &str,
    remote: &str,
    token: &str,
    write: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let _lease = guard(local, remote, token)?;
    write()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A registration from the page of `window` as it is now.
    fn acquire_now(dir: &Path, pair: String, window: &str) -> Result<String, String> {
        acquire_at(dir, pair, window, page_epoch(window).unwrap())
    }

    /// Item 8 of the review: a command the lost page had already dispatched
    /// reaches its handler only after the cleanup. With the run's token it is
    /// refused both while the orphan still holds the pair for work in flight
    /// and after the orphan is released, so it cannot write into a pair a new
    /// run owns. A command admitted before the page was lost holds the
    /// release back until it ends.
    #[tokio::test]
    async fn a_run_command_that_starts_after_its_page_is_lost_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let pair = crate::sync::index_filename("/admit-local", "/admit-remote");
        let lost = acquire_now(dir.path(), pair.clone(), "admit-window").unwrap();
        // Admitted while the page is alive: counted until it ends.
        let early = admit(Some(&lost)).unwrap();

        let orphans = orphan_window("admit-window");
        assert_eq!(orphans, std::slice::from_ref(&lost));
        // Orphaned but not released: the pair is still locked for `early`.
        assert!(
            admit(Some(&lost)).is_err(),
            "an orphaned lease admits nothing, even before it is released"
        );
        assert!(acquire_now(dir.path(), pair.clone(), "admit-replacement").is_err());
        let waiter = tokio::spawn(release_orphans_once_settled(orphans, native_work_settled));
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        assert!(
            !waiter.is_finished(),
            "the command admitted before the loss still runs"
        );

        drop(early);
        // Other tests of this process may hold their own native work for a
        // moment; the waiter returns as soon as all of it is over.
        tokio::time::timeout(std::time::Duration::from_secs(30), waiter)
            .await
            .expect("released once the admitted command ended")
            .unwrap();
        // A replacement run owns the pair now. The lost page's late command
        // is refused before it can touch it; one outside any run is counted.
        let replacement = acquire_now(dir.path(), pair, "admit-replacement").unwrap();
        assert!(
            admit(Some(&lost)).is_err(),
            "a released lease admits nothing"
        );
        assert!(admit(Some("unknown")).is_err());
        drop(admit(Some(&replacement)).unwrap());
        drop(admit(None).unwrap());
        release(&replacement).unwrap();
    }

    /// The lease is registered on a blocking worker, after the command began.
    /// A page reload in that gap used to register a lease no cleanup would
    /// ever orphan: the reload had already been handled. The registration now
    /// carries the page generation read when the command began, and one for a
    /// page that is gone is refused before any lock is taken, so a replacement
    /// run gets the pair.
    #[test]
    fn a_lease_requested_by_a_page_replaced_before_registration_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let pair = crate::sync::index_filename("/epoch-local", "/epoch-remote");
        // The command begins and reads its page's generation...
        let epoch = page_epoch("epoch-window").unwrap();
        // ...the page reloads while the worker has not registered yet...
        assert!(orphan_window("epoch-window").is_empty());
        // ...and the worker resumes.
        let late = acquire_at(dir.path(), pair.clone(), "epoch-window", epoch);
        assert!(
            late.as_ref()
                .is_err_and(|e| e.starts_with("SYNC_RUN_PAGE_REPLACED")),
            "{late:?}"
        );
        // Nothing holds the pair: the new page's run acquires it at once.
        let replacement = acquire_now(dir.path(), pair, "epoch-window").unwrap();
        assert!(admit(Some(&replacement)).is_ok());
        release(&replacement).unwrap();
    }

    /// The value `on_page_load` hands a document is set once and read-only,
    /// so a document holds the first generation it received and nothing
    /// written later, by the page or by a stray delivery, replaces it.
    #[test]
    fn the_page_generation_is_handed_to_a_document_once_and_read_only() {
        let script = page_epoch_script(7);
        assert!(script.contains(PAGE_EPOCH_GLOBAL));
        assert!(script.contains("value: 7"));
        assert!(script.contains("writable: false, configurable: false"));
        assert!(script.starts_with("if (!Object.prototype.hasOwnProperty.call(window, "));
    }

    /// The generation a document was given: the old document's, sent after the
    /// reload, is refused; the new document's is accepted; a begin without one
    /// is refused rather than given the generation of the moment.
    #[test]
    fn a_begin_carries_the_generation_its_page_read_at_start() {
        let dir = tempfile::tempdir().unwrap();
        let pair = crate::sync::index_filename("/start-local", "/start-remote");
        let read_at_start = page_epoch("start-window").unwrap();
        // The window starts loading another page.
        orphan_window("start-window");
        let new_page = page_epoch("start-window").unwrap();
        assert_ne!(read_at_start, new_page);
        let old = acquire_at(dir.path(), pair.clone(), "start-window", read_at_start);
        assert!(
            old.as_ref()
                .is_err_and(|e| e.starts_with("SYNC_RUN_PAGE_REPLACED")),
            "{old:?}"
        );
        // Refused before any folder is touched: no generation is read here.
        let unnamed = acquire_for_page("/start-local", "/start-remote", "start-window", None);
        assert!(
            unnamed
                .as_ref()
                .is_err_and(|e| e.starts_with("SYNC_RUN_PAGE_REPLACED")),
            "{unnamed:?}"
        );
        let token = acquire_at(dir.path(), pair, "start-window", new_page).unwrap();
        release(&token).unwrap();
    }

    #[test]
    fn missing_and_foreign_tokens_cannot_enter_the_native_intent_writer() {
        let dir = tempfile::tempdir().unwrap();
        let pair = crate::sync::index_filename("/protected-local", "/protected-remote");
        let valid = acquire_now(dir.path(), pair, "main").unwrap();
        let foreign = acquire_now(
            dir.path(),
            crate::sync::index_filename("/other", "/other"),
            "main",
        )
        .unwrap();
        let intent = dir.path().join("intent");
        std::fs::write(&intent, b"original durable intent").unwrap();
        for token in ["", "unknown", foreign.as_str()] {
            assert!(
                with_guard("/protected-local", "/protected-remote", token, || {
                    std::fs::write(&intent, b"overwritten intent").map_err(|e| e.to_string())
                })
                .is_err()
            );
            assert_eq!(std::fs::read(&intent).unwrap(), b"original durable intent");
        }
        with_guard("/protected-local", "/protected-remote", &valid, || {
            std::fs::write(&intent, b"authorized intent").map_err(|e| e.to_string())
        })
        .unwrap();
        assert_eq!(std::fs::read(&intent).unwrap(), b"authorized intent");
        release(&valid).unwrap();
        release(&foreign).unwrap();
    }

    #[test]
    fn replacement_cannot_start_before_the_previous_native_work_finishes() {
        let dir = tempfile::tempdir().unwrap();
        let pair = crate::sync::index_filename("/local", "/remote");
        let first = acquire_now(dir.path(), pair.clone(), "main").unwrap();
        assert!(acquire_now(dir.path(), pair.clone(), "main").is_err());
        assert!(guard("/other", "/remote", &first).is_err());
        let destination = dir.path().join("destination");
        std::fs::write(&destination, b"old upload completed").unwrap();
        release(&first).unwrap();
        let second = acquire_now(dir.path(), pair, "main").unwrap();
        std::fs::write(&destination, b"replacement upload completed").unwrap();
        assert_eq!(
            std::fs::read(&destination).unwrap(),
            b"replacement upload completed"
        );
        release(&second).unwrap();
    }

    #[test]
    fn an_intent_transaction_keeps_its_lock_even_if_the_handle_is_released() {
        let dir = tempfile::tempdir().unwrap();
        let pair = crate::sync::index_filename("/local2", "/remote2");
        let first = acquire_now(dir.path(), pair.clone(), "main").unwrap();
        let transaction = guard("/local2", "/remote2", &first).unwrap();
        release(&first).unwrap();
        assert!(acquire_now(dir.path(), pair.clone(), "main").is_err());
        drop(transaction);
        let second = acquire_now(dir.path(), pair, "main").unwrap();
        release(&second).unwrap();
    }
    #[tokio::test]
    async fn a_lost_page_keeps_its_pair_locked_until_native_work_settles() {
        let dir = tempfile::tempdir().unwrap();
        let pair = crate::sync::index_filename("/lost-local", "/lost-remote");
        let lost = acquire_now(dir.path(), pair.clone(), "lost-window").unwrap();
        let other_window = acquire_now(
            dir.path(),
            crate::sync::index_filename("/kept-local", "/kept-remote"),
            "kept-window",
        )
        .unwrap();
        // An accepted native command of the lost run still holds its pin.
        let state = std::sync::Arc::new(crate::provider_commands::ProviderState::new());
        let pin = state.pin_gui_target(Some(0)).await.unwrap();
        assert!(!state.gui_work_settled());

        let orphans = orphan_window("lost-window");
        assert_eq!(orphans, std::slice::from_ref(&lost));
        assert!(
            guard("/lost-local", "/lost-remote", &lost).is_err(),
            "the lost page can no longer open the intent writer"
        );
        assert!(guard("/kept-local", "/kept-remote", &other_window).is_ok());

        let waiter = {
            let state = std::sync::Arc::clone(&state);
            tokio::spawn(release_orphans_once_settled(orphans, move || {
                state.gui_work_settled()
            }))
        };
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        assert!(!waiter.is_finished());
        assert!(
            acquire_now(dir.path(), pair.clone(), "new-window").is_err(),
            "a replacement run must wait for the pending native work"
        );

        drop(pin);
        waiter.await.unwrap();
        let replacement = acquire_now(dir.path(), pair, "new-window").unwrap();
        release(&replacement).unwrap();
        release(&other_window).unwrap();
    }

    /// A local-local run gives its commands no expected target, so a copy it
    /// started holds no pin. The pair of its lost page stays locked while
    /// the real `copy_local_file` is still in flight, and is released once
    /// the command returns. The source is a FIFO no one has opened for
    /// writing, so the copy waits in its `open`; once a writer appears the
    /// standard library refuses a source that is not a regular file, and the
    /// command returns that error. Only how long it ran matters here.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_lost_local_run_keeps_its_pair_locked_until_an_unpinned_copy_returns() {
        /// Opens the FIFO for writing on drop, so a failed assertion does not
        /// leave the copy blocked in its open for the rest of the process.
        struct Unblock(std::path::PathBuf);
        impl Drop for Unblock {
            fn drop(&mut self) {
                use std::os::unix::fs::OpenOptionsExt;
                let _ = std::fs::OpenOptions::new()
                    .write(true)
                    .custom_flags(libc::O_NONBLOCK)
                    .open(&self.0);
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let pair = crate::sync::index_filename("/local-copy", "/local-copy-right");
        let lost = acquire_now(dir.path(), pair.clone(), "lost-local-window").unwrap();
        let source = dir.path().join("source.fifo");
        let c_source = std::ffi::CString::new(source.to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c_source.as_ptr(), 0o600) }, 0);
        let unblock = Unblock(source.clone());
        let destination = dir.path().join("copied.bin");

        // No target pin anywhere: the same state a local-local run leaves.
        let state = std::sync::Arc::new(crate::provider_commands::ProviderState::new());
        assert!(state.gui_work_settled());
        // Called as a local-local run calls it before this PR's admission
        // existed: no lease token, so the copy is only counted.
        let copy = tokio::spawn(crate::copy_local_file(
            source.to_string_lossy().into_owned(),
            destination.to_string_lossy().into_owned(),
            None,
            None,
        ));
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(!copy.is_finished(), "the copy waits for its source");

        let orphans = orphan_window("lost-local-window");
        assert_eq!(orphans, std::slice::from_ref(&lost));
        let waiter = {
            let state = std::sync::Arc::clone(&state);
            tokio::spawn(release_orphans_once_settled(orphans, move || {
                state.gui_work_settled() && native_work_settled()
            }))
        };
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        assert!(!waiter.is_finished(), "an unpinned copy is still running");
        assert!(
            acquire_now(dir.path(), pair.clone(), "new-window").is_err(),
            "a replacement run must wait for the running copy"
        );

        // A writer appears: the copy's `open` returns, the command ends (with
        // the refusal of a non-regular source) and the pair is released.
        tokio::task::spawn_blocking(move || {
            let _writer = std::fs::OpenOptions::new().write(true).open(&source);
        })
        .await
        .unwrap();
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(30), copy)
            .await
            .expect("the copy returns once its source is opened")
            .unwrap();
        assert!(
            outcome.is_err(),
            "a FIFO is not a regular file: {outcome:?}"
        );
        drop(unblock);
        // Other tests of this process may hold their own native work for a
        // moment; the waiter returns as soon as all of it is over.
        tokio::time::timeout(std::time::Duration::from_secs(30), waiter)
            .await
            .expect("released once the copy returned")
            .unwrap();
        let replacement = acquire_now(dir.path(), pair, "new-window").unwrap();
        release(&replacement).unwrap();
    }
}
