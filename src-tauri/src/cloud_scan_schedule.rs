// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! What AeroCloud's background worker remembers between cycles: when each
//! pair last had its remote folder scanned in full and how soon its provider
//! allows the next scan, and the local changes the watcher reported that no
//! cycle has synced yet.
//!
//! A full scan lists the whole remote folder. It runs at startup (when
//! configured), on "Sync now", at the scheduled interval, and as a fallback
//! when the watcher lost events or a change is too large to sync path by
//! path. The scheduled interval of a pair is the larger of the user's
//! interval and its provider's floor
//! ([`crate::providers::StorageProvider::min_full_scan_interval`]); the
//! floor holds the fallback back too. Between scans the watcher's paths are
//! synced one by one ([`crate::cloud_service::prepare_incremental`]).

use crate::cloud_config::CloudConfig;
use crate::cloud_service::{IncrementalHints, LocalChanges, ProviderScanTraits};
use crate::file_watcher::WatcherEvent;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// The interval between two scheduled full scans of a pair: the larger of
/// the user's interval and the provider's floor.
pub fn full_scan_interval(user_interval: Duration, floor: Option<Duration>) -> Duration {
    floor.map_or(user_interval, |floor| user_interval.max(floor))
}

/// A pair's identity for the scheduler: the paths its baseline is keyed by,
/// so the same folder pair is the same entry whichever list it came from.
pub fn pair_key(config: &CloudConfig) -> String {
    format!(
        "{}|{}",
        config.local_folder.to_string_lossy(),
        config.remote_folder
    )
}

#[derive(Debug, Default, Clone)]
struct PairScanState {
    /// When the last full scan started (attempted, whatever its outcome).
    last_full_scan: Option<Instant>,
    /// What the provider said the last time a cycle connected.
    traits: Option<ProviderScanTraits>,
    /// Why a full scan was asked for and has not run yet.
    pending: Option<String>,
}

impl PairScanState {
    fn floor(&self) -> Option<Duration> {
        self.traits.and_then(|traits| traits.min_full_scan_interval)
    }
}

/// When each pair may have its next full scan.
#[derive(Debug, Default)]
pub struct ScanLedger {
    pairs: HashMap<String, PairScanState>,
}

impl ScanLedger {
    /// Whether a scheduled tick scans the pair in full: never scanned, or
    /// scanned at least [`full_scan_interval`] ago. A floor not learned yet
    /// (no cycle connected) holds nothing back; the first connection learns it.
    pub fn scheduled_full_scan_due(
        &self,
        key: &str,
        now: Instant,
        user_interval: Duration,
    ) -> bool {
        let Some(state) = self.pairs.get(key) else {
            return true;
        };
        state.last_full_scan.is_none_or(|last| {
            now.saturating_duration_since(last) >= full_scan_interval(user_interval, state.floor())
        })
    }

    /// Whether a full scan asked for outside the schedule (the watcher lost
    /// events, a change too large) may run now: only the provider's floor
    /// holds it back, not the user's interval.
    pub fn full_scan_allowed(&self, key: &str, now: Instant) -> bool {
        let Some(state) = self.pairs.get(key) else {
            return true;
        };
        match (state.last_full_scan, state.floor()) {
            (Some(last), Some(floor)) => now.saturating_duration_since(last) >= floor,
            _ => true,
        }
    }

    /// Whether a full scan was asked for and may run now.
    pub fn pending_full_scan_due(&self, key: &str, now: Instant) -> bool {
        self.pairs
            .get(key)
            .is_some_and(|state| state.pending.is_some())
            && self.full_scan_allowed(key, now)
    }

    /// Whether any pair waits for a full scan it asked for.
    pub fn any_pending(&self) -> bool {
        self.pairs.values().any(|state| state.pending.is_some())
    }

    /// Ask for a full scan of the pair, to run as soon as its floor allows.
    /// The first reason is kept.
    pub fn request_full_scan(&mut self, key: &str, reason: &str) {
        let state = self.pairs.entry(key.to_string()).or_default();
        if state.pending.is_none() {
            tracing::info!("AeroCloud: full scan asked for {key}: {reason}");
            state.pending = Some(reason.to_string());
        }
    }

    /// A full scan of the pair started at `at`: it answers any request.
    pub fn record_full_scan(&mut self, key: &str, at: Instant) {
        let state = self.pairs.entry(key.to_string()).or_default();
        state.last_full_scan = Some(at);
        state.pending = None;
    }

