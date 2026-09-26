//! The one two-way engine: every bidirectional sync (AeroCloud, `aeroftp-cli
//! sync --direction both`, the GUI Plan Two-way) decides a path by comparing
//! each side with the baseline of the last successful sync, not the two sides
//! with each other.
//!
//! Without a baseline a two-way sync cannot tell a file deleted on one side
//! from a file created on the other, nor a file changed on both sides from one
//! changed on one side: comparing the sides alone resurrects deletions and
//! lets the newer copy overwrite an older edit. With it, each side is one of:
//!
//! | side      | meaning                                               |
//! |-----------|-------------------------------------------------------|
//! | created   | present now, not in the baseline                      |
//! | unchanged | present now, same size and time as in the baseline    |
//! | modified  | present now, different size or time                   |
//! | deleted   | in the baseline, absent now                           |
//! | absent    | neither in the baseline nor present                   |
//!
//! and [`decide`] maps the pair:
//!
//! | local \ remote | unchanged      | modified        | deleted        | created / absent |
//! |----------------|----------------|-----------------|----------------|------------------|
//! | unchanged      | nothing        | copy to local   | delete local   |                  |
//! | modified       | copy to remote | conflict (1)    | conflict (2)   |                  |
//! | deleted        | delete remote  | conflict (2)    | forget         |                  |
//! | created        |                |                 |                | conflict (1) / copy to remote |
//! | absent         |                |                 |                | copy to local    |
//!
//! (1) the same content on both sides is in sync, not a conflict.
//! (2) a delete against a modification keeps the modification: it is copied
//! back to the side that deleted it ([`TwoWayConflict::keeps_modification`]).
//!
//! The guarantees the callers rely on:
//!
//! - no delete without a baseline: a first run, or a key the index migration
//!   could not vouch for (the caller passes no baseline for it), only copies;
//! - no decision on an absence the scan cannot vouch for: when a side's scan
//!   was incomplete, a path absent there is held, neither deleted on the other
//!   side nor copied over ([`guard`]);
//! - a side that looks empty while the baseline is not stops the whole run
//!   before any action ([`refuse_plan`]);
//! - the baseline advances only for the paths whose action succeeded
//!   ([`baseline_after`], called by the executor per path).

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use super::mtime::ModifyWindow;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A path on one side, as the engine reads it: size and modification time
/// (Unix seconds, `None` when the side keeps none).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Side {
    pub size: u64,
    pub modified: Option<i64>,
}

impl Side {
    pub fn new(size: u64, modified: Option<DateTime<Utc>>) -> Self {
        Self {
            size,
            modified: modified.map(|t| t.timestamp()),
        }
    }

    pub fn of_file(info: &crate::sync::FileInfo) -> Self {
        Self::new(info.size, info.modified)
    }
}

/// One side of a path at the last successful sync, as the sync index stores
/// it. `modified` is `None` when the side's time was not known when the entry
/// was written (the copy a transfer just made, before any listing reported
/// it): that side is then compared by size alone until the next sync records
/// its time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SideBaseline {
    pub size: u64,
    #[serde(default)]
    pub modified: Option<DateTime<Utc>>,
}

impl SideBaseline {
    pub fn side(&self) -> Side {
        Side::new(self.size, self.modified)
    }

    /// The baseline of a side a transfer just wrote with `size` bytes.
    pub fn written(size: u64) -> Self {
        Self {
            size,
            modified: None,
        }
    }

    pub fn of_file(info: &crate::sync::FileInfo) -> Self {
        Self {
            size: info.size,
            modified: info.modified,
        }
    }
}

/// Both sides of a path at the last successful sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PairBaseline {
    pub local: SideBaseline,
    pub remote: SideBaseline,
}

/// What one side did to a path since the last successful sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SideChange {
    Created,
    Unchanged,
    Modified,
    Deleted,
    Absent,
}

