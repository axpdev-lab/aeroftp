//! Local memory of bytes uploaded by this client. No baseline network reads.
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use super::s3_delta_plan::{delta_grid_fits, delta_grid_size, S3_MAX_PARTS};
use crate::transfer_dag::EndpointIdentity;
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tokio::io::AsyncReadExt;

const ROW_LIMIT: usize = 10_000;
const DIGEST_LIMIT: usize = S3_MAX_PARTS as usize;
const ALGORITHM: &str = "sha256";
const VERSION: u32 = 1;

#[derive(Clone, Debug)]
pub struct BaselineKey {
    identity: String,
    bucket: String,
    key: String,
}

impl BaselineKey {
    pub fn new(identity: EndpointIdentity, bucket: &str, key: &str) -> Self {
        Self {
            identity: serde_json::json!([identity.protocol, identity.host, identity.account])
                .to_string(),
            bucket: bucket.to_owned(),
            key: key.to_owned(),
        }
    }
}

/// Hashes the object grid independently of signing and upload part boundaries.
/// Kept in memory until the upload has successfully returned its own ETag.
#[derive(Clone)]
pub struct BaselineHasher {
    size: u64,
    grid: u64,
    seen: u64,
    cell_len: u64,
    hash: Sha256,
    digests: Vec<[u8; 32]>,
}

impl BaselineHasher {
    pub fn new(size: u64) -> Option<Self> {
        Some(Self {
            size,
            grid: delta_grid_size(size)?,
            seen: 0,
            cell_len: 0,
            hash: Sha256::new(),
            digests: Vec::new(),
        })
    }

    pub fn update(&mut self, mut bytes: &[u8]) {
        self.seen = self.seen.saturating_add(bytes.len() as u64);
        // A source that grows during upload must not allocate unbounded rows.
        if self.seen > self.size {
            return;
        }
        while !bytes.is_empty() {
            let take = (self.grid - self.cell_len).min(bytes.len() as u64) as usize;
            self.hash.update(&bytes[..take]);
            self.cell_len += take as u64;
            bytes = &bytes[take..];
            if self.cell_len == self.grid {
                self.digests.push(self.hash.finalize_reset().into());
                self.cell_len = 0;
            }
        }
    }

    pub(crate) fn verification(&self) -> Option<PreparedDigests> {
        let row = self.clone().finish("pending-verification")?;
        Some(PreparedDigests {
            size: row.size,
            grid: row.grid,
            digests: row.digests,
        })
    }

    fn finish(mut self, etag: &str) -> Option<Baseline> {
        if self.seen != self.size || normalize_etag(etag).is_empty() {
            return None;
        }
        if self.cell_len != 0 {
            self.digests.push(self.hash.finalize().into());
        }
        Some(Baseline {
            etag: etag.to_owned(),
            size: self.size,
            grid: self.grid,
            digests: self.digests,
        })
    }
}

struct Baseline {
    etag: String,
    size: u64,
    grid: u64,
    digests: Vec<[u8; 32]>,
}

fn normalize_etag(etag: &str) -> &str {
    etag.trim().trim_matches('"')
}

#[derive(Debug, PartialEq, Eq)]
pub enum MatchRefusal {
    NoBaseline,
    EtagMismatch,
    SizeMismatch,
    GridMismatch,
    InvalidRow,
}

#[derive(Debug, PartialEq, Eq)]
pub enum MatchOutcome {
    Refused(MatchRefusal),
    Matches {
        grid: u64,
        matches: Vec<(u64, u64, u64)>,
    },
}

/// Per-user SQLite cache. Open and transact on a blocking worker; never keep a
/// connection locked while reading the source or performing an HTTP request.
#[derive(Clone)]
pub struct BaselineStore {
    path: PathBuf,
}

