//! Session-global tables of one rsync file-list direction, in wire order.
//!
//! rsync abbreviates repeated metadata inside a file list: an ACL or an
//! xattr set already sent earlier in the session is shipped as a
//! back-reference (`write_varint(ndx + 1)` with `ndx >= 0`), and uid/gid
//! names arrive either inline in the entries (incremental recursion) or
//! in trailing [`IdNameList`]s (no incremental recursion). The codec in
//! [`crate::aerorsync::real_wire`] deliberately leaves references
//! unresolved; this module owns the per-session tables that resolve them.
//!
//! Access and default ACLs have **separate index spaces**: each counts
//! its own literals from 0 (`12-multi-entry-wire-evidence.md` section 6:
//! `da/sub` refers to default 0 after two access ACLs). A literal, even
//! an empty one, consumes an index (section 4: the root directory is
//! xattr set 0), so the receiving side appends every literal to its
//! table in wire order and only references copy out of it.
//!
//! Caps: [`FileListStreamDecoder::new`](crate::aerorsync::real_wire::FileListStreamDecoder)
//! takes `max_entries` from its caller, so the entry ceiling is not a
//! constant of the codec. `grep -rn "FileListStreamDecoder::new"` over
//! `src-tauri/src/aerorsync` finds only test call sites, passing 0, 1, 8,
//! 10, `P8_ENTRIES = 50_000` and a `usize::MAX` placeholder; every entry
//! adds at most one element per table, so the caps below equal the
//! largest real `max_entries` seen (50_000). A stream that pushes past a
//! cap gets [`SessionTableError::TableFull`], never unbounded growth.
//!
//! [`SessionTableError`] is a type of its own, not a
//! [`RealWireError`](crate::aerorsync::real_wire::RealWireError) variant,
//! so it can never enter the driver's retry classification (the one that
//! makes `TruncatedBuffer` retryable): a table error is final by
//! construction.
//!
//! Interning compares whole values, exactly like rsync: an xattr set is
//! the full list of names and values in wire order, an ACL is the full
//! body. The structural `PartialEq` of [`XattrPair`] and [`RsyncAcl`]
//! matches that comparison; no divergence from rsync was found.

// SPDX-License-Identifier: MPL-2.0 OR GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::hash::{Hash, Hasher};

use crate::aerorsync::real_wire::{
    AclWireEntry, FileListEntry, IdNameList, RsyncAcl, XattrPair, XattrWireEntry,
};

/// Maximum number of ACLs in one ACL table (access and default each).
/// See the module doc for why this equals the largest real
/// `FileListStreamDecoder::new` `max_entries`.
pub const MAX_SESSION_ACLS: usize = 50_000;
/// Maximum number of xattr sets in the session table.
pub const MAX_SESSION_XATTR_SETS: usize = 50_000;
/// Maximum number of distinct names in one id-name map (uids and gids
/// each have their own map, each capped independently).
pub const MAX_SESSION_ID_NAMES: usize = 50_000;

/// Session-global tables of one rsync file-list direction, in wire order.
///
/// The `*_index` maps hash a value to the indices of the table entries
/// with that hash, so `intern_*` finds the first equal entry in O(1)
/// average instead of scanning the table. Values are never copied into
/// the index: only the hash and the indices are stored, and a hash hit
/// is always confirmed by a real `PartialEq` comparison on the table
/// entry, so collisions are not an error. The index is kept in sync on
/// every literal append, both on receive (`resolve_entry`) and on send
/// (`intern_*`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionTables {
    acl_access: Vec<RsyncAcl>,
    acl_default: Vec<RsyncAcl>,
    xattr_sets: Vec<Vec<XattrPair>>,
    uid_names: BTreeMap<i64, String>,
    gid_names: BTreeMap<i64, String>,
    acl_access_index: HashMap<u64, Vec<u32>>,
    acl_default_index: HashMap<u64, Vec<u32>>,
    xattr_index: HashMap<u64, Vec<u32>>,
}

