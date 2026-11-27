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
//! Two numberings exist on the wire: the wire ndx, kept on every
//! [`TreeEntry`] because B4 needs it for hardlinks, and the transfer
//! order of the sorted `dir_flist`. The gap `ndx_start - 1` at every
//! segment boundary is not a file: it is the numbering hole the parent
//! directory leaves (A4), skipped and never an item.

// SPDX-License-Identifier: MPL-2.0 OR GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use std::fmt;

use crate::aerorsync::events::EventSink;
use crate::aerorsync::native_driver::{map_realwire_error, AerorsyncDriver, AerorsyncSessionPhase};
use crate::aerorsync::real_wire::{
    decode_id_list, is_device_mode, is_special_mode, FileListCodecState, FileListDecodeOutcome,
    FileListEntry, FileListStreamDecoder, IdNameList, RealWireError, MAX_FILE_LIST_ENTRY_BYTES,
    NDX_DEL_STATS, NDX_DONE, NDX_FLIST_EOF, NDX_FLIST_OFFSET,
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

/// Default session ceiling on tree-list entries, matching the session
/// table caps of `session_state.rs` (50_000).
pub const DEFAULT_MAX_TREE_ENTRIES: usize = 50_000;

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
/// wire ndx (B4 needs it for hardlinks), and the device/special mark that
/// B3.2 turns into `Failed { SpecialUnsupported }` without a wire
/// request.
#[derive(Debug, Clone)]
pub struct TreeEntry {
    /// Wire ndx of this entry inside the session's file list.
    pub ndx: i32,
    /// The decoded entry, back-references still unresolved at codec level.
    pub entry: FileListEntry,
    /// ACL/xattr references resolved against the session tables.
    pub resolved: ResolvedMetadata,
    /// True for character/block devices, fifos and sockets: received and
    /// marked here, never refused at list level (D3).
    pub device_or_special: bool,
}

/// One file-list segment: the entries of one directory, starting at
/// `ndx_start`. `dir_ndx` is the `dir_flist` index the segment marker
/// carried (`NDX_FLIST_OFFSET - marker`), `None` for the initial list.
/// `gap_ndx` (= `ndx_start - 1`) is the numbering hole the parent
/// directory leaves (A4): skipped, never an item.
#[derive(Debug, Clone)]
pub struct DirSegment {
    /// Wire ndx of the segment's first entry.
    pub ndx_start: i32,
    /// `dir_flist` index announced by the segment marker; `None` for the
    /// initial list, which no marker announces.
    pub dir_ndx: Option<i32>,
    /// `ndx_start - 1`: the numbering gap, not the parent's ndx (A4).
    pub gap_ndx: i32,
    /// The segment's entries in wire order, wire ndx kept on each.
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
            // C4: rsync sends the uid list only with owner (or ACL)
            // preservation and not with --numeric-ids, the gid list
            // only with group (or ACL) preservation; this driver's argv
            // never carries --numeric-ids.
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
        // directory leaves; it is skipped, never an item.
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
                    Err(RealWireError::TruncatedBuffer { .. }) => {}
                    Err(other) => return Err(map_realwire_error(other, context)),
                }
            }
            let payload = self.next_data_frame(bridge).await?;
            state.buffered.extend_from_slice(&payload);
        }
    }
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
