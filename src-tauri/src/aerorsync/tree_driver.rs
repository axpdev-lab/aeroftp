//! Tree-list reception: the multi-entry file list of an incremental
//! recursion session, received segment by segment (B3.1).
//!
//! In a real download the sender emits the initial list and then the
//! segments of its own lookahead, and then waits for the receiver's
//! requests: no item ndx ever arrives unprompted on our inbound stream.
//! So reception is split in two calls the B3.2 event loop drives:
//! [`AerorsyncDriver::receive_initial_tree_list`] reads only the initial
//! list (plus the id-name lists of a non-incremental session) and never
//! reads an ndx after it, and [`AerorsyncDriver::receive_tree_segment`]
//! decodes one more segment after the loop has classified a segment
//! marker with [`classify_tree_ndx`]. One [`FileListStreamDecoder`] per
//! segment hands the codec state to the next through
//! [`FileListStreamDecoder::into_parts`]; one [`SessionTables`] and the
//! inbound `NdxState` live for the whole session inside [`TreeListState`],
//! and bytes read past a segment's end-of-list stay in the state, never
//! lost. Items (ndx >= 0) are B3.2: nothing here consumes them.
//!
//! Two numberings exist per segment: [`TreeEntry::ndx`] is the SORTED ndx
//! (`ndx_start +` the entry's position in the segment's sorted order,
//! [`tree_entry_cmp`], sorted per segment in inc and once over the whole
//! list in noinc), which is the ndx the B3.2 generator requests items at;
//! [`TreeEntry::wire_pos`] is the 0-based wire position inside the segment
//! (wire ndx = `ndx_start + wire_pos`), the wire position used by B4 for
//! the hardlink references. With `--iconv` rsync keeps the wire order
//! (`need_unsorted_flist`, options.c:2049); the product never sends
//! `--iconv`. The gap `ndx_start - 1` at every segment boundary is the
//! numbering hole the parent directory leaves (A4).
//! Measured on p9big-dl-inc (NAS 3113): the sender uses that hole as the
//! item ndx of the segment's directory (iflags IS_NEW|LOCAL_CHANGE, no
//! data); B3.2 serves it, B3.1 only records it.

// SPDX-License-Identifier: MPL-2.0 OR GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use std::fmt;
use std::path::{Path, PathBuf};

use crate::aerorsync::events::EventSink;
use crate::aerorsync::native_driver::{
    map_realwire_error, AerorsyncDriver, AerorsyncSessionPhase, LiteralCompression,
    ITEM_REPORT_XATTR,
};
use crate::aerorsync::real_wire::{
    decode_delta_stream, decode_id_list, decode_ndx, decode_varint, encode_item_flags, encode_ndx,
    encode_varint, is_device_mode, is_directory_mode, is_regular_mode, is_special_mode,
    is_symlink_mode, resolve_xattr_datum_section, FileListCodecState, FileListDecodeOutcome,
    FileListEntry, FileListStreamDecoder, IdNameList, RealWireError, XattrPair,
    MAX_FILE_LIST_ENTRY_BYTES, NDX_DEL_STATS, NDX_DONE, NDX_FLIST_EOF, NDX_FLIST_OFFSET,
};
use crate::aerorsync::session_state::{ResolvedMetadata, SessionTables};
use crate::aerorsync::transport::RawRemoteShellTransport;
use crate::aerorsync::types::{AerorsyncError, AerorsyncErrorKind};

/// `CF_INC_RECURSE` from rsync `compat.c`/`rsync.h`: the peer negotiated
/// incremental recursion, so segments arrive interleaved with the items
/// and the first entry of the initial list is ndx 1, not 0 (a segment
/// boundary always leaves a gap).
const CF_INC_RECURSE: i32 = 1;
/// `CF_ID0_NAMES` from rsync `compat.c`/`rsync.h`: id lists carry a name
/// for id 0 after the zero terminator.
const CF_ID0_NAMES: i32 = 1 << 8;

/// Default session ceiling on tree-list entries. Provisional 1.000.000:
/// it must stay above the B0 gate p8-dl (50.001 session entries), and B3.4
/// measures the RSS per entry and fixes the definitive value.
pub const DEFAULT_MAX_TREE_ENTRIES: usize = 1_000_000;

/// Caller-supplied ceilings for tree-list reception. Both are per-entry /
/// per-session bounds, never a bound on the whole list buffer:
/// `FILE_LIST_BUFFER_MAX` in `native_driver.rs` stays where it is and only
/// guards the single-file path (D1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TreeListLimits {
    /// Session ceiling on decoded entries. Default
    /// [`DEFAULT_MAX_TREE_ENTRIES`].
    pub max_entries: usize,
    /// Per-entry byte ceiling, handed to the segment decoder. Default
    /// [`MAX_FILE_LIST_ENTRY_BYTES`].
    pub max_entry_bytes: usize,
}

impl Default for TreeListLimits {
    fn default() -> Self {
        Self {
            max_entries: DEFAULT_MAX_TREE_ENTRIES,
            max_entry_bytes: MAX_FILE_LIST_ENTRY_BYTES,
        }
    }
}

/// One decoded tree entry: the wire entry plus its resolved metadata, the
/// sorted ndx, the wire position (B4 needs the wire ndx for hardlinks),
/// and the device/special mark that B3.2 turns into
/// `Failed { SpecialUnsupported }` without a wire request.
#[derive(Debug, Clone)]
pub struct TreeEntry {
    /// Sorted ndx of this entry: `ndx_start +` its position in the
    /// segment's sorted order ([`tree_entry_cmp`]). This is the ndx the
    /// B3.2 generator requests items at.
    pub ndx: i32,
    /// 0-based wire position inside the segment (wire ndx =
    /// `ndx_start + wire_pos`): the wire position used by B4 for the
    /// hardlink references (`first_hlink_ndx`, flist.c:507-510).
    pub wire_pos: usize,
    /// The decoded entry, back-references still unresolved at codec level.
    pub entry: FileListEntry,
    /// ACL/xattr references resolved against the session tables.
    pub resolved: ResolvedMetadata,
    /// True for character/block devices, fifos and sockets: received and
    /// marked here, never refused at list level (D3).
    pub device_or_special: bool,
}

/// The sorted order of one segment's entries, `f_name_cmp` (flist.c:3204,
/// confirmed from source by Falena Q7, NAS 3291): at the same level a
/// `t_ITEM` (non-directory) beats a `t_PATH` (directory, flist.c:3256-3258),
/// then the name bytes compare, `/` included for multi-component paths
/// (flist.c:3260-3325); the basename "." (the implied top dir) always sorts
/// first (flist.c:3221-3225). The sort is rsync's stable merge sort
/// (flist.c:1738-1756), so `Vec::sort_by` (stable) is used, never
/// `sort_unstable_by`. The sort runs per segment in inc recursion and once
/// over the whole list in noinc (`flist_sort_and_clean`, flist.c:2968,
/// :2980); the next segment's `ndx_start` is the previous one plus `used`
/// plus 1 (flist.c:2918), so the gap is unchanged.
pub fn tree_entry_cmp(a: &TreeEntry, b: &TreeEntry) -> std::cmp::Ordering {
    flist_entry_cmp(&a.entry, &b.entry)
}

/// [`tree_entry_cmp`] over bare list entries, so tests can sort a decoded
/// capture list with the same rule the driver applies.
pub fn flist_entry_cmp(a: &FileListEntry, b: &FileListEntry) -> std::cmp::Ordering {
    // flist.c:3221-3225: the basename "." always sorts first.
    let dot_a = a.path == b".";
    let dot_b = b.path == b".";
    dot_b.cmp(&dot_a).then_with(|| {
        tree_entry_key(&a.path, is_directory_mode(a.mode))
            .cmp(&tree_entry_key(&b.path, is_directory_mode(b.mode)))
    })
}

/// Sort key of one entry: every parent component maps to `(1, component)`
/// (a parent component is always a directory), the last component to
/// `(is_dir, name)`; keys compare lexicographically.
fn tree_entry_key(path: &[u8], is_dir: bool) -> Vec<(u8, &[u8])> {
    let parts: Vec<&[u8]> = path.split(|c| *c == b'/').collect();
    let last = parts.last().copied().unwrap_or(&[][..]);
    let parents = &parts[..parts.len().saturating_sub(1)];
    let mut key: Vec<(u8, &[u8])> = parents.iter().map(|c| (1u8, *c)).collect();
    key.push((u8::from(is_dir), last));
    key
}

