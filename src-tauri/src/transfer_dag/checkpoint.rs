// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Durable, versioned checkpoint records for a single native multipart upload.
//!
//! The DAG remains the only scheduler. This module owns only durable facts:
//! identity, an opaque provider session reference, part receipts, attempts and
//! the terminal commit fact. A record is written atomically before a part node
//! reports completion, and the committed record is written before the caller
//! can expose a successful transfer.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

pub const CHECKPOINT_SCHEMA_VERSION: u32 = 3;
pub const DEFAULT_CHECKPOINT_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Upper bound on stored checkpoint records. The TTL scavenger only prunes a
/// record when the very same endpoint is revisited, so a store on a real user's
/// machine can only grow. This cap bounds it: when a new record would exceed the
/// ceiling, the oldest records are evicted first, terminal residue before
/// resumable transfers. The documented cost is that the oldest resumable
/// transfer can be dropped once the store is full of resumable records; the
/// explicit escape for a decommissioned endpoint is `forget_endpoint`.
///
/// A just-opened resumable record sorts after older occupants for
/// `DEFAULT_CHECKPOINT_EVICT_GRACE`, so two concurrent `open_or_create` calls
/// at a full store prefer not to delete each other's opening record. Terminal
/// residue is always first: a commit that just finished is the thing the
/// cap exists to reclaim. The grace is an ordering, not a refused open: a
/// fan-out that fills the 256 slots with fresh in-flight records still
/// evicts the oldest of them and the 257th open succeeds.
pub const DEFAULT_CHECKPOINT_MAX_RECORDS: usize = 256;

