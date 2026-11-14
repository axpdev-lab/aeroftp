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

/// A pair's identity for the scheduler, its ledger and the change backlog:
/// the saved profile it syncs with and the paths its baseline is keyed by.
/// The profile is the id the config resolved to
/// ([`CloudConfig::resolved_profile_id`]), so a config naming a profile by
/// id and a pair naming the same one by name are one entry, whichever list
/// they came from. A config not resolved (no vault) is keyed by its pinned
/// id or else its name, each tagged, so a name equal to another profile's id
/// never shares its key. Paths alone were not enough: a placement can give a
/// `~/docs` pair on one server the paths of a `/home/u/docs` pair on another,
/// and a full scan of the first then cleared the second's pending recovery
/// scan and counted its watcher changes as covered.
pub fn pair_key(config: &CloudConfig) -> String {
    let pinned = config
        .resolved_profile_id
        .as_deref()
        .or(config.server_profile_id.as_deref())
        .filter(|id| !id.trim().is_empty());
    let server = match pinned {
        Some(id) => format!("id:{id}"),
        None => format!("name:{}", config.server_profile),
    };
    format!(
        "{server}\u{0}{}|{}",
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

    /// The pair keyed `from` connected and is now keyed `to`: a `~/...`
    /// remote folder placed under the login folder on this connect. What the
    /// ledger knew under the old key moves to the new one, so a request made
    /// before the placement is answered there, and nothing stays pending
    /// under a key no pair has any more.
    pub fn moved(&mut self, from: &str, to: &str) {
        if from == to {
            return;
        }
        let Some(old) = self.pairs.remove(from) else {
            return;
        };
        let state = self.pairs.entry(to.to_string()).or_default();
        state.last_full_scan = state.last_full_scan.max(old.last_full_scan);
        state.traits = state.traits.or(old.traits);
        state.pending = state.pending.take().or(old.pending);
    }

    /// As [`Self::moved`], for a key that another pair still uses: the state
    /// is merged into `to` and kept under `from`.
    pub fn copied(&mut self, from: &str, to: &str) {
        if from == to {
            return;
        }
        let Some(old) = self.pairs.get(from).cloned() else {
            return;
        };
        let state = self.pairs.entry(to.to_string()).or_default();
        state.last_full_scan = state.last_full_scan.max(old.last_full_scan);
        state.traits = state.traits.or(old.traits);
        state.pending = state.pending.take().or(old.pending);
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

    /// A pair whose `~/...` folder was placed on a connect moves to another
    /// key: what the ledger held under the old one follows it, a scan
    /// recorded after the move answers a request made before it, and the old
    /// key keeps nothing pending.
    #[test]
    fn a_placed_pair_takes_its_scan_state_to_the_new_key() {
        let now = Instant::now();
        let traits = ProviderScanTraits {
            min_full_scan_interval: Some(Duration::from_secs(1800)),
            exact_sizes: true,
        };
        let mut ledger = ScanLedger::default();
        ledger.learn("l|~/docs", traits);
        ledger.request_full_scan("l|~/docs", "the watcher lost events");
        ledger.moved("l|~/docs", "l|/home/u/docs");
        assert!(!ledger.pairs.contains_key("l|~/docs"));
        assert!(ledger.pending_full_scan_due("l|/home/u/docs", now));
        assert_eq!(ledger.pairs["l|/home/u/docs"].traits, Some(traits));
        ledger.record_full_scan("l|/home/u/docs", now);
        assert!(!ledger.any_pending());
        // The floor learned under the old key holds the next scan back.
        assert!(!ledger.full_scan_allowed("l|/home/u/docs", now + Duration::from_secs(60)));

        // Moving onto a key that knows more keeps the newer scan and its
        // own request; a key that did not change moves nothing.
        let mut ledger = ScanLedger::default();
        ledger.record_full_scan("l|~/a", now);
        ledger.record_full_scan("l|/h/a", now + Duration::from_secs(5));
        ledger.request_full_scan("l|/h/a", "kept");
        ledger.moved("l|~/a", "l|/h/a");
        assert_eq!(
            ledger.pairs["l|/h/a"].last_full_scan,
            Some(now + Duration::from_secs(5))
        );
        assert_eq!(ledger.pairs["l|/h/a"].pending.as_deref(), Some("kept"));
        ledger.moved("l|/h/a", "l|/h/a");
        assert!(ledger.pairs.contains_key("l|/h/a"));
        ledger.moved("l|missing", "l|/h/a");
        assert_eq!(ledger.pairs.len(), 1);
    }

    /// A key another pair still uses keeps its state when it is copied.
    #[test]
    fn a_copied_pair_keeps_its_scan_state_under_both_keys() {
        let mut ledger = ScanLedger::default();
        ledger.request_full_scan("l|~/docs", "the watcher lost events");
        ledger.copied("l|~/docs", "l|/home/u/docs");
        let now = Instant::now();
        assert!(ledger.pending_full_scan_due("l|~/docs", now));
        assert!(ledger.pending_full_scan_due("l|/home/u/docs", now));
        ledger.record_full_scan("l|/home/u/docs", now);
        assert!(ledger.pending_full_scan_due("l|~/docs", now));
    }

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

    /// A manual "Sync now" is the first scan of a pair: it records the scan
    /// (`trigger_cloud_sync`) and, on connecting, the provider's traits
    /// (`sync_one_config`). A pair whose provider declares a 30 minute floor
    /// then holds its scheduled scans back even though the user's interval is
    /// five minutes. Without the traits learned, the next tick scans again.
    #[test]
    fn a_manual_first_scan_holds_the_next_scheduled_scan_back_for_the_floor() {
        let half_hour = Duration::from_secs(30 * 60);
        let start = Instant::now();
        let mut learned = ScanLedger::default();
        learned.record_full_scan("p", start);
        learned.learn("p", traits(Some(half_hour)));
        for minutes in [5, 10, 29] {
            let now = start + Duration::from_secs(minutes * 60);
            assert!(
                !learned.scheduled_full_scan_due("p", now, FIVE_MIN),
                "{minutes} min after a manual scan"
            );
        }
        assert!(learned.scheduled_full_scan_due("p", start + half_hour, FIVE_MIN));

        // The same pair with its traits never learned scans again at once: the
        // gap the learn call in `sync_one_config` closes.
        let mut unlearned = ScanLedger::default();
        unlearned.record_full_scan("p", start);
        assert!(unlearned.scheduled_full_scan_due(
            "p",
            start + Duration::from_secs(5 * 60),
            FIVE_MIN
        ));
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

    /// Two pairs on `/local`, a `~/docs` one on server A and a `/home/u/docs`
    /// one on server B: A's placement gives it B's paths, never B's key. A
    /// full scan of A covers only A, and B, idle, still has its watcher change
    /// to sync.
    #[test]
    fn a_placed_pair_with_another_servers_paths_does_not_cover_its_changes() {
        let a = on_server("srv_a", "~/docs");
        let b = on_server("srv_b", "/home/u/docs");
        let placed_a = pair_key(&CloudConfig {
            remote_folder: "/home/u/docs".to_string(),
            ..a.clone()
        });
        assert_ne!(placed_a, pair_key(&b), "same paths, another server");
        let now = Instant::now();
        let mut backlog = ChangeBacklog::default();
        backlog.absorb(watcher_event(&["/local/f.txt"], false));
        let root = Path::new("/local");
        backlog.settle(
            &[
                (placed_a.clone(), root, PairJob::FullScan),
                (pair_key(&b), root, PairJob::Idle),
            ],
            now,
        );
        let left = backlog.changes_for(&pair_key(&b), root, now);
        assert_eq!(left.paths, vec![PathBuf::from("/local/f.txt")]);
        assert!(backlog.changes_for(&placed_a, root, now).is_empty());
    }

    /// The same two pairs in the ledger: B's recovery scan, deferred by its
    /// provider's floor, still waits after A's scan, which records A only.
    #[test]
    fn a_scan_on_one_server_leaves_another_servers_recovery_request() {
        let placed_a = CloudConfig {
            remote_folder: "/home/u/docs".to_string(),
            ..on_server("srv_a", "~/docs")
        };
        let b = on_server("srv_b", "/home/u/docs");
        let floor = Duration::from_secs(600);
        let start = Instant::now();
        let mut ledger = ScanLedger::default();
        ledger.learn(&pair_key(&b), traits(Some(floor)));
        ledger.record_full_scan(&pair_key(&b), start);
        ledger.request_full_scan(&pair_key(&b), "the watcher lost events");
        assert!(
            !ledger.pending_full_scan_due(&pair_key(&b), start),
            "the floor defers B's request"
        );
        ledger.record_full_scan(&pair_key(&placed_a), start + floor);
        assert!(
            ledger.pending_full_scan_due(&pair_key(&b), start + floor),
            "A's scan must not answer B's request"
        );
        ledger.record_full_scan(&pair_key(&b), start + floor);
        assert!(!ledger.pending_full_scan_due(&pair_key(&b), start + floor * 2));
    }

    /// The key is the profile a config selects, never how it names it: a
    /// config pinned by id and a pair naming the same profile by name share
    /// one key once resolved, two profiles with one name do not, and an
    /// unresolved name equal to another profile's id is not that id.
    #[test]
    fn a_pair_key_names_the_selected_profile() {
        let resolved = |id: &str, config: CloudConfig| CloudConfig {
            resolved_profile_id: Some(id.to_string()),
            ..config
        };
        let by_id = on_server("srv_a", "/docs");
        let by_name = CloudConfig {
            server_profile_id: None,
            server_profile: "nas".to_string(),
            ..by_id.clone()
        };
        assert_eq!(
            pair_key(&resolved("srv_a", by_id.clone())),
            pair_key(&resolved("srv_a", by_name.clone())),
            "one profile, one key"
        );
        assert_ne!(
            pair_key(&resolved("srv_a", by_name.clone())),
            pair_key(&resolved("srv_b", by_name.clone())),
            "two profiles named alike"
        );
        let name_is_an_id = CloudConfig {
            server_profile: "srv_a".to_string(),
            ..by_name
        };
        assert_ne!(pair_key(&name_is_an_id), pair_key(&by_id));
        // The worker keys the ledger and the backlog by this key only.
        let worker = include_str!("lib.rs");
        assert!(!worker.contains("backlog_key("), "one key for a pair");
    }

    /// A change the backlog holds is not lost when its pair's key changes
    /// (the vault opened and a name-only config resolved its profile): a
    /// key the backlog has not seen counts it as not yet covered.
    #[test]
    fn a_pending_change_is_reported_under_a_pairs_new_key() {
        let now = Instant::now();
        let mut backlog = ChangeBacklog::default();
        backlog.absorb(watcher_event(&["/local/f.txt"], false));
        let root = Path::new("/local");
        let by_name = CloudConfig {
            server_profile_id: None,
            ..on_server("srv_a", "/docs")
        };
        let resolved = CloudConfig {
            resolved_profile_id: Some("srv_a".to_string()),
            ..by_name.clone()
        };
        backlog.settle(&[(pair_key(&by_name), root, PairJob::Idle)], now);
        assert_eq!(
            backlog.changes_for(&pair_key(&resolved), root, now).paths,
            vec![PathBuf::from("/local/f.txt")]
        );
    }

    fn on_server(id: &str, remote: &str) -> CloudConfig {
        CloudConfig {
            local_folder: PathBuf::from("/local"),
            remote_folder: remote.to_string(),
            server_profile: "nas".to_string(),
            server_profile_id: Some(id.to_string()),
            ..CloudConfig::default()
        }
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