/// One file-list segment: the entries of one directory, starting at
/// `ndx_start`. `dir_ndx` is the `dir_flist` index the segment marker
/// carried (`NDX_FLIST_OFFSET - marker`), `None` for the initial list.
/// `gap_ndx` (= `ndx_start - 1`) is the numbering hole the parent
/// directory leaves (A4). The sender uses it as the item ndx of the
/// segment's directory (attributes only, no data); B3.2 serves it.
#[derive(Debug, Clone)]
pub struct DirSegment {
    /// Wire ndx of the segment's first entry.
    pub ndx_start: i32,
    /// `dir_flist` index announced by the segment marker; `None` for the
    /// initial list, which no marker announces.
    pub dir_ndx: Option<i32>,
    /// `ndx_start - 1`: the numbering gap, used by the sender as the item
    /// ndx of the segment's directory (no data); B3.2 serves it.
    pub gap_ndx: i32,
    /// The segment's entries in SORTED order ([`tree_entry_cmp`]): `ndx` is
    /// the sorted ndx (`ndx_start +` position here), `wire_pos` the 0-based
    /// wire position.
    pub entries: Vec<TreeEntry>,
}

/// The file list accumulated so far: every segment received, in arrival
/// order, plus the trailing id-name lists of a non-incremental session.
#[derive(Debug, Clone, Default)]
pub struct SessionFileList {
    pub segments: Vec<DirSegment>,
    /// Uid name list sent after a non-incremental list; empty otherwise.
    pub uid_names: IdNameList,
    /// Gid name list sent after a non-incremental list; empty otherwise.
    pub gid_names: IdNameList,
}

impl SessionFileList {
    /// Every entry of every segment, in segment order.
    pub fn entries(&self) -> impl Iterator<Item = &TreeEntry> {
        self.segments.iter().flat_map(|s| s.entries.iter())
    }

    /// Total number of entries across all segments.
    pub fn len(&self) -> usize {
        self.segments.iter().map(|s| s.entries.len()).sum()
    }

    /// True when no segment carried an entry.
    pub fn is_empty(&self) -> bool {
        self.segments.iter().all(|s| s.entries.is_empty())
    }
}

/// Per-entry outcome, B3.2 and later. Defined now so the B3.2 failure
/// taxonomy has a home; unused in B3.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryResult {
    Committed,
    Skipped,
    Failed(EntryFailure),
}

/// Why an entry failed, B3.2 and later.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryFailure {
    /// A device or special arrived in download: the generator never asks
    /// for it, so it fails without any wire request (D3).
    SpecialUnsupported,
    /// Whole-file checksum mismatch at finalize time (A3).
    WholeFileChecksumMismatch,
    /// Local I/O failure (create, write, symlink, metadata): the entry is
    /// marked Failed and the walk continues (D4).
    LocalIo,
    /// The peer chose a symlink whose target escapes the download root:
    /// Failed(UnsafeSymlink) and the walk continues.
    UnsafeSymlink,
}

/// What a decoded ndx means for the B3.2 event loop. Pure function:
/// `NDX_DONE` is a valid case, not an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeNdx {
    /// First (or next) item ndx, >= 0: the sender answered a request.
    Item(i32),
    /// A new segment was announced; `dir_ndx` is the `dir_flist` index
    /// the marker carried (`NDX_FLIST_OFFSET - ndx`).
    Segment { dir_ndx: i32 },
    /// The sender closed the file list (`NDX_FLIST_EOF`).
    FlistEof,
    /// End-of-phase marker (`NDX_DONE`).
    Done,
    /// Delete statistics (`NDX_DEL_STATS`); only ever seen after
    /// `FlistEof` with `--delete`, kept total so the classifier never
    /// panics on a sentinel it does not handle.
    DelStats,
}

/// Classify one decoded ndx for the B3.2 event loop.
pub fn classify_tree_ndx(ndx: i32) -> TreeNdx {
    if ndx >= 0 {
        TreeNdx::Item(ndx)
    } else if ndx == NDX_FLIST_EOF {
        TreeNdx::FlistEof
    } else if ndx == NDX_DONE {
        TreeNdx::Done
    } else if ndx == NDX_DEL_STATS {
        TreeNdx::DelStats
    } else {
        TreeNdx::Segment {
            dir_ndx: NDX_FLIST_OFFSET - ndx,
        }
    }
}

/// Session-wide state that survives between tree-list calls: the session
/// tables, the codec state handed segment to segment, the next wire ndx,
/// the entry total, the accumulated end-of-list I/O error flags, and the
/// bytes already read off the wire but not yet consumed (C1: `rest` from
/// `into_parts` and any frame tail past an end-of-list stay here, the
/// next reader consumes them first).
#[derive(Debug)]
pub struct TreeListState {
    pub tables: SessionTables,
    pub codec_state: FileListCodecState,
    /// Wire ndx the next decoded entry will take.
    pub next_ndx: i32,
    /// Entries decoded so far in this session.
    pub total_entries: usize,
    /// OR of every segment terminator's `io_error`; a segment that ends
    /// with an I/O error does not abort the tree (C3).
    pub io_error: i32,
    /// Bytes read from the wire but not consumed by any decoder yet.
    pub buffered: Vec<u8>,
    /// Session-wide zstd decompressor for the download path: stock rsync
    /// sends ONE zstd frame for the whole session, split across the
    /// files' DEFLATED_DATA records, so the DCtx must outlive each file.
    pub(crate) zstd_rx: Option<crate::aerorsync::real_wire::ZstdSessionDecoder>,
}

impl TreeListState {
    /// Fresh state for one session direction: with incremental recursion
    /// the first entry is ndx 1 (a segment boundary always leaves a gap),
    /// without it ndx 0.
    pub fn new(inc_recurse: bool) -> Self {
        Self {
            tables: SessionTables::new(),
            codec_state: FileListCodecState::new(),
            next_ndx: if inc_recurse { 1 } else { 0 },
            total_entries: 0,
            io_error: 0,
            buffered: Vec::new(),
            zstd_rx: None,
        }
    }
}

/// The result of [`AerorsyncDriver::receive_initial_tree_list`]: the
/// initial list as a segment, the session state to carry into the B3.2
/// event loop, and the id-name lists when the session is not incremental.
#[derive(Debug)]
pub struct TreeListStart {
    /// The initial list (`dir_ndx == None`).
    pub initial: DirSegment,
    /// Session state after the initial list; `buffered` may already hold
    /// the next ndx, unread (C1).
    pub state: TreeListState,
    /// Uid name list of a non-incremental session; empty otherwise.
    pub uid_names: IdNameList,
    /// Gid name list of a non-incremental session; empty otherwise.
    pub gid_names: IdNameList,
}

/// `PATH_MAX` bound for a joined local path (Linux default).
pub const PATH_MAX_BYTES: usize = 4096;

/// Why a peer-chosen file-list name is refused as a local path. In a tree
/// download the names come from the peer, so a hostile or broken sender
/// is the new attack surface: a refused name is a STREAM error, the
/// session stops (never a per-entry `Failed`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreePathError {
    /// Empty name.
    EmptyName,
    /// Absolute name (`/etc/x`).
    AbsoluteName,
    /// A `.` or `..` component.
    DotComponent,
    /// An empty component (`a//b`, trailing slash).
    EmptyComponent,
    /// A NUL byte in the name.
    NulByte,
    /// The joined path exceeds [`PATH_MAX_BYTES`].
    TooLong,
}

impl fmt::Display for TreePathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let msg = match self {
            TreePathError::EmptyName => "empty file-list name",
            TreePathError::AbsoluteName => "absolute file-list name",
            TreePathError::DotComponent => "file-list name with a . or .. component",
            TreePathError::EmptyComponent => "file-list name with an empty component",
            TreePathError::NulByte => "file-list name with a NUL byte",
            TreePathError::TooLong => "joined local path exceeds PATH_MAX",
        };
        f.write_str(msg)
    }
}

impl std::error::Error for TreePathError {}

impl From<TreePathError> for AerorsyncError {
    fn from(err: TreePathError) -> Self {
        // A refused peer-chosen name is a stream error: not a RealWireError,
        // so the retry classification never sees it.
        AerorsyncError::new(
            AerorsyncErrorKind::InvalidFrame,
            format!("unsafe peer-chosen path: {err}"),
        )
    }
}

/// Map one peer-chosen file-list name to a local path under `dest_root`.
/// Pure: no filesystem access. Refuses empty names, absolute names, `.` /
/// `..` / empty components, NUL bytes and joined paths over
/// [`PATH_MAX_BYTES`]. The returned path stays under `dest_root` by
/// construction; the no-symlink-component rule is enforced at creation
/// time (step 1's second rule).
pub fn tree_entry_local_path(
    dest_root: &std::path::Path,
    name: &[u8],
) -> Result<std::path::PathBuf, TreePathError> {
    if name.is_empty() {
        return Err(TreePathError::EmptyName);
    }
    if name[0] == b'/' {
        return Err(TreePathError::AbsoluteName);
    }
    if name.contains(&0u8) {
        return Err(TreePathError::NulByte);
    }
    for part in name.split(|b| *b == b'/') {
        if part.is_empty() {
            return Err(TreePathError::EmptyComponent);
        }
        if part == b"." || part == b".." {
            return Err(TreePathError::DotComponent);
        }
    }
    #[cfg(unix)]
    let joined = {
        use std::os::unix::ffi::OsStrExt;
        dest_root.join(std::ffi::OsStr::from_bytes(name))
    };
    #[cfg(not(unix))]
    let joined = match std::str::from_utf8(name) {
        Ok(text) => dest_root.join(text),
        Err(_) => return Err(TreePathError::NulByte),
    };
    if joined.as_os_str().len() > PATH_MAX_BYTES {
        return Err(TreePathError::TooLong);
    }
    Ok(joined)
}