    /// Remember what a connected provider said about scanning.
    pub fn learn(&mut self, key: &str, traits: ProviderScanTraits) {
        self.pairs.entry(key.to_string()).or_default().traits = Some(traits);
    }

    /// What the incremental pass of the pair may assume before connecting.
    pub fn hints(&self, key: &str, config: &CloudConfig) -> IncrementalHints {
        let exact_sizes = self
            .pairs
            .get(key)
            .and_then(|state| state.traits)
            .is_none_or(|traits| traits.exact_sizes);
        IncrementalHints {
            // The compress overlay always sizes on the wire.
            compare_size: exact_sizes && !config.compress_enabled,
        }
    }
}

/// The ledger the worker and "Sync now" share.
pub fn ledger() -> MutexGuard<'static, ScanLedger> {
    static LEDGER: LazyLock<Mutex<ScanLedger>> = LazyLock::new(Mutex::default);
    LEDGER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The most changed paths the backlog holds. Past it the change is the whole
/// tree, and the backlog reports an overflow (a full scan) instead.
const BACKLOG_PATH_CAP: usize = 100_000;

/// The first wait before a path whose sync failed is tried again; it doubles
/// with each failure up to [`RETRY_DELAY_CAP`].
const RETRY_DELAY_BASE: Duration = Duration::from_secs(30);
const RETRY_DELAY_CAP: Duration = Duration::from_secs(30 * 60);

/// The wait before the next try of a path that failed `attempts` times.
pub fn retry_delay(attempts: u32) -> Duration {
    RETRY_DELAY_BASE
        .saturating_mul(1u32 << attempts.saturating_sub(1).min(16))
        .min(RETRY_DELAY_CAP)
}

#[derive(Debug, Clone)]
struct Pending {
    /// How many cycles tried the path and failed.
    attempts: u32,
    /// Not handed out before this.
    not_before: Option<Instant>,
    /// Pairs that already covered this exact event generation.
    covered: HashSet<String>,
}

/// Local changes the watcher reported that no cycle has synced yet.
///
/// A path stays until every pair that holds it ran a cycle over it. One whose
/// sync failed for a reason worth retrying comes back after
/// [`retry_delay`]: the next watcher event names only its own paths, so a
/// path dropped on a failure would wait for the next full scan.
#[derive(Debug, Default)]
pub struct ChangeBacklog {
    paths: BTreeMap<PathBuf, Pending>,
    renames: BTreeMap<(PathBuf, PathBuf), HashSet<String>>,
}

/// Whether `path` is under `root` (or is it), the root also read with its
/// links resolved (FSEvents reports resolved paths).
fn under_root(path: &Path, root: &Path, resolved: Option<&Path>) -> bool {
    path.starts_with(root) || resolved.is_some_and(|resolved| path.starts_with(resolved))
}

fn resolved(root: &Path) -> Option<PathBuf> {
    root.canonicalize().ok().filter(|resolved| resolved != root)
}

impl ChangeBacklog {
    /// Take in a watcher event. Returns true when the event says changes were
    /// lost, or the backlog outgrew its cap (it is then emptied): only a full
    /// scan sees everything that changed.
    pub fn absorb(&mut self, event: WatcherEvent) -> bool {
        for path in event.paths {
            // A fresh change of a path that is waiting to be tried again
            // makes it ready now.
            self.paths.insert(
                path,
                Pending {
                    attempts: 0,
                    not_before: None,
                    covered: HashSet::new(),
                },
            );
        }
        for rename in event.renames {
            self.renames.insert(rename, HashSet::new());
        }
        if self.paths.len() > BACKLOG_PATH_CAP || self.renames.len() > BACKLOG_PATH_CAP {
            self.paths.clear();
            self.renames.clear();
            return true;
        }
        event.overflow
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty() && self.renames.is_empty()
    }

    /// The changes under `root` that are ready at `now`.
    pub fn changes_for(&self, pair: &str, root: &Path, now: Instant) -> LocalChanges {
        self.changes(Some(pair), root, now)
    }

    #[cfg(test)]
    fn changes_under(&self, root: &Path, now: Instant) -> LocalChanges {
        self.changes(None, root, now)
    }