impl SideChange {
    /// Whether the path is present on this side now.
    pub fn is_present(self) -> bool {
        matches!(self, Self::Created | Self::Unchanged | Self::Modified)
    }
}

/// How a path changed on each side since the last successful sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TwoWayState {
    pub local: SideChange,
    pub remote: SideChange,
}

/// A path changed in ways a two-way sync cannot reconcile on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TwoWayConflict {
    /// Changed on both sides since the last sync, to different contents.
    BothModified,
    /// Created on both sides with different contents, with no baseline.
    BothCreated,
    /// Deleted locally, modified on the remote.
    DeletedLocallyModifiedRemotely,
    /// Modified locally, deleted on the remote.
    ModifiedLocallyDeletedRemotely,
}

impl TwoWayConflict {
    /// A delete against a modification is resolved by the engine itself: the
    /// modification is kept and copied back to the side that deleted it, so
    /// no edit is ever lost to a delete. The two other conflicts are left to
    /// the caller's policy (`--conflict-mode`, the AeroCloud strategy).
    pub fn keeps_modification(self) -> Option<TwoWayAction> {
        match self {
            Self::DeletedLocallyModifiedRemotely => Some(TwoWayAction::CopyToLocal),
            Self::ModifiedLocallyDeletedRemotely => Some(TwoWayAction::CopyToRemote),
            Self::BothModified | Self::BothCreated => None,
        }
    }
}

/// What a two-way sync does with a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "conflict", rename_all = "snake_case")]
pub enum TwoWayAction {
    /// Both sides hold the same content: nothing moves, and the baseline
    /// records the pair as it is now.
    InSync,
    CopyToRemote,
    CopyToLocal,
    DeleteRemote,
    DeleteLocal,
    /// Deleted on both sides: nothing moves, and the baseline forgets it.
    Forget,
    /// Left as it is on both sides and in the baseline: what a scan could not
    /// vouch for, or a conflict nobody resolved.
    Hold,
    Conflict(TwoWayConflict),
}

/// How one side changed: its state now against its baseline. Sizes are
/// compared unless `compare_size` is off (a crypt overlay whose sizes do not
/// map); times are compared under `window` when both are known, so a side
/// whose baseline time is unknown is compared by size alone.
pub fn side_change(
    current: Option<Side>,
    baseline: Option<Side>,
    window: ModifyWindow,
    compare_size: bool,
) -> SideChange {
    match (current, baseline) {
        (None, None) => SideChange::Absent,
        (Some(_), None) => SideChange::Created,
        (None, Some(_)) => SideChange::Deleted,
        (Some(now), Some(then)) => {
            let size_changed = compare_size && now.size != then.size;
            let time_changed = matches!(
                window.order(now.modified, then.modified),
                Some(std::cmp::Ordering::Less | std::cmp::Ordering::Greater)
            );
            if size_changed || time_changed {
                SideChange::Modified
            } else {
                SideChange::Unchanged
            }
        }
    }
}

/// Whether the two sides hold the same content now, as far as size and time
/// can tell: the same size (unless sizes are not compared) and the same
/// instant under `window` when both times are known.
pub fn same_content(local: Side, remote: Side, window: ModifyWindow, compare_size: bool) -> bool {
    (!compare_size || local.size == remote.size)
        && !matches!(
            window.order(local.modified, remote.modified),
            Some(std::cmp::Ordering::Less | std::cmp::Ordering::Greater)
        )
}