/// Tree-list failure with its own type, so it never enters the driver's
/// retry classification (which inspects only `RealWireError`), exactly
/// like `SessionTableError` in `session_state.rs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeListError {
    /// The session decoded more entries than the caller's ceiling.
    TooManyEntries { limit: usize },
}

impl fmt::Display for TreeListError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TreeListError::TooManyEntries { limit } => {
                write!(f, "tree list exceeds the {limit} entry session limit")
            }
        }
    }
}

impl std::error::Error for TreeListError {}

impl From<TreeListError> for AerorsyncError {
    fn from(err: TreeListError) -> Self {
        // Typed and final: it is not a RealWireError, so the retry
        // classification never sees it.
        match err {
            TreeListError::TooManyEntries { limit } => AerorsyncError::new(
                AerorsyncErrorKind::InvalidFrame,
                format!("tree list session limit exceeded: {limit} entries"),
            ),
        }
    }
}

impl<T: RawRemoteShellTransport> AerorsyncDriver<T> {
    /// Receive only the initial file list, up to its end-of-list marker,
    /// plus the id-name lists when the session is not incremental (C4:
    /// sent only when `-o`/`-g`/ACL are negotiated and `--numeric-ids`
    /// is not, which this driver's argv never carries). Never reads an
    /// ndx after the list: bytes already fetched past the end-of-list stay
    /// in `TreeListStart::state.buffered` for the B3.2 loop (C1).
    pub async fn receive_initial_tree_list(
        &mut self,
        limits: TreeListLimits,
        bridge: &mut dyn EventSink,
    ) -> Result<TreeListStart, AerorsyncError> {
        self.phase = AerorsyncSessionPhase::FileListReceiving;
        let inc_recurse = self.compat_flags & CF_INC_RECURSE != 0;
        let mut state = TreeListState::new(inc_recurse);
        let initial = self
            .receive_tree_segment_inner(None, limits, &mut state, bridge)
            .await?;
        let mut uid_names = IdNameList::default();
        let mut gid_names = IdNameList::default();
        if !inc_recurse {
            // C4 (Falena Q5, rsync 3.2.7): send_id_lists (uidlist.c:407-413)
            // sends the uid list iff preserve_uid || preserve_acls and the
            // gid list iff preserve_gid || preserve_acls, and it is called
            // only from flist.c:2513-2514 under numeric_ids <= 0 &&
            // !inc_recurse; this driver's argv never carries --numeric-ids.
            let want_uids =
                self.effective_metadata.preserve_owner || self.effective_metadata.preserve_acls;
            let want_gids =
                self.effective_metadata.preserve_group || self.effective_metadata.preserve_acls;
            let id0_names = self.compat_flags & CF_ID0_NAMES != 0;
            if want_uids {
                uid_names = self
                    .read_tree_id_list(&mut state, id0_names, "tree list uid id list", bridge)
                    .await?;
            }
            if want_gids {
                gid_names = self
                    .read_tree_id_list(&mut state, id0_names, "tree list gid id list", bridge)
                    .await?;
            }
            state
                .tables
                .record_id_names(&uid_names, &gid_names)
                .map_err(|e| {
                    AerorsyncError::new(
                        AerorsyncErrorKind::InvalidFrame,
                        format!("tree list id names: {e}"),
                    )
                })?;
        }
        self.phase = AerorsyncSessionPhase::FileListReceived;
        Ok(TreeListStart {
            initial,
            state,
            uid_names,
            gid_names,
        })
    }

    /// Decode one more segment, called by the B3.2 event loop right after
    /// it classified a segment marker (`ndx <= NDX_FLIST_OFFSET`).
    /// `marker_ndx` is the raw marker; the `dir_flist` index it carries is
    /// kept on the returned segment (C2).
    pub async fn receive_tree_segment(
        &mut self,
        marker_ndx: i32,
        state: &mut TreeListState,
        limits: TreeListLimits,
        bridge: &mut dyn EventSink,
    ) -> Result<DirSegment, AerorsyncError> {
        if marker_ndx > NDX_FLIST_OFFSET {
            return Err(AerorsyncError::invalid_frame(format!(
                "receive_tree_segment called with non-segment marker {marker_ndx}"
            )));
        }
        let dir_ndx = NDX_FLIST_OFFSET - marker_ndx;
        self.receive_tree_segment_inner(Some(dir_ndx), limits, state, bridge)
            .await
    }

    /// Decode one segment (the initial list when `dir_ndx` is `None`)
    /// with one [`FileListStreamDecoder`], continuing the session codec
    /// state, resolving every entry against the session tables, and
    /// leaving the bytes past the end-of-list in `state.buffered`.
    async fn receive_tree_segment_inner(
        &mut self,
        dir_ndx: Option<i32>,
        limits: TreeListLimits,
        state: &mut TreeListState,
        bridge: &mut dyn EventSink,
    ) -> Result<DirSegment, AerorsyncError> {
        let opts = self.build_flist_options(self.negotiated_file_checksum_len()?);
        let ndx_start = state.next_ndx;
        // A4: the gap ndx_start - 1 is the numbering hole the parent
        // directory leaves; the sender reuses it as the segment directory's
        // item ndx (attributes only), so it is recorded, not skipped here.
        let gap_ndx = ndx_start - 1;
        let mut decoder = FileListStreamDecoder::new(
            opts,
            std::mem::take(&mut state.codec_state),
            limits.max_entries,
        )
        .with_max_entry_bytes(limits.max_entry_bytes);
        decoder.feed(&state.buffered);
        state.buffered.clear();
        let mut entries: Vec<TreeEntry> = Vec::new();
        let io_error = loop {
            self.check_cancel("receive_tree_list segment")?;
            match decoder.next_outcome() {
                Ok(Some(FileListDecodeOutcome::Entry(entry))) => {
                    let wire_ndx = state.next_ndx;
                    state.next_ndx += 1;
                    state.total_entries += 1;
                    if state.total_entries > limits.max_entries {
                        return Err(TreeListError::TooManyEntries {
                            limit: limits.max_entries,
                        }
                        .into());
                    }
                    let resolved = state.tables.resolve_entry(&entry).map_err(|e| {
                        AerorsyncError::new(
                            AerorsyncErrorKind::InvalidFrame,
                            format!("tree list session tables: {e}"),
                        )
                    })?;
                    let device_or_special =
                        is_device_mode(entry.mode) || is_special_mode(entry.mode);
                    entries.push(TreeEntry {
                        ndx: wire_ndx,
                        wire_pos: (wire_ndx - ndx_start) as usize,
                        entry,
                        resolved,
                        device_or_special,
                    });
                }
                Ok(Some(FileListDecodeOutcome::EndOfList { io_error })) => break io_error,
                Ok(None) => {
                    let payload = self.next_data_frame(bridge).await?;
                    decoder.feed(&payload);
                }
                Err(RealWireError::FileListTooManyEntries { limit }) => {
                    return Err(TreeListError::TooManyEntries { limit }.into());
                }
                Err(other) => {
                    return Err(map_realwire_error(other, "tree list segment"));
                }
            }
        };
        // C3: an end-of-list I/O error does not abort the tree; the flags
        // accumulate in the state and B3.2 decides how to present them.
        state.io_error |= io_error;
        let (codec_state, rest) = decoder.into_parts();
        state.codec_state = codec_state;
        // C1: bytes past the end-of-list (possibly the next ndx) stay in
        // the state; nothing is read past the segment here.
        state.buffered = rest;
        // NAS 3282 (Q-a): each segment is sorted on its own (a noinc
        // session is one single segment) and `ndx` becomes the SORTED ndx;
        // the wire position stays on `wire_pos`. The generator requests
        // items at the sorted ndx, not at the wire ndx.
        entries.sort_by(tree_entry_cmp);
        for (pos, e) in entries.iter_mut().enumerate() {
            e.ndx = ndx_start + pos as i32;
        }
        // Advance past the segment's entries plus the numbering gap.
        state.next_ndx += 1;
        Ok(DirSegment {
            ndx_start,
            dir_ndx,
            gap_ndx,
            entries,
        })
    }