    fn changes(&self, pair: Option<&str>, root: &Path, now: Instant) -> LocalChanges {
        let resolved = resolved(root);
        let ready = |path: &Path| {
            self.paths
                .get(path)
                .is_none_or(|pending| pending.not_before.is_none_or(|at| at <= now))
        };
        LocalChanges {
            paths: self
                .paths
                .iter()
                .filter(|(path, pending)| {
                    under_root(path, root, resolved.as_deref())
                        && ready(path)
                        && pair.is_none_or(|key| !pending.covered.contains(key))
                })
                .map(|(path, _)| path.clone())
                .collect(),
            renames: self
                .renames
                .iter()
                .filter(|((from, to), covered)| {
                    (under_root(from, root, resolved.as_deref())
                        || under_root(to, root, resolved.as_deref()))
                        && ready(from)
                        && ready(to)
                        && pair.is_none_or(|key| !covered.contains(key))
                })
                .map(|((from, to), _)| (from.clone(), to.clone()))
                .collect(),
        }
    }

    /// How long until the next path waiting to be tried again is ready,
    /// `None` when none waits.
    pub fn next_retry_in(&self, now: Instant) -> Option<Duration> {
        self.paths
            .values()
            .filter_map(|pending| pending.not_before)
            .map(|at| at.saturating_duration_since(now))
            .min()
    }

    /// Forget the changes the cycle synced. `roots` are the local folders of
    /// the enabled pairs and what each covered this cycle. A full scan also
    /// covers deferred retries, but only for its own pair: any overlapping
    /// idle pair or pass that has not read a retry keeps that change alive.
    /// A path no enabled pair holds is dropped. Renames are handed out once:
    /// a pass that does not replay one still syncs both of its paths.
    pub fn settle<K: AsRef<str>>(&mut self, roots: &[(K, &Path, PairJob)], now: Instant) {
        let roots: Vec<(&str, &Path, Option<PathBuf>, PairJob)> = roots
            .iter()
            .map(|(key, root, job)| (key.as_ref(), *root, resolved(root), *job))
            .collect();
        let active: HashSet<&str> = roots.iter().map(|(key, _, _, _)| *key).collect();
        self.paths.retain(|path, pending| {
            pending.covered.retain(|key| active.contains(key.as_str()));
            let ready = pending.not_before.is_none_or(|at| at <= now);
            let mut waits = false;
            for (key, root, resolved, job) in &roots {
                if under_root(path, root, resolved.as_deref()) {
                    if *job == PairJob::FullScan || (*job == PairJob::Changes && ready) {
                        pending.covered.insert((*key).to_string());
                    }
                    waits |= !pending.covered.contains(*key);
                }
            }
            waits
        });
        self.renames.retain(|(from, to), covered| {
            covered.retain(|key| active.contains(key.as_str()));
            let mut waits = false;
            for (key, root, resolved, job) in &roots {
                if under_root(from, root, resolved.as_deref())
                    || under_root(to, root, resolved.as_deref())
                {
                    if *job != PairJob::Idle {
                        covered.insert((*key).to_string());
                    }
                    waits |= !covered.contains(*key);
                }
            }
            waits
        });
    }

    /// Put back `paths` whose sync failed for a reason worth retrying: they
    /// are handed out again after [`retry_delay`].
    pub fn retry(&mut self, paths: impl IntoIterator<Item = PathBuf>, now: Instant) {
        for path in paths {
            let attempts = self
                .paths
                .get(&path)
                .map_or(0, |pending| pending.attempts)
                .saturating_add(1);
            self.paths.insert(
                path,
                Pending {
                    attempts,
                    not_before: Some(now + retry_delay(attempts)),
                    covered: HashSet::new(),
                },
            );
        }
    }
}

/// What one pair does in a cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairJob {
    /// List the whole remote folder.
    FullScan,
    /// Sync the watcher's paths under the pair's folder.
    Changes,
    /// Nothing.
    Idle,
}