/// Number of real `PartialEq` comparisons done while interning.
/// Test-only: the hash-index test asserts it stays under `2 * n` on `n`
/// distinct internings, which a linear scan cannot.
#[cfg(test)]
pub(crate) static EQUALITY_CHECKS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

#[cfg(test)]
fn note_equality_check() {
    EQUALITY_CHECKS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

#[cfg(not(test))]
#[inline(always)]
fn note_equality_check() {}

/// Stable-enough hash of an ACL body for the in-session index. The hash
/// is never persisted, so `DefaultHasher`'s instability across releases
/// does not matter; only equality within one session does.
fn hash_acl(acl: &RsyncAcl) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    acl.user_obj.hash(&mut h);
    acl.group_obj.hash(&mut h);
    acl.mask_obj.hash(&mut h);
    acl.other_obj.hash(&mut h);
    acl.names.len().hash(&mut h);
    for n in &acl.names {
        n.id.hash(&mut h);
        match n.principal {
            crate::aerorsync::real_wire::AclPrincipal::User => 0u8.hash(&mut h),
            crate::aerorsync::real_wire::AclPrincipal::Group => 1u8.hash(&mut h),
        }
        n.access.hash(&mut h);
        n.name.hash(&mut h);
    }
    h.finish()
}

/// Hash of a whole xattr set (names and values, in order), same rules as
/// [`hash_acl`].
fn hash_xattr_set(set: &[XattrPair]) -> u64 {
    use crate::aerorsync::real_wire::XattrDatum;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    set.len().hash(&mut h);
    for p in set {
        p.name.hash(&mut h);
        match &p.datum {
            XattrDatum::Inline(v) => {
                0u8.hash(&mut h);
                v.hash(&mut h);
            }
            XattrDatum::Deferred { len, digest } => {
                1u8.hash(&mut h);
                len.hash(&mut h);
                digest.hash(&mut h);
            }
        }
    }
    h.finish()
}

/// Which session table an index or a cap refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionTable {
    AclAccess,
    AclDefault,
    XattrSet,
    IdNames,
}

/// Which ACL table an ACL operation targets. Access and default ACLs
/// have separate index spaces, and an ACL operation can never target
/// the xattr or id-name tables, so this selector is ACL-only by type:
/// the impossible state cannot be written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AclTable {
    Access,
    Default,
}

impl From<AclTable> for SessionTable {
    fn from(table: AclTable) -> Self {
        match table {
            AclTable::Access => SessionTable::AclAccess,
            AclTable::Default => SessionTable::AclDefault,
        }
    }
}

/// Metadata of one file-list entry after resolving its back-references.
/// `None` where the entry does not carry the field at all (distinct from
/// a present-but-empty value).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolvedMetadata {
    pub acl_access: Option<RsyncAcl>,
    pub acl_default: Option<RsyncAcl>,
    pub xattrs: Option<Vec<XattrPair>>,
}

/// Session-table failure. Deliberately a standalone type and not a
/// `RealWireError` variant: the driver's retry classification only
/// inspects `RealWireError`, so these errors are final by construction
/// and never retried as if they were a truncated buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionTableError {
    /// A wire back-reference points past the end of its table.
    ReferenceOutOfRange {
        table: SessionTable,
        index: u32,
        len: usize,
    },
    /// Appending would exceed the table's cap.
    TableFull { table: SessionTable, cap: usize },
}

impl SessionTables {
    pub fn new() -> Self {
        Self::default()
    }

    /// The ACL vector behind `table`. Infallible: `AclTable` only names
    /// the two ACL spaces.
    fn acl_slot(&mut self, table: AclTable) -> &mut Vec<RsyncAcl> {
        match table {
            AclTable::Access => &mut self.acl_access,
            AclTable::Default => &mut self.acl_default,
        }
    }

    /// The hash index behind `table`, kept in sync with `acl_slot`.
    fn acl_index(&mut self, table: AclTable) -> &mut HashMap<u64, Vec<u32>> {
        match table {
            AclTable::Access => &mut self.acl_access_index,
            AclTable::Default => &mut self.acl_default_index,
        }
    }

