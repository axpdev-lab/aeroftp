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
}

impl TransferCheckpointStore {
    pub fn new(dir: impl Into<PathBuf>) -> Result<Self, String> {
        Self::with_ttl(dir, DEFAULT_CHECKPOINT_TTL)
    }

    pub fn with_ttl(dir: impl Into<PathBuf>, ttl: Duration) -> Result<Self, String> {
        let dir = dir.into();
        fs::create_dir_all(&dir)
            .map_err(|e| format!("cannot create transfer checkpoint directory: {e}"))?;
        Ok(Self { dir, ttl })
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
        let Some(mut loaded) = self.load_path(&path)? else {
            self.persist(&fresh)?;
            return Ok(CheckpointOpen {
                checkpoint: fresh,
                resumed: false,
            });
        };
        if !same_identity(&loaded, &fresh) || loaded.status.is_terminal() {
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
            if let Some(record) = self.load_path(&entry.path())? {
                if !record.status.is_terminal()
                    && now.saturating_sub(record.updated_unix_secs) >= self.ttl.as_secs()
                {
                    stale.push(record);
                }
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
}