/// What a pair does this cycle. `startup` scans every pair in full whatever
/// its floor (the user asked for it); a scheduled tick scans a pair whose
/// interval has elapsed; a full scan asked for runs once its floor allows,
/// like the watcher's own changes only once `watcher_ready` (the cooldown is
/// over and no folder transfer runs).
pub fn choose_pair_job(
    ledger: &ScanLedger,
    key: &str,
    now: Instant,
    user_interval: Duration,
    startup: bool,
    scheduled_tick: bool,
    watcher_ready: bool,
) -> PairJob {
    if startup
        || (scheduled_tick && ledger.scheduled_full_scan_due(key, now, user_interval))
        || (watcher_ready && ledger.pending_full_scan_due(key, now))
    {
        PairJob::FullScan
    } else if watcher_ready {
        PairJob::Changes
    } else {
        PairJob::Idle
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_watcher::WatcherEventKind;

    const HOUR: Duration = Duration::from_secs(3600);
    const FIVE_MIN: Duration = Duration::from_secs(300);

    fn traits(floor: Option<Duration>) -> ProviderScanTraits {
        ProviderScanTraits {
            min_full_scan_interval: floor,
            exact_sizes: true,
        }
    }

    #[test]
    fn the_interval_is_the_larger_of_the_users_and_the_floor() {
        assert_eq!(full_scan_interval(FIVE_MIN, None), FIVE_MIN);
        assert_eq!(full_scan_interval(FIVE_MIN, Some(HOUR)), HOUR);
        assert_eq!(full_scan_interval(2 * HOUR, Some(HOUR)), 2 * HOUR);
    }

    /// The scheduler of a pair whose provider declares an hour: a tick every
    /// five minutes scans it once an hour, while a pair without a floor is
    /// scanned at every tick.
    #[test]
    fn a_scheduled_tick_scans_a_floored_pair_only_once_its_floor_elapsed() {
        let start = Instant::now();
        let mut ledger = ScanLedger::default();
        assert!(
            ledger.scheduled_full_scan_due("p", start, FIVE_MIN),
            "never scanned"
        );
        ledger.record_full_scan("p", start);
        ledger.learn("p", traits(Some(HOUR)));
        ledger.record_full_scan("q", start);
        ledger.learn("q", traits(None));
        for minutes in [5, 30, 59] {
            let now = start + Duration::from_secs(minutes * 60);
            assert!(
                !ledger.scheduled_full_scan_due("p", now, FIVE_MIN),
                "{minutes} min"
            );
            assert!(
                ledger.scheduled_full_scan_due("q", now, FIVE_MIN),
                "{minutes} min"
            );
        }
        assert!(ledger.scheduled_full_scan_due("p", start + HOUR, FIVE_MIN));
        // The user's interval still wins when it is the larger.
        assert!(!ledger.scheduled_full_scan_due("p", start + HOUR, 2 * HOUR));
    }

    /// A full scan asked for outside the schedule waits for the floor only:
    /// not for the user's interval, and never sooner than the floor.
    #[test]
    fn a_requested_full_scan_waits_for_the_floor_and_not_the_users_interval() {
        let start = Instant::now();
        let mut ledger = ScanLedger::default();
        ledger.record_full_scan("p", start);
        ledger.learn("p", traits(Some(HOUR)));
        assert!(!ledger.pending_full_scan_due("p", start), "nothing asked");
        ledger.request_full_scan("p", "the watcher lost events");
        assert!(ledger.any_pending());
        assert!(!ledger.pending_full_scan_due("p", start + Duration::from_secs(600)));
        assert!(ledger.pending_full_scan_due("p", start + HOUR));

        ledger.record_full_scan("q", start);
        ledger.learn("q", traits(None));
        ledger.request_full_scan("q", "too many changes");
        assert!(
            ledger.pending_full_scan_due("q", start),
            "without a floor it runs at once, 24 h interval or not"
        );

        ledger.record_full_scan("p", start + HOUR);
        assert!(
            !ledger.pending_full_scan_due("p", start + 2 * HOUR),
            "answered"
        );
        assert!(ledger.any_pending(), "q still waits");
    }

    #[test]
    fn choose_pair_job_follows_the_ledger() {
        let start = Instant::now();
        let mut ledger = ScanLedger::default();
        ledger.record_full_scan("p", start);
        ledger.learn("p", traits(Some(HOUR)));
        let at = start + FIVE_MIN;
        let job = |startup, tick, ready, ledger: &ScanLedger| {
            choose_pair_job(ledger, "p", at, FIVE_MIN, startup, tick, ready)
        };
        assert_eq!(
            job(true, false, false, &ledger),
            PairJob::FullScan,
            "startup ignores the floor"
        );
        assert_eq!(
            job(false, true, true, &ledger),
            PairJob::Changes,
            "the floor holds the tick back"
        );
        assert_eq!(job(false, true, false, &ledger), PairJob::Idle);
        ledger.request_full_scan("p", "overflow");
        assert_eq!(
            job(false, false, true, &ledger),
            PairJob::Changes,
            "still inside the floor"
        );
        let later = start + HOUR;
        assert_eq!(
            choose_pair_job(&ledger, "p", later, FIVE_MIN, false, false, true),
            PairJob::FullScan
        );
        assert_eq!(
            choose_pair_job(&ledger, "p", later, FIVE_MIN, false, false, false),
            PairJob::Idle,
            "the cooldown holds a requested scan back like the changes"
        );
    }

    #[test]
    fn hints_read_sizes_only_when_the_provider_reports_them_exactly() {
        let mut ledger = ScanLedger::default();
        let config = CloudConfig::default();
        assert!(
            ledger.hints("p", &config).compare_size,
            "unknown reads as exact"
        );
        ledger.learn(
            "p",
            ProviderScanTraits {
                min_full_scan_interval: None,
                exact_sizes: false,
            },
        );
        assert!(!ledger.hints("p", &config).compare_size);
        let compressed = CloudConfig {
            compress_enabled: true,
            ..CloudConfig::default()
        };
        assert!(!ledger.hints("q", &compressed).compare_size);
    }

    fn watcher_event(paths: &[&str], overflow: bool) -> WatcherEvent {
        WatcherEvent {
            paths: paths.iter().map(PathBuf::from).collect(),
            renames: Vec::new(),
            overflow,
            kind: WatcherEventKind::Modified,
        }
    }

    #[test]
    fn the_backlog_routes_paths_to_the_pairs_that_hold_them() {
        let now = Instant::now();
        let mut backlog = ChangeBacklog::default();
        assert!(!backlog.absorb(watcher_event(
            &["/a/x.txt", "/b/y.txt", "/elsewhere/z"],
            false
        )));
        let a = backlog.changes_under(Path::new("/a"), now);
        assert_eq!(a.paths, vec![PathBuf::from("/a/x.txt")]);
        // `/ab` is not under `/a`.
        backlog.absorb(watcher_event(&["/ab/w"], false));
        assert_eq!(backlog.changes_under(Path::new("/a"), now).paths.len(), 1);

        // Pair /a ran, /b did not: /b's path waits, the others go.
        backlog.settle(
            &[
                ("a", Path::new("/a"), PairJob::Changes),
                ("b", Path::new("/b"), PairJob::Idle),
            ],
            now,
        );
        assert_eq!(
            backlog.changes_under(Path::new("/b"), now).paths,
            vec![PathBuf::from("/b/y.txt")]
        );
        assert!(backlog.changes_under(Path::new("/a"), now).paths.is_empty());
        assert!(backlog
            .changes_under(Path::new("/elsewhere"), now)
            .paths
            .is_empty());
    }

    /// The event set overflows: the kernel said so, or more paths changed
    /// than the backlog holds. Either way it reports it, for a full scan.
    #[test]
    fn an_overflowing_event_set_asks_for_a_full_scan() {
        let mut backlog = ChangeBacklog::default();
        assert!(backlog.absorb(watcher_event(&[], true)));
        let many: Vec<String> = (0..=BACKLOG_PATH_CAP).map(|i| format!("/a/{i}")).collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        assert!(backlog.absorb(watcher_event(&refs, false)));
        assert!(
            backlog.is_empty(),
            "emptied: the full scan reads the folder"
        );
    }

    #[test]
    fn a_failed_path_comes_back_after_a_growing_delay() {
        let now = Instant::now();
        let mut backlog = ChangeBacklog::default();
        backlog.absorb(watcher_event(&["/a/x"], false));
        backlog.settle(&[("a", Path::new("/a"), PairJob::Changes)], now);
        backlog.retry([PathBuf::from("/a/x")], now);
        assert!(backlog.changes_under(Path::new("/a"), now).paths.is_empty());
        assert_eq!(backlog.next_retry_in(now), Some(RETRY_DELAY_BASE));
        // A cycle that runs before the delay keeps it waiting.
        backlog.settle(&[("a", Path::new("/a"), PairJob::Changes)], now);
        let ready_at = now + RETRY_DELAY_BASE;
        assert_eq!(
            backlog.changes_under(Path::new("/a"), ready_at).paths.len(),
            1
        );
        backlog.retry([PathBuf::from("/a/x")], ready_at);
        assert_eq!(backlog.next_retry_in(ready_at), Some(2 * RETRY_DELAY_BASE));
        // A fresh change makes it ready at once.
        backlog.absorb(watcher_event(&["/a/x"], false));
        assert_eq!(
            backlog.changes_under(Path::new("/a"), ready_at).paths.len(),
            1
        );
        assert_eq!(retry_delay(30), RETRY_DELAY_CAP);
    }

    #[test]
    fn a_full_scan_clears_the_pairs_changes() {
        let now = Instant::now();
        let mut backlog = ChangeBacklog::default();
        backlog.absorb(watcher_event(&["/a/x", "/b/y"], false));
        backlog.retry([PathBuf::from("/a/z")], now);
        backlog.settle(
            &[
                ("a", Path::new("/a"), PairJob::FullScan),
                ("b", Path::new("/b"), PairJob::Idle),
            ],
            now,
        );
        assert!(backlog
            .changes_under(Path::new("/a"), now + HOUR)
            .paths
            .is_empty());
        assert_eq!(backlog.changes_under(Path::new("/b"), now).paths.len(), 1);
    }

    #[test]
    fn a_full_scan_does_not_discharge_an_overlapping_idle_pairs_changes() {
        let now = Instant::now();
        for other in ["/a", "/a/sub"] {
            let mut backlog = ChangeBacklog::default();
            backlog.absorb(watcher_event(&["/a/sub/x"], false));
            backlog.retry([PathBuf::from("/a/sub/later")], now);
            backlog.settle(
                &[
                    ("a", Path::new("/a"), PairJob::FullScan),
                    ("b", Path::new(other), PairJob::Idle),
                ],
                now,
            );
            assert_eq!(
                backlog.changes_under(Path::new(other), now).paths,
                vec![PathBuf::from("/a/sub/x")]
            );
            assert_eq!(
                backlog
                    .changes_under(Path::new(other), now + HOUR)
                    .paths
                    .len(),
                2
            );
            // Even a later full scan cannot clear another pair's retry that
            // its ordinary pass has not read yet.
            backlog.settle(
                &[
                    ("a", Path::new("/a"), PairJob::FullScan),
                    ("b", Path::new(other), PairJob::Changes),
                ],
                now,
            );
            assert_eq!(
                backlog.changes_under(Path::new(other), now + HOUR).paths,
                vec![PathBuf::from("/a/sub/later")]
            );
            backlog.settle(
                &[
                    ("a", Path::new("/a"), PairJob::Changes),
                    ("b", Path::new(other), PairJob::Changes),
                ],
                now + HOUR,
            );
            assert!(backlog.is_empty());
        }
    }

    #[test]
    fn staggered_full_scans_acknowledge_paths_and_renames_across_cycles() {
        let now = Instant::now();
        for other in ["/a", "/a/sub"] {
            let mut backlog = ChangeBacklog::default();
            let mut event = watcher_event(&["/a/sub/from", "/a/sub/to"], false);
            event
                .renames
                .push(("/a/sub/from".into(), "/a/sub/to".into()));
            backlog.absorb(event.clone());
            backlog.settle(
                &[
                    ("a", Path::new("/a"), PairJob::FullScan),
                    ("b", Path::new(other), PairJob::Idle),
                ],
                now,
            );
            assert!(backlog.changes_for("a", Path::new("/a"), now).is_empty());
            let pending = backlog.changes_for("b", Path::new(other), now);
            assert_eq!(pending.paths.len(), 2);
            assert_eq!(pending.renames.len(), 1);
            // A stays idle: its completed obligation must survive this turn.
            backlog.settle(
                &[
                    ("a", Path::new("/a"), PairJob::Idle),
                    ("b", Path::new(other), PairJob::FullScan),
                ],
                now,
            );
            assert!(backlog.is_empty());

            backlog.absorb(event.clone());
            backlog.settle(
                &[
                    ("a", Path::new("/a"), PairJob::FullScan),
                    ("b", Path::new(other), PairJob::Idle),
                ],
                now,
            );
            // A new generation of the same event must be seen by A again.
            backlog.absorb(event);
            let fresh = backlog.changes_for("a", Path::new("/a"), now);
            assert_eq!(fresh.paths.len(), 2);
            assert_eq!(fresh.renames.len(), 1);
            backlog.settle(
                &[
                    ("a", Path::new("/a"), PairJob::FullScan),
                    ("b", Path::new(other), PairJob::Idle),
                ],
                now,
            );
            // A removed pair no longer owns an obligation or an acknowledgement.
            backlog.settle(&[("b", Path::new(other), PairJob::Idle)], now);
            assert!(backlog
                .paths
                .values()
                .all(|pending| pending.covered.is_empty()));
            assert!(backlog.renames.values().all(HashSet::is_empty));
            backlog.settle(&[("b", Path::new(other), PairJob::FullScan)], now);
            assert!(backlog.is_empty());
        }
    }
}