    /// Record a freshly appended literal in the index. `index` is its
    /// position in the table vector.
    fn acl_index_push(&mut self, table: AclTable, hash: u64, index: u32) {
        self.acl_index(table).entry(hash).or_default().push(index);
    }

    fn xattr_index_push(&mut self, hash: u64, index: u32) {
        self.xattr_index.entry(hash).or_default().push(index);
    }

    /// Find the first table entry equal to `value` among the indices
    /// stored in `bucket`, comparing for real (collisions are fine).
    /// Returns the table index of the first equal entry.
    fn find_equal<T: PartialEq>(table: &[T], bucket: Option<&Vec<u32>>, value: &T) -> Option<u32> {
        let bucket = bucket?;
        for &idx in bucket {
            note_equality_check();
            if table.get(idx as usize).is_some_and(|v| *v == *value) {
                return Some(idx);
            }
        }
        None
    }

    /// Receiving side: resolve one entry's ACL and xattr references
    /// against the tables. Every literal is appended to its own table in
    /// wire order (an empty literal consumes an index too: the root
    /// directory is xattr set 0); every reference copies the referenced
    /// element or fails with [`SessionTableError::ReferenceOutOfRange`].
    pub fn resolve_entry(
        &mut self,
        entry: &FileListEntry,
    ) -> Result<ResolvedMetadata, SessionTableError> {
        let mut resolved = ResolvedMetadata::default();
        if let Some(acls) = &entry.acls {
            resolved.acl_access = Some(self.resolve_acl(AclTable::Access, &acls.access)?);
            if let Some(default) = &acls.default {
                resolved.acl_default = Some(self.resolve_acl(AclTable::Default, default)?);
            }
        }
        if let Some(xattrs) = &entry.xattrs {
            resolved.xattrs = Some(self.resolve_xattrs(xattrs)?);
        }
        Ok(resolved)
    }

    fn resolve_acl(
        &mut self,
        table: AclTable,
        wire: &AclWireEntry,
    ) -> Result<RsyncAcl, SessionTableError> {
        let session_table = SessionTable::from(table);
        match wire {
            AclWireEntry::Literal(acl) => {
                let hash = hash_acl(acl);
                let slot = self.acl_slot(table);
                if slot.len() >= MAX_SESSION_ACLS {
                    return Err(SessionTableError::TableFull {
                        table: session_table,
                        cap: MAX_SESSION_ACLS,
                    });
                }
                let index = slot.len() as u32;
                slot.push(acl.clone());
                self.acl_index_push(table, hash, index);
                Ok(acl.clone())
            }
            AclWireEntry::Reference(index) => {
                let slot = self.acl_slot(table);
                slot.get(*index as usize)
                    .cloned()
                    .ok_or(SessionTableError::ReferenceOutOfRange {
                        table: session_table,
                        index: *index,
                        len: slot.len(),
                    })
            }
        }
    }

    fn resolve_xattrs(
        &mut self,
        wire: &XattrWireEntry,
    ) -> Result<Vec<XattrPair>, SessionTableError> {
        match wire {
            XattrWireEntry::Literal(set) => {
                let hash = hash_xattr_set(set);
                if self.xattr_sets.len() >= MAX_SESSION_XATTR_SETS {
                    return Err(SessionTableError::TableFull {
                        table: SessionTable::XattrSet,
                        cap: MAX_SESSION_XATTR_SETS,
                    });
                }
                let index = self.xattr_sets.len() as u32;
                self.xattr_sets.push(set.clone());
                self.xattr_index_push(hash, index);
                Ok(set.clone())
            }
            XattrWireEntry::Reference(index) => {
                self.xattr_sets.get(*index as usize).cloned().ok_or(
                    SessionTableError::ReferenceOutOfRange {
                        table: SessionTable::XattrSet,
                        index: *index,
                        len: self.xattr_sets.len(),
                    },
                )
            }
        }
    }