/// The decision table (see the module documentation). `same_content` is read
/// only where both sides changed or were created.
pub fn decide(state: TwoWayState, same_content: bool) -> TwoWayAction {
    use SideChange::*;
    match (state.local, state.remote) {
        (Unchanged, Unchanged) => TwoWayAction::InSync,
        (Modified, Unchanged) => TwoWayAction::CopyToRemote,
        (Unchanged, Modified) => TwoWayAction::CopyToLocal,
        (Modified, Modified) | (Created, Created) if same_content => TwoWayAction::InSync,
        (Modified, Modified) => TwoWayAction::Conflict(TwoWayConflict::BothModified),
        (Created, Created) => TwoWayAction::Conflict(TwoWayConflict::BothCreated),
        (Deleted, Unchanged) => TwoWayAction::DeleteRemote,
        (Unchanged, Deleted) => TwoWayAction::DeleteLocal,
        (Deleted, Modified) => {
            TwoWayAction::Conflict(TwoWayConflict::DeletedLocallyModifiedRemotely)
        }
        (Modified, Deleted) => {
            TwoWayAction::Conflict(TwoWayConflict::ModifiedLocallyDeletedRemotely)
        }
        (Deleted, Deleted) => TwoWayAction::Forget,
        (Created, Absent) => TwoWayAction::CopyToRemote,
        (Absent, Created) => TwoWayAction::CopyToLocal,
        // A baseline is on both sides or on neither, so the other mixes
        // (a side created while the other is unchanged, or absent while the
        // other was deleted) cannot come from `side_change`; they are held.
        _ => TwoWayAction::Hold,
    }
}

/// What each side's scan could vouch for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanHealth {
    pub local_complete: bool,
    pub remote_complete: bool,
}

impl Default for ScanHealth {
    fn default() -> Self {
        Self {
            local_complete: true,
            remote_complete: true,
        }
    }
}

/// What a run's safety checks allow each of its rows: the scans' health, and
/// which deletes are held this run. Both are held after a mass disappearance
/// that is more likely a listing failure than a user's delete; local deletes
/// alone when the remote listing can omit a stored object, so its absence
/// cannot authorise deleting the local copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TwoWayGate {
    pub health: ScanHealth,
    pub hold_local_deletes: bool,
    pub hold_remote_deletes: bool,
}

/// `action` as the run's gate allows it: [`guard`] for the scans, then a
/// held delete stays where it is (it is neither propagated nor undone by
/// copying the file back).
pub fn gated(action: TwoWayAction, state: TwoWayState, gate: TwoWayGate) -> TwoWayAction {
    match guard(action, state, gate.health) {
        TwoWayAction::DeleteLocal if gate.hold_local_deletes => TwoWayAction::Hold,
        TwoWayAction::DeleteRemote if gate.hold_remote_deletes => TwoWayAction::Hold,
        action => action,
    }
}

/// The action a row runs with: the table, a delete-against-modification
/// conflict resolved by keeping the modification, and the run's gate. The
/// other conflicts come back as `Conflict` for the caller's policy.
pub fn resolve(state: TwoWayState, same_content: bool, gate: TwoWayGate) -> TwoWayAction {
    let action = match decide(state, same_content) {
        TwoWayAction::Conflict(conflict) => conflict
            .keeps_modification()
            .unwrap_or(TwoWayAction::Conflict(conflict)),
        action => action,
    };
    gated(action, state, gate)
}

/// `action` as the scans allow it: a path absent on a side whose scan was
/// incomplete may only be unseen, so no decision rests on that absence (no
/// delete propagated from it, no copy written over it); the path is held.
pub fn guard(action: TwoWayAction, state: TwoWayState, health: ScanHealth) -> TwoWayAction {
    let unsure_local = !health.local_complete && !state.local.is_present();
    let unsure_remote = !health.remote_complete && !state.remote.is_present();
    if (unsure_local || unsure_remote) && action != TwoWayAction::InSync {
        TwoWayAction::Hold
    } else {
        action
    }
}

/// Why a two-way run is refused before any action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TwoWayRefusal {
    /// The local side lists nothing while the last sync left files there.
    LocalSideEmpty,
    /// The remote side lists nothing while the last sync left files there.
    RemoteSideEmpty,
}