    /// Read one trailing id-name list (uid or gid) of a non-incremental
    /// tree list, consuming from `state.buffered` first and pulling frames
    /// only when the list is incomplete.
    async fn read_tree_id_list(
        &mut self,
        state: &mut TreeListState,
        id0_names: bool,
        context: &'static str,
        bridge: &mut dyn EventSink,
    ) -> Result<IdNameList, AerorsyncError> {
        loop {
            self.check_cancel("receive_tree_list id list")?;
            if !state.buffered.is_empty() {
                match decode_id_list(&state.buffered, id0_names) {
                    Ok((list, consumed)) => {
                        state.buffered.drain(..consumed);
                        return Ok(list);
                    }
                    // Same incompleteness rule as the entry decoder: a
                    // declared name length that overshoots the buffer is
                    // recoverable by pulling another frame.
                    Err(RealWireError::TruncatedBuffer { .. }) => {}
                    Err(RealWireError::InvalidNameLen {
                        declared,
                        available,
                    }) if available < declared => {}
                    Err(other) => return Err(map_realwire_error(other, context)),
                }
            }
            let payload = self.next_data_frame(bridge).await?;
            state.buffered.extend_from_slice(&payload);
        }
    }
}

/// Options for [`AerorsyncDriver::drive_download_tree`]: the list limits
/// plus the metadata policies, mirroring the single-file defaults.
#[derive(Debug, Clone, Copy)]
pub struct TreeDownloadOptions {
    pub limits: TreeListLimits,
    pub preserve_acls: bool,
    pub preserve_xattrs: bool,
    pub preserve_uid: bool,
    pub preserve_gid: bool,
    pub fail_on_metadata_loss: bool,
}

impl Default for TreeDownloadOptions {
    fn default() -> Self {
        Self {
            limits: TreeListLimits::default(),
            preserve_acls: false,
            preserve_xattrs: false,
            preserve_uid: false,
            preserve_gid: false,
            fail_on_metadata_loss: false,
        }
    }
}

/// The outcome of [`AerorsyncDriver::drive_download_tree`]: one result per
/// file-list entry, in ndx order, the sender's summary (mandatory: a
/// download without stats is a stream error), and the accumulated
/// end-of-list I/O error flags of the tree.
#[derive(Debug)]
pub struct TreeDownloadOutcome {
    pub results: Vec<(i32, EntryResult)>,
    pub summary: crate::aerorsync::real_wire::SummaryFrame,
    pub io_error: i32,
}

/// iflags of a file item request, from the p9-dl-inc capture (0xa000 =
/// `ITEM_TRANSFER | ITEM_IS_NEW`); the capture is the source, not a
/// hand-written constant.
const TREE_ITEM_IFLAGS: u16 = 0xa000;
/// iflags of a directory item request (the segment gap `ndx_start - 1`,
/// Q4): `ITEM_IS_NEW | ITEM_LOCAL_CHANGE`, no `ITEM_TRANSFER`, attrs only.
/// In a noinc session every directory entry is requested the same way at
/// its own sorted ndx (NAS 3282, Q-c).
const TREE_DIR_ITEM_IFLAGS: u16 = 0x6000;
/// Phase NDX_DONE markers sent BEFORE waiting for the echoes and the stats
/// (generator.c:2366, :2370, :2392/:2394). The 5th phase marker
/// (generator.c:2420) goes out after the SummaryFrame as our ACK, which
/// the trailing NDX_DONE answers: S dir_flist dones (one per passage to
/// the next list, including the exit from the initial list; none after the
/// last one, NAS 3282) + 4 here + 1 ACK = S + 5 in total.
const TREE_PHASE_NDX_DONE_COUNT: usize = 4;
/// Extra NDX_DONE echoes the sender owes on top of the dir_flist dones
/// (sender.c:234-258): S + 3 in download, then stats, then one final one.
const TREE_SENDER_ECHO_EXTRA: i32 = 3;

/// Generator-side xattr request section (`xattrs.c::send_xattr_request`):
/// one skip varint per over-threshold attribute (the distance from the
/// previous one, starting from a notional -1, so the first record is
/// always 1), then a single `varint(0)`. Sent between the iflags and the
/// sum_head when the iflags carry `ITEM_REPORT_XATTR`; a set of small-only
/// attributes still emits the bare terminator.
fn encode_xattr_request_section(pairs: &[XattrPair]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut previous: isize = -1;
    for (index, pair) in pairs.iter().enumerate() {
        if !pair.datum.is_out_of_band() {
            continue;
        }
        out.extend_from_slice(&encode_varint((index as isize - previous) as i32));
        previous = index as isize;
    }
    out.extend_from_slice(&encode_varint(0));
    out
}

fn ingest_segment(
    seg: &DirSegment,
    by_ndx: &mut std::collections::HashMap<i32, TreeEntry>,
    total_size: &mut i64,
) {
    for e in &seg.entries {
        if !is_directory_mode(e.entry.mode) {
            *total_size += e.entry.size.max(0);
        }
        by_ndx.insert(e.ndx, e.clone());
    }
}

/// Refuse if any intermediate component of `local` under `root` is a
/// symlink (checked with `symlink_metadata`, never followed). A symlink
/// component is a stream error: the peer chose a name that escapes the
/// tree we are building.
async fn ensure_no_symlink_components(root: &Path, local: &Path) -> Result<(), AerorsyncError> {
    let rel = local
        .strip_prefix(root)
        .map_err(|_| AerorsyncError::invalid_frame("tree entry path escapes the download root"))?;
    let mut cur = root.to_path_buf();
    for comp in rel.components() {
        use std::path::Component;
        match comp {
            Component::Normal(os) => {
                cur.push(os);
                match tokio::fs::symlink_metadata(&cur).await {
                    Ok(md) if md.file_type().is_symlink() => {
                        return Err(AerorsyncError::invalid_frame(format!(
                            "symlink component {} in the tree path",
                            cur.display()
                        )));
                    }
                    _ => {}
                }
            }
            _ => {
                return Err(AerorsyncError::invalid_frame(
                    "tree entry path has a non-normal component",
                ))
            }
        }
    }
    Ok(())
}