    /// Sending side: intern one ACL into `table`. Returns a reference to
    /// the FIRST equal ACL already present, otherwise appends and
    /// returns the literal. rsync compares the whole ACL body; the
    /// structural `PartialEq` of [`RsyncAcl`] is that comparison.
    pub fn intern_acl(
        &mut self,
        table: AclTable,
        acl: RsyncAcl,
    ) -> Result<AclWireEntry, SessionTableError> {
        let session_table = SessionTable::from(table);
        let hash = hash_acl(&acl);
        let (slot, index) = match table {
            AclTable::Access => (&self.acl_access, &self.acl_access_index),
            AclTable::Default => (&self.acl_default, &self.acl_default_index),
        };
        if let Some(pos) = Self::find_equal(slot, index.get(&hash), &acl) {
            return Ok(AclWireEntry::Reference(pos));
        }
        let slot = self.acl_slot(table);
        if slot.len() >= MAX_SESSION_ACLS {
            return Err(SessionTableError::TableFull {
                table: session_table,
                cap: MAX_SESSION_ACLS,
            });
        }
        let pos = slot.len() as u32;
        slot.push(acl.clone());
        self.acl_index_push(table, hash, pos);
        Ok(AclWireEntry::Literal(acl))
    }

    /// Sending side: intern one xattr set. Same rule as
    /// [`Self::intern_acl`]: reference to the first equal set, else
    /// append and return the literal. rsync compares the whole list of
    /// names and values in order; `Vec<XattrPair>`'s structural
    /// `PartialEq` is that comparison.
    pub fn intern_xattrs(
        &mut self,
        set: Vec<XattrPair>,
    ) -> Result<XattrWireEntry, SessionTableError> {
        let hash = hash_xattr_set(&set);
        if let Some(pos) = Self::find_equal(&self.xattr_sets, self.xattr_index.get(&hash), &set) {
            return Ok(XattrWireEntry::Reference(pos));
        }
        if self.xattr_sets.len() >= MAX_SESSION_XATTR_SETS {
            return Err(SessionTableError::TableFull {
                table: SessionTable::XattrSet,
                cap: MAX_SESSION_XATTR_SETS,
            });
        }
        let pos = self.xattr_sets.len() as u32;
        self.xattr_sets.push(set.clone());
        self.xattr_index_push(hash, pos);
        Ok(XattrWireEntry::Literal(set))
    }

    /// Record the trailing uid/gid name lists sent after a
    /// non-incremental file list ([`decode_id_list`](crate::aerorsync::real_wire::decode_id_list)).
    pub fn record_id_names(
        &mut self,
        uids: &IdNameList,
        gids: &IdNameList,
    ) -> Result<(), SessionTableError> {
        for (id, name) in &uids.names {
            insert_id_name(&mut self.uid_names, *id, name)?;
        }
        if let Some(name) = &uids.id0_name {
            insert_id_name(&mut self.uid_names, 0, name)?;
        }
        for (id, name) in &gids.names {
            insert_id_name(&mut self.gid_names, *id, name)?;
        }
        if let Some(name) = &gids.id0_name {
            insert_id_name(&mut self.gid_names, 0, name)?;
        }
        Ok(())
    }

    /// Record the inline `uid_name`/`gid_name` of one entry; with
    /// incremental recursion the names ride inside the entries and no
    /// trailing lists are sent.
    pub fn record_inline_names(&mut self, entry: &FileListEntry) -> Result<(), SessionTableError> {
        if let (Some(uid), Some(name)) = (entry.uid, &entry.uid_name) {
            insert_id_name(&mut self.uid_names, uid, name)?;
        }
        if let (Some(gid), Some(name)) = (entry.gid, &entry.gid_name) {
            insert_id_name(&mut self.gid_names, gid, name)?;
        }
        Ok(())
    }

    /// Name of a uid recorded earlier, inline or from a trailing list.
    pub fn uid_name(&self, uid: i64) -> Option<&str> {
        self.uid_names.get(&uid).map(String::as_str)
    }