impl TwoWayRefusal {
    pub fn describe(self) -> &'static str {
        match self {
            Self::LocalSideEmpty => {
                "the local folder lists no file while the last sync left files in it: refusing to run (an unmounted or emptied folder would delete every file on the remote)"
            }
            Self::RemoteSideEmpty => {
                "the remote folder lists no file while the last sync left files in it: refusing to run (an unreadable or emptied remote would delete every local file)"
            }
        }
    }
}

/// A side that looks empty while the baseline holds files stops the run: an
/// unmounted drive, a revoked token or an emptied bucket reads exactly like a
/// user who deleted everything, and a two-way sync would propagate it.
pub fn refuse_plan(
    baseline_files: usize,
    local_files: usize,
    remote_files: usize,
) -> Option<TwoWayRefusal> {
    if baseline_files == 0 {
        return None;
    }
    if local_files == 0 && remote_files > 0 {
        Some(TwoWayRefusal::LocalSideEmpty)
    } else if remote_files == 0 && local_files > 0 {
        Some(TwoWayRefusal::RemoteSideEmpty)
    } else {
        None
    }
}

/// The baseline a path keeps once `action` succeeded. `None` removes it. A
/// side a copy just wrote records the size it was given and no time, so the
/// next run compares it by size until that run records the time its listing
/// reports. `Hold` and an unresolved `Conflict` keep the prior baseline.
pub fn baseline_after(
    action: TwoWayAction,
    local: Option<SideBaseline>,
    remote: Option<SideBaseline>,
    prior: Option<PairBaseline>,
) -> Option<PairBaseline> {
    match action {
        TwoWayAction::InSync => match (local, remote) {
            (Some(local), Some(remote)) => Some(PairBaseline { local, remote }),
            _ => prior,
        },
        TwoWayAction::CopyToRemote => local.map(|local| PairBaseline {
            local,
            remote: SideBaseline::written(local.size),
        }),
        TwoWayAction::CopyToLocal => remote.map(|remote| PairBaseline {
            local: SideBaseline::written(remote.size),
            remote,
        }),
        TwoWayAction::DeleteRemote | TwoWayAction::DeleteLocal | TwoWayAction::Forget => None,
        TwoWayAction::Hold | TwoWayAction::Conflict(_) => prior,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use SideChange::*;

    const W: ModifyWindow = ModifyWindow::Seconds { secs: 2 };

    fn side(size: u64, modified: i64) -> Option<Side> {
        Some(Side {
            size,
            modified: Some(modified),
        })
    }

    fn state(local: SideChange, remote: SideChange) -> TwoWayState {
        TwoWayState { local, remote }
    }

    #[test]
    fn a_side_is_read_against_its_own_baseline() {
        assert_eq!(side_change(None, None, W, true), Absent);
        assert_eq!(side_change(side(1, 100), None, W, true), Created);
        assert_eq!(side_change(None, side(1, 100), W, true), Deleted);
        assert_eq!(side_change(side(1, 101), side(1, 100), W, true), Unchanged);
        assert_eq!(side_change(side(2, 100), side(1, 100), W, true), Modified);
        assert_eq!(side_change(side(1, 200), side(1, 100), W, true), Modified);
        // Sizes that do not map (crypt overlay): time alone.
        assert_eq!(side_change(side(2, 100), side(1, 100), W, false), Unchanged);
        // A baseline written by a copy has no time: size alone.
        let written = Some(Side {
            size: 1,
            modified: None,
        });
        assert_eq!(side_change(side(1, 999), written, W, true), Unchanged);
        assert_eq!(side_change(side(2, 999), written, W, true), Modified);
    }

    /// One assertion per row of the decision table.
    #[test]
    fn the_decision_table() {
        let rows: &[(SideChange, SideChange, bool, TwoWayAction)] = &[
            (Unchanged, Unchanged, false, TwoWayAction::InSync),
            (Modified, Unchanged, false, TwoWayAction::CopyToRemote),
            (Unchanged, Modified, false, TwoWayAction::CopyToLocal),
            (
                Modified,
                Modified,
                false,
                TwoWayAction::Conflict(TwoWayConflict::BothModified),
            ),
            (Modified, Modified, true, TwoWayAction::InSync),
            (Deleted, Unchanged, false, TwoWayAction::DeleteRemote),
            (Unchanged, Deleted, false, TwoWayAction::DeleteLocal),
            (
                Deleted,
                Modified,
                false,
                TwoWayAction::Conflict(TwoWayConflict::DeletedLocallyModifiedRemotely),
            ),
            (
                Modified,
                Deleted,
                false,
                TwoWayAction::Conflict(TwoWayConflict::ModifiedLocallyDeletedRemotely),
            ),
            (Deleted, Deleted, false, TwoWayAction::Forget),
            (Created, Created, true, TwoWayAction::InSync),
            (
                Created,
                Created,
                false,
                TwoWayAction::Conflict(TwoWayConflict::BothCreated),
            ),
            (Created, Absent, false, TwoWayAction::CopyToRemote),
            (Absent, Created, false, TwoWayAction::CopyToLocal),
        ];
        for (local, remote, same, want) in rows {
            assert_eq!(
                decide(state(*local, *remote), *same),
                *want,
                "{local:?} / {remote:?} (same content: {same})"
            );
        }
    }

    /// A delete never wins against a modification: the modified copy goes
    /// back to the side that deleted it.
    #[test]
    fn a_delete_against_a_modification_keeps_the_modification() {
        assert_eq!(
            TwoWayConflict::DeletedLocallyModifiedRemotely.keeps_modification(),
            Some(TwoWayAction::CopyToLocal)
        );
        assert_eq!(
            TwoWayConflict::ModifiedLocallyDeletedRemotely.keeps_modification(),
            Some(TwoWayAction::CopyToRemote)
        );
        assert_eq!(TwoWayConflict::BothModified.keeps_modification(), None);
        assert_eq!(TwoWayConflict::BothCreated.keeps_modification(), None);
    }

    /// No decision rests on an absence a scan could not vouch for.
    #[test]
    fn an_absence_on_an_incomplete_side_is_held() {
        let remote_partial = ScanHealth {
            local_complete: true,
            remote_complete: false,
        };
        let deleted_remotely = state(Unchanged, Deleted);
        assert_eq!(
            guard(
                decide(deleted_remotely, false),
                deleted_remotely,
                remote_partial
            ),
            TwoWayAction::Hold
        );
        let created_locally = state(Created, Absent);
        assert_eq!(
            guard(
                decide(created_locally, false),
                created_locally,
                remote_partial
            ),
            TwoWayAction::Hold
        );
        // A path present on the incomplete side is still decided.
        let modified_remotely = state(Unchanged, Modified);
        assert_eq!(
            guard(
                decide(modified_remotely, false),
                modified_remotely,
                remote_partial
            ),
            TwoWayAction::CopyToLocal
        );
        // A complete scan decides every row.
        let complete = ScanHealth {
            local_complete: true,
            remote_complete: true,
        };
        assert_eq!(
            guard(decide(deleted_remotely, false), deleted_remotely, complete),
            TwoWayAction::DeleteLocal
        );
    }

    /// A run whose deletes are held neither propagates a delete nor undoes it;
    /// a delete against a modification is resolved before the gate reads it.
    #[test]
    fn resolve_keeps_modifications_and_applies_the_gate() {
        let open = TwoWayGate::default();
        let held = TwoWayGate {
            hold_local_deletes: true,
            hold_remote_deletes: true,
            ..TwoWayGate::default()
        };
        let local_held = TwoWayGate {
            hold_local_deletes: true,
            ..TwoWayGate::default()
        };
        assert_eq!(
            resolve(state(Unchanged, Deleted), false, local_held),
            TwoWayAction::Hold
        );
        assert_eq!(
            resolve(state(Deleted, Unchanged), false, local_held),
            TwoWayAction::DeleteRemote
        );
        assert_eq!(
            resolve(state(Unchanged, Deleted), false, open),
            TwoWayAction::DeleteLocal
        );
        assert_eq!(
            resolve(state(Unchanged, Deleted), false, held),
            TwoWayAction::Hold
        );
        assert_eq!(
            resolve(state(Deleted, Modified), false, held),
            TwoWayAction::CopyToLocal
        );
        assert_eq!(
            resolve(state(Modified, Deleted), false, open),
            TwoWayAction::CopyToRemote
        );
        assert_eq!(
            resolve(state(Modified, Modified), false, open),
            TwoWayAction::Conflict(TwoWayConflict::BothModified)
        );
        let partial = TwoWayGate {
            health: ScanHealth {
                local_complete: false,
                remote_complete: true,
            },
            ..TwoWayGate::default()
        };
        assert_eq!(
            resolve(state(Deleted, Modified), false, partial),
            TwoWayAction::Hold
        );
    }

    #[test]
    fn a_side_that_looks_empty_stops_the_run() {
        assert_eq!(refuse_plan(10, 0, 10), Some(TwoWayRefusal::LocalSideEmpty));
        assert_eq!(refuse_plan(10, 10, 0), Some(TwoWayRefusal::RemoteSideEmpty));
        assert_eq!(refuse_plan(10, 3, 4), None);
        // No baseline: a first sync into an empty side copies everything.
        assert_eq!(refuse_plan(0, 0, 10), None);
        // Both empty: nothing to do, nothing to refuse.
        assert_eq!(refuse_plan(10, 0, 0), None);
    }

    #[test]
    fn the_baseline_advances_with_the_action() {
        let t = chrono::DateTime::from_timestamp(1_000, 0);
        let local = SideBaseline {
            size: 5,
            modified: t,
        };
        let remote = SideBaseline {
            size: 5,
            modified: chrono::DateTime::from_timestamp(2_000, 0),
        };
        let prior = PairBaseline {
            local: SideBaseline::written(1),
            remote: SideBaseline::written(1),
        };
        assert_eq!(
            baseline_after(TwoWayAction::InSync, Some(local), Some(remote), Some(prior)),
            Some(PairBaseline { local, remote })
        );
        assert_eq!(
            baseline_after(TwoWayAction::CopyToRemote, Some(local), None, None),
            Some(PairBaseline {
                local,
                remote: SideBaseline::written(5)
            })
        );
        assert_eq!(
            baseline_after(TwoWayAction::CopyToLocal, None, Some(remote), None),
            Some(PairBaseline {
                local: SideBaseline::written(5),
                remote
            })
        );
        for gone in [
            TwoWayAction::DeleteLocal,
            TwoWayAction::DeleteRemote,
            TwoWayAction::Forget,
        ] {
            assert_eq!(
                baseline_after(gone, Some(local), Some(remote), Some(prior)),
                None
            );
        }
        for kept in [
            TwoWayAction::Hold,
            TwoWayAction::Conflict(TwoWayConflict::BothModified),
        ] {
            assert_eq!(
                baseline_after(kept, Some(local), Some(remote), Some(prior)),
                Some(prior)
            );
        }
    }

    #[test]
    fn same_content_reads_size_and_time() {
        let a = Side {
            size: 3,
            modified: Some(100),
        };
        let b = Side {
            size: 3,
            modified: Some(101),
        };
        assert!(same_content(a, b, W, true));
        assert!(!same_content(
            a,
            Side {
                size: 4,
                modified: Some(100)
            },
            W,
            true
        ));
        assert!(!same_content(
            a,
            Side {
                size: 3,
                modified: Some(200)
            },
            W,
            true
        ));
        let size_only = ModifyWindow::SizeOnly {
            reason: crate::sync_core::mtime::SizeOnlyReason::NoComparableTime,
        };
        assert!(same_content(
            a,
            Side {
                size: 3,
                modified: Some(200)
            },
            size_only,
            true
        ));
    }
}