impl<T: RawRemoteShellTransport> AerorsyncDriver<T> {
    /// Download a whole tree, lock-step (A1): the generator (us) walks the
    /// file list with a cursor, one entry at a time, and sends the request
    /// for an entry before reading its echo; the sender never sends an
    /// item ndx unprompted. Items are requested at the SORTED ndx
    /// ([`tree_entry_cmp`], NAS 3282 Q-a). Entering a segment requests its
    /// parent dir item (gap `ndx_start - 1`, iflags 0x6000, Q4); in a noinc
    /// session every directory entry is instead requested as an item at its
    /// own sorted ndx (Q-c). Leaving a dir_flist sends its dir_flist done
    /// NDX_DONE, including the exit from the initial list (Falena's
    /// p9-dl-inc row 0x1a, NAS 3282); no done goes out after the LAST list.
    /// In inc recursion directory and symlink list entries are served
    /// locally without a wire request; specials fail without one (D3).
    /// While waiting for an echo, new segments and NDX_DONE echoes may
    /// arrive; an item ndx different from the requested one is a stream
    /// error. After NDX_FLIST_EOF: the 4 pre-stats phase markers, then
    /// exactly the echoes still owed (S + 3 minus counted), the
    /// SummaryFrame (mandatory), the 5th phase marker as our ACK, the
    /// trailing NDX_DONE. Total NDX_DONE we send: S + 5.
    pub async fn drive_download_tree(
        &mut self,
        dest_root: &Path,
        options: TreeDownloadOptions,
        adapter: &dyn crate::aerorsync::engine_adapter::DeltaEngineAdapter,
        sink: &mut dyn EventSink,
    ) -> Result<TreeDownloadOutcome, AerorsyncError> {
        let inc_recurse = self.compat_flags & CF_INC_RECURSE != 0;
        let start = self.receive_initial_tree_list(options.limits, sink).await?;
        let mut state = start.state;
        let mut segments: Vec<DirSegment> = vec![start.initial];
        let mut by_ndx: std::collections::HashMap<i32, TreeEntry> =
            std::collections::HashMap::new();
        let mut total_size: i64 = 0;
        ingest_segment(&segments[0], &mut by_ndx, &mut total_size);
        let mut results: Vec<(i32, EntryResult)> = Vec::new();
        let mut pending_dirs: Vec<(i32, PathBuf, i64, Option<i32>)> = Vec::new();
        let mut seg_idx = 0usize;
        let mut entry_idx = 0usize;
        let mut need_gap_req = false;
        // In a noinc session the single list is complete right after the
        // id lists: no NDX_FLIST_EOF follows it on the wire (measured on
        // p9-dl-noinc), so the walk treats it as done from the start.
        let mut list_done = !inc_recurse;
        let mut done_echoes: i32 = 0;
        'outer: loop {
            if need_gap_req {
                need_gap_req = false;
                let gap = segments[seg_idx].gap_ndx;
                self.send_tree_dir_item_request(gap).await?;
                let _ = self
                    .wait_tree_echo(
                        gap,
                        &mut state,
                        &mut segments,
                        &mut by_ndx,
                        &mut total_size,
                        &mut done_echoes,
                        &mut list_done,
                        &options,
                        sink,
                    )
                    .await?;
            }
            if entry_idx >= segments[seg_idx].entries.len() {
                // The cursor leaves this dir_flist.
                if seg_idx + 1 < segments.len() {
                    // A passage to another dir_flist exists, so its done
                    // goes out now, including when leaving the INITIAL list
                    // (NAS 3282: Falena's p9-dl-inc table, row 0x1a, the done
                    // right after the ndx-3 request).
                    self.emit_ndx_done_marker().await?;
                    seg_idx += 1;
                    entry_idx = 0;
                    need_gap_req = true;
                    continue 'outer;
                }
                if list_done {
                    // Leaving the LAST dir_flist: no NDX_DONE after it.
                    break 'outer;
                }
                // read more of the list (lookahead)
                let ndx = self.read_tree_ndx(&mut state, sink).await?;
                match classify_tree_ndx(ndx) {
                    TreeNdx::Segment { .. } => {
                        let seg = self
                            .receive_tree_segment(ndx, &mut state, options.limits, sink)
                            .await?;
                        ingest_segment(&seg, &mut by_ndx, &mut total_size);
                        segments.push(seg);
                    }
                    TreeNdx::Done => {
                        done_echoes += 1;
                    }
                    TreeNdx::FlistEof | TreeNdx::DelStats => {
                        list_done = true;
                    }
                    TreeNdx::Item(n) => {
                        return Err(AerorsyncError::invalid_frame(format!(
                            "item ndx {n} arrived without a request"
                        )))
                    }
                }
                continue 'outer;
            }
            let entry = segments[seg_idx].entries[entry_idx].clone();
            entry_idx += 1;
            let mode = entry.entry.mode;
            if entry.device_or_special {
                results.push((
                    entry.ndx,
                    EntryResult::Failed(EntryFailure::SpecialUnsupported),
                ));
                continue 'outer;
            }
            if is_directory_mode(mode) {
                if !inc_recurse {
                    // Q-c (NAS 3282): in a noinc session every directory of
                    // the list is requested as an item (0x6000) at its sorted
                    // ndx, in list order (p9-dl-noinc: 0, 2, 5, 7, 9). In inc
                    // only the segment gap item is sent (Q4).
                    self.send_tree_dir_item_request(entry.ndx).await?;
                    let _ = self
                        .wait_tree_echo(
                            entry.ndx,
                            &mut state,
                            &mut segments,
                            &mut by_ndx,
                            &mut total_size,
                            &mut done_echoes,
                            &mut list_done,
                            &options,
                            sink,
                        )
                        .await?;
                }
                let result = self
                    .serve_tree_dir(dest_root, &entry, &mut pending_dirs)
                    .await?;
                if matches!(result, EntryResult::Committed) {
                    self.committed = true;
                }
                results.push((entry.ndx, result));
                continue 'outer;
            }
            if is_symlink_mode(mode) {
                let result = self.serve_tree_symlink(dest_root, &entry).await?;
                if matches!(result, EntryResult::Committed) {
                    self.committed = true;
                }
                results.push((entry.ndx, result));
                continue 'outer;
            }
            if !is_regular_mode(mode) {
                results.push((entry.ndx, EntryResult::Failed(EntryFailure::LocalIo)));
                continue 'outer;
            }
            // Regular file: send ndx + iflags (+ xattr request section) +
            // sum_head, wait for the echo.
            self.send_tree_file_request(dest_root, &entry, adapter)
                .await?;
            let want = entry.ndx;
            let echo_iflags = self
                .wait_tree_echo(
                    want,
                    &mut state,
                    &mut segments,
                    &mut by_ndx,
                    &mut total_size,
                    &mut done_echoes,
                    &mut list_done,
                    &options,
                    sink,
                )
                .await?;
            let result = self
                .serve_tree_file(
                    dest_root,
                    &entry,
                    echo_iflags,
                    &options,
                    adapter,
                    &mut state,
                    sink,
                )
                .await?;
            if matches!(result, EntryResult::Committed) {
                self.committed = true;
            }
            results.push((entry.ndx, result));
        }
        // Closing sequence (Q1, NAS 3282): the 4 pre-stats phase markers,
        // sent BEFORE waiting for the echoes; the 5th (the ACK after the
        // SummaryFrame, generator.c:2420) goes out below. With the S
        // dir_flist dones that is S + 5 NDX_DONE in total.
        self.write_data_frame(&[0x00; TREE_PHASE_NDX_DONE_COUNT])
            .await?;
        // Then exactly the echoes still owed: S + 3 minus the counted ones.
        let segments_extra = (segments.len() - 1) as i32;
        while done_echoes < segments_extra + TREE_SENDER_ECHO_EXTRA {
            let ndx = self.read_tree_ndx(&mut state, sink).await?;
            match classify_tree_ndx(ndx) {
                TreeNdx::Done => done_echoes += 1,
                other => {
                    return Err(AerorsyncError::invalid_frame(format!(
                        "expected NDX_DONE echo, got {other:?}"
                    )))
                }
            }
        }
        // Stats (mandatory), our ACK, the trailing NDX_DONE: the same close
        // rule as the single-file path.
        self.receive_summary_phase(sink).await?;
        let summary = self.received_summary().cloned().ok_or_else(|| {
            AerorsyncError::invalid_frame("download closed without a SummaryFrame")
        })?;
        if summary.total_size != total_size {
            return Err(AerorsyncError::invalid_frame(format!(
                "total_size mismatch: list says {total_size}, sender summary says {}",
                summary.total_size
            )));
        }
        self.emit_ndx_done_marker().await?;
        self.read_trailing_ndx_done(sink).await?;
        // Directory mtimes last, after their children, like rsync. A failed
        // set marks that directory's row Failed(LocalIo) (D4).
        for (ndx, path, mtime, nsec) in &pending_dirs {
            if set_mtime(path, *mtime, *nsec).await.is_err() {
                if let Some(row) = results.iter_mut().find(|(n, _)| n == ndx) {
                    row.1 = EntryResult::Failed(EntryFailure::LocalIo);
                }
            }
        }
        Ok(TreeDownloadOutcome {
            results,
            summary,
            io_error: state.io_error,
        })
    }

    /// Send one directory item request: ndx + attrs-only iflags, no
    /// sum_head (Q4: the sender echoes the item back with the same iflags
    /// and no data; the receiver applies only the attributes).
    async fn send_tree_dir_item_request(&mut self, ndx: i32) -> Result<(), AerorsyncError> {
        let mut payload = encode_ndx(ndx, &mut self.outbound_ndx_state);
        payload.extend_from_slice(&encode_item_flags(TREE_DIR_ITEM_IFLAGS));
        self.write_data_frame(&payload).await
    }

    /// Send one file item request: ndx + iflags (+ the xattr request
    /// section when the entry carries attributes and `-X` was negotiated)
    /// + sum_head. The signatures use the same construction as the
    /// single-file path (block size from the adapter, strong = negotiated
    /// block algo truncated to s2length); with no local baseline the head
    /// is all zeros, exactly like the p9-dl-inc capture.
    async fn send_tree_file_request(
        &mut self,
        dest_root: &Path,
        entry: &TreeEntry,
        adapter: &dyn crate::aerorsync::engine_adapter::DeltaEngineAdapter,
    ) -> Result<(), AerorsyncError> {
        use crate::aerorsync::real_wire::{encode_sum_block, encode_sum_head, SumBlock, SumHead};
        let local = tree_entry_local_path(dest_root, &entry.entry.path)?;
        let baseline: Vec<u8> = match tokio::fs::metadata(&local).await {
            Ok(md) if md.is_file() => tokio::fs::read(&local).await.unwrap_or_default(),
            _ => Vec::new(),
        };
        let (head, sum_blocks) = if baseline.is_empty() {
            (
                SumHead {
                    count: 0,
                    block_length: 0,
                    checksum_length: 0,
                    remainder_length: 0,
                },
                Vec::new(),
            )
        } else {
            let file_len = baseline.len() as u64;
            let block_size = adapter.compute_block_size(file_len);
            let s2length = crate::aerorsync::native_driver::A2_2_DOWNLOAD_S2LENGTH;
            let strong_algo = self.block_strong_algo();
            let sigs = adapter.build_signatures(&baseline, block_size);
            let block_length = block_size as i32;
            let remainder_length = if file_len == 0 {
                0
            } else {
                (file_len % block_size as u64) as i32
            };
            let head = SumHead {
                count: sigs.len() as i32,
                block_length,
                checksum_length: s2length,
                remainder_length,
            };
            let mut blocks = Vec::with_capacity(sigs.len());
            for sig in &sigs {
                let start = sig.index as usize * block_size;
                let end = start
                    .saturating_add(sig.block_len as usize)
                    .min(baseline.len());
                let block = baseline.get(start..end).unwrap_or(&[]);
                let strong_wire = self.wire_block_strong(block, strong_algo);
                let strong = strong_wire[..(s2length as usize).min(strong_wire.len())].to_vec();
                blocks.push(SumBlock {
                    rolling: sig.rolling,
                    strong,
                });
            }
            (head, blocks)
        };
        // X.2b: an entry that carries attributes is requested with
        // ITEM_REPORT_XATTR (measured p10-dl: 0xa100 for entries with a
        // set, 0xa000 for f4 whose set is empty), and the request section
        // rides between the iflags and the sum_head.
        let xattr_pairs: Option<&[XattrPair]> = entry.resolved.xattrs.as_deref();
        let report_xattr = self.negotiated_xattrs && xattr_pairs.is_some_and(|p| !p.is_empty());
        let iflags = if report_xattr {
            TREE_ITEM_IFLAGS | ITEM_REPORT_XATTR
        } else {
            TREE_ITEM_IFLAGS
        };
        let mut payload = encode_ndx(entry.ndx, &mut self.outbound_ndx_state);
        payload.extend_from_slice(&encode_item_flags(iflags));
        if let Some(pairs) = xattr_pairs.filter(|_| report_xattr) {
            payload.extend_from_slice(&encode_xattr_request_section(pairs));
        }
        payload.extend_from_slice(&encode_sum_head(&head));
        for b in &sum_blocks {
            payload.extend_from_slice(&encode_sum_block(b));
        }
        self.write_data_frame(&payload).await
    }

    /// Read until the echo of `want`: consume its ndx AND its 2-byte
    /// iflags, and return those iflags (the caller needs them to know
    /// whether an out-of-band xattr datum section follows). Segments and
    /// NDX_DONE echoes may arrive in between (a segment is received and
    /// ingested, but its gap dir request is deferred to the main loop; NO
    /// dir_flist done is sent here: the done is tied to the cursor leaving
    /// a dir_flist, not to a segment arriving). Any other item ndx is a
    /// stream error.
    async fn wait_tree_echo(
        &mut self,
        want: i32,
        state: &mut TreeListState,
        segments: &mut Vec<DirSegment>,
        by_ndx: &mut std::collections::HashMap<i32, TreeEntry>,
        total_size: &mut i64,
        done_echoes: &mut i32,
        list_done: &mut bool,
        options: &TreeDownloadOptions,
        sink: &mut dyn EventSink,
    ) -> Result<u16, AerorsyncError> {
        use crate::aerorsync::real_wire::decode_item_flags;
        loop {
            let ndx = self.read_tree_ndx(state, sink).await?;
            match classify_tree_ndx(ndx) {
                TreeNdx::Item(n) if n == want => {
                    // consume the echo's iflags too
                    loop {
                        self.check_cancel("tree echo iflags")?;
                        if state.buffered.len() >= 2 {
                            match decode_item_flags(&state.buffered) {
                                Ok((iflags, c)) => {
                                    state.buffered.drain(..c);
                                    return Ok(iflags);
                                }
                                Err(e) => return Err(map_realwire_error(e, "tree echo iflags")),
                            }
                        }
                        let p = self.next_data_frame(sink).await?;
                        state.buffered.extend_from_slice(&p);
                    }
                }
                TreeNdx::Item(n) => {
                    return Err(AerorsyncError::invalid_frame(format!(
                        "item ndx {n} echoed while waiting for {want}"
                    )))
                }
                TreeNdx::Done => *done_echoes += 1,
                TreeNdx::Segment { .. } => {
                    let seg = self
                        .receive_tree_segment(ndx, state, options.limits, sink)
                        .await?;
                    ingest_segment(&seg, by_ndx, total_size);
                    segments.push(seg);
                }
                TreeNdx::FlistEof | TreeNdx::DelStats => {
                    // the whole list may arrive before the first item echo
                    // (lookahead): mark it and keep waiting for the echo
                    *list_done = true;
                }
            }
        }
    }

    /// Read the next ndx off the wire into `state.buffered` first.
    async fn read_tree_ndx(
        &mut self,
        state: &mut TreeListState,
        sink: &mut dyn EventSink,
    ) -> Result<i32, AerorsyncError> {
        loop {
            self.check_cancel("tree download ndx")?;
            if !state.buffered.is_empty() {
                match decode_ndx(&state.buffered, &mut self.inbound_ndx_state) {
                    Ok((n, consumed)) => {
                        state.buffered.drain(..consumed);
                        return Ok(n);
                    }
                    Err(RealWireError::NdxTruncated { .. }) => {}
                    Err(other) => return Err(map_realwire_error(other, "tree download ndx")),
                }
            }
            let payload = self.next_data_frame(sink).await?;
            state.buffered.extend_from_slice(&payload);
        }
    }

    /// Serve a directory entry: mkdir (path rules of step 1), mode now,
    /// mtime deferred to the end (after its children, like rsync).
    async fn serve_tree_dir(
        &mut self,
        dest_root: &Path,
        entry: &TreeEntry,
        pending_dirs: &mut Vec<(i32, PathBuf, i64, Option<i32>)>,
    ) -> Result<EntryResult, AerorsyncError> {
        let local = tree_entry_local_path(dest_root, &entry.entry.path)?;
        ensure_no_symlink_components(dest_root, &local).await?;
        match tokio::fs::create_dir(&local).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Ok(EntryResult::Failed(EntryFailure::LocalIo)),
        }
        let _ = tokio::fs::set_permissions(
            &local,
            std::os::unix::fs::PermissionsExt::from_mode(entry.entry.mode),
        )
        .await;
        pending_dirs.push((entry.ndx, local, entry.entry.mtime, entry.entry.mtime_nsec));
        Ok(EntryResult::Committed)
    }

    /// Serve a symlink entry: never followed, target validated against the
    /// download root; an unsafe target is Failed(UnsafeSymlink) and the
    /// walk continues (rsync --safe-links behaviour).
    async fn serve_tree_symlink(
        &mut self,
        dest_root: &Path,
        entry: &TreeEntry,
    ) -> Result<EntryResult, AerorsyncError> {
        let local = tree_entry_local_path(dest_root, &entry.entry.path)?;
        ensure_no_symlink_components(dest_root, &local).await?;
        let Some(target_bytes) = entry.entry.symlink_target.as_deref() else {
            return Ok(EntryResult::Failed(EntryFailure::LocalIo));
        };
        let Ok(target) = std::str::from_utf8(target_bytes) else {
            return Ok(EntryResult::Failed(EntryFailure::LocalIo));
        };
        let rel = local.strip_prefix(dest_root).unwrap_or(&local);
        #[cfg(unix)]
        {
            if !crate::aerorsync::delta_transport_impl::symlink_target_is_safe_in_root(rel, target)
            {
                return Ok(EntryResult::Failed(EntryFailure::UnsafeSymlink));
            }
            return match tokio::fs::symlink(target, &local).await {
                Ok(()) => Ok(EntryResult::Committed),
                Err(_) => Ok(EntryResult::Failed(EntryFailure::LocalIo)),
            };
        }
        #[cfg(not(unix))]
        {
            let _ = (rel, target);
            Ok(EntryResult::Failed(EntryFailure::LocalIo))
        }
    }

    /// Serve one regular file whose echo ndx + iflags were just consumed:
    /// when the echoed iflags carry `ITEM_REPORT_XATTR` the sender's
    /// out-of-band xattr datum section follows (before the sum_head), then
    /// the delta is read from `state.buffered` (C1: bytes past the entry
    /// stay there), written through a `StreamingAtomicWriter`, verified
    /// against the whole-file checksum (step 3 helper), then finalized.
    async fn serve_tree_file(
        &mut self,
        dest_root: &Path,
        entry: &TreeEntry,
        echo_iflags: u16,
        options: &TreeDownloadOptions,
        adapter: &dyn crate::aerorsync::engine_adapter::DeltaEngineAdapter,
        state: &mut TreeListState,
        sink: &mut dyn EventSink,
    ) -> Result<EntryResult, AerorsyncError> {
        let local = tree_entry_local_path(dest_root, &entry.entry.path)?;
        ensure_no_symlink_components(dest_root, &local).await?;
        let file_checksum_len = self.negotiated_file_checksum_len()?;
        // The delta grammar follows the session's negotiated compression
        // (the same state the single-file path reads), never the test argv.
        let compression = self.literal_compression();
        let grammar = tree_delta_grammar(&compression);
        let mut xattr_pairs = if options.preserve_xattrs {
            entry.resolved.xattrs.clone()
        } else {
            None
        };
        // X.2b: on the sender's stream the out-of-band xattr datum section
        // sits between the echoed iflags and the sum_head (measured,
        // 06-xattr-oob-wire-evidence.md §2; sender.c writes it right after
        // write_ndx_and_attrs). Consume it whenever the echo carries the
        // bit; the values fold into the entry's pairs, MD5-verified.
        if self.negotiated_xattrs && echo_iflags & ITEM_REPORT_XATTR != 0 {
            match xattr_pairs.as_mut() {
                Some(pairs) => loop {
                    self.check_cancel("tree file xattr oob")?;
                    if !state.buffered.is_empty() {
                        // Resolve into a candidate so a truncated record
                        // cannot leave an earlier one half-resolved.
                        let mut candidate = pairs.clone();
                        match resolve_xattr_datum_section(&state.buffered, &mut candidate) {
                            Ok(consumed) => {
                                state.buffered.drain(..consumed);
                                *pairs = candidate;
                                break;
                            }
                            Err(RealWireError::TruncatedBuffer { .. }) => {}
                            Err(e) => return Err(map_realwire_error(e, "tree file xattr oob")),
                        }
                    }
                    let p = self.next_data_frame(sink).await?;
                    state.buffered.extend_from_slice(&p);
                },
                None => loop {
                    // No -X locally: skip the section without applying it.
                    self.check_cancel("tree file xattr oob skip")?;
                    if !state.buffered.is_empty() {
                        match decode_varint(&state.buffered) {
                            Ok((skip, c)) => {
                                if skip == 0 {
                                    state.buffered.drain(..c);
                                    break;
                                }
                                // One record = skip + len + datum; commit the
                                // drain only when the whole record is in the
                                // buffer, so a frame-straddling datum just
                                // pulls another frame.
                                match decode_varint(&state.buffered[c..]) {
                                    Ok((len, c2))
                                        if len >= 0
                                            && state.buffered.len() >= c + c2 + len as usize =>
                                    {
                                        state.buffered.drain(..c + c2 + len as usize);
                                    }
                                    Ok((len, _)) if len < 0 => {
                                        return Err(AerorsyncError::invalid_frame(
                                            "tree xattr oob negative datum length",
                                        ));
                                    }
                                    _ => {}
                                }
                            }
                            Err(RealWireError::TruncatedBuffer { .. }) => {}
                            Err(e) => return Err(map_realwire_error(e, "tree xattr oob skip")),
                        }
                    }
                    let p = self.next_data_frame(sink).await?;
                    state.buffered.extend_from_slice(&p);
                },
            }
        }
        // The sender's answer carries its own sum_head after the iflags
        // (already consumed by wait_tree_echo): skip it before the delta.
        // The count feeds decode_delta_stream below as Some(head.count).
        let sum_head_count: Option<i32> = loop {
            self.check_cancel("tree file sum_head")?;
            if !state.buffered.is_empty() {
                match crate::aerorsync::real_wire::decode_sum_head(&state.buffered) {
                    Ok((head, consumed)) => {
                        state.buffered.drain(..consumed);
                        break Some(head.count);
                    }
                    Err(RealWireError::TruncatedBuffer { .. }) => {}
                    Err(e) => return Err(map_realwire_error(e, "tree file sum_head")),
                }
            }
            let p = self.next_data_frame(sink).await?;
            state.buffered.extend_from_slice(&p);
        };
        let (ops, file_checksum) = loop {
            self.check_cancel("tree file delta")?;
            if !state.buffered.is_empty() {
                let decoded = match grammar {
                    // Compressed sessions share the single-file decoder;
                    // there is no second copy of the grammar here.
                    TreeDeltaGrammar::Compressed => {
                        decode_delta_stream(&state.buffered, file_checksum_len, sum_head_count)
                            .map(|(report, consumed)| (report.ops, report.file_checksum, consumed))
                    }
                    TreeDeltaGrammar::Raw => {
                        decode_delta_stream_raw(&state.buffered, file_checksum_len)
                    }
                };
                match decoded {
                    Ok((ops, csum, consumed)) => {
                        state.buffered.drain(..consumed);
                        break (ops, csum);
                    }
                    Err(RealWireError::DeltaTokenTruncated { .. })
                    | Err(RealWireError::TruncatedBuffer { .. }) => {}
                    Err(e) => return Err(map_realwire_error(e, "tree file delta")),
                }
            }
            let p = self.next_data_frame(sink).await?;
            state.buffered.extend_from_slice(&p);
        };
        let baseline: Vec<u8> = match tokio::fs::metadata(&local).await {
            Ok(md) if md.is_file() => tokio::fs::read(&local).await.unwrap_or_default(),
            _ => Vec::new(),
        };
        let engine_ops = self.delta_wire_to_engine_ops(&ops, &compression, &mut state.zstd_rx)?;
        let block_size = adapter.compute_block_size(baseline.len() as u64).max(1);
        let reconstructed = adapter
            .apply_delta(&baseline, &engine_ops, block_size)
            .map_err(|e| AerorsyncError::invalid_frame(format!("apply_delta: {e}")))?;
        let mut writer =
            match crate::aerorsync::streaming_writer::StreamingAtomicWriter::new(&local).await {
                Ok(w) => w,
                Err(_) => return Ok(EntryResult::Failed(EntryFailure::LocalIo)),
            };
        use tokio::io::AsyncWriteExt;
        if writer.write_all(&reconstructed).await.is_err() || writer.flush().await.is_err() {
            crate::aerorsync::delta_transport_impl::discard_streaming_temp(writer).await;
            return Ok(EntryResult::Failed(EntryFailure::LocalIo));
        }
        // Whole-file verify before finalize (step 3 helper, unseeded).
        let algo = self.effective_checksum_algo();
        let actual = self
            .file_checksum_kind()
            .ok()
            .map(|k| k.digest(&reconstructed));
        let decision = crate::aerorsync::delta_transport_impl::decide_whole_file_checksum(
            algo,
            Some(&file_checksum),
            actual.as_deref(),
        );
        use crate::aerorsync::delta_transport_impl::WholeFileChecksumDecision as D;
        match decision {
            D::Match | D::Absent => {}
            D::Mismatch => {
                crate::aerorsync::delta_transport_impl::discard_streaming_temp(writer).await;
                return Ok(EntryResult::Failed(EntryFailure::WholeFileChecksumMismatch));
            }
            D::Unverifiable => {
                crate::aerorsync::delta_transport_impl::discard_streaming_temp(writer).await;
                return Err(AerorsyncError::invalid_frame(
                    "tree file has a checksum but no resolved algorithm; refusing unverified",
                ));
            }
        }
        // The pairs as resolved above (the OOB section may have filled in
        // the over-threshold values).
        let xattrs = xattr_pairs;
        let acls = if options.preserve_acls {
            entry.entry.acls.clone()
        } else {
            None
        };
        match writer
            .finalize(
                Some(entry.entry.mode),
                Some((
                    entry.entry.mtime,
                    entry.entry.mtime_nsec.unwrap_or(0) as u32,
                )),
                xattrs,
                acls,
                options.fail_on_metadata_loss,
            )
            .await
        {
            Ok(_) => Ok(EntryResult::Committed),
            Err(_) => Ok(EntryResult::Failed(EntryFailure::LocalIo)),
        }
    }
}