impl BaselineStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn default_path() -> Option<PathBuf> {
        crate::portable::cli_app_config_dir().map(|p| p.join("s3_delta_baselines.db"))
    }

    fn open(&self) -> Result<Connection, String> {
        let conn = Connection::open(&self.path).map_err(|e| e.to_string())?;
        conn.busy_timeout(std::time::Duration::from_secs(2))
            .map_err(|e| e.to_string())?;
        conn.execute_batch(&format!(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             CREATE TABLE IF NOT EXISTS baselines (
               identity TEXT NOT NULL, bucket TEXT NOT NULL, object_key TEXT NOT NULL,
               etag TEXT NOT NULL, size INTEGER NOT NULL, grid INTEGER NOT NULL,
               algorithm TEXT NOT NULL, version INTEGER NOT NULL,
               digests BLOB NOT NULL CHECK(length(digests) <= {}),
               last_use INTEGER NOT NULL,
               PRIMARY KEY(identity, bucket, object_key));
             CREATE INDEX IF NOT EXISTS baseline_lru ON baselines(last_use);",
            DIGEST_LIMIT * 32
        ))
        .map_err(|e| e.to_string())?;
        Ok(conn)
    }

    /// Forget the previous row before attempting a replacement. A failed or
    /// cancelled attempt cannot leave a row claiming to describe that attempt.
    pub async fn invalidate(&self, key: BaselineKey) -> Result<(), String> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || {
            let conn = store.open()?;
            delete(&conn, &key)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    /// Call only after a successful PUT/completion, with its response ETag.
    pub async fn record_completed(
        &self,
        key: BaselineKey,
        hasher: BaselineHasher,
        etag: String,
    ) -> Result<(), String> {
        let Some(row) = hasher.finish(&etag) else {
            return Ok(());
        };
        let store = self.clone();
        tokio::task::spawn_blocking(move || store.insert(&key, row))
            .await
            .map_err(|e| e.to_string())?
    }

    fn insert(&self, key: &BaselineKey, row: Baseline) -> Result<(), String> {
        if row.digests.len() > DIGEST_LIMIT {
            return Err("Too many baseline digests".into());
        }
        let mut conn = self.open()?;
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        let blob: Vec<u8> = row.digests.into_iter().flatten().collect();
        tx.execute(
            "INSERT OR REPLACE INTO baselines VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,
             (SELECT COALESCE(MAX(last_use),0)+1 FROM baselines))",
            params![
                key.identity,
                key.bucket,
                key.key,
                row.etag,
                row.size,
                row.grid,
                ALGORITHM,
                VERSION,
                blob
            ],
        )
        .map_err(|e| e.to_string())?;
        evict(&tx, ROW_LIMIT)?;
        tx.commit().map_err(|e| e.to_string())
    }

    fn lookup(
        &self,
        key: &BaselineKey,
        etag: &str,
        size: u64,
        local_len: u64,
    ) -> Result<Result<Baseline, MatchRefusal>, String> {
        let mut conn = self.open()?;
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        // Do not materialize a malformed unbounded BLOB, even from an older or
        // externally modified database without the CHECK constraint.
        let row = tx
            .query_row(
                "SELECT etag,size,grid,algorithm,version,
             CASE WHEN length(digests)<=?4 THEN digests ELSE X'' END
             FROM baselines WHERE identity=?1 AND bucket=?2 AND object_key=?3",
                params![key.identity, key.bucket, key.key, DIGEST_LIMIT * 32],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, u64>(1)?,
                        r.get::<_, u64>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, u32>(4)?,
                        r.get::<_, Vec<u8>>(5)?,
                    ))
                },
            )
            .optional();
        let row = match row {
            Ok(None) => return Ok(Err(MatchRefusal::NoBaseline)),
            Ok(Some(row)) => row,
            Err(
                rusqlite::Error::FromSqlConversionFailure(..)
                | rusqlite::Error::IntegralValueOutOfRange(..)
                | rusqlite::Error::InvalidColumnType(..),
            ) => {
                delete(&tx, key)?;
                tx.commit().map_err(|e| e.to_string())?;
                return Ok(Err(MatchRefusal::InvalidRow));
            }
            Err(e) => return Err(e.to_string()),
        };
        let (stored_etag, stored_size, grid, algorithm, version, blob) = row;
        let refusal = if normalize_etag(&stored_etag).is_empty()
            || normalize_etag(&stored_etag) != normalize_etag(etag)
        {
            Some(MatchRefusal::EtagMismatch)
        } else if stored_size != size {
            Some(MatchRefusal::SizeMismatch)
        } else if !delta_grid_fits(grid, local_len) || !delta_grid_fits(grid, stored_size) {
            Some(MatchRefusal::GridMismatch)
        } else if algorithm != ALGORITHM
            || version != VERSION
            || blob.len() != stored_size.div_ceil(grid) as usize * 32
        {
            Some(MatchRefusal::InvalidRow)
        } else {
            None
        };
        if let Some(reason) = refusal {
            delete(&tx, key)?;
            tx.commit().map_err(|e| e.to_string())?;
            return Ok(Err(reason));
        }
        tx.execute(
            "UPDATE baselines SET last_use=(SELECT COALESCE(MAX(last_use),0)+1 FROM baselines)
                    WHERE identity=?1 AND bucket=?2 AND object_key=?3",
            params![key.identity, key.bucket, key.key],
        )
        .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(Ok(Baseline {
            etag: stored_etag,
            size,
            grid,
            digests: blob
                .chunks_exact(32)
                .map(|c| c.try_into().expect("32 byte chunk"))
                .collect(),
        }))
    }

    /// Tier 1: only same-offset cells, fused into runs. The baseline's final
    /// partial cell is compared over its original length even after an append.
    /// Refusals delete stale rows before any source bytes are read.
    pub async fn match_file(
        &self,
        key: BaselineKey,
        current_etag: String,
        current_size: u64,
        local_path: &Path,
    ) -> Result<MatchOutcome, String> {
        self.prepare_file(key, current_etag, current_size, local_path)
            .await
            .map(|(outcome, _)| outcome)
    }

    /// Also prepares the new object's signature in the same local read. The
    /// caller may certify it only with its own successful completion ETag.
    pub async fn prepare_file(
        &self,
        key: BaselineKey,
        current_etag: String,
        current_size: u64,
        local_path: &Path,
    ) -> Result<(MatchOutcome, Option<BaselineHasher>), String> {
        let mut file = tokio::fs::File::open(local_path)
            .await
            .map_err(|e| e.to_string())?;
        let before = file.metadata().await.map_err(|e| e.to_string())?;
        let local_len = before.len();
        let store = self.clone();
        let row = tokio::task::spawn_blocking(move || {
            store.lookup(&key, &current_etag, current_size, local_len)
        })
        .await
        .map_err(|e| e.to_string())??;
        let row = match row {
            Ok(row) => row,
            Err(reason) => return Ok((MatchOutcome::Refused(reason), None)),
        };
        let mut fresh = BaselineHasher::new(local_len);
        let mut read_bytes = 0u64;
        let mut matches: Vec<(u64, u64, u64)> = Vec::new();
        let mut buf = vec![0u8; 64 * 1024];
        for (i, expected) in row.digests.iter().enumerate() {
            let off = i as u64 * row.grid;
            let len = row.grid.min(row.size - off);
            if off + len > local_len {
                break;
            }
            let mut hash = Sha256::new();
            let mut remaining = len;
            while remaining != 0 {
                let take = remaining.min(buf.len() as u64) as usize;
                file.read_exact(&mut buf[..take])
                    .await
                    .map_err(|e| e.to_string())?;
                hash.update(&buf[..take]);
                if let Some(fresh) = &mut fresh {
                    fresh.update(&buf[..take]);
                }
                read_bytes += take as u64;
                remaining -= take as u64;
            }
            let actual: [u8; 32] = hash.finalize().into();
            if actual == *expected {
                if let Some(last) = matches.last_mut().filter(|last| last.0 + last.2 == off) {
                    last.2 += len;
                } else {
                    matches.push((off, off, len));
                }
            }
        }
        // The old row ends before an appended tail; include that tail in the
        // next baseline without a second read of the source or a remote GET.
        if !matches.is_empty() {
            while read_bytes < local_len {
                let take = (local_len - read_bytes).min(buf.len() as u64) as usize;
                file.read_exact(&mut buf[..take])
                    .await
                    .map_err(|e| e.to_string())?;
                if let Some(fresh) = &mut fresh {
                    fresh.update(&buf[..take]);
                }
                read_bytes += take as u64;
            }
        } else {
            fresh = None;
        }
        let after = file.metadata().await.map_err(|e| e.to_string())?;
        if after.len() != local_len || before.modified().ok() != after.modified().ok() {
            return Err("Local file changed while matching S3 baseline".into());
        }
        Ok((
            MatchOutcome::Matches {
                grid: row.grid,
                matches,
            },
            fresh,
        ))
    }
}

