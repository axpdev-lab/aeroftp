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
    /// The one document of each window that owns its runs: the nonce it
    /// bound at its start, and the generation it bound in (see [`bind_page`]).
    bindings: HashMap<String, (u64, String)>,
    /// The generation whose first `Finished` each window has had.
    finished: HashMap<String, u64>,
}

static REGISTRY: LazyLock<Mutex<Registry>> = LazyLock::new(|| Mutex::new(Registry::default()));

/// The refusal of a document that is not, or no longer, the window's page.
/// The runner binds its nonce once more and retries once (`beginRunLease`):
/// a live document gets its window back, a dead one sends nothing.
const PAGE_REPLACED: &str =
    "SYNC_RUN_PAGE_REPLACED: this window's page was replaced; reload it and compare again";

/// The document whose load nonce is `nonce` binds its window, as its first
/// native call at start (`sync_run_page_bind_cmd`); a begin never binds.
///
/// The rules, with the page events (`orphan_window` on `Started`,
/// [`page_finished`] on the first `Finished` of a generation):
/// - a binding is refused while the current one holds a live lease: the
///   document running a sync is the live one, and a binding sent earlier by
///   a document that is gone (dispatched before the navigation, handled
///   after) cannot take its window from it;
/// - otherwise the last binding wins (it holds nothing to orphan);
/// - `Started` and the first `Finished` of a generation end the binding and
///   orphan the window's leases: what was bound or taken between them may be
///   the old document's, and the live one binds again (the runner's retry);
/// - a later `Finished` of the same generation changes nothing, so a
///   spurious one (WebView2 reports a failed or overlapped navigation as one)
///   never interrupts a live sync.
///
/// A window runs one document at a time, so a bind with another nonce while
/// the bound document holds leases comes from a document loaded after it
/// (or from a late message of one already gone): the bound document is no
/// longer the window's. Its leases are orphaned as a `Started` orphans them,
/// and the tokens returned for [`release_orphans_once_settled`]: the pairs
/// stay locked until the work they admitted has settled, then the new
/// document acquires them with no reload (T6a). This covers a first
/// `Finished` that was spurious, after which the old document took a lease.
pub(crate) fn bind_page(window: &str, nonce: &str) -> Result<Vec<String>, String> {
    let valid = (16..=128).contains(&nonce.len())
        && nonce
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-');
    if !valid {
        return Err(PAGE_REPLACED.into());
    }
    let mut registry = REGISTRY
        .lock()
        .map_err(|_| "Sync lease registry unavailable")?;
    bind_page_locked(&mut registry, window, nonce)
}

fn bind_page_locked(
    registry: &mut Registry,
    window: &str,
    nonce: &str,
) -> Result<Vec<String>, String> {
    let generation = registry.page_epochs.get(window).copied().unwrap_or(0);
    let replaced = registry
        .bindings
        .get(window)
        .is_some_and(|(_, owner)| owner != nonce);
    let orphans = if replaced {
        orphan_leases_of(registry, window)
    } else {
        Vec::new()
    };
    registry
        .bindings
        .insert(window.to_owned(), (generation, nonce.to_owned()));
    Ok(orphans)
}

/// The first `Finished` of the window's current generation: the binding ends
/// and the window's leases are orphaned (see [`bind_page`]); the tokens are
/// returned for [`release_orphans_once_settled`]. Any later `Finished` of
/// the same generation does nothing.
pub(crate) fn page_finished(window: &str) -> Vec<String> {
    let Ok(mut registry) = REGISTRY.lock() else {
        return Vec::new();
    };
    let generation = registry.page_epochs.get(window).copied().unwrap_or(0);
    if registry.finished.get(window) == Some(&generation) {
        return Vec::new();
    }
    registry.finished.insert(window.to_owned(), generation);
    registry.bindings.remove(window);
    orphan_leases_of(&mut registry, window)
}