/// Decode an uncompressed (no `-z`) delta token stream: int32 LE tokens,
/// `0` = end of file data, `> 0` = literal of that many raw bytes,
/// `< 0` = single-block match `-(tag + 1)`, then the `checksum_len`-byte
/// whole-file checksum. Measured on the b0c0 no-`-z` captures
/// (`capture/decode_rsync_wire.py`: `tok = int32; 0 end; >0 literal; <0 match`).
/// Returns the ops, the file checksum and the consumed byte count.
/// Which delta grammar the sender uses in this session. The choice comes
/// from the negotiated compression alone, read through
/// `AerorsyncDriver::literal_compression` (native_driver.rs:4123, over
/// `negotiated_compression_algo` at native_driver.rs:4111), never from test
/// argv: with a negotiated compressor the tokens follow the compressed
/// grammar and the tree path shares the single-file decoder; without
/// compression the tokens are raw int32 LE and `decode_delta_stream_raw`
/// reads them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TreeDeltaGrammar {
    Compressed,
    Raw,
}

pub(crate) fn tree_delta_grammar(compression: &LiteralCompression) -> TreeDeltaGrammar {
    match compression {
        LiteralCompression::None | LiteralCompression::Unsupported(_) => TreeDeltaGrammar::Raw,
        LiteralCompression::Zstd | LiteralCompression::Deflate => TreeDeltaGrammar::Compressed,
    }
}