    /// Name of a gid recorded earlier, inline or from a trailing list.
    pub fn gid_name(&self, gid: i64) -> Option<&str> {
        self.gid_names.get(&gid).map(String::as_str)
    }
}

/// Insert one id-to-name pair, enforcing [`MAX_SESSION_ID_NAMES`] on new
/// keys only (re-recording an existing id overwrites and does not grow
/// the map).
fn insert_id_name(
    map: &mut BTreeMap<i64, String>,
    id: i64,
    name: &str,
) -> Result<(), SessionTableError> {
    if !map.contains_key(&id) && map.len() >= MAX_SESSION_ID_NAMES {
        return Err(SessionTableError::TableFull {
            table: SessionTable::IdNames,
            cap: MAX_SESSION_ID_NAMES,
        });
    }
    map.insert(id, name.to_string());
    Ok(())
}

impl fmt::Display for SessionTable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            SessionTable::AclAccess => "acl_access",
            SessionTable::AclDefault => "acl_default",
            SessionTable::XattrSet => "xattr_set",
            SessionTable::IdNames => "id_names",
        };
        f.write_str(name)
    }
}

impl fmt::Display for SessionTableError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SessionTableError::ReferenceOutOfRange { table, index, len } => write!(
                f,
                "session table {} back-reference {} out of range (table len {})",
                table, index, len
            ),
            SessionTableError::TableFull { table, cap } => {
                write!(f, "session table {} is full (cap {})", table, cap)
            }
        }
    }
}