/// Move every live lease of `window` to the orphans; their tokens.
fn orphan_leases_of(registry: &mut Registry, window: &str) -> Vec<String> {
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

/// Whether the document of `nonce` owns `window` in its current generation.
fn owns_page(registry: &Registry, window: &str, nonce: &str) -> bool {
    let generation = registry.page_epochs.get(window).copied().unwrap_or(0);
    registry
        .bindings
        .get(window)
        .is_some_and(|(bound, owner)| *bound == generation && owner == nonce)
}

fn acquire_at(dir: &Path, pair: String, window: &str, nonce: &str) -> Result<String, String> {
    let mut registry = REGISTRY
        .lock()
        .map_err(|_| "Sync lease registry unavailable")?;
    // Under the same lock as page loss and as binding: a document replaced
    // or taken over while the request waited for this worker is refused
    // before any folder lock, so no lease outlives the cleanup of its page.
    if !owns_page(&registry, window, nonce) {
        return Err(PAGE_REPLACED.into());
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

/// Register a lease for the document of `window` whose load nonce is
/// `nonce` (see [`bind_page`]). A request that names none is refused.
pub(crate) fn acquire_for_page(
    local: &str,
    remote: &str,
    window: &str,
    nonce: Option<&str>,
) -> Result<String, String> {
    let nonce = nonce.ok_or(PAGE_REPLACED)?;
    acquire_at(
        &crate::sync::sync_index_dir()?,
        crate::sync::index_filename(local, remote),
        window,
        nonce,
    )
}

/// A lease as a page's run holds one, for a test of a command a run owns.
#[cfg(test)]
pub(crate) fn acquire_for_test(dir: &Path, pair: &str, window: &str) -> Result<String, String> {
    let nonce = format!("test-run-{window}");
    bind_page(window, &nonce)?;
    acquire_at(dir, pair.to_owned(), window, &nonce)
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
    // The document that owned the generation now over is gone; the next
    // one binds at its start (see [`bind_page`]).
    registry.bindings.remove(window);
    orphan_leases_of(&mut registry, window)
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

/// What a page-load event of `window` does to the registry: `Started`
/// orphans the window ([`orphan_window`]), `Finished` ends the generation's
/// binding once ([`page_finished`]). The orphans go to
/// [`release_orphans_once_settled`]. The app and the page-load self-test
/// call this same function, so the self-test replays the engine's real
/// events through the production code (U11).
pub(crate) fn page_load_event(window: &str, finished: bool) -> Vec<String> {
    if finished {
        page_finished(window)
    } else {
        orphan_window(window)
    }
}

/// [`bind_page`] on a task of its own, which also hands the orphans of a
/// takeover to [`release_orphans_once_settled`] (U11). The command only
/// waits for it: a command future dropped while the bind runs (a page that
/// went away) leaves the task to finish, so the orphaned pairs always get
/// their cleanup and never stay locked for good.
pub(crate) async fn bind_page_and_schedule_cleanup<S>(
    window: String,
    nonce: String,
    settled: S,
) -> Result<(), String>
where
    S: Fn() -> bool + Send + 'static,
{
    // On the runtime the command runs on (a command is always awaited inside
    // one): the tasks outlive the command's future, not the runtime.
    tokio::spawn(async move {
        let orphans = tokio::task::spawn_blocking(move || bind_page(&window, &nonce))
            .await
            .map_err(|_| "Sync run lock worker failed".to_string())??;
        if !orphans.is_empty() {
            tokio::spawn(release_orphans_once_settled(orphans, settled));
        }
        Ok(())
    })
    .await
    .map_err(|_| "Sync run lock worker failed".to_string())?
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

/// The page-load self-test (U11, V2): a real webview (WebView2 on Windows,
/// WKWebView on macOS, WebKitGTK on Linux) is reloaded and navigated over
/// itself, round after round. Every `PageLoadEvent` the engine reports goes
/// through [`page_load_event`], the app's own handler, and every orphan it
/// makes goes to [`release_orphans_once_settled`] with
/// [`native_work_settled`], as in the app. Each document binds and asks for
/// the lease from its own script, as the sync runner does at start, and
/// asks again after a refusal.
///
/// Each round:
/// - before the navigations, the displayed document must hold a lease it
///   was granted; a refusal is counted by its kind and never stands for one;
/// - that lease admits native work ([`admit`]), held across the navigations;
/// - after them, its token must be an orphan that keeps the pair locked (no
///   live lease of the pair, and the lock file refuses another handle), and
///   the new document must be refused the pair, until the work is dropped;
/// - then the orphan must be released, and the next round starts by
///   requiring the new document's granted lease.
///
/// After every event and every request, the window's live leases must all
/// belong to the document bound now (never two documents with admitted
/// work). At the end the displayed document must be the bound one and hold
/// a granted lease.
///
/// It proves the invariants on the orders the engine produced in that run.
/// It cannot force the rare interleavings; the order-independent unit tests
/// of this module cover every order.
#[cfg(any(
    all(test, any(windows, target_os = "linux")),
    feature = "page-load-selftest"
))]
// Built on Linux too, so the shared code is checked where the suite runs;
// it runs on Windows (a lib test) and macOS (the feature's binary), and on
// a Linux desktop on demand (an ignored lib test).
#[cfg_attr(not(any(windows, feature = "page-load-selftest")), allow(dead_code))]
pub(crate) mod page_load_selftest {
    use super::*;
    use std::time::{Duration, Instant};

    const WINDOW: &str = "page-load-selftest";

    /// One page-load event as the engine reported it.
    #[derive(Debug, Clone)]
    pub(crate) struct Seen {
        pub finished: bool,
        pub url: String,
    }

    /// The numbers of a run.
    #[derive(Debug, Clone, Default)]
    pub(crate) struct Counts {
        /// Leases granted to a document's request.
        pub leases_granted: u32,
        /// Expected transient refusals: a lease asked by a document that is
        /// not, or no longer, the bound one (`SYNC_RUN_PAGE_REPLACED`).
        pub refused_replaced: u32,
        /// Expected transient refusals: a lease asked while an orphan still
        /// keeps the pair locked for its work (`SYNC_RUN_BUSY`).
        pub refused_busy: u32,
        /// Orphans found holding the pair while their admitted work ran.
        pub orphans_held: u32,
        /// Those orphans released once the work had settled.
        pub orphans_released: u32,
    }

    impl std::fmt::Display for Counts {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(
                f,
                "leases granted {}, transient refusals {} (page replaced {}, pair busy {}), orphans held {}, orphans released {}",
                self.leases_granted,
                self.refused_replaced + self.refused_busy,
                self.refused_replaced,
                self.refused_busy,
                self.orphans_held,
                self.orphans_released
            )
        }
    }

    /// What a run saw.
    #[derive(Debug)]
    pub(crate) struct Report {
        pub seen: Vec<Seen>,
        pub counts: Counts,
    }

    /// What the run saw and decided, shared with the engine's callbacks.
    #[derive(Default)]
    struct Run {
        seen: Vec<Seen>,
        /// Lease token -> the document (nonce) that was granted it.
        holders: HashMap<String, String>,
        counts: Counts,
        violation: Option<String>,
        alive: Option<String>,
        last_event: Option<Instant>,
    }

    /// The pages: each document binds itself from its script with a fresh
    /// nonce and asks for the lease; after a refusal it binds and asks
    /// again, as `beginRunLease` does, until it is granted or gone. It keeps
    /// its nonce for the "which document is displayed" question.
    const PAGE: &str = r#"<!doctype html><title>selftest</title><script>
window.__doc = 'doc' + crypto.randomUUID().split('-').join('');
(async () => {
  const call = step => fetch(location.origin + '/' + step + '/' + window.__doc).then(r => r.status);
  for (;;) {
    if (await call('bind') === 204 && await call('lease') === 204) return;
    await new Promise(r => setTimeout(r, 100));
  }
})();
</script>"#;

    #[cfg(windows)]
    const ORIGIN: &str = "http://selftest.localhost";
    #[cfg(not(windows))]
    const ORIGIN: &str = "selftest://localhost";

    fn violation(run: &mut Run, what: String) {
        run.violation.get_or_insert(what);
    }

    /// The invariant, read from the registry itself.
    fn check(run: &mut Run, when: &str) {
        let Ok(registry) = REGISTRY.lock() else {
            return;
        };
        let bound = registry
            .bindings
            .get(WINDOW)
            .map(|(_, nonce)| nonce.clone());
        let documents: std::collections::BTreeSet<&String> = registry
            .leases
            .iter()
            .filter(|(_, lease)| lease.window == WINDOW)
            .filter_map(|(token, _)| run.holders.get(token))
            .collect();
        let wrong = documents.len() > 1
            || documents
                .iter()
                .any(|holder| Some(*holder) != bound.as_ref());
        if wrong {
            let what = format!("{when}: live leases of {documents:?} while {bound:?} is bound");
            drop(registry);
            violation(run, what);
        }
    }

    /// The app's cleanup of a page's orphans: the pairs stay locked until no
    /// native work of a run is in flight.
    fn release_once_settled(orphans: Vec<String>) {
        if !orphans.is_empty() {
            tauri::async_runtime::spawn(release_orphans_once_settled(orphans, native_work_settled));
        }
    }

    /// The live lease the document `nonce` was granted, if it holds one.
    fn lease_of(state: &Mutex<Run>, nonce: &str) -> Option<String> {
        let run = state.lock().unwrap();
        let registry = REGISTRY.lock().ok()?;
        registry
            .leases
            .iter()
            .find(|(token, lease)| {
                lease.window == WINDOW && run.holders.get(*token).map(String::as_str) == Some(nonce)
            })
            .map(|(token, _)| token.clone())
    }

    /// `token` is an orphan and the pair has no live lease.
    fn held_by_orphan(token: &str, pair: &str) -> bool {
        REGISTRY.lock().is_ok_and(|registry| {
            registry.orphans.contains_key(token)
                && !registry.leases.values().any(|lease| lease.pair == pair)
        })
    }

    /// Another handle can lock the pair's lock file (it is then unlocked at
    /// once).
    fn lock_file_is_free(dir: &Path, pair: &str) -> bool {
        std::fs::OpenOptions::new()
            .write(true)
            .open(dir.join(pair).with_extension("run-lock"))
            .is_ok_and(|file| file.try_lock().is_ok())
    }

    /// The self-test build's whole program: run on the main thread (macOS
    /// allows the event loop nowhere else) and exit 0, or 1 on a broken
    /// invariant. Both outcomes print the counts.
    #[cfg(feature = "page-load-selftest")]
    pub(crate) fn run_and_exit() -> ! {
        let code = match run(tauri::Builder::default(), 25, Duration::from_secs(300)) {
            Ok(report) => {
                println!(
                    "page-load self-test passed: {} events, {} finished, last {}; {}",
                    report.seen.len(),
                    report.seen.iter().filter(|event| event.finished).count(),
                    report
                        .seen
                        .last()
                        .map(|event| event.url.as_str())
                        .unwrap_or(""),
                    report.counts
                );
                0
            }
            Err(error) => {
                eprintln!("page-load self-test FAILED: {error}");
                1
            }
        };
        std::process::exit(code);
    }

    /// Run `rounds` rounds of reload and overlapping navigations on a window
    /// of `builder`'s app, within `budget`. `Err` names the violated
    /// invariant, or what had not happened by the deadline, followed by the
    /// counts.
    pub(crate) fn run(
        builder: tauri::Builder<tauri::Wry>,
        rounds: u32,
        budget: Duration,
    ) -> Result<Report, String> {
        let dir =
            std::env::temp_dir().join(format!("aeroftp-page-load-selftest-{}", std::process::id()));
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let pair = crate::sync::index_filename("/selftest-local", "/selftest-remote");
        let state: Arc<Mutex<Run>> = Arc::default();
        let (on_load, on_request) = (state.clone(), state.clone());
        let (lease_dir, lease_pair) = (dir.clone(), pair.clone());
        let app = builder
            .register_uri_scheme_protocol("selftest", move |_ctx, request| {
                let path = request.uri().path().to_string();
                let mut status = 204;
                let mut body = Vec::new();
                let mut run = on_request.lock().unwrap();
                if let Some(nonce) = path.strip_prefix("/bind/") {
                    match bind_page(WINDOW, nonce) {
                        Ok(orphans) => release_once_settled(orphans),
                        Err(error) => {
                            status = 500;
                            violation(&mut run, format!("bind {nonce}: {error}"));
                        }
                    }
                    check(&mut run, &format!("bind {nonce}"));
                } else if let Some(nonce) = path.strip_prefix("/lease/") {
                    match acquire_at(&lease_dir, lease_pair.clone(), WINDOW, nonce) {
                        Ok(token) => {
                            run.holders.insert(token, nonce.to_string());
                            run.counts.leases_granted += 1;
                        }
                        Err(error) if error.starts_with("SYNC_RUN_PAGE_REPLACED") => {
                            status = 409;
                            run.counts.refused_replaced += 1;
                        }
                        Err(error) if error.starts_with("SYNC_RUN_BUSY") => {
                            status = 409;
                            run.counts.refused_busy += 1;
                        }
                        Err(error) => {
                            status = 500;
                            violation(&mut run, format!("lease {nonce}: {error}"));
                        }
                    }
                    check(&mut run, &format!("lease {nonce}"));
                } else if let Some(nonce) = path.strip_prefix("/alive/") {
                    run.alive = Some(nonce.to_string());
                } else {
                    status = 200;
                    body = PAGE.as_bytes().to_vec();
                }
                tauri::http::Response::builder()
                    .status(status)
                    .header("Content-Type", "text/html")
                    .body(body)
                    .unwrap()
            })
            .on_page_load(move |_webview, payload| {
                let finished = payload.event() == tauri::webview::PageLoadEvent::Finished;
                // The production handler, and the app's cleanup of what it
                // orphans.
                release_once_settled(page_load_event(WINDOW, finished));
                let mut run = on_load.lock().unwrap();
                run.seen.push(Seen {
                    finished,
                    url: payload.url().to_string(),
                });
                run.last_event = Some(Instant::now());
                check(&mut run, if finished { "Finished" } else { "Started" });
            })
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .map_err(|e| format!("the app did not build: {e}"))?;
        let page = |n: u32| -> tauri::Url { format!("{ORIGIN}/doc/{n}").parse().unwrap() };
        let window = tauri::WebviewWindowBuilder::new(
            &app,
            WINDOW,
            tauri::WebviewUrl::CustomProtocol(page(0)),
        )
        .build()
        .map_err(|e| format!("the window did not open: {e}"))?;
        let driver_state = state.clone();
        let driver_dir = dir.clone();
        let handle = app.handle().clone();
        let deadline = Instant::now() + budget;
        let driver = std::thread::spawn(move || -> Result<(), String> {
            let state = &*driver_state;
            // Until `done`, or the first violation, or the deadline.
            let wait = |what: &str, done: &dyn Fn() -> bool| -> Result<(), String> {
                loop {
                    if let Some(violation) = state.lock().unwrap().violation.clone() {
                        return Err(violation);
                    }
                    if done() {
                        return Ok(());
                    }
                    if Instant::now() > deadline {
                        return Err(format!("timed out waiting for {what}"));
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
            };
            // A page finished loading after the first `since` events, then
            // quiet for 400 ms: the engine has nothing more to report.
            let settle = |what: &str, since: usize| {
                wait(what, &|| {
                    let run = state.lock().unwrap();
                    run.seen.iter().skip(since).any(|event| event.finished)
                        && run
                            .last_event
                            .is_some_and(|at| at.elapsed() > Duration::from_millis(400))
                })
            };
            let events = || state.lock().unwrap().seen.len();
            // Ask the displayed document which one it is.
            let displayed = |what: &str| -> Result<String, String> {
                state.lock().unwrap().alive = None;
                window
                    .eval("fetch(location.origin + '/alive/' + window.__doc)")
                    .map_err(|e| e.to_string())?;
                wait(what, &|| state.lock().unwrap().alive.is_some())?;
                Ok(state.lock().unwrap().alive.clone().unwrap_or_default())
            };
            // The lease the displayed document was granted.
            let granted = |what: &str| -> Result<String, String> {
                let document = displayed(&format!("{what}: the displayed document"))?;
                wait(&format!("{what}: a lease granted to {document}"), &|| {
                    lease_of(state, &document).is_some()
                })?;
                lease_of(state, &document)
                    .ok_or_else(|| format!("{what}: {document} lost its lease"))
            };
            settle("the first page", 0)?;
            for round in 0..rounds {
                let at = format!("round {round}");
                // (a) A granted lease, before the navigations.
                let token = granted(&at)?;
                // (c) Native work it admitted, held across the navigations.
                let work = admit(Some(&token))
                    .map_err(|e| format!("{at}: the granted lease admitted no work: {e}"))?;
                let busy_before = state.lock().unwrap().counts.refused_busy;
                let since = events();
                // Two navigations back to back, then a reload over them.
                window
                    .navigate(page(2 * round + 1))
                    .map_err(|e| e.to_string())?;
                window
                    .navigate(page(2 * round + 2))
                    .map_err(|e| e.to_string())?;
                window.reload().map_err(|e| e.to_string())?;
                settle(&at, since)?;
                if !held_by_orphan(&token, &pair) {
                    return Err(format!(
                        "{at}: the old lease is not an orphan keeping the pair while its work runs"
                    ));
                }
                if lock_file_is_free(&driver_dir, &pair) {
                    return Err(format!(
                        "{at}: the pair's lock file was free while the work ran"
                    ));
                }
                wait(
                    &format!("{at}: the new document refused the busy pair"),
                    &|| state.lock().unwrap().counts.refused_busy > busy_before,
                )?;
                if !held_by_orphan(&token, &pair) {
                    return Err(format!(
                        "{at}: the orphan let the pair go before its work settled"
                    ));
                }
                state.lock().unwrap().counts.orphans_held += 1;
                drop(work);
                wait(
                    &format!("{at}: the orphan released once its work settled"),
                    &|| {
                        REGISTRY
                            .lock()
                            .is_ok_and(|registry| !registry.orphans.contains_key(&token))
                    },
                )?;
                state.lock().unwrap().counts.orphans_released += 1;
            }
            // (a) The displayed replacement document is granted the pair.
            granted("after the last round")?;
            Ok(())
        });
        let exit = handle.clone();
        let watcher = std::thread::spawn(move || {
            let outcome = driver
                .join()
                .unwrap_or_else(|_| Err("the driver panicked".into()));
            exit.exit(0);
            outcome
        });
        app.run_return(|_, _| {});
        let driven = watcher
            .join()
            .unwrap_or_else(|_| Err("the watcher panicked".into()));
        let run = std::mem::take(&mut *state.lock().unwrap());
        // The document bound now and the documents holding a live lease, read
        // before the registry is left as found (this window's leases and
        // orphans gone).
        let (bound, lessees) = match REGISTRY.lock() {
            Ok(mut registry) => {
                let bound = registry
                    .bindings
                    .get(WINDOW)
                    .map(|(_, nonce)| nonce.clone());
                let lessees: Vec<String> = registry
                    .leases
                    .iter()
                    .filter(|(_, lease)| lease.window == WINDOW)
                    .filter_map(|(token, _)| run.holders.get(token).cloned())
                    .collect();
                orphan_leases_of(&mut registry, WINDOW);
                registry.orphans.retain(|_, lease| lease.window != WINDOW);
                registry.bindings.remove(WINDOW);
                (bound, lessees)
            }
            Err(_) => (None, Vec::new()),
        };
        let _ = std::fs::remove_dir_all(&dir);
        let counts = run.counts;
        let failed =
            |error: String| -> Result<Report, String> { Err(format!("{error}; {counts}")) };
        if let Err(error) = driven {
            return failed(error);
        }
        if let Some(violation) = run.violation {
            return failed(violation);
        }
        let Some(alive) = run.alive else {
            return failed("the displayed document never answered".into());
        };
        if bound.as_deref() != Some(alive.as_str()) {
            return failed(format!(
                "the displayed document {alive} is not the bound one ({bound:?}) after {} events",
                run.seen.len()
            ));
        }
        if lessees != [alive.clone()] {
            return failed(format!(
                "the displayed document {alive} does not alone hold the pair: {lessees:?}"
            ));
        }
        Ok(Report {
            seen: run.seen,
            counts,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The nonce a test's document of `window` binds: one per window, so
    /// tests sharing a window agree on it and never take it from each other.
    fn test_nonce(window: &str) -> String {
        let cleaned: String = window
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        format!("test-document-{cleaned}")
    }

    /// A registration from the current document of `window`: its nonce is
    /// chosen and bound in one call under the registry lock ([`bind_page`],
    /// idempotent for the same nonce), then the lease is asked for.
    fn acquire_now(dir: &Path, pair: String, window: &str) -> Result<String, String> {
        let nonce = test_nonce(window);
        bind_page(window, &nonce)?;
        acquire_at(dir, pair, window, &nonce)
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

    fn refused_as(result: &Result<String, String>, marker: &str) -> bool {
        result.as_ref().is_err_and(|e| e.starts_with(marker))
    }

    fn nonce(label: &str) -> String {
        format!("{label}-{}", uuid::Uuid::new_v4())
    }

    /// A document binds at its start and owns its window: its begins are
    /// accepted, another document's (never bound) are refused; a begin never
    /// binds by itself; after a reload's `Started` the old binding is gone.
    #[test]
    fn a_page_owns_its_window_from_its_binding_until_the_next_load() {
        let dir = tempfile::tempdir().unwrap();
        let pair = crate::sync::index_filename("/bind-local", "/bind-remote");
        let window = "bind-window";
        let old = nonce("old");
        bind_page(window, &old).unwrap();
        let token = acquire_at(dir.path(), pair.clone(), window, &old).unwrap();
        release(&token).unwrap();
        assert!(refused_as(
            &acquire_at(dir.path(), pair.clone(), window, &nonce("stranger")),
            "SYNC_RUN_PAGE_REPLACED"
        ));
        orphan_window(window);
        assert!(refused_as(
            &acquire_at(dir.path(), pair.clone(), window, &old),
            "SYNC_RUN_PAGE_REPLACED"
        ));
        let new = nonce("new");
        bind_page(window, &new).unwrap();
        let token = acquire_at(dir.path(), pair, window, &new).unwrap();
        release(&token).unwrap();
    }

    /// The orders of a reload, each with its outcome:
    /// - an old document's delayed binding handled before the new one binds:
    ///   the first `Finished` ends it, the new document binds and runs;
    /// - the new document binds before `Finished`: `Finished` ends its
    ///   binding, its begin is refused, it binds again (the runner's retry)
    ///   and runs;
    /// - an old document still alive between `Started` and `Finished` takes a
    ///   lease: the new document's binding orphans it (T6a);
    /// - an old document's delayed binding handled after the new document
    ///   took a lease: it orphans the new lease (availability only), the new
    ///   document binds again and runs once the work settled; at no moment do
    ///   two documents hold admitted work on the pair;
    /// - a later, spurious `Finished` of the same generation changes nothing.
    #[test]
    fn every_order_of_a_reload_leaves_the_window_to_the_live_document() {
        let dir = tempfile::tempdir().unwrap();
        let pair = crate::sync::index_filename("/orders-local", "/orders-remote");
        let window = "orders-window";
        let (old, new) = (nonce("old"), nonce("new"));
        bind_page(window, &old).unwrap();

        // The reload starts; the old document is still alive and takes a lease.
        orphan_window(window);
        bind_page(window, &old).unwrap();
        let stale = acquire_at(dir.path(), pair.clone(), window, &old).unwrap();
        // The new document binds before `Finished`: the old lease is an
        // orphan from here, and admits nothing more.
        let orphans = bind_page(window, &new).unwrap();
        assert_eq!(orphans, std::slice::from_ref(&stale));
        assert!(
            admit(Some(&stale)).is_err(),
            "the old document's lease admits nothing"
        );
        // Until its work settled, the pair is still locked: no second owner.
        assert!(acquire_at(dir.path(), pair.clone(), window, &new).is_err());
        release_orphans(&orphans);
        // `Finished`: nothing left to orphan; it ends the new binding.
        assert!(page_finished(window).is_empty());
        // The new document's begin is refused until it binds again.
        assert!(refused_as(
            &acquire_at(dir.path(), pair.clone(), window, &new),
            "SYNC_RUN_PAGE_REPLACED"
        ));
        bind_page(window, &new).unwrap();
        let live = acquire_at(dir.path(), pair.clone(), window, &new).unwrap();
        // A spurious second `Finished` of the same generation does nothing.
        assert!(page_finished(window).is_empty());
        assert!(admit(Some(&live)).is_ok(), "the live sync goes on");
        // A delayed binding of the dead old document lands now: the live
        // lease is orphaned (its run stops: no command of it is admitted),
        // and the dead document holds nothing. At no moment do two documents
        // hold admitted work on the pair: the live one's admission ends here,
        // and the pair stays locked until its work settled.
        let orphans = bind_page(window, &old).unwrap();
        assert_eq!(orphans, std::slice::from_ref(&live));
        assert!(admit(Some(&live)).is_err());
        assert!(acquire_at(dir.path(), pair.clone(), window, &old).is_err());
        release_orphans(&orphans);
        // The live document binds again (taking nothing from the dead one)
        // and runs.
        assert!(bind_page(window, &new).unwrap().is_empty());
        let live = acquire_at(dir.path(), pair.clone(), window, &new).unwrap();
        release(&live).unwrap();
        // With nothing held, a delayed old binding may land; the live document
        // is refused once, binds again and runs.
        bind_page(window, &old).unwrap();
        assert!(refused_as(
            &acquire_at(dir.path(), pair.clone(), window, &new),
            "SYNC_RUN_PAGE_REPLACED"
        ));
        bind_page(window, &new).unwrap();
        let token = acquire_at(dir.path(), pair, window, &new).unwrap();
        release(&token).unwrap();
    }

    /// T6a: the first `Finished` of a generation was spurious, and the old
    /// document, still alive, binds and takes a lease after it; then it dies
    /// and its real `Finished` is not acted on (already had). The new
    /// document's binding takes the window over as a reload would: the old
    /// lease is orphaned, the pair stays locked until the old document's work
    /// has settled, and the new document then acquires it with no reload.
    #[test]
    fn a_new_document_takes_the_window_over_after_a_spurious_finished() {
        let dir = tempfile::tempdir().unwrap();
        let pair = crate::sync::index_filename("/residual-local", "/residual-remote");
        let window = "residual-window";
        let (old, new) = (nonce("old"), nonce("new"));
        orphan_window(window);
        assert!(
            page_finished(window).is_empty(),
            "spurious, nothing to orphan"
        );
        bind_page(window, &old).unwrap();
        let stale = acquire_at(dir.path(), pair.clone(), window, &old).unwrap();
        // The real `Finished` of this generation is not acted on: already had.
        assert!(page_finished(window).is_empty());
        // The new document binds: the old lease is orphaned.
        let orphans = bind_page(window, &new).unwrap();
        assert_eq!(orphans, std::slice::from_ref(&stale));
        assert!(admit(Some(&stale)).is_err());
        // Its work not settled yet: the pair is still the old document's.
        assert!(acquire_at(dir.path(), pair.clone(), window, &new).is_err());
        // Settled: the new document acquires the pair, with no reload.
        release_orphans(&orphans);
        let token = acquire_at(dir.path(), pair, window, &new).unwrap();
        release(&token).unwrap();
    }

    /// U11 (b): the page-load self-test on WebView2, the engine of every
    /// Windows install, through the production page-load handler. See
    /// [`page_load_selftest`] for what it proves and what it cannot.
    #[cfg(windows)]
    #[test]
    fn webview2_page_loads_keep_one_document_with_work_and_bind_the_live_one() {
        page_load_selftest_on(tauri::Builder::default().any_thread(), "WebView2");
    }

    /// The same on WebKitGTK, on demand: it opens a window, so it needs a
    /// desktop session (`cargo test --lib -- --ignored webkitgtk_page_loads`).
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "opens a window: needs a desktop session"]
    fn webkitgtk_page_loads_keep_one_document_with_work_and_bind_the_live_one() {
        page_load_selftest_on(tauri::Builder::default().any_thread(), "WebKitGTK");
    }

    /// V2: every round must have granted the displayed document its lease,
    /// held an orphan by admitted work across the navigations, and released
    /// it after; the counts are printed for the record.
    #[cfg(any(windows, target_os = "linux"))]
    fn page_load_selftest_on(builder: tauri::Builder<tauri::Wry>, engine: &str) {
        const ROUNDS: u32 = 25;
        let report = page_load_selftest::run(builder, ROUNDS, std::time::Duration::from_secs(180))
            .unwrap_or_else(|error| panic!("{engine} page-load self-test: {error}"));
        let counts = &report.counts;
        println!(
            "{engine} page-load self-test: {} events; {counts}",
            report.seen.len()
        );
        assert!(
            report.seen.iter().filter(|event| event.finished).count() >= ROUNDS as usize,
            "fewer page loads than rounds: {:?}",
            report.seen
        );
        assert!(counts.leases_granted > ROUNDS, "{counts}");
        assert!(counts.refused_busy >= ROUNDS, "{counts}");
        assert_eq!(
            (counts.orphans_held, counts.orphans_released),
            (ROUNDS, ROUNDS),
            "{counts}"
        );
    }

    /// U11: the command that binds is dropped while its bind waits (the
    /// registry is busy): the bind still runs, takes the window over, and
    /// its orphan is still released once the old work settles. The pair is
    /// then the new document's.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_abandoned_bind_still_releases_the_orphans_it_makes() {
        let dir = tempfile::tempdir().unwrap();
        let pair = crate::sync::index_filename("/abandoned-local", "/abandoned-remote");
        // Its own window: `an_abandoned_copy_keeps_its_admission_until_the_copy_ends`
        // orphans "abandoned-window" while it runs.
        let window = "abandoned-bind-window";
        let (old, new) = (nonce("old"), nonce("new"));
        bind_page(window, &old).unwrap();
        let stale = acquire_at(dir.path(), pair.clone(), window, &old).unwrap();
        let settled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = settled.clone();
        // The registry is busy (held on a thread of its own): the bind waits
        // on its worker.
        let (locked_tx, locked_rx) = std::sync::mpsc::channel();
        let (free_tx, free_rx) = std::sync::mpsc::channel::<()>();
        let holder = std::thread::spawn(move || {
            let _busy = REGISTRY.lock().unwrap();
            locked_tx.send(()).unwrap();
            let _ = free_rx.recv();
        });
        locked_rx.recv().unwrap();
        let command = bind_page_and_schedule_cleanup(window.into(), new.clone(), move || {
            flag.load(std::sync::atomic::Ordering::SeqCst)
        });
        // Polled, then dropped: the page that sent it went away.
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), command)
                .await
                .is_err()
        );
        free_tx.send(()).unwrap();
        holder.join().unwrap();
        // The bind ran: the old lease is orphaned, the pair still locked.
        // Generous: the cleanup polls every 250 ms on the shared async
        // runtime, which a full parallel test run keeps busy.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while admit(Some(&stale)).is_ok() {
            assert!(std::time::Instant::now() < deadline, "the bind never ran");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(acquire_at(dir.path(), pair.clone(), window, &new).is_err());
        // The old work settles: the cleanup the bind scheduled releases it.
        settled.store(true, std::sync::atomic::Ordering::SeqCst);
        let token = loop {
            let attempt = acquire_at(dir.path(), pair.clone(), window, &new);
            if let Ok(token) = attempt {
                break token;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the orphan was never released: {attempt:?}"
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        };
        release(&token).unwrap();
    }

    /// The registration runs on a blocking worker after the command began. A
    /// reload in that gap is a `Started` under the same lock: the late
    /// registration of the document it replaced is refused before any folder
    /// lock, so the new page's run gets the pair. A malformed or missing
    /// nonce is refused before any folder is touched.
    #[test]
    fn a_lease_requested_by_a_page_replaced_before_registration_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let pair = crate::sync::index_filename("/epoch-local", "/epoch-remote");
        let window = "epoch-window";
        let page = nonce("page");
        bind_page(window, &page).unwrap();
        orphan_window(window);
        assert!(refused_as(
            &acquire_at(dir.path(), pair.clone(), window, &page),
            "SYNC_RUN_PAGE_REPLACED"
        ));
        for bad in ["", "short", "has a space in it ok", &"x".repeat(129)] {
            assert!(bind_page(window, bad).is_err());
        }
        assert!(refused_as(
            &acquire_for_page("/epoch-local", "/epoch-remote", window, None),
            "SYNC_RUN_PAGE_REPLACED"
        ));
        let replacement = acquire_now(dir.path(), pair, window).unwrap();
        assert!(admit(Some(&replacement)).is_ok());
        release(&replacement).unwrap();
    }

    /// C13: tests that share a window choose and bind its nonce atomically:
    /// many registrations at once, on different pairs, all succeed.
    #[test]
    fn concurrent_test_registrations_on_one_window_do_not_conflict() {
        let dir = std::sync::Arc::new(tempfile::tempdir().unwrap());
        let handles: Vec<_> = (0..8)
            .map(|n| {
                let dir = dir.clone();
                std::thread::spawn(move || {
                    let pair = crate::sync::index_filename(&format!("/concurrent-{n}"), "/remote");
                    acquire_now(dir.path(), pair, "concurrent-window")
                })
            })
            .collect();
        for handle in handles {
            let token = handle.join().unwrap().expect("no nonce race");
            release(&token).unwrap();
        }
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

    /// T6b: the legacy `upload_file` runs its upload in a task that owns the
    /// admission (`run_owning_admission`). The command's future is dropped
    /// while the upload still runs: the lost page's pair stays locked until
    /// the upload ends, then the replacement acquires it.
    #[tokio::test]
    async fn an_abandoned_upload_keeps_its_admission_until_the_upload_ends() {
        let source = include_str!("lib.rs");
        let start = source.find("\nasync fn upload_file(").unwrap();
        let body = &source[start..start + source[start..].find("\n}\n").unwrap()];
        assert!(body.contains("let native_work = crate::sync_run_lease::admit("));
        assert!(body.contains("run_owning_admission(native_work, async move {"));

        let dir = tempfile::tempdir().unwrap();
        let pair = crate::sync::index_filename("/abandoned-upload", "/abandoned-upload-right");
        let lost = acquire_now(dir.path(), pair.clone(), "abandoned-upload-window").unwrap();
        let (finish, finished) = tokio::sync::oneshot::channel::<()>();
        let command = tokio::spawn(crate::run_owning_admission(
            admit(Some(&lost)).unwrap(),
            async move {
                let _ = finished.await;
            },
        ));
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        // The command's future is dropped; its upload still runs.
        command.abort();
        let _ = command.await;
        let orphans = orphan_window("abandoned-upload-window");
        assert_eq!(orphans, std::slice::from_ref(&lost));
        let waiter = tokio::spawn(release_orphans_once_settled(orphans, native_work_settled));
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        assert!(
            !waiter.is_finished(),
            "the abandoned upload still runs and still counts"
        );
        finish.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(30), waiter)
            .await
            .expect("released once the upload ended")
            .unwrap();
        let replacement = acquire_now(dir.path(), pair, "new-upload-window").unwrap();
        release(&replacement).unwrap();
    }

    /// The copy runs on the blocking pool and owns the admission: dropping
    /// the command's future (the task awaiting it is aborted) does not end the
    /// count while the copy itself still runs, so a lost page's pair stays
    /// locked until the mutation really ends.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_abandoned_copy_keeps_its_admission_until_the_copy_ends() {
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
        let pair = crate::sync::index_filename("/abandoned-copy", "/abandoned-copy-right");
        let lost = acquire_now(dir.path(), pair.clone(), "abandoned-window").unwrap();
        let source = dir.path().join("source.fifo");
        let c_source = std::ffi::CString::new(source.to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c_source.as_ptr(), 0o600) }, 0);
        let unblock = Unblock(source.clone());
        let destination = dir.path().join("copied.bin");
        let copy = tokio::spawn(crate::copy_local_file_with(
            std::sync::Arc::default(),
            source.to_string_lossy().into_owned(),
            destination.to_string_lossy().into_owned(),
            None,
            None,
        ));
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        // The future awaiting the copy is dropped; the copy is still blocked.
        copy.abort();
        let _ = copy.await;
        let orphans = orphan_window("abandoned-window");
        assert_eq!(orphans, std::slice::from_ref(&lost));
        let waiter = tokio::spawn(release_orphans_once_settled(orphans, native_work_settled));
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        assert!(
            !waiter.is_finished(),
            "the abandoned copy still runs and still counts"
        );
        // A writer appears: the copy's open returns and the count ends.
        tokio::task::spawn_blocking(move || {
            let _writer = std::fs::OpenOptions::new().write(true).open(&source);
        })
        .await
        .unwrap();
        drop(unblock);
        tokio::time::timeout(std::time::Duration::from_secs(30), waiter)
            .await
            .expect("released once the copy ended")
            .unwrap();
        let replacement = acquire_now(dir.path(), pair, "new-window").unwrap();
        release(&replacement).unwrap();
    }

    /// A Stop during a long local copy: the copy observes the flag between
    /// chunks, returns the native cancellation at once, and leaves neither a
    /// partial file under the final name nor its temporary. Through the
    /// command, a Stop already raised returns before anything is written, the
    /// run's own admission ends with it, and the run's normal end releases
    /// the pair for the next run.
    #[test]
    fn a_stopped_local_copy_returns_promptly_and_leaves_no_partial_file() {
        use std::sync::atomic::{AtomicBool, Ordering};
        struct StopAfterFirstChunk<'a> {
            cancel: &'a AtomicBool,
            reads: usize,
        }
        impl std::io::Read for StopAfterFirstChunk<'_> {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                self.reads += 1;
                if self.reads == 2 {
                    // The user presses Stop while the copy is in flight.
                    self.cancel.store(true, Ordering::SeqCst);
                }
                buf.fill(7);
                Ok(buf.len())
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("copied.bin");
        let cancel = AtomicBool::new(false);
        let mut endless = StopAfterFirstChunk {
            cancel: &cancel,
            reads: 0,
        };
        // A FILE's permissions, as production passes the source's: on Windows
        // a directory's carry FILE_ATTRIBUTE_DIRECTORY, which `set_permissions`
        // on the temporary's handle refuses with ERROR_INVALID_PARAMETER (os
        // error 87). Outside `dir`, which must end empty.
        let probe = tempfile::NamedTempFile::new().unwrap();
        let permissions = probe.as_file().metadata().unwrap().permissions();
        let started = std::time::Instant::now();
        let copied = crate::copy_into_place(&mut endless, permissions, &destination, &cancel);
        assert!(
            matches!(copied, Err(crate::sync::IpcError::Cancelled)),
            "{copied:?}"
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        assert_eq!(endless.reads, 2, "stopped at the next chunk");
        assert!(
            !destination.exists(),
            "no partial file under the final name"
        );
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            0,
            "the temporary is removed"
        );

        // Through the command, with the run's lease.
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let pair = crate::sync::index_filename("/stopped-copy", "/stopped-copy-right");
        let token = acquire_now(dir.path(), pair.clone(), "stopped-copy-window").unwrap();
        let source = dir.path().join("source.bin");
        std::fs::write(&source, vec![1u8; 3 << 20]).unwrap();
        let stopped = std::sync::Arc::new(AtomicBool::new(true));
        let returned = runtime.block_on(crate::copy_local_file_with(
            stopped,
            source.to_string_lossy().into_owned(),
            destination.to_string_lossy().into_owned(),
            None,
            Some(token.clone()),
        ));
        let message = returned.unwrap_err();
        assert!(
            message.starts_with("Transfer cancelled by user"),
            "{message}"
        );
        assert!(message.contains("[sync-error:"), "{message}");
        assert!(!destination.exists());
        // The run's own end releases the pair; the next run takes it.
        release(&token).unwrap();
        let next = acquire_now(dir.path(), pair, "stopped-copy-window").unwrap();
        release(&next).unwrap();
        // Without a Stop the same command copies the whole file.
        runtime
            .block_on(crate::copy_local_file_with(
                std::sync::Arc::default(),
                source.to_string_lossy().into_owned(),
                destination.to_string_lossy().into_owned(),
                None,
                None,
            ))
            .unwrap();
        assert_eq!(std::fs::read(&destination).unwrap().len(), 3 << 20);
    }

    /// D3: a Stop raised while the source returns its end (after the last
    /// chunk-boundary check): the copy is not published, the destination keeps
    /// its old content, and the temporary is removed.
    #[test]
    fn a_stop_at_the_end_of_the_source_does_not_publish_the_copy() {
        use std::sync::atomic::{AtomicBool, Ordering};
        struct StopAtEof<'a> {
            cancel: &'a AtomicBool,
            sent: bool,
        }
        impl std::io::Read for StopAtEof<'_> {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                if self.sent {
                    self.cancel.store(true, Ordering::SeqCst);
                    return Ok(0);
                }
                self.sent = true;
                buf[..3].copy_from_slice(b"new");
                Ok(3)
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("kept.txt");
        std::fs::write(&destination, b"old content").unwrap();
        let cancel = AtomicBool::new(false);
        let copied = crate::copy_into_place(
            &mut StopAtEof {
                cancel: &cancel,
                sent: false,
            },
            std::fs::metadata(&destination).unwrap().permissions(),
            &destination,
            &cancel,
        );
        assert!(
            matches!(copied, Err(crate::sync::IpcError::Cancelled)),
            "{copied:?}"
        );
        assert_eq!(std::fs::read(&destination).unwrap(), b"old content");
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            1,
            "no temporary left"
        );
    }

    /// While the data of a private source arrives, the temporary is owner-only
    /// (never the umask's 0644), and it carries no part of the destination's
    /// name: a name near the filesystem's 255-byte limit still copies.
    #[cfg(unix)]
    #[test]
    fn a_local_copy_temporary_is_private_and_fits_any_valid_name() {
        use std::os::unix::fs::PermissionsExt;
        use std::sync::atomic::AtomicBool;
        struct Inspecting<'a> {
            dir: &'a std::path::Path,
            reads: usize,
            modes: Vec<u32>,
        }
        impl std::io::Read for Inspecting<'_> {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                self.reads += 1;
                for entry in std::fs::read_dir(self.dir).unwrap() {
                    let entry = entry.unwrap();
                    if entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".aeroftp-copy-")
                    {
                        self.modes
                            .push(entry.metadata().unwrap().permissions().mode() & 0o777);
                    }
                }
                if self.reads > 3 {
                    return Ok(0);
                }
                buf[..4].copy_from_slice(b"data");
                Ok(4)
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let name = format!("{}.txt", "n".repeat(250));
        assert_eq!(name.len(), 254);
        let destination = dir.path().join(&name);
        let mut source = Inspecting {
            dir: dir.path(),
            reads: 0,
            modes: Vec::new(),
        };
        crate::copy_into_place(
            &mut source,
            std::fs::Permissions::from_mode(0o600),
            &destination,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(
            !source.modes.is_empty(),
            "the temporary was seen while written"
        );
        assert!(
            source.modes.iter().all(|mode| *mode == 0o600),
            "{:?}",
            source.modes
        );
        assert_eq!(std::fs::read(&destination).unwrap(), b"datadatadata");
        assert_eq!(
            std::fs::metadata(&destination)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
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
        let copy = tokio::spawn(crate::copy_local_file_with(
            std::sync::Arc::default(),
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