fn decode_delta_stream_raw(
    buf: &[u8],
    checksum_len: usize,
) -> Result<(Vec<crate::aerorsync::real_wire::DeltaOp>, Vec<u8>, usize), RealWireError> {
    use crate::aerorsync::real_wire::DeltaOp;
    let mut cursor = 0usize;
    let mut ops = Vec::new();
    loop {
        if buf.len() < cursor + 4 {
            return Err(RealWireError::DeltaTokenTruncated {
                at: "raw_token",
                needed: 4,
                available: buf.len() - cursor,
            });
        }
        let tag = i32::from_le_bytes(buf[cursor..cursor + 4].try_into().unwrap());
        cursor += 4;
        if tag == 0 {
            break;
        }
        if tag > 0 {
            let len = tag as usize;
            if buf.len() < cursor + len {
                return Err(RealWireError::DeltaTokenTruncated {
                    at: "raw_literal",
                    needed: len,
                    available: buf.len() - cursor,
                });
            }
            ops.push(DeltaOp::Literal {
                compressed_payload: buf[cursor..cursor + len].to_vec(),
            });
            cursor += len;
        } else {
            ops.push(DeltaOp::CopyRun {
                start_token_index: -(tag + 1),
                run_length: 1,
            });
        }
    }
    if buf.len() < cursor + checksum_len {
        return Err(RealWireError::TruncatedBuffer {
            at: "raw_checksum",
            needed: checksum_len,
            available: buf.len() - cursor,
        });
    }
    let file_checksum = buf[cursor..cursor + checksum_len].to_vec();
    cursor += checksum_len;
    Ok((ops, file_checksum, cursor))
}