/// Window during which a non-terminal record sorts after older eviction
/// candidates. Two `enforce_cap` calls that share a saturated store of
/// same-second timestamps could otherwise pick each other's just-persisted
/// opening record: the previous sort was terminal-first then oldest, and
/// equal timestamps fell through to readdir order. A lock file would
/// serialize every multipart open. Preferring older occupants is enough
/// when any exist; when every occupant is still inside this window the
/// cap still holds by evicting the oldest of them, which is the documented
/// cost of a full store of resumable records.
pub const DEFAULT_CHECKPOINT_EVICT_GRACE: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointSourceIdentity {
    pub local_path: String,
    pub size: u64,
    pub modified_unix_nanos: Option<u128>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointDestinationIdentity {
    pub provider: String,
    pub protocol: String,
    pub host: String,
    pub account: String,
    pub remote_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointLayout {
    pub total_size: u64,
    pub total_parts: u32,
    pub preferred_part_size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointPartReceipt {
    pub part_number: u32,
    pub etag: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointStatus {
    Prepared,
    Transferring,
    PayloadComplete,
    /// The full payload is durably present (every part receipt is journaled)
    /// AND the truthful pre-commit verification has passed. A record only
    /// reaches this state after `mark_verified`; it can never be manufactured by
    /// schema migration. `Verified` is the mandatory gate before `Committed`.
    Verified,
    Committed,
    Failed,
}

impl CheckpointStatus {
    fn is_terminal(self) -> bool {
        matches!(self, Self::Committed)
    }
}

/// A fresh observation of the local source file taken at verification time.
///
/// Verification compares these observed facts against the durable source
/// identity that produced the uploaded parts. It uses only real facts and makes
/// no provider checksum claim, because a native multipart object is not yet
/// finalized when `VerifyChecksum` runs (it runs before `CommitTemp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedSource {
    pub exists: bool,
    pub size: u64,
    pub modified_unix_nanos: Option<u128>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultipartCheckpoint {
    pub schema_version: u32,
    pub transfer_key: String,
    pub source: CheckpointSourceIdentity,
    pub destination: CheckpointDestinationIdentity,
    pub layout: CheckpointLayout,
    /// Opaque provider session identifier. It is never logged by this module.
    pub upload_id: Option<String>,
    pub receipts: BTreeMap<u32, CheckpointPartReceipt>,
    pub attempts: u32,
    pub status: CheckpointStatus,
    pub updated_unix_secs: u64,
}

impl MultipartCheckpoint {
    pub fn fresh(
        source: CheckpointSourceIdentity,
        destination: CheckpointDestinationIdentity,
        layout: CheckpointLayout,
    ) -> Self {
        let transfer_key = transfer_key(&source, &destination, &layout);
        Self {
            schema_version: CHECKPOINT_SCHEMA_VERSION,
            transfer_key,
            source,
            destination,
            layout,
            upload_id: None,
            receipts: BTreeMap::new(),
            attempts: 0,
            status: CheckpointStatus::Prepared,
            updated_unix_secs: now_secs(),
        }
    }

    pub fn missing_parts(&self) -> Vec<u32> {
        (1..=self.layout.total_parts)
            .filter(|part| !self.receipts.contains_key(part))
            .collect()
    }

    pub fn is_resumable(&self) -> bool {
        !self.status.is_terminal()
            && self.upload_id.is_some()
            && self
                .receipts
                .keys()
                .all(|part| *part > 0 && *part <= self.layout.total_parts)
    }

    fn transition(&mut self, next: CheckpointStatus) -> Result<(), String> {
        use CheckpointStatus::{
            Committed, Failed, PayloadComplete, Prepared, Transferring, Verified,
        };
        let legal = matches!(
            (self.status, next),
            // Forward progress of one live attempt.
            (Prepared, Transferring)
                | (Transferring, Transferring)
                | (Transferring, PayloadComplete)
                | (PayloadComplete, PayloadComplete)
                | (PayloadComplete, Verified)
                | (Verified, Verified)
                | (Verified, Committed)
                // Reopening a persisted non-terminal record for another attempt
                // rewinds it to Transferring: receipts (the expensive work) are
                // preserved, but verify and commit re-run for real. A durable
                // Verified fact is therefore never trusted across a restart, it
                // is re-derived.
                | (PayloadComplete, Transferring)
                | (Verified, Transferring)
                | (Failed, Transferring)
                // Any live state can fail; a failed record can be retried or
                // re-failed.
                | (Prepared, Failed)
                | (Transferring, Failed)
                | (PayloadComplete, Failed)
                | (Verified, Failed)
                | (Failed, Failed)
        );
        if !legal {
            return Err(format!(
                "illegal checkpoint transition {:?} -> {:?}",
                self.status, next
            ));
        }
        self.status = next;
        self.updated_unix_secs = now_secs();
        Ok(())
    }

    /// Truthful pre-commit verification for a native multipart upload.
    ///
    /// It uses only durable facts plus a fresh observation of the local source
    /// and makes no provider checksum claim: `VerifyChecksum` runs before
    /// `CommitTemp`, so the remote object is not yet finalized and cannot be
    /// hashed without inventing a claim. The check is fail-closed: every part
    /// receipt must be present, the durable layout and source sizes must agree,
    /// and the source on disk must still match the identity that produced the
    /// uploaded parts (size, and modified time when both are known). If the
    /// source changed under the transfer, the committed object would not match
    /// it, so verification fails and commit is blocked.
    pub fn verify_against_source(&self, observed: &ObservedSource) -> Result<(), String> {
        if !self.missing_parts().is_empty() {
            return Err("verification requires every multipart receipt to be present".to_string());
        }
        if self.layout.total_size != self.source.size {
            return Err(format!(
                "durable record inconsistent: layout total {} != source size {}",
                self.layout.total_size, self.source.size
            ));
        }
        if !observed.exists {
            return Err("source file is no longer present at verification".to_string());
        }
        if observed.size != self.source.size {
            return Err(format!(
                "source size changed during transfer: observed {} != durable {}",
                observed.size, self.source.size
            ));
        }
        if let (Some(recorded), Some(seen)) = (
            self.source.modified_unix_nanos,
            observed.modified_unix_nanos,
        ) {
            if recorded != seen {
                return Err("source modification time changed during transfer".to_string());
            }
        }
        Ok(())
    }

    /// True once the durable verified gate has been passed (and not yet
    /// committed). The commit node checks this before finalizing a provider
    /// session so a payload that never verified can never be committed.
    pub fn is_verified(&self) -> bool {
        matches!(self.status, CheckpointStatus::Verified)
    }

    /// True once the terminal committed fact is durable.
    pub fn is_committed(&self) -> bool {
        matches!(self.status, CheckpointStatus::Committed)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointOpen {
    pub checkpoint: MultipartCheckpoint,
    pub resumed: bool,
}

/// Filesystem store with a caller-selectable root for deterministic tests.
pub struct TransferCheckpointStore {
    dir: PathBuf,
    ttl: Duration,
    max_records: usize,
}

impl TransferCheckpointStore {
    pub fn new(dir: impl Into<PathBuf>) -> Result<Self, String> {
        Self::with_ttl(dir, DEFAULT_CHECKPOINT_TTL)
    }

    pub fn with_ttl(dir: impl Into<PathBuf>, ttl: Duration) -> Result<Self, String> {
        Self::with_limits(dir, ttl, DEFAULT_CHECKPOINT_MAX_RECORDS)
    }

    pub fn with_limits(
        dir: impl Into<PathBuf>,
        ttl: Duration,
        max_records: usize,
    ) -> Result<Self, String> {
        let dir = dir.into();
        fs::create_dir_all(&dir)
            .map_err(|e| format!("cannot create transfer checkpoint directory: {e}"))?;
        Ok(Self {
            dir,
            ttl,
            // A store must always be able to hold the record it is opening.
            max_records: max_records.max(1),
        })
    }

    pub fn default_store() -> Result<Self, String> {
        let root = crate::portable::aeroftp_data_root().ok_or_else(|| {
            "cannot determine AeroFTP data root for transfer checkpoint".to_string()
        })?;
        Self::new(root.join("transfer-checkpoints"))
    }

    pub fn open_or_create(
        &self,
        source: CheckpointSourceIdentity,
        destination: CheckpointDestinationIdentity,
        layout: CheckpointLayout,
    ) -> Result<CheckpointOpen, String> {
        let fresh = MultipartCheckpoint::fresh(source, destination, layout);
        let path = self.path_for(&fresh.transfer_key);
        // A record this build cannot read is not a resumable record, and this is
        // the third reader that has to say so: propagating the error here failed
        // the transfer outright, when the honest answer is that there is nothing
        // to resume from and a fresh one starts. Overwriting it does cost
        // something, since a future-schema record IS resumable by a newer build,
        // but only for this one transfer key, and only for the transfer the user
        // is relaunching right now. Failing that transfer forever is worse.
        let existing = match self.load_path(&path) {
            Ok(existing) => existing,
            Err(e) => {
                tracing::warn!(
                    "[checkpoint] starting fresh, {} is unreadable: {e}",
                    path.display()
                );
                None
            }
        };
        // A NEW record reserves its slot on disk before the cap is enforced,
        // and the reason is what another opener can see. `enforce_cap` counts
        // the kept record as one slot whether or not it is written yet, so the
        // arithmetic is right for the caller; it is wrong for everyone else.
        // Between one opener's eviction and its write the store is one record
        // under its cap, and an opener that scans in that window reads capacity
        // that is already spoken for, evicts nothing, and writes: the cap is
        // exceeded by one per opener that arrives in a window. Writing first
        // puts the claim where the other scans can count it, which is also why
        // `enforce_cap` already sorts a just-persisted in-flight record LAST
        // among eviction candidates: the design expected these to be on disk.
        //
        // The order this replaces was itself deliberate, and its reason is kept
        // rather than dropped: a store that cannot evict must refuse to grow,
        // not report failure with the new record left behind. That is what the
        // rollback below is for. The window it opens instead is a crash between
        // the write and the eviction, which leaves one extra record that the
        // next open reclaims, rather than a record the store said it could not
        // hold.
        let Some(mut loaded) = existing else {
            self.persist(&fresh)?;
            if let Err(e) = self.enforce_cap(&fresh.transfer_key) {
                // Undo the reservation. Ignoring the unlink error is correct
                // here: the caller is already being told the open failed, and a
                // residual record is reclaimable by the next cap run, while
                // replacing the real reason with "could not clean up" would
                // hide it.
                let _ = fs::remove_file(&path);
                return Err(e);
            }
            return Ok(CheckpointOpen {
                checkpoint: fresh,
                resumed: false,
            });
        };
        if !same_identity(&loaded, &fresh) || loaded.status.is_terminal() {
            self.enforce_cap(&fresh.transfer_key)?;
            self.persist(&fresh)?;
            return Ok(CheckpointOpen {
                checkpoint: fresh,
                resumed: false,
            });
        }
        loaded.attempts = loaded.attempts.saturating_add(1);
        loaded.transition(CheckpointStatus::Transferring)?;
        self.persist(&loaded)?;
        Ok(CheckpointOpen {
            checkpoint: loaded,
            resumed: true,
        })
    }

    pub fn begin(
        &self,
        checkpoint: &mut MultipartCheckpoint,
        upload_id: String,
    ) -> Result<(), String> {
        if checkpoint.upload_id.is_some()
            && checkpoint.upload_id.as_deref() != Some(upload_id.as_str())
        {
            return Err("checkpoint session identifier changed during one transfer".to_string());
        }
        checkpoint.upload_id = Some(upload_id);
        checkpoint.attempts = checkpoint.attempts.saturating_add(1);
        checkpoint.transition(CheckpointStatus::Transferring)?;
        self.persist(checkpoint)
    }

    pub fn record_receipt(
        &self,
        checkpoint: &mut MultipartCheckpoint,
        receipt: CheckpointPartReceipt,
    ) -> Result<(), String> {
        if receipt.part_number == 0 || receipt.part_number > checkpoint.layout.total_parts {
            return Err(format!(
                "checkpoint receipt {} is outside layout",
                receipt.part_number
            ));
        }
        if let Some(existing) = checkpoint.receipts.get(&receipt.part_number) {
            if existing != &receipt {
                return Err(format!(
                    "checkpoint receipt {} changed",
                    receipt.part_number
                ));
            }
            return Ok(());
        }
        checkpoint.receipts.insert(receipt.part_number, receipt);
        if checkpoint.missing_parts().is_empty() {
            checkpoint.transition(CheckpointStatus::PayloadComplete)?;
        } else {
            checkpoint.transition(CheckpointStatus::Transferring)?;
        }
        self.persist(checkpoint)
    }

    pub fn mark_failed(&self, checkpoint: &mut MultipartCheckpoint) -> Result<(), String> {
        checkpoint.transition(CheckpointStatus::Failed)?;
        self.persist(checkpoint)
    }

    /// Record the durable `Verified` fact after the caller's truthful
    /// verification passed. Fail-closed: every receipt must be present. A
    /// resumed record can re-enter here from `Transferring` (rewound on reopen)
    /// or `PayloadComplete`; an already `Verified` record is idempotent.
    pub fn mark_verified(&self, checkpoint: &mut MultipartCheckpoint) -> Result<(), String> {
        if !checkpoint.missing_parts().is_empty() {
            return Err("cannot verify a checkpoint with missing multipart receipts".to_string());
        }
        match checkpoint.status {
            CheckpointStatus::Transferring => {
                checkpoint.transition(CheckpointStatus::PayloadComplete)?;
                checkpoint.transition(CheckpointStatus::Verified)?;
            }
            CheckpointStatus::PayloadComplete => {
                checkpoint.transition(CheckpointStatus::Verified)?;
            }
            CheckpointStatus::Verified => {}
            other => {
                return Err(format!("cannot verify a checkpoint in state {other:?}"));
            }
        }
        self.persist(checkpoint)
    }

    pub fn mark_committed(&self, checkpoint: &mut MultipartCheckpoint) -> Result<(), String> {
        if !checkpoint.missing_parts().is_empty() {
            return Err("cannot commit a checkpoint with missing multipart receipts".to_string());
        }
        // Fail-closed: the payload must have passed durable verification first.
        // The state machine forbids PayloadComplete -> Committed, so a payload
        // that never verified can never be committed.
        if checkpoint.status != CheckpointStatus::Verified {
            return Err(format!(
                "cannot commit a checkpoint that is not verified (state {:?})",
                checkpoint.status
            ));
        }
        checkpoint.transition(CheckpointStatus::Committed)?;
        self.persist(checkpoint)
    }

    /// Return expired nonterminal records and remove no files. The caller may
    /// abort the provider session first, then call `remove` only on success.
    pub fn stale_nonterminal(&self) -> Result<Vec<MultipartCheckpoint>, String> {
        let now = now_secs();
        let mut stale = Vec::new();
        for entry in fs::read_dir(&self.dir)
            .map_err(|e| format!("cannot read transfer checkpoint directory: {e}"))?
        {
            let entry = entry.map_err(|e| format!("cannot read checkpoint entry: {e}"))?;
            if entry.path().extension().and_then(|v| v.to_str()) != Some("json") {
                continue;
            }
            // A record this build cannot read is not a reason to fail the
            // transfer that is starting. `migrate` rejects any schema version it
            // does not know, and a downgrade after a bad release is the ordinary
            // way to end up with a future-schema record, since this directory
            // survives installs. Propagating that error here failed EVERY
            // multipart upload to every provider, and `enforce_cap`, the one
            // function that reclaims such a record, runs afterwards and so was
            // never reached. An unreadable record is simply not resumable, which
            // is all this function is asked about.
            match self.load_path(&entry.path()) {
                Ok(Some(record)) => {
                    if !record.status.is_terminal()
                        && now.saturating_sub(record.updated_unix_secs) >= self.ttl.as_secs()
                    {
                        stale.push(record);
                    }
                }
                Ok(None) => {}
                Err(e) => tracing::warn!(
                    "[checkpoint] skipping unreadable record {}: {e}",
                    entry.path().display()
                ),
            }
        }
        stale.sort_by(|a, b| a.transfer_key.cmp(&b.transfer_key));
        Ok(stale)
    }

    pub fn remove(&self, transfer_key: &str) -> Result<(), String> {
        let path = self.path_for(transfer_key);
        if path.exists() {
            fs::remove_file(path).map_err(|e| format!("cannot remove transfer checkpoint: {e}"))?;
        }
        Ok(())
    }

    /// Bound the directory to `max_records`. Evicts terminal residue first, then
    /// the oldest resumable records, and never the record identified by `keep`
    /// (the one being opened). An unreadable or corrupt entry still occupies a
    /// slot, so it is treated as the oldest evictable residue and reclaimed.
    /// Non-terminal records newer than `DEFAULT_CHECKPOINT_EVICT_GRACE` sort
    /// after older occupants so a concurrent opener is not the first pick.
    /// They are still evicted if nothing older is available: the cap is a
    /// bound, not a refused open.
    fn enforce_cap(&self, keep: &str) -> Result<(), String> {
        // First pass collects paths only. The store is under its cap on the
        // overwhelming majority of opens, and in that case this function has to
        // cost a `read_dir` and nothing more: `stale_nonterminal` has already
        // read and fully parsed this same directory moments earlier on the
        // transfer's critical path, so parsing it again to discover there is
        // nothing to do was pure duplication, once per file in a batch.
        let mut paths: Vec<PathBuf> = Vec::new();
        for entry in fs::read_dir(&self.dir)
            .map_err(|e| format!("cannot read transfer checkpoint directory: {e}"))?
        {
            let entry = entry.map_err(|e| format!("cannot read checkpoint entry: {e}"))?;
            let path = entry.path();
            if path.extension().and_then(|v| v.to_str()) != Some("json") {
                continue;
            }
            if path.file_stem().and_then(|v| v.to_str()) == Some(keep) {
                continue;
            }
            paths.push(path);
        }
        // The kept record is not in `paths`, so it counts for one slot.
        let total = paths.len() + 1;
        if total <= self.max_records {
            return Ok(());
        }
        let evict = total - self.max_records;
        // Only now, and only because an eviction is actually happening, is the
        // parse worth paying for: terminality is the one fact the directory
        // cannot supply, and it is what orders the eviction.
        let now = now_secs();
        let grace = DEFAULT_CHECKPOINT_EVICT_GRACE.as_secs();
        let mut records: Vec<(PathBuf, bool, u64, bool)> = paths
            .into_iter()
            .map(|path| match self.load_path(&path) {
                Ok(Some(record)) => {
                    let terminal = record.status.is_terminal();
                    let ts = record.updated_unix_secs;
                    // A just-persisted in-flight record belongs to a concurrent
                    // open and sorts last. Terminal residue is never delayed:
                    // reclaiming it is the point of the cap.
                    let fresh = !terminal && now.saturating_sub(ts) < grace;
                    (path, terminal, ts, fresh)
                }
                // Corrupt or vanished: count it as the oldest terminal residue so
                // the cap can reclaim the slot rather than being wedged by it.
                Ok(None) | Err(_) => (path, true, 0, false),
            })
            .collect();
        // Terminal (0), then aged resumable (1), then in-grace resumable (2);
        // within each, oldest first. Fresh records are last, not skipped: a
        // store of 256 in-flight opens still accepts the 257th.
        records.sort_by(|a, b| {
            let rank = |terminal: bool, fresh: bool| -> u8 {
                if terminal {
                    0
                } else if fresh {
                    2
                } else {
                    1
                }
            };
            rank(a.1, a.3).cmp(&rank(b.1, b.3)).then(a.2.cmp(&b.2))
        });
        Self::reclaim_slots(records.into_iter().map(|(path, _, _, _)| path), evict)
    }

    /// Remove every record bound to one destination endpoint, ignoring the
    /// Free `needed` slots by deleting candidates in order, counting only the
    /// deletions THIS caller performed.
    ///
    /// The distinction is the whole function. A candidate that is already gone
    /// when the unlink runs was taken by a concurrent opener, and that opener
    /// took the slot for its own record: the file is gone, and no slot was
    /// freed for us. Counting it as ours is how the cap was exceeded.
    ///
    /// Two opens sharing a saturated store both scanned 256, both computed one
    /// eviction and both selected the same oldest record. One unlinked it, the
    /// other got `NotFound`, counted it, and wrote: 257 records in a store whose
    /// cap is 256, with a wider fan-out overshooting by more. Nothing reported
    /// it, because from inside each call the arithmetic was consistent.
    ///
    /// No lock is needed to fix it, and one was considered before being
    /// rejected. `unlink` IS the mutual exclusion: for a given path exactly one
    /// caller can succeed and every other gets `NotFound`, on Unix and on
    /// Windows alike, across processes and not merely across tasks. What was
    /// missing was not exclusion but bookkeeping: counting somebody else's
    /// success as our own. A lock file would add stale-lock recovery, a
    /// filesystem assumption and a new failure mode to buy a guarantee the
    /// kernel already gives. The GUI and the CLI running at once, which is the
    /// case that makes this real, are covered by the same argument.
    ///
    /// A stuck candidate (permissions, a Windows sharing violation, a read-only
    /// filesystem) is skipped rather than reselected forever, and the call fails
    /// only when the target cannot be met at all, which is the pre-existing
    /// behaviour and the reason this returns an error rather than shrugging.
    fn reclaim_slots(
        candidates: impl IntoIterator<Item = PathBuf>,
        needed: usize,
    ) -> Result<(), String> {
        let mut reclaimed = 0usize;
        let mut vanished = 0usize;
        let mut failures: Vec<String> = Vec::new();
        for path in candidates {
            if reclaimed == needed {
                break;
            }
            match fs::remove_file(&path) {
                Ok(()) => reclaimed += 1,
                // Gone before we got to it: somebody else's eviction, and
                // somebody else's slot. Move to the next candidate.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => vanished += 1,
                Err(e) => failures.push(format!("{}: {e}", path.display())),
            }
        }
        if reclaimed < needed {
            return Err(format!(
                "checkpoint cap reclaimed only {reclaimed} of {needed} records \
                 ({vanished} taken by a concurrent eviction): {}",
                failures.join("; ")
            ));
        }
        Ok(())
    }

    /// per-file remote path. This is the explicit, honest escape for a
    /// decommissioned server: the TTL scavenger only prunes a record when the
    /// same endpoint is revisited, so without this a server you stop using keeps
    /// its records forever. Returns the number of records removed.
    ///
    /// Reached from `aeroftp checkpoints forget`, paired with `endpoints()`
    /// behind `checkpoints list` because the four values below are matched
    /// exactly and nobody could otherwise supply them. There is deliberately no
    /// Tauri command: one existed briefly with nothing in the GUI calling it,
    /// which is an orphan export rather than a second way in.
    /// Every destination endpoint the store currently holds records for, with
    /// how many, newest activity first.
    ///
    /// `forget_endpoint` needs four exact strings to match on, and the provider
    /// field is a Debug-formatted enum rather than anything a user would guess.
    /// Without a way to read them back, the escape would be documented but
    /// unusable in practice, which is most of what was wrong with it.
    pub fn endpoints(&self) -> Result<Vec<(CheckpointDestinationIdentity, usize, u64)>, String> {
        let mut seen: Vec<(CheckpointDestinationIdentity, usize, u64)> = Vec::new();
        for entry in fs::read_dir(&self.dir)
            .map_err(|e| format!("cannot read transfer checkpoint directory: {e}"))?
        {
            let entry = entry.map_err(|e| format!("cannot read checkpoint entry: {e}"))?;
            let path = entry.path();
            if path.extension().and_then(|v| v.to_str()) != Some("json") {
                continue;
            }
            // Unreadable records are skipped rather than fatal, same rule as the
            // other readers: a listing must not be the one operation a corrupt
            // file can still break.
            let Ok(Some(record)) = self.load_path(&path) else {
                continue;
            };
            let d = &record.destination;
            match seen.iter_mut().find(|(id, _, _)| {
                id.provider == d.provider
                    && id.protocol == d.protocol
                    && id.host == d.host
                    && id.account == d.account
            }) {
                Some((_, count, newest)) => {
                    *count += 1;
                    *newest = (*newest).max(record.updated_unix_secs);
                }
                None => seen.push((
                    CheckpointDestinationIdentity {
                        remote_path: String::new(),
                        ..d.clone()
                    },
                    1,
                    record.updated_unix_secs,
                )),
            }
        }
        seen.sort_by_key(|entry| std::cmp::Reverse(entry.2));
        Ok(seen)
    }

    pub fn forget_endpoint(
        &self,
        provider: &str,
        protocol: &str,
        host: &str,
        account: &str,
    ) -> Result<usize, String> {
        let mut removed = 0usize;
        for entry in fs::read_dir(&self.dir)
            .map_err(|e| format!("cannot read transfer checkpoint directory: {e}"))?
        {
            let entry = entry.map_err(|e| format!("cannot read checkpoint entry: {e}"))?;
            let path = entry.path();
            if path.extension().and_then(|v| v.to_str()) != Some("json") {
                continue;
            }
            // Same rule as `stale_nonterminal`: an unreadable record must not
            // stop the sweep the user asked for. It cannot be matched against an
            // endpoint, so it is left for `enforce_cap` to reclaim under actual
            // capacity pressure. Deleting it here on sight would be tempting and
            // wrong: a transient EACCES or EMFILE produces the same error, and
            // destroying a valid resume record because the disk hiccuped costs
            // the user a large transfer.
            let record = match self.load_path(&path) {
                Ok(Some(record)) => record,
                Ok(None) => continue,
                Err(e) => {
                    tracing::warn!(
                        "[checkpoint] skipping unreadable record {}: {e}",
                        path.display()
                    );
                    continue;
                }
            };
            let d = &record.destination;
            if d.provider == provider
                && d.protocol == protocol
                && d.host == host
                && d.account == account
            {
                // Already gone is the end state this asked for, so the sweep is
                // idempotent: a concurrent cleanup must not make a retry fail on
                // a different file every time.
                match fs::remove_file(&path) {
                    Ok(()) => removed += 1,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => removed += 1,
                    Err(e) => return Err(format!("cannot remove transfer checkpoint: {e}")),
                }
            }
        }
        Ok(removed)
    }

    fn path_for(&self, transfer_key: &str) -> PathBuf {
        self.dir.join(format!("{transfer_key}.json"))
    }

    fn load_path(&self, path: &Path) -> Result<Option<MultipartCheckpoint>, String> {
        if !path.exists() {
            return Ok(None);
        }
        let data = fs::read(path).map_err(|e| format!("cannot read transfer checkpoint: {e}"))?;
        let mut record: MultipartCheckpoint = serde_json::from_slice(&data)
            .map_err(|e| format!("cannot parse transfer checkpoint: {e}"))?;
        migrate(&mut record)?;
        Ok(Some(record))
    }

    fn persist(&self, checkpoint: &MultipartCheckpoint) -> Result<(), String> {
        let data = serde_json::to_vec_pretty(checkpoint)
            .map_err(|e| format!("cannot serialize transfer checkpoint: {e}"))?;
        atomic_replace(&self.path_for(&checkpoint.transfer_key), &data)
    }
}

fn same_identity(a: &MultipartCheckpoint, b: &MultipartCheckpoint) -> bool {
    a.transfer_key == b.transfer_key
        && a.source == b.source
        && a.destination == b.destination
        && a.layout == b.layout
}

fn transfer_key(
    source: &CheckpointSourceIdentity,
    destination: &CheckpointDestinationIdentity,
    layout: &CheckpointLayout,
) -> String {
    let encoded = serde_json::to_vec(&(source, destination, layout))
        .expect("checkpoint identity is serializable");
    blake3::hash(&encoded).to_hex().to_string()
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn migrate(record: &mut MultipartCheckpoint) -> Result<(), String> {
    match record.schema_version {
        CHECKPOINT_SCHEMA_VERSION => Ok(()),
        2 => {
            // v2 introduced PayloadComplete/Committed but had NO durable verify
            // step: provider-complete alone advanced a record to Committed. v3
            // inserts a mandatory Verified state before Committed. Migration must
            // never let an old record become Verified: no v2 status maps to
            // Verified, so a non-terminal v2 record stays exactly where it was
            // and must pass the real VerifyChecksum node before it can commit. A
            // v2 Committed record genuinely finished under v2 semantics, so it
            // stays terminal: open_or_create treats it as terminal and restarts
            // the transfer from a fresh session rather than trusting or reusing
            // the old one. Pure version bump; no status is rewritten.
            record.schema_version = CHECKPOINT_SCHEMA_VERSION;
            Ok(())
        }
        1 => {
            // v1 carried the same identity and receipts but did not distinguish
            // payload-complete from committed. It was never allowed to report
            // visible completion, therefore a legacy terminal is safely retried
            // as payload-complete; it never becomes Verified or Committed by
            // migration and must pass the real verify + commit nodes.
            if record.status == CheckpointStatus::Committed {
                record.status = CheckpointStatus::PayloadComplete;
            }
            record.schema_version = CHECKPOINT_SCHEMA_VERSION;
            Ok(())
        }
        version => Err(format!("unsupported transfer checkpoint schema {version}")),
    }
}

fn atomic_replace(path: &Path, data: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "checkpoint has no parent directory".to_string())?;
    fs::create_dir_all(parent).map_err(|e| format!("cannot create checkpoint parent: {e}"))?;
    let tmp = parent.join(format!(
        ".{}.{}.{}.tmp",
        path.file_name()
            .and_then(|v| v.to_str())
            .unwrap_or("checkpoint"),
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .map_err(|e| format!("cannot create checkpoint temporary file: {e}"))?;
    file.write_all(data)
        .and_then(|_| file.sync_all())
        .map_err(|e| format!("cannot write transfer checkpoint: {e}"))?;
    drop(file);
    fs::rename(&tmp, path).map_err(|e| format!("cannot replace transfer checkpoint: {e}"))?;
    // Directory sync makes the rename durable on Unix. Some platforms reject
    // opening a directory as a file, where the atomic rename still remains the
    // correctness boundary and the next write repairs persistence.
    if let Ok(dir) = File::open(parent) {
        let _ = dir.sync_all();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn source() -> CheckpointSourceIdentity {
        CheckpointSourceIdentity {
            local_path: "/tmp/source.bin".into(),
            size: 10,
            modified_unix_nanos: Some(7),
        }
    }
    fn destination() -> CheckpointDestinationIdentity {
        CheckpointDestinationIdentity {
            provider: "s3".into(),
            protocol: "s3".into(),
            host: "test".into(),
            account: "acct".into(),
            remote_path: "/out.bin".into(),
        }
    }
    fn layout() -> CheckpointLayout {
        CheckpointLayout {
            total_size: 10,
            total_parts: 3,
            preferred_part_size: 4,
        }
    }

    #[test]
    fn restart_reuses_receipts_and_exposes_only_missing_parts() {
        let temp = TempDir::new().unwrap();
        let store = TransferCheckpointStore::new(temp.path()).unwrap();
        let mut first = store
            .open_or_create(source(), destination(), layout())
            .unwrap()
            .checkpoint;
        store.begin(&mut first, "session-1".into()).unwrap();
        store
            .record_receipt(
                &mut first,
                CheckpointPartReceipt {
                    part_number: 1,
                    etag: "a".into(),
                },
            )
            .unwrap();
        store
            .record_receipt(
                &mut first,
                CheckpointPartReceipt {
                    part_number: 3,
                    etag: "c".into(),
                },
            )
            .unwrap();

        let resumed = store
            .open_or_create(source(), destination(), layout())
            .unwrap();
        assert!(resumed.resumed);
        assert_eq!(resumed.checkpoint.upload_id.as_deref(), Some("session-1"));
        assert_eq!(resumed.checkpoint.missing_parts(), vec![2]);
        assert_eq!(resumed.checkpoint.attempts, 2);
    }

    #[test]
    fn commit_requires_every_receipt_and_persists_terminal_fact() {
        let temp = TempDir::new().unwrap();
        let store = TransferCheckpointStore::new(temp.path()).unwrap();
        let mut record = store
            .open_or_create(source(), destination(), layout())
            .unwrap()
            .checkpoint;
        store.begin(&mut record, "session-1".into()).unwrap();
        assert!(store.mark_committed(&mut record).is_err());
        for part in 1..=3 {
            store
                .record_receipt(
                    &mut record,
                    CheckpointPartReceipt {
                        part_number: part,
                        etag: part.to_string(),
                    },
                )
                .unwrap();
        }
        // Fail-closed: an unverified payload cannot be committed.
        assert!(
            store.mark_committed(&mut record).is_err(),
            "commit must be blocked until the payload is durably verified"
        );
        assert_eq!(record.status, CheckpointStatus::PayloadComplete);
        store
            .mark_verified(&mut record)
            .expect("payload verifies once all receipts are present");
        assert_eq!(record.status, CheckpointStatus::Verified);
        store.mark_committed(&mut record).unwrap();
        assert_eq!(record.status, CheckpointStatus::Committed);
        let reopened = store
            .open_or_create(source(), destination(), layout())
            .unwrap();
        assert!(
            !reopened.resumed,
            "committed records are never blindly reported as complete after restart"
        );
    }

    /// Success writes Verified then Committed, in that order, and neither step
    /// can be skipped or reordered.
    #[test]
    fn verify_then_commit_is_ordered_and_neither_step_is_skippable() {
        let temp = TempDir::new().unwrap();
        let store = TransferCheckpointStore::new(temp.path()).unwrap();
        let mut record = store
            .open_or_create(source(), destination(), layout())
            .unwrap()
            .checkpoint;
        store.begin(&mut record, "session-1".into()).unwrap();
        for part in 1..=3 {
            store
                .record_receipt(
                    &mut record,
                    CheckpointPartReceipt {
                        part_number: part,
                        etag: part.to_string(),
                    },
                )
                .unwrap();
        }
        // Commit before verify is refused, leaving the record unchanged.
        assert!(store.mark_committed(&mut record).is_err());
        assert_eq!(record.status, CheckpointStatus::PayloadComplete);
        // Verify, then commit, in order.
        store.mark_verified(&mut record).unwrap();
        assert_eq!(record.status, CheckpointStatus::Verified);
        // Verify is idempotent (a resumed attempt may re-run it).
        store.mark_verified(&mut record).unwrap();
        assert_eq!(record.status, CheckpointStatus::Verified);
        store.mark_committed(&mut record).unwrap();
        assert_eq!(record.status, CheckpointStatus::Committed);
    }

    /// Truthful verification fails closed when the source changed under the
    /// transfer, and a failed verification never advances the record so commit
    /// stays blocked.
    #[test]
    fn verification_fails_closed_and_blocks_commit_when_source_changes() {
        let temp = TempDir::new().unwrap();
        let store = TransferCheckpointStore::new(temp.path()).unwrap();
        let mut record = store
            .open_or_create(source(), destination(), layout())
            .unwrap()
            .checkpoint;
        store.begin(&mut record, "session-1".into()).unwrap();
        for part in 1..=3 {
            store
                .record_receipt(
                    &mut record,
                    CheckpointPartReceipt {
                        part_number: part,
                        etag: part.to_string(),
                    },
                )
                .unwrap();
        }
        // source().size == 10 == layout().total_size. A same-size observation
        // verifies; a changed size or a vanished source does not.
        let matching = ObservedSource {
            exists: true,
            size: 10,
            modified_unix_nanos: Some(7),
        };
        assert!(record.verify_against_source(&matching).is_ok());

        let grew = ObservedSource {
            exists: true,
            size: 11,
            modified_unix_nanos: Some(7),
        };
        assert!(record.verify_against_source(&grew).is_err());
        let vanished = ObservedSource {
            exists: false,
            size: 10,
            modified_unix_nanos: Some(7),
        };
        assert!(record.verify_against_source(&vanished).is_err());
        let retouched = ObservedSource {
            exists: true,
            size: 10,
            modified_unix_nanos: Some(9),
        };
        assert!(record.verify_against_source(&retouched).is_err());

        // The record is still only PayloadComplete, so commit remains blocked.
        assert_eq!(record.status, CheckpointStatus::PayloadComplete);
        assert!(store.mark_committed(&mut record).is_err());
    }

    /// A crash between verify and commit is safe: the persisted Verified record
    /// is resumable (not terminal), reopens without discarding receipts, and can
    /// be re-verified then committed. A committed record restarts fresh.
    #[test]
    fn crash_between_verify_and_commit_is_resumable_and_safe() {
        let temp = TempDir::new().unwrap();
        let store = TransferCheckpointStore::new(temp.path()).unwrap();
        let mut record = store
            .open_or_create(source(), destination(), layout())
            .unwrap()
            .checkpoint;
        store.begin(&mut record, "session-1".into()).unwrap();
        for part in 1..=3 {
            store
                .record_receipt(
                    &mut record,
                    CheckpointPartReceipt {
                        part_number: part,
                        etag: part.to_string(),
                    },
                )
                .unwrap();
        }
        store.mark_verified(&mut record).unwrap();
        assert_eq!(record.status, CheckpointStatus::Verified);
        assert!(
            record.is_resumable(),
            "a verified-but-uncommitted record resumes"
        );

        // Simulate a crash before commit: reopen from disk.
        let reopened = store
            .open_or_create(source(), destination(), layout())
            .unwrap();
        assert!(reopened.resumed);
        let mut resumed = reopened.checkpoint;
        assert_eq!(
            resumed.missing_parts(),
            Vec::<u32>::new(),
            "no part is re-uploaded on resume"
        );
        // Reopening rewinds the durable status to Transferring: the verified fact
        // is re-derived, never trusted across the restart.
        assert_eq!(resumed.status, CheckpointStatus::Transferring);
        store.mark_verified(&mut resumed).unwrap();
        store.mark_committed(&mut resumed).unwrap();
        assert_eq!(resumed.status, CheckpointStatus::Committed);

        // A committed record restarts completely fresh (new session).
        let after_commit = store
            .open_or_create(source(), destination(), layout())
            .unwrap();
        assert!(!after_commit.resumed);
        assert!(after_commit.checkpoint.receipts.is_empty());
    }

    /// A v2 record migrates with a pure version bump: a non-terminal v2 record
    /// keeps its exact status (it is NOT promoted to Verified or Committed and
    /// must pass the real verify + commit nodes), and a v2 Committed record
    /// stays terminal so its transfer restarts from a fresh session.
    #[test]
    fn schema_v2_migrates_without_manufacturing_verified_or_committed() {
        let temp = TempDir::new().unwrap();
        let store = TransferCheckpointStore::new(temp.path()).unwrap();

        // v2 payload-complete record: stays payload-complete after migration.
        let mut payload = MultipartCheckpoint::fresh(source(), destination(), layout());
        payload.schema_version = 2;
        payload.upload_id = Some("v2-session".into());
        for part in 1..=3 {
            payload.receipts.insert(
                part,
                CheckpointPartReceipt {
                    part_number: part,
                    etag: part.to_string(),
                },
            );
        }
        payload.status = CheckpointStatus::PayloadComplete;
        let path = store.path_for(&payload.transfer_key);
        fs::write(&path, serde_json::to_vec(&payload).unwrap()).unwrap();
        let migrated = store.load_path(&path).unwrap().unwrap();
        assert_eq!(migrated.schema_version, CHECKPOINT_SCHEMA_VERSION);
        assert_eq!(
            migrated.status,
            CheckpointStatus::PayloadComplete,
            "a v2 payload-complete record is never promoted by migration"
        );

        // v2 committed record: stays terminal, so a reopen starts fresh.
        let mut committed = MultipartCheckpoint::fresh(source(), destination(), layout());
        committed.schema_version = 2;
        committed.status = CheckpointStatus::Committed;
        let cpath = store.path_for(&committed.transfer_key);
        fs::write(&cpath, serde_json::to_vec(&committed).unwrap()).unwrap();
        let migrated_committed = store.load_path(&cpath).unwrap().unwrap();
        assert_eq!(migrated_committed.schema_version, CHECKPOINT_SCHEMA_VERSION);
        assert_eq!(migrated_committed.status, CheckpointStatus::Committed);
        let reopened = store
            .open_or_create(source(), destination(), layout())
            .unwrap();
        assert!(
            !reopened.resumed,
            "a migrated committed record restarts from a fresh session"
        );
    }

    #[test]
    fn mismatched_identity_starts_fresh_and_never_reuses_receipts() {
        let temp = TempDir::new().unwrap();
        let store = TransferCheckpointStore::new(temp.path()).unwrap();
        let mut record = store
            .open_or_create(source(), destination(), layout())
            .unwrap()
            .checkpoint;
        store.begin(&mut record, "session-1".into()).unwrap();
        store
            .record_receipt(
                &mut record,
                CheckpointPartReceipt {
                    part_number: 1,
                    etag: "a".into(),
                },
            )
            .unwrap();
        let mut changed = destination();
        changed.remote_path = "/other.bin".into();
        let opened = store.open_or_create(source(), changed, layout()).unwrap();
        assert!(!opened.resumed);
        assert!(opened.checkpoint.receipts.is_empty());
    }

    #[test]
    fn schema_v1_migrates_without_trusting_legacy_terminal() {
        let temp = TempDir::new().unwrap();
        let store = TransferCheckpointStore::new(temp.path()).unwrap();
        let mut record = MultipartCheckpoint::fresh(source(), destination(), layout());
        record.schema_version = 1;
        record.status = CheckpointStatus::Committed;
        let path = store.path_for(&record.transfer_key);
        fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
        let migrated = store.load_path(&path).unwrap().unwrap();
        assert_eq!(migrated.schema_version, CHECKPOINT_SCHEMA_VERSION);
        assert_eq!(migrated.status, CheckpointStatus::PayloadComplete);
    }

    #[test]
    fn stale_scavenger_returns_only_expired_nonterminal_records() {
        let temp = TempDir::new().unwrap();
        let store = TransferCheckpointStore::with_ttl(temp.path(), Duration::from_secs(1)).unwrap();
        let mut record = store
            .open_or_create(source(), destination(), layout())
            .unwrap()
            .checkpoint;
        store.begin(&mut record, "session-1".into()).unwrap();
        record.updated_unix_secs = now_secs().saturating_sub(2);
        store.persist(&record).unwrap();
        assert_eq!(store.stale_nonterminal().unwrap().len(), 1);
        store.remove(&record.transfer_key).unwrap();
        assert!(store.stale_nonterminal().unwrap().is_empty());
    }

    fn present_keys(dir: &Path) -> std::collections::HashSet<String> {
        fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|v| v.to_str()) == Some("json"))
            .filter_map(|e| {
                e.path()
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .map(str::to_string)
            })
            .collect()
    }

    fn record_at(remote: &str, status: CheckpointStatus, ts: u64) -> MultipartCheckpoint {
        let dest = CheckpointDestinationIdentity {
            remote_path: remote.into(),
            ..destination()
        };
        let mut rec = MultipartCheckpoint::fresh(source(), dest, layout());
        rec.status = status;
        rec.updated_unix_secs = ts;
        rec
    }

    #[test]
    fn cap_evicts_oldest_resumable_and_never_the_opening_record() {
        let temp = TempDir::new().unwrap();
        let store =
            TransferCheckpointStore::with_limits(temp.path(), DEFAULT_CHECKPOINT_TTL, 3).unwrap();
        // Five distinct resumable records, strictly ageing oldest to newest.
        let keys: Vec<String> = (0..5u64)
            .map(|i| {
                let rec = record_at(
                    &format!("/f{i}.bin"),
                    CheckpointStatus::Transferring,
                    1000 + i,
                );
                store.persist(&rec).unwrap();
                rec.transfer_key
            })
            .collect();
        // The record being opened is the OLDEST, yet the cap must spare it.
        store.enforce_cap(&keys[0]).unwrap();
        let present = present_keys(temp.path());
        assert_eq!(present.len(), 3);
        assert!(
            present.contains(&keys[0]),
            "opening record is never evicted"
        );
        assert!(present.contains(&keys[4]));
        assert!(present.contains(&keys[3]));
        assert!(!present.contains(&keys[1]));
        assert!(!present.contains(&keys[2]));
    }

    /// The deterministic pin: a candidate that is already gone did not free a
    /// slot for THIS caller, so it must not be counted as one.
    ///
    /// This is the whole defect in one call. The eviction loop treated
    /// `NotFound` as a success, with a comment saying it was "the desired end
    /// state": true of the file, false of the slot. The file was taken by a
    /// concurrent opener that then used the slot for its own record, so the
    /// caller that merely watched it disappear wrote on top of a full store.
    ///
    /// Run against the previous behaviour, `first` survives and nothing is
    /// reclaimed, because the loop stops as soon as it has counted one.
    #[test]
    fn a_vanished_candidate_does_not_count_as_a_slot_we_freed() {
        let temp = TempDir::new().unwrap();
        let taken_by_someone_else = temp.path().join("already-evicted.json");
        let first = temp.path().join("first.json");
        let second = temp.path().join("second.json");
        fs::write(&first, b"{}").unwrap();
        fs::write(&second, b"{}").unwrap();

        TransferCheckpointStore::reclaim_slots(
            [taken_by_someone_else.clone(), first.clone(), second.clone()],
            1,
        )
        .unwrap();

        assert!(
            !first.exists(),
            "the vanished candidate was counted as ours and no slot was actually freed",
        );
        assert!(
            second.exists(),
            "only the one slot that was needed should be reclaimed",
        );
    }

    /// And the failure is reported rather than hidden when the candidates run
    /// out: a store whose only candidates were taken by other openers cannot
    /// grow, and saying so is the contract `enforce_cap` already had.
    #[test]
    fn reclaiming_nothing_is_an_error_and_names_the_reason() {
        let temp = TempDir::new().unwrap();
        let gone = temp.path().join("gone.json");
        let err = TransferCheckpointStore::reclaim_slots([gone], 1).unwrap_err();
        assert!(
            err.contains("reclaimed only 0 of 1"),
            "the error must say how far it got: {err}",
        );
        assert!(
            err.contains("concurrent eviction"),
            "and why, so the next reader is not left guessing: {err}",
        );
    }

    /// The ordering change has one risk and this is it: writing the record
    /// before enforcing the cap could leave it behind when the store turns out
    /// to have no room, which is exactly what the previous order existed to
    /// prevent. The rollback is what keeps that promise, and this fails without
    /// it.
    ///
    /// The eviction is made to fail deterministically rather than by timing: a
    /// DIRECTORY named like a record is an unremovable candidate (`remove_file`
    /// gives `IsADirectory`, not `NotFound`), so the cap cannot be met and the
    /// open must refuse.
    #[test]
    fn a_store_that_cannot_evict_does_not_keep_the_record_it_refused() {
        let temp = TempDir::new().unwrap();
        let store =
            TransferCheckpointStore::with_limits(temp.path(), DEFAULT_CHECKPOINT_TTL, 1).unwrap();
        fs::create_dir(temp.path().join("unremovable.json")).unwrap();

        let key = MultipartCheckpoint::fresh(source(), destination(), layout()).transfer_key;
        let err = store
            .open_or_create(source(), destination(), layout())
            .unwrap_err();
        assert!(
            err.contains("reclaimed only"),
            "the refusal must say why: {err}",
        );
        assert!(
            !store.path_for(&key).exists(),
            "a store that said it could not hold the record must not be holding it",
        );
    }

    /// Three openers, which is the shape a reviewer raised against the counting
    /// fix alone: with the cap enforced BEFORE the write, one opener deletes its
    /// victim and has not written yet, and a third opener scanning in that
    /// window reads capacity that is already spoken for, evicts nothing, and
    /// writes. Reserving the slot first removes the window rather than guarding
    /// it: the claim is on disk for every other scan to count.
    ///
    /// What this test can and cannot do. It exercises the invariant with real
    /// concurrency, and it is the reason the fan-out case is not left to an
    /// argument alone; it cannot FORCE the interleaving, because the store has
    /// no hook between its scan and its write. The property is carried by the
    /// ordering, and the deterministic pin for the ordering is the rollback
    /// test above.
    #[test]
    fn three_overlapping_opens_do_not_push_the_store_past_the_cap() {
        const CAP: usize = 8;
        let temp = TempDir::new().unwrap();
        let store =
            TransferCheckpointStore::with_limits(temp.path(), DEFAULT_CHECKPOINT_TTL, CAP).unwrap();
        for i in 0..CAP {
            let r = record_at(
                &format!("/filled-{i}.bin"),
                CheckpointStatus::Transferring,
                1000 + i as u64,
            );
            store.persist(&r).unwrap();
        }

        let barrier = std::sync::Barrier::new(3);
        std::thread::scope(|scope| {
            for i in 0..3 {
                let store = &store;
                let barrier = &barrier;
                scope.spawn(move || {
                    // A distinct source path is what makes a distinct key, so
                    // the three openers really are three admissions and not one
                    // record opened three times.
                    let mut opening = source();
                    opening.local_path = format!("/opening-{i}.bin");
                    barrier.wait();
                    store
                        .open_or_create(opening, destination(), layout())
                        .unwrap();
                });
            }
        });

        let present = present_keys(temp.path()).len();
        assert!(
            present <= CAP,
            "{present} records in a store capped at {CAP}",
        );
    }

    /// The same thing end to end, with two openers actually overlapping.
    ///
    /// Honest about what this is: the interleaving is not forced, so this test
    /// reproduces the overshoot only when the two scans really do straddle the
    /// unlink. That is why the pin above exists and is the one that holds the
    /// behaviour; this one is here because an invariant stated at the level the
    /// user sees ("the store never exceeds its cap") is worth asserting even
    /// when the path to breaking it is timing.
    ///
    /// It also covers the case the fix is FOR, which is two PROCESSES rather
    /// than two tasks: threads are what a test can run, and the exclusion the
    /// fix relies on is the kernel's, not the runtime's. `unlink` gives exactly
    /// one caller success per path whether the callers share an address space
    /// or not, so the GUI and the CLI are covered by the same mechanism this
    /// exercises.
    #[test]
    fn two_overlapping_opens_do_not_push_the_store_past_the_cap() {
        const CAP: usize = 8;
        let temp = TempDir::new().unwrap();
        let store =
            TransferCheckpointStore::with_limits(temp.path(), DEFAULT_CHECKPOINT_TTL, CAP).unwrap();
        // A FULL store, which is the only state in which the cap runs at all. A
        // test that starts empty never reaches the branch.
        for i in 0..CAP {
            let r = record_at(
                &format!("/filled-{i}.bin"),
                CheckpointStatus::Transferring,
                1000 + i as u64,
            );
            store.persist(&r).unwrap();
        }
        assert_eq!(present_keys(temp.path()).len(), CAP);

        let barrier = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            for i in 0..2 {
                let store = &store;
                let barrier = &barrier;
                scope.spawn(move || {
                    let opening = record_at(
                        &format!("/opening-{i}.bin"),
                        CheckpointStatus::Transferring,
                        9000 + i as u64,
                    );
                    barrier.wait();
                    store.enforce_cap(&opening.transfer_key).unwrap();
                    store.persist(&opening).unwrap();
                });
            }
        });

        let present = present_keys(temp.path()).len();
        assert!(
            present <= CAP,
            "the cap is a bound, not an average: {present} records in a store capped at {CAP}",
        );
    }

    #[test]
    fn cap_evicts_terminal_residue_before_a_newer_resumable() {
        let temp = TempDir::new().unwrap();
        let store =
            TransferCheckpointStore::with_limits(temp.path(), DEFAULT_CHECKPOINT_TTL, 2).unwrap();
        // A committed record that is the NEWEST by time, plus two older resumable
        // records. The cap must still drop the terminal one first.
        let terminal = record_at("/done.bin", CheckpointStatus::Committed, 9000);
        let resumable_old = record_at("/old.bin", CheckpointStatus::Transferring, 1000);
        let resumable_new = record_at("/new.bin", CheckpointStatus::Transferring, 2000);
        for r in [&terminal, &resumable_old, &resumable_new] {
            store.persist(r).unwrap();
        }
        // Opening a fourth, never-persisted key: three on disk, cap 2, evict two.
        store.enforce_cap("opening-key-not-on-disk").unwrap();
        let present = present_keys(temp.path());
        assert_eq!(present.len(), 1);
        assert!(present.contains(&resumable_new.transfer_key));
        assert!(!present.contains(&terminal.transfer_key));
        assert!(!present.contains(&resumable_old.transfer_key));
    }

    /// R-01. When an older occupant exists, a concurrent open must evict that
    /// one rather than a sibling still inside the grace window.
    #[test]
    fn cap_prefers_an_old_record_over_a_fresh_sibling() {
        let temp = TempDir::new().unwrap();
        let store =
            TransferCheckpointStore::with_limits(temp.path(), DEFAULT_CHECKPOINT_TTL, 2).unwrap();
        let now = now_secs();
        let aged = now.saturating_sub(DEFAULT_CHECKPOINT_EVICT_GRACE.as_secs() + 1);
        let old = record_at("/old.bin", CheckpointStatus::Transferring, aged);
        let fresh = record_at("/fresh.bin", CheckpointStatus::Transferring, now);
        store.persist(&old).unwrap();
        store.persist(&fresh).unwrap();
        store
            .enforce_cap("opening-key-not-on-disk")
            .expect("cap must still hold");
        let present = present_keys(temp.path());
        assert_eq!(present.len(), 1);
        assert!(
            present.contains(&fresh.transfer_key),
            "the in-grace sibling is not the first pick"
        );
        assert!(!present.contains(&old.transfer_key));
    }

    /// A DAG fan-out that fills the cap with in-flight records inside the
    /// grace window must still accept the next open. The cap holds by
    /// dropping the oldest occupant, which is the documented cost; the open
    /// itself must not fail.
    #[test]
    fn cap_still_holds_when_every_occupant_is_within_grace() {
        let temp = TempDir::new().unwrap();
        let store =
            TransferCheckpointStore::with_limits(temp.path(), DEFAULT_CHECKPOINT_TTL, 2).unwrap();
        let now = now_secs();
        let older = record_at(
            "/a.bin",
            CheckpointStatus::Transferring,
            now.saturating_sub(1),
        );
        let newer = record_at("/b.bin", CheckpointStatus::Transferring, now);
        store.persist(&older).unwrap();
        store.persist(&newer).unwrap();
        store
            .enforce_cap("opening-key-not-on-disk")
            .expect("a full store of fresh records must not refuse the next open");
        let present = present_keys(temp.path());
        assert_eq!(present.len(), 1, "the cap still holds");
        assert!(present.contains(&newer.transfer_key));
        assert!(!present.contains(&older.transfer_key));
    }

    /// A commit that just finished is residue, not an in-flight opener. The
    /// grace window does not protect it.
    #[test]
    fn cap_still_evicts_fresh_terminal_residue() {
        let temp = TempDir::new().unwrap();
        let store =
            TransferCheckpointStore::with_limits(temp.path(), DEFAULT_CHECKPOINT_TTL, 2).unwrap();
        let now = now_secs();
        let terminal = record_at("/done.bin", CheckpointStatus::Committed, now);
        let live = record_at("/live.bin", CheckpointStatus::Transferring, now);
        store.persist(&terminal).unwrap();
        store.persist(&live).unwrap();
        store
            .enforce_cap("opening-key-not-on-disk")
            .expect("terminal residue must still be reclaimable");
        let present = present_keys(temp.path());
        assert_eq!(present.len(), 1);
        assert!(present.contains(&live.transfer_key));
        assert!(!present.contains(&terminal.transfer_key));
    }

    #[test]
    fn cap_evicts_a_resumable_record_once_the_grace_expires() {
        let temp = TempDir::new().unwrap();
        let store =
            TransferCheckpointStore::with_limits(temp.path(), DEFAULT_CHECKPOINT_TTL, 2).unwrap();
        let expired = now_secs().saturating_sub(DEFAULT_CHECKPOINT_EVICT_GRACE.as_secs() + 1);
        let older = record_at("/old.bin", CheckpointStatus::Transferring, expired - 10);
        let newer = record_at("/new.bin", CheckpointStatus::Transferring, expired);
        store.persist(&older).unwrap();
        store.persist(&newer).unwrap();
        store
            .enforce_cap("opening-key-not-on-disk")
            .expect("expired records are ordinary eviction candidates");
        let present = present_keys(temp.path());
        assert_eq!(present.len(), 1);
        assert!(present.contains(&newer.transfer_key));
        assert!(!present.contains(&older.transfer_key));
    }

    /// Pre-tag audit. Making the cap fail loudly created a worse failure than
    /// the one it fixed: the record was persisted FIRST, so a store that could
    /// not evict returned an error to the caller with the new record already on
    /// disk, growing past the cap in the very call that reported it could not
    /// hold it. And `open_or_create` was the one reader still propagating a read
    /// error, so an unreadable record under this transfer's own key failed the
    /// transfer instead of starting a fresh one.
    #[test]
    fn an_unreadable_own_record_starts_fresh_instead_of_failing() {
        let temp = TempDir::new().unwrap();
        let store = TransferCheckpointStore::new(temp.path()).unwrap();
        let fresh = MultipartCheckpoint::fresh(source(), destination(), layout());
        let path = store.path_for(&fresh.transfer_key);
        // A record written by a schema this build refuses, under the key this
        // transfer is about to use.
        fs::write(&path, br#"{"schema_version":99,"transfer_key":"x"}"#).unwrap();

        let opened = store
            .open_or_create(source(), destination(), layout())
            .expect("an unreadable own record must not fail the transfer");
        assert!(!opened.resumed, "there is nothing to resume from");
        // And it was replaced, not left to wedge the next attempt too.
        let reread = store.load_path(&path).expect("now readable");
        assert!(reread.is_some());
    }

    /// The store survives installs, so a user who downgrades after a bad release
    /// meets a record written by a schema this build refuses. `stale_nonterminal`
    /// runs at the start of EVERY multipart transfer and used to propagate that
    /// error, so one unreadable file failed every upload to every provider, and
    /// `enforce_cap`, which reclaims such records, runs afterwards and was never
    /// reached. Unreadable means not resumable, which is all this is asked.
    #[test]
    fn one_unreadable_record_does_not_fail_every_transfer() {
        let temp = TempDir::new().unwrap();
        let store = TransferCheckpointStore::new(temp.path()).unwrap();
        let good = store
            .open_or_create(source(), destination(), layout())
            .unwrap()
            .checkpoint;
        // A record from a schema this build does not know, plus outright garbage.
        fs::write(
            temp.path().join("future.json"),
            br#"{"schema_version":99,"transfer_key":"future"}"#,
        )
        .unwrap();
        fs::write(temp.path().join("garbage.json"), b"not json at all").unwrap();

        let stale = store
            .stale_nonterminal()
            .expect("must not fail the transfer");
        assert!(stale.iter().all(|r| r.transfer_key != "future"));

        // The sweep must not abort on them either, and must still remove the
        // records it was asked about.
        let removed = store.forget_endpoint("s3", "s3", "test", "acct").unwrap();
        assert_eq!(removed, 1, "the readable matching record must be removed");
        assert!(!store.path_for(&good.transfer_key).exists());
        // The unreadable ones are left for the cap, not deleted on sight: a
        // transient EACCES looks identical and must not destroy a resume record.
        assert!(temp.path().join("future.json").exists());
    }

    /// The cap advertises a hard bound. Ignoring every unlink error made it a
    /// claim instead: an undeletable oldest candidate was reselected forever
    /// while new records kept being accepted.
    #[test]
    fn the_cap_reports_when_it_cannot_evict_enough() {
        let temp = TempDir::new().unwrap();
        let store = TransferCheckpointStore::with_limits(
            temp.path(),
            std::time::Duration::from_secs(3600),
            2,
        )
        .unwrap();
        for p in ["/a.bin", "/b.bin"] {
            let dest = CheckpointDestinationIdentity {
                remote_path: p.into(),
                ..destination()
            };
            let mut rec = store
                .open_or_create(source(), dest, layout())
                .unwrap()
                .checkpoint;
            // Age them past the grace window so this test is about unlink
            // success, not about protecting a concurrent opener.
            rec.updated_unix_secs = now_secs()
                .saturating_sub(DEFAULT_CHECKPOINT_EVICT_GRACE.as_secs().saturating_add(1));
            store.persist(&rec).unwrap();
        }
        // A third open must evict one and succeed while the directory is writable.
        let dest = CheckpointDestinationIdentity {
            remote_path: "/c.bin".into(),
            ..destination()
        };
        store
            .open_or_create(source(), dest, layout())
            .expect("eviction must succeed on a writable store");
        let count = fs::read_dir(temp.path()).unwrap().count();
        assert!(count <= 2, "the cap must hold: {count} records");
    }

    /// `endpoints()` is what makes the escape usable: `forget_endpoint` matches
    /// four exact strings and `provider` is an internal name, so without a way
    /// to read them back the escape is documented but unusable.
    #[test]
    fn endpoints_groups_records_and_survives_an_unreadable_one() {
        let temp = TempDir::new().unwrap();
        let store = TransferCheckpointStore::new(temp.path()).unwrap();
        for p in ["/a.bin", "/b.bin"] {
            let dest = CheckpointDestinationIdentity {
                remote_path: p.into(),
                ..destination()
            };
            store.open_or_create(source(), dest, layout()).unwrap();
        }
        let other = CheckpointDestinationIdentity {
            account: "other".into(),
            ..destination()
        };
        store.open_or_create(source(), other, layout()).unwrap();
        fs::write(temp.path().join("garbage.json"), b"nope").unwrap();

        let endpoints = store.endpoints().expect("listing must not fail");
        assert_eq!(endpoints.len(), 2, "two distinct accounts");
        let acct = endpoints
            .iter()
            .find(|(id, _, _)| id.account == "acct")
            .expect("acct endpoint");
        assert_eq!(acct.1, 2, "two records grouped under one endpoint");
        // The remote path is not part of the endpoint identity and is cleared,
        // so the four values printed are exactly the four `forget` matches on.
        assert!(acct.0.remote_path.is_empty());
    }

    #[test]
    fn forget_endpoint_removes_only_the_matching_destination() {
        let temp = TempDir::new().unwrap();
        let store = TransferCheckpointStore::new(temp.path()).unwrap();
        // Two files on the endpoint to forget (same account, different paths).
        for p in ["/a.bin", "/b.bin"] {
            let dest = CheckpointDestinationIdentity {
                remote_path: p.into(),
                ..destination()
            };
            store.open_or_create(source(), dest, layout()).unwrap();
        }
        // One file on a different account must survive.
        let other = CheckpointDestinationIdentity {
            account: "other".into(),
            ..destination()
        };
        let survivor = store
            .open_or_create(source(), other, layout())
            .unwrap()
            .checkpoint;
        let removed = store.forget_endpoint("s3", "s3", "test", "acct").unwrap();
        assert_eq!(removed, 2);
        assert!(store.path_for(&survivor.transfer_key).exists());
        // Forgetting an endpoint with no records is zero, not an error.
        assert_eq!(
            store.forget_endpoint("s3", "s3", "nope", "acct").unwrap(),
            0
        );
    }
}
