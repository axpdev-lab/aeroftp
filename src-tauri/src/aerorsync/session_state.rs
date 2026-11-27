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

use std::collections::BTreeMap;
use std::fmt;

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
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionTables {
    acl_access: Vec<RsyncAcl>,
    acl_default: Vec<RsyncAcl>,
    xattr_sets: Vec<Vec<XattrPair>>,
    uid_names: BTreeMap<i64, String>,
    gid_names: BTreeMap<i64, String>,
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
        let slot = self.acl_slot(table);
        match wire {
            AclWireEntry::Literal(acl) => {
                if slot.len() >= MAX_SESSION_ACLS {
                    return Err(SessionTableError::TableFull {
                        table: session_table,
                        cap: MAX_SESSION_ACLS,
                    });
                }
                slot.push(acl.clone());
                Ok(acl.clone())
            }
            AclWireEntry::Reference(index) => {
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
                if self.xattr_sets.len() >= MAX_SESSION_XATTR_SETS {
                    return Err(SessionTableError::TableFull {
                        table: SessionTable::XattrSet,
                        cap: MAX_SESSION_XATTR_SETS,
                    });
                }
                self.xattr_sets.push(set.clone());
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
        let slot = self.acl_slot(table);
        if let Some(pos) = slot.iter().position(|a| *a == acl) {
            return Ok(AclWireEntry::Reference(pos as u32));
        }
        if slot.len() >= MAX_SESSION_ACLS {
            return Err(SessionTableError::TableFull {
                table: session_table,
                cap: MAX_SESSION_ACLS,
            });
        }
        slot.push(acl.clone());
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
        if let Some(pos) = self.xattr_sets.iter().position(|s| *s == set) {
            return Ok(XattrWireEntry::Reference(pos as u32));
        }
        if self.xattr_sets.len() >= MAX_SESSION_XATTR_SETS {
            return Err(SessionTableError::TableFull {
                table: SessionTable::XattrSet,
                cap: MAX_SESSION_XATTR_SETS,
            });
        }
        self.xattr_sets.push(set.clone());
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