fn delete(conn: &Connection, key: &BaselineKey) -> Result<(), String> {
    conn.execute(
        "DELETE FROM baselines WHERE identity=?1 AND bucket=?2 AND object_key=?3",
        params![key.identity, key.bucket, key.key],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

fn evict(conn: &Connection, cap: usize) -> Result<(), String> {
    conn.execute("DELETE FROM baselines WHERE rowid IN
                  (SELECT rowid FROM baselines ORDER BY last_use DESC, rowid DESC LIMIT -1 OFFSET ?1)", [cap]).map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::s3_delta_plan::{plan_delta_parts, DeltaPart, DELTA_PART_SIZE};
    use super::*;
    use std::io::{Seek, SeekFrom, Write};

    fn key(name: &str) -> BaselineKey {
        BaselineKey::new(
            EndpointIdentity::new("s3", "host", "account"),
            "bucket",
            name,
        )
    }

    fn store() -> (tempfile::TempDir, BaselineStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = BaselineStore::new(dir.path().join("baseline.db"));
        (dir, store)
    }

    // Independently construct a sparse zero baseline, without using the
    // streaming hasher to generate the matcher's expected digest rows.
    fn zero_row(size: u64) -> Baseline {
        let grid = delta_grid_size(size).unwrap();
        let full: [u8; 32] = Sha256::digest(vec![0; grid as usize]).into();
        let tail = size % grid;
        let mut digests = vec![full; (size / grid) as usize];
        if tail != 0 {
            digests.push(Sha256::digest(vec![0; tail as usize]).into());
        }
        Baseline {
            etag: "\"ours\"".into(),
            size,
            grid,
            digests,
        }
    }

    fn sparse(path: &Path, size: u64) -> std::fs::File {
        let file = std::fs::File::create(path).unwrap();
        file.set_len(size).unwrap();
        file
    }

    fn count(store: &BaselineStore) -> usize {
        store
            .open()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM baselines", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn s3_baseline_hasher_ignores_upload_chunk_boundaries() {
        // Small grid here isolates streaming arithmetic from protocol eligibility.
        let bytes = b"abcdefghijklmno";
        for chunk_size in [1, 3, 4, 7, 16] {
            let mut hash = BaselineHasher {
                size: 15,
                grid: 4,
                seen: 0,
                cell_len: 0,
                hash: Sha256::new(),
                digests: vec![],
            };
            for chunk in bytes.chunks(chunk_size) {
                hash.update(chunk);
            }
            let row = hash.finish("response-etag").unwrap();
            let expected: Vec<[u8; 32]> = bytes
                .chunks(4)
                .map(|chunk| Sha256::digest(chunk).into())
                .collect();
            assert_eq!(row.digests, expected);
        }
    }

    #[test]
    fn s3_baseline_incomplete_or_growing_stream_cannot_certify_bytes() {
        for consumed in [0, 14, 16] {
            let mut hash = BaselineHasher {
                size: 15,
                grid: 4,
                seen: 0,
                cell_len: 0,
                hash: Sha256::new(),
                digests: vec![],
            };
            hash.update(&vec![0; consumed]);
            assert!(hash.finish("ours").is_none());
        }
    }

    #[tokio::test]
    async fn s3_baseline_append_reuses_partial_prefix_and_uploads_exact_tail() {
        let (dir, store) = store();
        let old_size = 25 * DELTA_PART_SIZE + 1024 * 1024;
        let tail = 9 * 1024 * 1024;
        store.insert(&key("object"), zero_row(old_size)).unwrap();
        let path = dir.path().join("append.bin");
        let mut file = sparse(&path, old_size + tail);
        file.seek(SeekFrom::Start(old_size)).unwrap();
        file.write_all(&vec![0x7b; tail as usize]).unwrap();
        let MatchOutcome::Matches { grid, matches } = store
            .match_file(key("object"), "ours".into(), old_size, &path)
            .await
            .unwrap()
        else {
            panic!("refused append")
        };
        assert_eq!(matches, vec![(0, 0, old_size)]);
        let plan = plan_delta_parts(old_size + tail, &matches, grid, S3_MAX_PARTS).unwrap();
        let puts: Vec<_> = plan
            .iter()
            .filter_map(|p| match p {
                DeltaPart::Put {
                    local_start, len, ..
                } => Some((*local_start, *len)),
                _ => None,
            })
            .collect();
        // Expected bytes come from the fixture edit, not from match counters.
        assert_eq!(puts, vec![(old_size, tail)]);
        assert_eq!(
            std::fs::read(&path).unwrap()[old_size as usize..],
            vec![0x7b; tail as usize]
        );
    }

    #[tokio::test]
    async fn s3_baseline_middle_edit_uploads_only_touched_grid_cell() {
        let (dir, store) = store();
        let size = 26 * DELTA_PART_SIZE;
        store.insert(&key("object"), zero_row(size)).unwrap();
        let path = dir.path().join("middle.bin");
        let mut file = sparse(&path, size);
        let changed_start = 12 * DELTA_PART_SIZE;
        file.seek(SeekFrom::Start(changed_start + 123)).unwrap();
        file.write_all(b"changed bytes").unwrap();
        let MatchOutcome::Matches { grid, matches } = store
            .match_file(key("object"), "\"ours\"".into(), size, &path)
            .await
            .unwrap()
        else {
            panic!("refused middle edit")
        };
        assert_eq!(
            matches,
            vec![
                (0, 0, changed_start),
                (
                    changed_start + grid,
                    changed_start + grid,
                    size - changed_start - grid
                )
            ]
        );
        let plan = plan_delta_parts(size, &matches, grid, S3_MAX_PARTS).unwrap();
        let puts: Vec<_> = plan
            .iter()
            .filter_map(|p| match p {
                DeltaPart::Put {
                    local_start, len, ..
                } => Some((*local_start, *len)),
                _ => None,
            })
            .collect();
        assert_eq!(puts, vec![(changed_start, DELTA_PART_SIZE)]);
        // Independently replay the plan against baseline zero bytes and local
        // payload, then compare actual output to the edited source.
        let local = std::fs::read(path).unwrap();
        let mut rebuilt = Vec::new();
        for part in plan {
            match part {
                DeltaPart::Copy {
                    src_start,
                    src_end_inclusive,
                    ..
                } => rebuilt.resize(
                    rebuilt.len() + (src_end_inclusive - src_start + 1) as usize,
                    0,
                ),
                DeltaPart::Put {
                    local_start, len, ..
                } => rebuilt
                    .extend_from_slice(&local[local_start as usize..(local_start + len) as usize]),
            }
        }
        assert_eq!(rebuilt, local);
    }

    #[tokio::test]
    async fn s3_baseline_stale_rows_are_deleted_at_matcher_door() {
        let (dir, store) = store();
        let size = 26 * DELTA_PART_SIZE;
        let path = dir.path().join("local");
        sparse(&path, size);
        for (etag, remote_size, reason) in [
            ("theirs", size, MatchRefusal::EtagMismatch),
            ("ours", size + 1, MatchRefusal::SizeMismatch),
        ] {
            store.insert(&key("object"), zero_row(size)).unwrap();
            assert_eq!(
                store
                    .match_file(key("object"), etag.into(), remote_size, &path)
                    .await
                    .unwrap(),
                MatchOutcome::Refused(reason)
            );
            assert_eq!(count(&store), 0);
            assert_eq!(
                store
                    .match_file(key("object"), "ours".into(), size, &path)
                    .await
                    .unwrap(),
                MatchOutcome::Refused(MatchRefusal::NoBaseline)
            );
        }
    }

    #[tokio::test]
    async fn s3_baseline_grid_overflow_is_refused_before_reading_sparse_file() {
        let (dir, store) = store();
        let size = 26 * DELTA_PART_SIZE;
        store.insert(&key("object"), zero_row(size)).unwrap();
        let path = dir.path().join("grew");
        sparse(&path, DELTA_PART_SIZE * u64::from(S3_MAX_PARTS) + 1);
        assert_eq!(
            store
                .match_file(key("object"), "ours".into(), size, &path)
                .await
                .unwrap(),
            MatchOutcome::Refused(MatchRefusal::GridMismatch)
        );
        assert_eq!(count(&store), 0);
    }

    #[test]
    fn s3_baseline_malformed_rows_are_discarded_not_repaired() {
        let (_dir, store) = store();
        let size = 26 * DELTA_PART_SIZE;
        for mutation in [
            "algorithm='md5'",
            "version=2",
            "digests=X'00'",
            "grid=0",
            "size=-1",
        ] {
            store.insert(&key("object"), zero_row(size)).unwrap();
            store
                .open()
                .unwrap()
                .execute(&format!("UPDATE baselines SET {mutation}"), [])
                .unwrap();
            assert!(store
                .lookup(&key("object"), "ours", size, size)
                .unwrap()
                .is_err());
            assert_eq!(count(&store), 0);
        }
        let conn = store.open().unwrap();
        conn.execute_batch("PRAGMA ignore_check_constraints=ON")
            .unwrap();
        store.insert(&key("object"), zero_row(size)).unwrap();
        conn.execute(
            "UPDATE baselines SET digests=zeroblob(?1)",
            [DIGEST_LIMIT * 32 + 1],
        )
        .unwrap();
        assert!(store
            .lookup(&key("object"), "ours", size, size)
            .unwrap()
            .is_err());
        assert_eq!(count(&store), 0);
    }

    #[test]
    fn s3_baseline_lru_uses_reads_and_enforces_production_cap() {
        let (_dir, store) = store();
        let size = 26 * DELTA_PART_SIZE;
        store.insert(&key("hot"), zero_row(size)).unwrap();
        store.insert(&key("cold"), zero_row(size)).unwrap();
        store
            .lookup(&key("hot"), "ours", size, size)
            .unwrap()
            .unwrap();
        let conn = store.open().unwrap();
        // Seed cheap SQL rows to exercise the actual 10,000 row cap without
        // hashing 10,000 objects or doing 10,000 fsyncs.
        conn.execute("WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<?1)
            INSERT INTO baselines SELECT 'other','bucket',CAST(x AS TEXT),'e',1,1,'sha256',1,X'',x+100 FROM n", [ROW_LIMIT - 2]).unwrap();
        store.insert(&key("new"), zero_row(size)).unwrap();
        assert_eq!(count(&store), ROW_LIMIT);
        assert_eq!(
            store
                .lookup(&key("cold"), "ours", size, size)
                .unwrap()
                .err(),
            Some(MatchRefusal::NoBaseline)
        );
        assert!(store
            .lookup(&key("hot"), "ours", size, size)
            .unwrap()
            .is_ok());
    }

    #[test]
    fn s3_baseline_key_keeps_bucket_object_case_and_account_separate() {
        let (_dir, store) = store();
        let size = 26 * DELTA_PART_SIZE;
        store.insert(&key("Object"), zero_row(size)).unwrap();
        let mut others = vec![key("object")];
        others.push(BaselineKey::new(
            EndpointIdentity::new("s3", "host", "other"),
            "bucket",
            "Object",
        ));
        others.push(BaselineKey::new(
            EndpointIdentity::new("s3", "host", "account"),
            "Bucket",
            "Object",
        ));
        for other in others {
            assert_eq!(
                store.lookup(&other, "ours", size, size).unwrap().err(),
                Some(MatchRefusal::NoBaseline)
            );
        }
        assert_eq!(count(&store), 1);
    }
}

/// Per-part signatures for the clone-pool DAG hook. No payload bytes are kept.
/// Multipart completion establishes part order; only aligned boundaries can
/// safely concatenate independently computed cell hashes.
pub(crate) struct MultipartPartDigests {
    len: u64,
    digests: Vec<[u8; 32]>,
}

pub(crate) fn hash_multipart_part(total_size: u64, bytes: &[u8]) -> Option<MultipartPartDigests> {
    let grid = delta_grid_size(total_size)?;
    if bytes.len() as u64 > total_size {
        return None;
    }
    let digests = bytes
        .chunks(grid as usize)
        .map(|cell| Sha256::digest(cell).into())
        .collect();
    Some(MultipartPartDigests {
        len: bytes.len() as u64,
        digests,
    })
}

pub(crate) struct MultipartBaseline {
    pub size: u64,
    pub store: BaselineStore,
    pub touched: std::time::Instant,
    pub parts: std::collections::BTreeMap<u32, (String, MultipartPartDigests)>,
}

impl MultipartBaseline {
    pub fn record(&mut self, number: u32, etag: String, part: MultipartPartDigests) -> bool {
        let existing: usize = self
            .parts
            .iter()
            .filter(|(n, _)| **n != number)
            .map(|(_, (_, p))| p.digests.len())
            .sum();
        if number == 0
            || number > S3_MAX_PARTS
            || existing.saturating_add(part.digests.len()) > DIGEST_LIMIT
        {
            return false;
        }
        self.parts.insert(number, (etag, part));
        self.touched = std::time::Instant::now();
        true
    }

    pub fn finish(
        mut self,
        completed: &[(u32, String)],
    ) -> Option<(BaselineStore, BaselineHasher)> {
        let grid = delta_grid_size(self.size)?;
        if completed.len() > DIGEST_LIMIT || completed.len() != self.parts.len() {
            return None;
        }
        let mut seen = 0u64;
        let mut digests = Vec::new();
        for (index, (number, etag)) in completed.iter().enumerate() {
            if *number != index as u32 + 1 || seen % grid != 0 {
                return None;
            }
            let (sent_etag, part) = self.parts.remove(number)?;
            if sent_etag != *etag || part.len == 0 {
                return None;
            }
            seen = seen.checked_add(part.len)?;
            digests.extend(part.digests);
            if digests.len() > DIGEST_LIMIT {
                return None;
            }
        }
        if seen != self.size {
            return None;
        }
        Some((
            self.store,
            BaselineHasher {
                size: self.size,
                grid,
                seen,
                cell_len: 0,
                hash: Sha256::new(),
                digests,
            },
        ))
    }
}

pub(crate) struct PreparedDigests {
    size: u64,
    grid: u64,
    digests: Vec<[u8; 32]>,
}

impl PreparedDigests {
    /// Confirm complete cells from the very PUT buffer handed to the request.
    /// Cells split by repaired part boundaries retain the metadata guard.
    pub fn verify_put(&self, offset: u64, bytes: &[u8]) -> bool {
        let end = offset.saturating_add(bytes.len() as u64);
        for index in offset.div_ceil(self.grid)..end.div_ceil(self.grid) {
            let start = index * self.grid;
            let cell_end = (start + self.grid).min(self.size);
            if cell_end > end || cell_end <= start {
                continue;
            }
            let digest: [u8; 32] =
                Sha256::digest(&bytes[(start - offset) as usize..(cell_end - offset) as usize])
                    .into();
            if self.digests.get(index as usize) != Some(&digest) {
                return false;
            }
        }
        true
    }
}