/// Best-effort mtime set (seconds + nanoseconds), like the single-file path.
async fn set_mtime(path: &Path, mtime: i64, nsec: Option<i32>) -> std::io::Result<()> {
    let secs = mtime;
    let nsec = nsec.unwrap_or(0) as u32;
    let t = filetime::FileTime::from_unix_time(secs, nsec);
    filetime::set_file_mtime(path, t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aerorsync::events::CollectingSink;
    use crate::aerorsync::mock::{MockRemoteShellTransport, MockTransportConfig};
    use crate::aerorsync::real_wire::{
        decode_ndx, encode_file_list_entry, encode_file_list_terminator, encode_ndx,
        encode_server_preamble, FileListDecodeOptions, MuxHeader, MuxTag, NdxState, ServerPreamble,
    };
    use crate::aerorsync::remote_command::RemoteCommandSpec;
    use crate::aerorsync::transport::CancelHandle;
    use crate::aerorsync::types::SessionRole;

    const XMIT_SAME_UID: u32 = 0x0008;
    const XMIT_SAME_GID: u32 = 0x0010;
    const XMIT_LONG_NAME: u32 = 0x0040;
    const XMIT_SAME_TIME: u32 = 0x0080;

    fn sample_entry(path: &str) -> FileListEntry {
        FileListEntry {
            flags: XMIT_LONG_NAME | XMIT_SAME_UID | XMIT_SAME_GID | XMIT_SAME_TIME,
            path: path.as_bytes().to_vec(),
            size: 4096,
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
            xattrs: None,
            acls: None,
        }
    }

    fn opts() -> FileListDecodeOptions {
        FileListDecodeOptions {
            protocol: 31,
            xfer_flags_as_varint: true,
            always_checksum: false,
            csum_len: 16,
            preserve_uid: true,
            preserve_gid: true,
            preserve_acls: false,
            preserve_xattrs: false,
            preserve_links: true,
            preserve_devices: true,
            preserve_specials: true,
        }
    }

    fn mux_frame(payload: &[u8]) -> Vec<u8> {
        let header = MuxHeader {
            tag: MuxTag::Data,
            length: payload.len() as u32,
        };
        let mut out = Vec::with_capacity(4 + payload.len());
        out.extend_from_slice(&header.encode());
        out.extend_from_slice(payload);
        out
    }

    /// A driver on a mock whose inbound stream is `preamble + frames`.
    async fn tree_driver(
        inbound: Vec<u8>,
        compat_flags: i32,
    ) -> AerorsyncDriver<MockRemoteShellTransport> {
        let mut preamble = encode_server_preamble(&ServerPreamble {
            protocol_version: 31,
            compat_flags,
            checksum_algos: "xxh128".to_string(),
            compression_algos: "none".to_string(),
            checksum_seed: 0x1234_5678,
            consumed: 0,
        });
        preamble.extend_from_slice(&inbound);
        let cfg = MockTransportConfig::healthy_download().with_raw_inbound(preamble);
        let mut d = AerorsyncDriver::new(MockRemoteShellTransport::new(cfg), CancelHandle::inert());
        d.session_role = Some(SessionRole::Receiver);
        d.open_raw_stream_internal(&RemoteCommandSpec::capture_download("/remote/target"))
            .await
            .unwrap();
        d.perform_preamble_exchange(31, "xxh128", "none")
            .await
            .unwrap();
        // the hand-built lists below carry no per-entry checksum
        d.file_list_checksum_enabled = false;
        d
    }

    /// C1: the initial list must stop at its end-of-list and leave any
    /// following bytes (here an item ndx) unread in the state. Red with
    /// the old shape that kept reading the next ndx inside the call.
    #[tokio::test]
    async fn initial_list_leaves_the_next_ndx_unread_in_the_state() {
        let o = opts();
        let mut payload =
            encode_file_list_entry(&sample_entry("a.txt"), &o, &mut FileListCodecState::new());
        payload.extend_from_slice(&encode_file_list_terminator(&o, 0));
        // one item ndx right after the terminator, in the SAME frame
        payload.extend_from_slice(&encode_ndx(0, &mut NdxState::new()));
        let mut d = tree_driver(mux_frame(&payload), 0x80 | CF_INC_RECURSE).await;
        let start = d
            .receive_initial_tree_list(TreeListLimits::default(), &mut CollectingSink::default())
            .await
            .expect("the initial list decodes");
        assert_eq!(start.initial.entries.len(), 1);
        assert_eq!(start.initial.dir_ndx, None);
        assert_eq!(
            start.initial.ndx_start, 1,
            "inc-recursive sessions start at ndx 1"
        );
        assert!(
            !start.state.buffered.is_empty(),
            "the item ndx must stay unread"
        );
        let mut ndx_state = NdxState::new();
        let (ndx, _) = decode_ndx(&start.state.buffered, &mut ndx_state).unwrap();
        assert_eq!(classify_tree_ndx(ndx), TreeNdx::Item(0));
    }

    /// flist.c:3221-3225 (NAS 3291, Q7): the basename "." (the implied top
    /// dir) always sorts first. No frozen capture carries it, so this test
    /// pins the rule.
    #[test]
    fn dot_basename_sorts_first() {
        let mut dot = sample_entry(".");
        dot.mode = 0o040_775;
        let file = sample_entry("a.txt");
        let mut zdir = sample_entry("z");
        zdir.mode = 0o040_775;
        assert_eq!(flist_entry_cmp(&dot, &file), std::cmp::Ordering::Less);
        assert_eq!(flist_entry_cmp(&dot, &zdir), std::cmp::Ordering::Less);
        assert_eq!(flist_entry_cmp(&file, &dot), std::cmp::Ordering::Greater);
        let mut v = vec![zdir, file, dot];
        v.sort_by(flist_entry_cmp);
        assert_eq!(v[0].path, b".");
        assert_eq!(v[1].path, b"a.txt");
        assert_eq!(v[2].path, b"z");
    }

    /// Step 1: one test per refused name shape.
    #[test]
    fn tree_entry_local_path_refuses_unsafe_names() {
        let root = std::path::Path::new("/dest");
        assert_eq!(
            tree_entry_local_path(root, b""),
            Err(TreePathError::EmptyName)
        );
        assert_eq!(
            tree_entry_local_path(root, b"/etc/x"),
            Err(TreePathError::AbsoluteName)
        );
        assert_eq!(
            tree_entry_local_path(root, b"../x"),
            Err(TreePathError::DotComponent)
        );
        assert_eq!(
            tree_entry_local_path(root, b"a/../../b"),
            Err(TreePathError::DotComponent)
        );
        assert_eq!(
            tree_entry_local_path(root, b"a/./b"),
            Err(TreePathError::DotComponent)
        );
        assert_eq!(
            tree_entry_local_path(root, b"a//b"),
            Err(TreePathError::EmptyComponent)
        );
        assert_eq!(
            tree_entry_local_path(root, b"a/"),
            Err(TreePathError::EmptyComponent)
        );
        assert_eq!(
            tree_entry_local_path(root, b"a\0b"),
            Err(TreePathError::NulByte)
        );
        let long = vec![b'a'; PATH_MAX_BYTES + 1];
        assert_eq!(
            tree_entry_local_path(root, &long),
            Err(TreePathError::TooLong)
        );
        assert_eq!(
            tree_entry_local_path(root, b"a/b/c.txt").unwrap(),
            root.join("a/b/c.txt")
        );
    }

    /// C3: an end-of-list I/O error accumulates in the state instead of
    /// aborting the tree. Red with the old shape that returned Err.
    #[tokio::test]
    async fn segment_io_error_accumulates_without_aborting() {
        let o = opts();
        let mut payload =
            encode_file_list_entry(&sample_entry("a.txt"), &o, &mut FileListCodecState::new());
        payload.extend_from_slice(&encode_file_list_terminator(&o, 2));
        let mut d = tree_driver(mux_frame(&payload), 0x80 | CF_INC_RECURSE).await;
        let start = d
            .receive_initial_tree_list(TreeListLimits::default(), &mut CollectingSink::default())
            .await
            .expect("an io_error terminator must not abort the tree");
        assert_eq!(start.initial.entries.len(), 1);
        assert_eq!(start.state.io_error, 2);
    }
}