impl std::error::Error for SessionTableError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aerorsync::real_wire::{AclNamedEntry, AclPrincipal};
    use std::sync::atomic::Ordering;

    fn acl_with(id: u32) -> RsyncAcl {
        RsyncAcl {
            names: vec![AclNamedEntry {
                id,
                principal: AclPrincipal::User,
                access: 6,
                name: None,
            }],
            ..RsyncAcl::default()
        }
    }

    fn xattr_set_with(tag: u32) -> Vec<XattrPair> {
        vec![XattrPair::inline(
            format!("user.tag{tag}"),
            tag.to_le_bytes().to_vec(),
        )]
    }

    /// Interning returns the FIRST equal entry, not the last (B3M11).
    /// Duplicates can only exist on the RECEIVING side: resolve_entry
    /// appends every literal in wire order, so two entries carrying the
    /// same ACL A land at indices 0 and 1 (same for xattr set X). Then
    /// intern_acl(A) must reference 0, not 1; a last-equal mutation goes
    /// red here (a send-side A, B, A sequence cannot: intern only appends
    /// when find_equal finds nothing, so the sender table never holds
    /// duplicates).
    #[test]
    fn intern_returns_the_first_equal_entry() {
        use crate::aerorsync::real_wire::{FileListAcls, FileListEntry};
        let mut tables = SessionTables::new();
        let entry_with = |acl: AclWireEntry, x: XattrWireEntry| FileListEntry {
            flags: 0,
            path: b"f".to_vec(),
            size: 0,
            mtime: 0,
            mtime_nsec: None,
            mode: 0o100_644,
            uid: None,
            uid_name: None,
            gid: None,
            gid_name: None,
            checksum: Vec::new(),
            symlink_target: None,
            rdev: None,
            xattrs: Some(x),
            acls: Some(FileListAcls {
                access: acl,
                default: None,
            }),
        };
        let acl_a = || AclWireEntry::Literal(acl_with(1));
        let xset_x = || XattrWireEntry::Literal(xattr_set_with(7));
        tables
            .resolve_entry(&entry_with(acl_a(), xset_x()))
            .expect("first literal lands at index 0");
        tables
            .resolve_entry(&entry_with(acl_a(), xset_x()))
            .expect("the duplicate literal lands at index 1");
        assert_eq!(
            tables.intern_acl(AclTable::Access, acl_with(1)).unwrap(),
            AclWireEntry::Reference(0)
        );
        assert_eq!(
            tables.intern_xattrs(xattr_set_with(7)).unwrap(),
            XattrWireEntry::Reference(0)
        );
    }

    /// The hash index must keep equality comparisons under 2*n on n
    /// distinct internings; a linear scan needs about n^2/2 and goes red.
    #[test]
    fn intern_acl_stays_under_two_comparisons_per_entry_on_50k_distinct() {
        EQUALITY_CHECKS.store(0, Ordering::Relaxed);
        let mut tables = SessionTables::new();
        let n = MAX_SESSION_ACLS as u32;
        for i in 0..n {
            let wire = tables
                .intern_acl(AclTable::Access, acl_with(i))
                .expect("under the cap");
            assert!(
                matches!(wire, AclWireEntry::Literal(_)),
                "distinct value {i} must append"
            );
        }
        assert_eq!(
            tables.intern_acl(AclTable::Access, acl_with(0)).unwrap(),
            AclWireEntry::Reference(0)
        );
        let checks = EQUALITY_CHECKS.load(Ordering::Relaxed);
        assert!(
            checks < 2 * n as u64,
            "linear scan would need ~n^2/2 comparisons, got {checks} for n={n}"
        );
    }

    /// Same bound for the xattr sets.
    #[test]
    fn intern_xattrs_stays_under_two_comparisons_per_entry_on_50k_distinct() {
        EQUALITY_CHECKS.store(0, Ordering::Relaxed);
        let mut tables = SessionTables::new();
        let n = MAX_SESSION_XATTR_SETS as u32;
        for i in 0..n {
            let wire = tables
                .intern_xattrs(xattr_set_with(i))
                .expect("under the cap");
            assert!(matches!(wire, XattrWireEntry::Literal(_)));
        }
        assert_eq!(
            tables.intern_xattrs(xattr_set_with(0)).unwrap(),
            XattrWireEntry::Reference(0)
        );
        let checks = EQUALITY_CHECKS.load(Ordering::Relaxed);
        assert!(checks < 2 * n as u64, "got {checks} for n={n}");
    }

    /// A forced hash collision must still compare the real values: two
    /// different ACLs under one hash stay two distinct entries and each
    /// interns to its own (first-equal) index.
    #[test]
    fn hash_collision_still_compares_real_values() {
        let mut tables = SessionTables::new();
        let a = acl_with(1);
        let b = acl_with(2);
        assert_ne!(a, b);
        tables.intern_acl(AclTable::Access, a.clone()).unwrap();
        tables.intern_acl(AclTable::Access, b.clone()).unwrap();
        // force b into a's bucket, as a real collision would
        let ha = hash_acl(&a);
        tables.acl_access_index.entry(ha).or_default().push(1);
        assert_eq!(
            tables.intern_acl(AclTable::Access, a.clone()).unwrap(),
            AclWireEntry::Reference(0),
            "first equal in the bucket wins"
        );
        assert_eq!(
            tables.intern_acl(AclTable::Access, b).unwrap(),
            AclWireEntry::Reference(1),
            "the colliding entry is distinct"
        );
        assert_eq!(tables.acl_access.len(), 2);
    }

    /// First-equal semantics across the two ACL spaces: the same body in
    /// the default table does not match the access table's index.
    #[test]
    fn acl_spaces_have_separate_indexes() {
        let mut tables = SessionTables::new();
        let acl = acl_with(7);
        assert!(matches!(
            tables.intern_acl(AclTable::Access, acl.clone()).unwrap(),
            AclWireEntry::Literal(_)
        ));
        assert!(matches!(
            tables.intern_acl(AclTable::Default, acl).unwrap(),
            AclWireEntry::Literal(_)
        ));
        assert_eq!(
            tables.intern_acl(AclTable::Access, acl_with(7)).unwrap(),
            AclWireEntry::Reference(0)
        );
        assert_eq!(
            tables.intern_acl(AclTable::Default, acl_with(7)).unwrap(),
            AclWireEntry::Reference(0)
        );
    }
}
