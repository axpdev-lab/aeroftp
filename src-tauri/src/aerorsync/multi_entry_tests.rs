//! B1 oracles: multi-entry file lists decoded and re-encoded against the
//! B0 wire captures (`capture/artifacts_real/frozen/b0c0/`).
//!
//! The captures are versioned, so a missing one fails the test instead of
//! skipping it. The reference for every expected value below is the byte
//! listing in `12-multi-entry-wire-evidence.md`, produced by
//! `capture/decode_rsync_wire.py` from the same files; the source of truth
//! is the capture itself, which is why every oracle ends in a byte-exact
//! re-encode of the list it decoded.

// SPDX-License-Identifier: MPL-2.0 OR GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

#![cfg(test)]

use std::io::Read;
use std::path::PathBuf;

use crate::aerorsync::real_wire::{
    decode_delta_stream, decode_file_list_entry, decode_id_list, decode_ndx, decode_varint,
    encode_file_list_entry, encode_file_list_terminator, encode_id_list, encode_varint,
    encode_varlong, is_directory_mode, is_regular_mode, reassemble_msg_data, AclNamedEntry,
    AclPrincipal, AclWireEntry, FileListAcls, FileListCodecState, FileListDecodeOptions,
    FileListDecodeOutcome, FileListEntry, FileListStreamDecoder, IdNameList, MuxPoll,
    MuxStreamReader, NdxState, Rdev, RealWireError, RsyncAcl, XattrPair, XattrWireEntry,
    MAXPATHLEN, MAX_ACL_NAMED_ENTRIES, MAX_ACL_NAME_LEN, MAX_FILE_LIST_ENTRY_BYTES, MAX_FULL_DATUM,
    MAX_XATTR_NAME_LEN, MAX_XATTR_PAIRS, NDX_FLIST_EOF, NDX_FLIST_OFFSET, XMIT_EXTENDED_FLAGS,
    XMIT_GROUP_NAME_FOLLOWS, XMIT_LONG_NAME, XMIT_MOD_NSEC, XMIT_SAME_GID, XMIT_SAME_MODE,
    XMIT_SAME_NAME, XMIT_SAME_RDEV_MAJOR, XMIT_SAME_TIME, XMIT_SAME_UID, XMIT_TOP_DIR,
    XMIT_USER_NAME_FOLLOWS,
};
use crate::aerorsync::session_state::{
    AclTable, SessionTable, SessionTableError, SessionTables, MAX_SESSION_ACLS,
    MAX_SESSION_ID_NAMES, MAX_SESSION_XATTR_SETS,
};

const B0C0_REL: &str = "src/aerorsync/capture/artifacts_real/frozen/b0c0";
/// The B1 addendum captures (`capture/b1_wire_campaign.py`), named `b1-*`.
const B1_REL: &str = "src/aerorsync/capture/artifacts_real/frozen/b1";
/// `CF_VARINT_FLIST_FLAGS`: set by a 3.2+ server that saw the client's `v`.
/// It decides both the flag encoding and whether the checksum and
/// compression lists are negotiated at all (`compat.c`).
const CF_VARINT_FLIST_FLAGS: i64 = 1 << 7;
const MTIME: i64 = 1_700_000_000;
const DIR_MODE: u32 = 0o040_775;
const FILE_MODE: u32 = 0o100_644;

// ---------------------------------------------------------------------------
// Capture access
// ---------------------------------------------------------------------------

/// One file of a capture, transparently gunzipped when only the `.gz`
/// copy is versioned (captures above 256 KiB).
fn capture_bytes(run: &str, file: &str) -> Vec<u8> {
    let rel = if run.starts_with("b1-") {
        B1_REL
    } else {
        B0C0_REL
    };
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(rel)
        .join(run);
    if let Ok(bytes) = std::fs::read(dir.join(file)) {
        return bytes;
    }
    let gz = dir.join(format!("{file}.gz"));
    let compressed = std::fs::read(&gz).unwrap_or_else(|e| {
        panic!(
            "capture {run}/{file} missing ({e}): the b0c0 and b1 oracles are \
             versioned, so this is a broken checkout, not a reason to skip"
        )
    });
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(compressed.as_slice())
        .read_to_end(&mut out)
        .expect("gunzip capture");
    out
}

/// The server's compat flags: the varint after its protocol version.
fn server_compat(run: &str) -> (i64, usize) {
    let out = capture_bytes(run, "capture_out.bin");
    decode_varint(&out[4..]).expect("compat flags")
}

/// True when the capture's own server command carries `-z`: the session
/// negotiated a compression algo (zstd in the p9z capture), so its item
/// answers use the compressed byte-tag token framing instead of the raw
/// int32 tokens of a no-`-z` session.
fn capture_compressed(run: &str) -> bool {
    let command = String::from_utf8(capture_bytes(run, "remote_command.txt")).expect("utf-8");
    command
        .split_whitespace()
        .find(|a| a.starts_with('-') && !a.starts_with("--"))
        .is_some_and(|b| b.split('.').next().unwrap_or("").contains('z'))
}

/// Raw bytes of the direction that carries the file list (the sender)
/// and the offset where multiplexing starts in it.
///
/// Both sides open with the protocol version. The server adds its compat
/// flags varint and, last, the checksum seed. When the compat flags carry
/// `CF_VARINT_FLIST_FLAGS` each side then sends its checksum list (and its
/// compression list under `-z`), one length byte and the names; a server
/// older than 3.2 never sets the flag, and neither side sends a list
/// (measured against 3.1.3: server `1f000000 3f <seed>`, client `1f000000`).
fn sender_raw(run: &str) -> (Vec<u8>, usize) {
    let (compat, compat_len) = server_compat(run);
    let negotiated = compat & CF_VARINT_FLIST_FLAGS != 0;
    let command = String::from_utf8(capture_bytes(run, "remote_command.txt")).expect("utf-8");
    let compress = capture_compressed(run);
    let skip_lists = |raw: &[u8], mut at: usize| {
        if negotiated {
            for _ in 0..if compress { 2 } else { 1 } {
                at += 1 + raw[at] as usize;
            }
        }
        at
    };
    if command.contains("--sender") {
        let raw = capture_bytes(run, "capture_out.bin");
        let start = skip_lists(&raw, 4 + compat_len) + 4;
        (raw, start)
    } else {
        let raw = capture_bytes(run, "capture_in.bin");
        let start = skip_lists(&raw, 4);
        (raw, start)
    }
}

/// The sender's application stream: every `MSG_DATA` payload, in order.
fn sender_app_stream(run: &str) -> Vec<u8> {
    let (raw, start) = sender_raw(run);
    reassemble_msg_data(&raw[start..])
        .expect("sender stream demultiplexes")
        .app_stream
}

/// Options for the flag strings of the campaign: protocol 31 negotiated,
/// `CF_VARINT_FLIST_FLAGS` on, xxh128 (16 bytes) when `-c` is present.
/// `full` is `-a` (the server sees `-logD`); otherwise `-rltp` (`-l` only).
fn options(full: bool, checksum: bool) -> FileListDecodeOptions {
    FileListDecodeOptions {
        protocol: 31,
        xfer_flags_as_varint: true,
        always_checksum: checksum,
        csum_len: 16,
        preserve_uid: full,
        preserve_gid: full,
        preserve_acls: false,
        preserve_xattrs: false,
        preserve_links: true,
        preserve_devices: full,
        preserve_specials: full,
    }
}

/// Options read from the capture's own server command, the way the driver
/// reads them from the argv it sends: short options are the letters before
/// the `.`, and `i` after it is incremental recursion. Returns the options
/// and whether the session is incremental.
fn options_from_capture(run: &str) -> (FileListDecodeOptions, bool) {
    let command = String::from_utf8(capture_bytes(run, "remote_command.txt")).expect("utf-8");
    let bundle = command
        .split_whitespace()
        .find(|a| a.starts_with('-') && !a.starts_with("--"))
        .unwrap_or_else(|| panic!("{run}: no short-option bundle in {command:?}"));
    let (short, caps) = bundle.split_once('.').unwrap_or((bundle, ""));
    let has = |c: char| short.contains(c);
    let options = FileListDecodeOptions {
        protocol: 31,
        xfer_flags_as_varint: server_compat(run).0 & CF_VARINT_FLIST_FLAGS != 0,
        always_checksum: has('c'),
        csum_len: 16,
        preserve_uid: has('o'),
        preserve_gid: has('g'),
        preserve_acls: has('A'),
        preserve_xattrs: has('X'),
        preserve_links: has('l'),
        preserve_devices: has('D'),
        preserve_specials: has('D'),
    };
    (options, caps.contains('i'))
}

// ---------------------------------------------------------------------------
// List walking
// ---------------------------------------------------------------------------

/// One decoded list, the stream span `[start, end)` it occupied
/// (terminator included) and the I/O error its terminator carried.
struct DecodedList {
    entries: Vec<FileListEntry>,
    start: usize,
    end: usize,
    io_error: i32,
}

fn decode_list(
    app: &[u8],
    start: usize,
    opts: &FileListDecodeOptions,
    state: &mut FileListCodecState,
) -> DecodedList {
    let mut cursor = start;
    let mut entries = Vec::new();
    let io_error = loop {
        let (outcome, consumed) = decode_file_list_entry(&app[cursor..], opts, state)
            .unwrap_or_else(|e| panic!("entry at stream offset {cursor:#x}: {e}"));
        cursor += consumed;
        match outcome {
            FileListDecodeOutcome::Entry(entry) => entries.push(entry),
            FileListDecodeOutcome::EndOfList { io_error } => break io_error,
        }
    };
    DecodedList {
        entries,
        start,
        end: cursor,
        io_error,
    }
}

/// Decode every segment of an incremental-recursion session: the initial
/// list at offset 0, then one list after each `NDX_FLIST_OFFSET - n`,
/// until `NDX_FLIST_EOF`. ONE codec state runs through all of them, as on
/// the wire. The ndx handling here is test scaffolding: segments are B3.
fn decode_inc_session(app: &[u8], opts: &FileListDecodeOptions) -> Vec<DecodedList> {
    let mut state = FileListCodecState::new();
    let mut ndx_state = NdxState::new();
    let mut lists = vec![decode_list(app, 0, opts, &mut state)];
    loop {
        let cursor = lists.last().expect("one list").end;
        let (ndx, consumed) = decode_ndx(&app[cursor..], &mut ndx_state).expect("segment ndx");
        if ndx == NDX_FLIST_EOF {
            return lists;
        }
        assert!(
            ndx <= NDX_FLIST_OFFSET,
            "expected a segment marker at {cursor:#x}, got ndx {ndx}"
        );
        lists.push(decode_list(app, cursor + consumed, opts, &mut state));
    }
}

/// Where the file list starts in the sender's stream. In an upload with
/// `--delete` the receiver wants the filter list, so the client sends it
/// first, inside MSG_DATA (`c13-up-delete`: `00 00 00 00`, an empty list):
/// int32 lengths and rules, closed by a zero length.
fn file_list_start(run: &str, app: &[u8]) -> usize {
    let command = String::from_utf8(capture_bytes(run, "remote_command.txt")).expect("utf-8");
    let is_upload = !command.contains("--sender");
    if !(is_upload && command.contains("--delete")) {
        return 0;
    }
    let mut cursor = 0;
    loop {
        let len = i32::from_le_bytes(app[cursor..cursor + 4].try_into().expect("4 bytes"));
        cursor += 4;
        if len == 0 {
            return cursor;
        }
        cursor += usize::try_from(len).expect("positive filter rule length");
    }
}

/// Every list of a session: the segments of an incremental one, or the one
/// list of a non-incremental one.
fn decode_session(app: &[u8], opts: &FileListDecodeOptions, inc: bool) -> Vec<DecodedList> {
    if inc {
        decode_inc_session(app, opts)
    } else {
        vec![decode_list(app, 0, opts, &mut FileListCodecState::new())]
    }
}

/// The same session through [`FileListStreamDecoder`], fed `chunk` bytes at
/// a time, with one codec state handed from segment to segment through
/// `into_parts`. Returns each list's entries and the final state.
fn stream_session(
    app: &[u8],
    opts: FileListDecodeOptions,
    inc: bool,
    chunk: usize,
) -> (Vec<Vec<FileListEntry>>, FileListCodecState) {
    let placeholder = || FileListStreamDecoder::new(opts, FileListCodecState::new(), 0);
    let mut decoder = FileListStreamDecoder::new(opts, FileListCodecState::new(), usize::MAX);
    let mut lists = vec![Vec::new()];
    let mut between: Option<(FileListCodecState, Vec<u8>)> = None;
    let mut ndx_state = NdxState::new();
    for piece in app.chunks(chunk) {
        match between.as_mut() {
            Some((_, pending)) => pending.extend_from_slice(piece),
            None => decoder.feed(piece),
        }
        loop {
            if let Some((_, pending)) = between.as_ref() {
                let (ndx, n) = match decode_ndx(pending, &mut ndx_state) {
                    Ok(v) => v,
                    Err(RealWireError::NdxTruncated { .. }) => break,
                    Err(e) => panic!("segment marker: {e}"),
                };
                let (state, pending) = between.take().expect("between");
                if ndx == NDX_FLIST_EOF {
                    return (lists, state);
                }
                assert!(ndx <= NDX_FLIST_OFFSET, "segment marker, got ndx {ndx}");
                decoder = FileListStreamDecoder::new(opts, state, usize::MAX);
                decoder.feed(&pending[n..]);
                lists.push(Vec::new());
            }
            match decoder
                .next_outcome()
                .expect("no hard error while streaming")
            {
                Some(FileListDecodeOutcome::Entry(entry)) => {
                    lists.last_mut().expect("a list").push(entry)
                }
                Some(FileListDecodeOutcome::EndOfList { .. }) => {
                    let (state, rest) = std::mem::replace(&mut decoder, placeholder()).into_parts();
                    if !inc {
                        return (lists, state);
                    }
                    between = Some((state, rest));
                }
                None => break,
            }
        }
    }
    panic!("stream ended inside the file list");
}

/// Re-encode `lists` with one encoder state running through all of them
/// and require each to reproduce its original span byte for byte.
fn assert_lists_reencode(app: &[u8], lists: &[DecodedList], opts: &FileListDecodeOptions) {
    let mut state = FileListCodecState::new();
    for (i, list) in lists.iter().enumerate() {
        let mut bytes = Vec::new();
        for entry in &list.entries {
            bytes.extend_from_slice(&encode_file_list_entry(entry, opts, &mut state));
        }
        bytes.extend_from_slice(&encode_file_list_terminator(opts, list.io_error));
        let original = &app[list.start..list.end];
        let divergence = bytes.iter().zip(original).position(|(a, b)| a != b);
        assert!(
            bytes == original,
            "list {i} re-encodes to {} bytes against {} original, first divergence at {divergence:?}",
            bytes.len(),
            original.len()
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn assert_entry(
    entry: &FileListEntry,
    path: &str,
    mode: u32,
    size: i64,
    uid: Option<i64>,
    gid: Option<i64>,
    uid_name: Option<&str>,
    checksum_len: usize,
) {
    assert_eq!(entry.path, path.as_bytes(), "path");
    assert_eq!(entry.mode, mode, "mode of {path}");
    assert_eq!(entry.size, size, "size of {path}");
    assert_eq!(entry.mtime, MTIME, "mtime of {path}");
    assert_eq!(entry.uid, uid, "uid of {path}");
    assert_eq!(entry.gid, gid, "gid of {path}");
    assert_eq!(entry.uid_name.as_deref(), uid_name, "uid name of {path}");
    assert_eq!(
        entry.gid_name.as_deref(),
        uid_name,
        "gid name of {path} (one name for both in every capture)"
    );
    assert_eq!(entry.checksum.len(), checksum_len, "checksum of {path}");
}

// ---------------------------------------------------------------------------
// Point 1: directory with two files, incremental recursion (§2)
// ---------------------------------------------------------------------------

/// The same session in both directions and with `-c`: a top directory in
/// the initial segment, its two files in the next one. Every file entry
/// inherits mtime (SAME_TIME), the second also the mode (SAME_MODE), and
/// the first file compresses its name against "d", the last entry of the
/// PREVIOUS segment.
fn check_point1(run: &str, owner: &str, checksum: bool) {
    let app = sender_app_stream(run);
    let opts = options(true, checksum);
    let lists = decode_inc_session(&app, &opts);
    assert_eq!(lists.len(), 2, "{run}: initial segment and segment \"d\"");
    let csum = if checksum { 16 } else { 0 };

    assert_eq!(lists[0].entries.len(), 1);
    assert_entry(
        &lists[0].entries[0],
        "d",
        DIR_MODE,
        4096,
        Some(1002),
        Some(1002),
        Some(owner),
        0,
    );
    assert_eq!(lists[1].entries.len(), 2);
    assert_entry(
        &lists[1].entries[0],
        "d/alpha.txt",
        FILE_MODE,
        10,
        Some(1002),
        Some(1002),
        None,
        csum,
    );
    assert_entry(
        &lists[1].entries[1],
        "d/alpha2.txt",
        FILE_MODE,
        20,
        Some(1002),
        Some(1002),
        None,
        csum,
    );
    assert_lists_reencode(&app, &lists, &opts);
}

#[test]
fn b1_point1_upload_decodes_across_segments_and_reencodes() {
    check_point1("p1-up", "axpdev", false);
}

#[test]
fn b1_point1_download_decodes_across_segments_and_reencodes() {
    check_point1("p1-dl", "testuser", false);
}

/// `-c`: 16 bytes of xxh128 after each regular file and none after the
/// directory. Reading a checksum on "d" would swallow the terminator.
#[test]
fn b1_point1_checksum_rides_on_regular_files_only() {
    check_point1("p1c-dl", "testuser", true);
    let app = sender_app_stream("p1c-dl");
    let lists = decode_inc_session(&app, &options(true, true));
    assert_ne!(
        lists[1].entries[0].checksum, lists[1].entries[1].checksum,
        "two different contents, two different digests"
    );
}

// ---------------------------------------------------------------------------
// Point 2: three-level tree without incremental recursion, id lists (§3, §8)
// ---------------------------------------------------------------------------

const P2_WIRE_ORDER: [(&str, u32, i64); 9] = [
    ("t", DIR_MODE, 4096),
    ("t/b", DIR_MODE, 4096),
    ("t/x", DIR_MODE, 4096),
    ("t/a.txt", FILE_MODE, 4),
    ("t/x/f1", FILE_MODE, 4),
    ("t/x/y", DIR_MODE, 4096),
    ("t/x/y/z", DIR_MODE, 4096),
    ("t/x/y/f2", FILE_MODE, 4),
    ("t/x/y/z/leaf.txt", FILE_MODE, 5),
];

/// One list with ndx from 0, in the order the sender read the tree (not
/// the sorted order), entries with bare ids, then the uid and gid lists
/// carrying the names, each closed by the name of id 0 (`CF_ID0_NAMES`).
fn check_point2(run: &str, owner: &str) {
    let app = sender_app_stream(run);
    let opts = options(true, false);
    let mut state = FileListCodecState::new();
    let list = decode_list(&app, 0, &opts, &mut state);

    assert_eq!(list.entries.len(), P2_WIRE_ORDER.len());
    for (entry, (path, mode, size)) in list.entries.iter().zip(P2_WIRE_ORDER) {
        assert_entry(entry, path, mode, size, Some(1002), Some(1002), None, 0);
    }
    assert_lists_reencode(&app, std::slice::from_ref(&list), &opts);

    assert_eq!(list.end, 0x73, "{run}: the id lists start where §8 says");
    let expected = IdNameList {
        names: vec![(1002, owner.to_string())],
        id0_name: Some("root".to_string()),
    };
    let (uids, uid_len) = decode_id_list(&app[list.end..], true).expect("uid list");
    assert_eq!(uids, expected, "{run}: uid list");
    let gid_at = list.end + uid_len;
    let (gids, gid_len) = decode_id_list(&app[gid_at..], true).expect("gid list");
    assert_eq!(gids, expected, "{run}: gid list");
    assert_eq!(
        encode_id_list(&uids, true),
        app[list.end..gid_at],
        "{run}: uid list re-encodes"
    );
    assert_eq!(
        encode_id_list(&gids, true),
        app[gid_at..gid_at + gid_len],
        "{run}: gid list re-encodes"
    );
}

#[test]
fn b1_point2_download_noinc_single_list_and_id_lists() {
    check_point2("p2-dl-noinc", "testuser");
}

#[test]
fn b1_point2_upload_noinc_single_list_and_id_lists() {
    check_point2("p2-up-noinc", "axpdev");
}

// ---------------------------------------------------------------------------
// Point 7: uid/gid follow the flag string, never the bits (§8)
// ---------------------------------------------------------------------------

/// Full profile (`-og`) and product profile (`-ltp`) of the same tree. In
/// the product profile the sender still sets SAME_UID|SAME_GID on every
/// entry, the root included, and no id travels: a decoder that keyed the
/// ids on the bits would read one out of the mode bytes.
fn check_point7(full: &str, product: &str, owner: &str) {
    let expected: [(&str, u32, i64); 3] = [
        ("g", DIR_MODE, 4096),
        ("g/f1", FILE_MODE, 4),
        ("g/f2", FILE_MODE, 4),
    ];

    let app_full = sender_app_stream(full);
    let opts_full = options(true, false);
    let lists_full = decode_inc_session(&app_full, &opts_full);
    let full_entries: Vec<&FileListEntry> =
        lists_full.iter().flat_map(|l| l.entries.iter()).collect();
    assert_eq!(full_entries.len(), expected.len());
    for (i, (entry, (path, mode, size))) in full_entries.iter().zip(expected).enumerate() {
        let name = (i == 0).then_some(owner);
        assert_entry(entry, path, mode, size, Some(1002), Some(1002), name, 0);
    }
    assert_lists_reencode(&app_full, &lists_full, &opts_full);

    let app_product = sender_app_stream(product);
    let opts_product = options(false, false);
    let lists_product = decode_inc_session(&app_product, &opts_product);
    let product_entries: Vec<&FileListEntry> = lists_product
        .iter()
        .flat_map(|l| l.entries.iter())
        .collect();
    assert_eq!(product_entries.len(), expected.len());
    for (entry, (path, mode, size)) in product_entries.iter().zip(expected) {
        assert_entry(entry, path, mode, size, None, None, None, 0);
        assert_eq!(
            entry.flags & (XMIT_SAME_UID | XMIT_SAME_GID),
            XMIT_SAME_UID | XMIT_SAME_GID,
            "{path}: the product sender sets both bits with no id behind them"
        );
    }
    assert_lists_reencode(&app_product, &lists_product, &opts_product);

    assert!(
        lists_product[0].end < lists_full[0].end,
        "the product root entry is shorter: no ids, no names"
    );
}

#[test]
fn b1_point7_upload_ids_follow_the_flag_string_not_the_bits() {
    check_point7("p7-up-full", "p7-up-product", "axpdev");
}

#[test]
fn b1_point7_download_ids_follow_the_flag_string_not_the_bits() {
    check_point7("p7-dl-full", "p7-dl-product", "testuser");
}

// ---------------------------------------------------------------------------
// Points 4 and 5: directories under `-A` carry a second (default) ACL (§5, §6)
// ---------------------------------------------------------------------------

/// Which ACL a decoded entry carries: `None` for "no default slot", else
/// `Some(Ok(()))` for a literal and `Some(Err(index))` for a reference.
type AclShape = (Result<(), u32>, Option<Result<(), u32>>);

fn acl_shape(entry: &FileListEntry) -> AclShape {
    let shape = |acl: &AclWireEntry| match acl {
        AclWireEntry::Literal(_) => Ok(()),
        AclWireEntry::Reference(index) => Err(*index),
    };
    let acls = entry
        .acls
        .as_ref()
        .unwrap_or_else(|| panic!("{}: -A session, ACLs expected", entry.path_lossy()));
    (shape(&acls.access), acls.default.as_ref().map(shape))
}

/// Decode a `-A` session and compare path, mode, size and ACL shape; the
/// references stay unresolved (the tables are B2).
fn check_acl_session(run: &str, expected: &[(&str, u32, i64, AclShape)]) {
    let app = sender_app_stream(run);
    let opts = FileListDecodeOptions {
        preserve_acls: true,
        ..options(true, false)
    };
    let lists = decode_inc_session(&app, &opts);
    let entries: Vec<&FileListEntry> = lists.iter().flat_map(|l| l.entries.iter()).collect();
    assert_eq!(entries.len(), expected.len(), "{run}");
    for (entry, (path, mode, size, shape)) in entries.iter().zip(expected) {
        assert_eq!(entry.path, path.as_bytes(), "{run}");
        assert_eq!(entry.mode, *mode, "{run}: mode of {path}");
        assert_eq!(entry.size, *size, "{run}: size of {path}");
        assert_eq!(acl_shape(entry), *shape, "{run}: ACLs of {path}");
    }
    assert_lists_reencode(&app, &lists, &opts);
}

/// `da/sub` is a directory sent with XMIT_SAME_MODE: its default ACL (a
/// reference to the default of `da`) is there only because the inherited
/// mode is a directory. With the mode read as zero, the slot is skipped
/// and the reference byte is taken for the next entry's flags. The last
/// segment ("da/sub") is empty and still a segment.
#[test]
fn b1_point5_same_mode_directory_keeps_its_default_acl_slot() {
    let expected = [
        ("da", DIR_MODE, 4096, (Ok(()), Some(Ok(())))),
        ("da/sub", DIR_MODE, 4096, (Ok(()), Some(Err(0)))),
        ("da/f", FILE_MODE, 9, (Ok(()), None)),
    ];
    for run in ["p5-up", "p5-dl"] {
        let app = sender_app_stream(run);
        let opts = FileListDecodeOptions {
            preserve_acls: true,
            ..options(true, false)
        };
        let lists = decode_inc_session(&app, &opts);
        assert_eq!(lists.len(), 3, "{run}: initial, \"da\", empty \"da/sub\"");
        assert!(lists[2].entries.is_empty(), "{run}");
        let sub = &lists[1].entries[0];
        assert_ne!(
            sub.flags & XMIT_SAME_MODE,
            0,
            "{run}: da/sub rides on SAME_MODE"
        );
        check_acl_session(run, &expected);
    }
}

/// Files only in the segment: the third file is SAME_MODE and its access
/// ACL is a reference, the first file's mode is 0664 (not the default).
#[test]
fn b1_point4_access_acl_literals_and_reference_across_files() {
    let expected = [
        ("ac", DIR_MODE, 4096, (Ok(()), Some(Ok(())))),
        ("ac/f3", 0o100_664, 8, (Ok(()), None)),
        ("ac/f1", FILE_MODE, 8, (Ok(()), None)),
        ("ac/f2", FILE_MODE, 8, (Err(2), None)),
    ];
    check_acl_session("p4-up", &expected);
}

// ---------------------------------------------------------------------------
// Encoder contract: a SAME flag that lies is a caller bug
// ---------------------------------------------------------------------------

fn regular_entry(mode: u32, mtime: i64, flags: u32) -> FileListEntry {
    FileListEntry {
        flags,
        path: b"f".to_vec(),
        size: 1,
        mtime,
        mtime_nsec: None,
        mode,
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

/// The peer would give the file the previous entry's mode.
#[test]
#[should_panic(expected = "sets XMIT_SAME_MODE but its mode differs")]
fn b1_encoder_refuses_a_same_mode_flag_that_lies() {
    let mut state = FileListCodecState::new();
    let opts = options(false, false);
    encode_file_list_entry(
        &regular_entry(FILE_MODE, MTIME, XMIT_EXTENDED_FLAGS),
        &opts,
        &mut state,
    );
    encode_file_list_entry(
        &regular_entry(0o100_755, MTIME, XMIT_SAME_MODE),
        &opts,
        &mut state,
    );
}

/// The peer would give the file the previous entry's mtime.
#[test]
#[should_panic(expected = "sets XMIT_SAME_TIME but its mtime differs")]
fn b1_encoder_refuses_a_same_time_flag_that_lies() {
    let mut state = FileListCodecState::new();
    let opts = options(false, false);
    encode_file_list_entry(
        &regular_entry(FILE_MODE, MTIME, XMIT_EXTENDED_FLAGS),
        &opts,
        &mut state,
    );
    encode_file_list_entry(
        &regular_entry(FILE_MODE, MTIME + 1, XMIT_SAME_TIME),
        &opts,
        &mut state,
    );
}

/// A truthful SAME flag after a real entry encodes, and the decoder
/// reproduces the inherited values from its own state.
#[test]
fn b1_encoder_and_decoder_agree_on_inherited_values() {
    let opts = options(false, false);
    // rsync sends XMIT_EXTENDED_FLAGS for an entry with no other flag:
    // a bare zero is the end-of-list marker.
    let first = regular_entry(0o100_600, MTIME, XMIT_EXTENDED_FLAGS);
    let second = FileListEntry {
        path: b"g".to_vec(),
        ..regular_entry(0o100_600, MTIME, XMIT_SAME_MODE | XMIT_SAME_TIME)
    };
    let mut enc = FileListCodecState::new();
    let mut bytes = encode_file_list_entry(&first, &opts, &mut enc);
    let second_at = bytes.len();
    bytes.extend_from_slice(&encode_file_list_entry(&second, &opts, &mut enc));

    let mut dec = FileListCodecState::new();
    let (_, n) = decode_file_list_entry(&bytes, &opts, &mut dec).unwrap();
    assert_eq!(n, second_at);
    let (outcome, _) = decode_file_list_entry(&bytes[n..], &opts, &mut dec).unwrap();
    assert_eq!(outcome, FileListDecodeOutcome::Entry(second));
    assert_eq!(dec, enc, "both ends end in the same state");
}

/// The flag bytes rsync's sender writes (`flist.c::send_file_entry`):
///
/// ```text
/// varint:  write_varint(f, xflags ? xflags : XMIT_EXTENDED_FLAGS);
/// classic: if (!xflags && !S_ISDIR(mode)) xflags |= XMIT_TOP_DIR;
///          if ((xflags & 0xFF00) || !xflags) { xflags |= XMIT_EXTENDED_FLAGS; write_shortint(f, xflags); }
///          else write_byte(f, xflags);
/// ```
///
/// The flags an entry decodes back to, for a given entry and encoding.
fn rsync_wire_flags(flags: u32, directory: bool, varint: bool) -> u32 {
    if varint {
        return if flags == 0 {
            XMIT_EXTENDED_FLAGS
        } else {
            flags
        };
    }
    let mut x = flags;
    if x == 0 && !directory {
        x |= XMIT_TOP_DIR;
    }
    if x & 0xFF00 != 0 || x == 0 {
        x |= XMIT_EXTENDED_FLAGS;
    }
    x
}

/// Pins the mapping on the three cases that decide it. With the classic
/// flags of a pre-3.2 peer, an entry built with only XMIT_MOD_NSEC (the
/// product's own upload entry) needs its high byte, so the encoder adds
/// XMIT_EXTENDED_FLAGS: `04 20`. Writing the low byte alone gave `00`, the
/// end of the list; found by the coordinator's live upload against rsync
/// 3.1.3 ("File-list index 9 not in 0 - 0"). Zero flags are not a caller
/// bug: rsync maps them, to TOP_DIR for a file (`01`), to EXTENDED for a
/// directory (`04 00`) and in varint (`04`).
#[test]
fn b1_encoder_writes_the_flag_bytes_rsync_writes() {
    let classic = FileListDecodeOptions {
        xfer_flags_as_varint: false,
        ..options(false, false)
    };
    let varint = options(false, false);
    let nsec = FileListEntry {
        mtime_nsec: Some(5),
        ..regular_entry(FILE_MODE, MTIME, XMIT_MOD_NSEC)
    };
    let dir = regular_entry(DIR_MODE, MTIME, 0);
    let file = regular_entry(FILE_MODE, MTIME, 0);
    for (entry, opts, head) in [
        (&nsec, classic, &[0x04, 0x20][..]),
        (&file, classic, &[0x01][..]),
        (&dir, classic, &[0x04, 0x00][..]),
        (&file, varint, &[0x04][..]),
        // io.c::write_varint: 0x2000 is `a0 00` (the 0x20 fits the first
        // byte under its length bits), as the measured 0x12b8 is `92 b8`.
        (&nsec, varint, &[0xa0, 0x00][..]),
    ] {
        let bytes = encode_file_list_entry(entry, &opts, &mut FileListCodecState::new());
        assert_eq!(&bytes[..head.len()], head, "{:#x}", entry.flags);
        let (outcome, consumed) =
            decode_file_list_entry(&bytes, &opts, &mut FileListCodecState::new())
                .expect("the peer reads an entry, not the end of the list");
        assert_eq!(consumed, bytes.len());
        let expected = FileListEntry {
            flags: rsync_wire_flags(
                entry.flags,
                is_directory_mode(entry.mode),
                opts.xfer_flags_as_varint,
            ),
            ..entry.clone()
        };
        assert_eq!(outcome, FileListDecodeOutcome::Entry(expected.clone()));
        // And the decoded entry, which now carries the bits rsync added,
        // goes back out in the same bytes (`04 00` stays two bytes).
        assert_eq!(
            encode_file_list_entry(&expected, &opts, &mut FileListCodecState::new()),
            bytes,
            "{:#x} re-encoded",
            entry.flags
        );
    }
}

/// The capture oracles cannot see an encoder defect that only shows on an
/// entry built by code: an entry decoded from a capture already carries
/// every bit rsync's sender added, so its round trip passes whatever the
/// encoder does with a bit the caller left out. This is how a classic
/// encoder that dropped the high byte of the flags passed 44 captures and
/// 43 mutations. Here the entries are built the way the product builds
/// them, over every combination of the bits that decide the flag bytes, for
/// a file and a directory, in both encodings; each must decode back to
/// itself, flags aside from what rsync itself adds.
#[test]
fn b1_constructed_entries_round_trip_in_both_flag_encodings() {
    let bits = [
        XMIT_TOP_DIR,
        XMIT_LONG_NAME,
        XMIT_USER_NAME_FOLLOWS,
        XMIT_GROUP_NAME_FOLLOWS,
        XMIT_MOD_NSEC,
    ];
    for mask in 0..1u32 << bits.len() {
        let flags = bits
            .iter()
            .enumerate()
            .filter(|(i, _)| mask & (1 << i) != 0)
            .fold(0, |acc, (_, bit)| acc | bit);
        for varint in [true, false] {
            for directory in [false, true] {
                let opts = FileListDecodeOptions {
                    xfer_flags_as_varint: varint,
                    ..options(true, false)
                };
                let entry = FileListEntry {
                    uid: Some(1000),
                    uid_name: (flags & XMIT_USER_NAME_FOLLOWS != 0).then(|| "u".to_string()),
                    gid: Some(1000),
                    gid_name: (flags & XMIT_GROUP_NAME_FOLLOWS != 0).then(|| "g".to_string()),
                    mtime_nsec: (flags & XMIT_MOD_NSEC != 0).then_some(7),
                    ..regular_entry(if directory { DIR_MODE } else { FILE_MODE }, MTIME, flags)
                };
                let bytes = encode_file_list_entry(&entry, &opts, &mut FileListCodecState::new());
                let (outcome, consumed) =
                    decode_file_list_entry(&bytes, &opts, &mut FileListCodecState::new())
                        .unwrap_or_else(|e| panic!("{flags:#x} varint={varint}: {e}"));
                assert_eq!(consumed, bytes.len(), "{flags:#x} varint={varint}");
                let expected = FileListEntry {
                    flags: rsync_wire_flags(flags, directory, varint),
                    ..entry
                };
                assert_eq!(
                    outcome,
                    FileListDecodeOutcome::Entry(expected),
                    "{flags:#x} varint={varint} directory={directory}"
                );
            }
        }
    }
}

/// A directory never carries the `-c` checksum; handing one to the
/// encoder would silently drop it, so it panics instead.
#[test]
#[should_panic(expected = "carries a checksum that rsync never puts on the wire")]
fn b1_encoder_refuses_a_checksum_on_a_directory() {
    let entry = FileListEntry {
        checksum: vec![0; 16],
        ..regular_entry(DIR_MODE, MTIME, XMIT_EXTENDED_FLAGS)
    };
    encode_file_list_entry(
        &entry,
        &options(false, true),
        &mut FileListCodecState::new(),
    );
}

// ---------------------------------------------------------------------------
// Point 6: a hard-link follower is a typed refusal, not a misparse (§7)
// ---------------------------------------------------------------------------

/// Segment "h" of `p6-dl` sends `c` (group leader), `sub`, `b` (leader),
/// `z`, then `a`, a follower whose body is `first_hlink_ndx = 5` and
/// nothing else. Leaders are full entries and decode; the follower is
/// refused by name, and the codec state still points at `z`.
#[test]
fn b1_point6_hardlink_follower_is_refused_by_name() {
    let app = sender_app_stream("p6-dl");
    let opts = options(true, false);
    let mut state = FileListCodecState::new();
    let initial = decode_list(&app, 0, &opts, &mut state);
    assert_eq!(initial.entries.len(), 1);

    let mut ndx_state = NdxState::new();
    let (ndx, consumed) = decode_ndx(&app[initial.end..], &mut ndx_state).unwrap();
    assert_eq!(ndx, NDX_FLIST_OFFSET);
    let mut cursor = initial.end + consumed;

    let mut decoded = Vec::new();
    let err = loop {
        match decode_file_list_entry(&app[cursor..], &opts, &mut state) {
            Ok((FileListDecodeOutcome::Entry(entry), n)) => {
                decoded.push(entry.path_lossy().into_owned());
                cursor += n;
            }
            Ok((FileListDecodeOutcome::EndOfList { .. }, _)) => {
                panic!("segment \"h\" ended without reaching the follower")
            }
            Err(err) => break err,
        }
    };
    assert_eq!(decoded, ["h/c", "h/sub", "h/b", "h/z"]);
    assert_eq!(
        err,
        RealWireError::HardlinkUnsupported {
            path: "h/a".to_string()
        }
    );
    assert_eq!(
        state.previous_name(),
        Some(&b"h/z"[..]),
        "a refused entry leaves the state untouched"
    );
}

// ---------------------------------------------------------------------------
// Point 8: 50,000 entries in one segment, decoded in streaming (§9)
// ---------------------------------------------------------------------------

const P8_ENTRIES: usize = 50_000;
/// End of the 50,000-entry segment in the sender stream (§9).
const P8_LIST_END: usize = 0x26dbff;
/// Largest `MSG_DATA` payload of `p8-dl` (§9).
const P8_MAX_FRAME: usize = 65_532;

/// Feed `p8-dl` through the multiplex reader in 4 KiB network reads, and
/// the `MSG_DATA` payloads through the streaming decoder as they come out,
/// keeping nothing but a count. The segment is 2.5 MB; the decoder never
/// holds more than one frame plus one unfinished entry. The initial
/// segment and the `NDX_FLIST_OFFSET` marker are consumed on the way, and
/// the state they leave is what the big segment starts from.
///
/// The bound is measured on the bytes the decoder physically holds
/// (`retained`), right after each feed, not on the unconsumed ones: a
/// decoder that consumed entries but never released their bytes would
/// keep `buffered` small while holding the whole list.
#[test]
fn b1_point8_fifty_thousand_entries_stream_within_one_frame_of_memory() {
    #[derive(PartialEq)]
    enum Phase {
        Initial,
        Marker,
        Big,
    }
    let (raw, start) = sender_raw("p8-dl");
    let opts = options(true, false);
    let placeholder = || FileListStreamDecoder::new(opts, FileListCodecState::new(), 0);
    let mut mux = MuxStreamReader::new();
    let mut decoder = FileListStreamDecoder::new(opts, FileListCodecState::new(), 1);
    let mut phase = Phase::Initial;
    let mut carried: Option<FileListCodecState> = None;
    let mut between: Vec<u8> = Vec::new();
    let mut ndx_state = NdxState::new();
    let mut peak = 0usize;
    let mut last_path = String::new();
    let mut app_bytes = 0usize;

    'read: for chunk in raw[start..].chunks(4096) {
        mux.feed(chunk);
        while let Some(frame) = mux.poll_frame() {
            let MuxPoll::Data(payload) = frame.expect("mux frame") else {
                continue;
            };
            app_bytes += payload.len();
            if phase == Phase::Marker {
                between.extend_from_slice(&payload);
            } else {
                decoder.feed(&payload);
                peak = peak.max(decoder.retained());
            }
            loop {
                if phase == Phase::Marker {
                    match decode_ndx(&between, &mut ndx_state) {
                        Ok((ndx, n)) => {
                            assert_eq!(ndx, NDX_FLIST_OFFSET, "segment for dir_flist[0]");
                            let state = carried.take().expect("state of the initial segment");
                            decoder = FileListStreamDecoder::new(opts, state, P8_ENTRIES);
                            decoder.feed(&between[n..]);
                            peak = peak.max(decoder.retained());
                            between.clear();
                            phase = Phase::Big;
                        }
                        Err(RealWireError::NdxTruncated { .. }) => break,
                        Err(e) => panic!("segment marker: {e}"),
                    }
                }
                match decoder.next_outcome().expect("streaming decode") {
                    Some(FileListDecodeOutcome::Entry(entry)) => {
                        last_path = entry.path_lossy().into_owned()
                    }
                    Some(FileListDecodeOutcome::EndOfList { .. }) => {
                        if phase == Phase::Big {
                            break 'read;
                        }
                        let (state, rest) =
                            std::mem::replace(&mut decoder, placeholder()).into_parts();
                        carried = Some(state);
                        between = rest;
                        phase = Phase::Marker;
                    }
                    None => break,
                }
            }
        }
    }

    assert!(phase == Phase::Big, "both segments decoded");
    assert!(decoder.is_finished());
    assert_eq!(decoder.entries_decoded(), P8_ENTRIES);
    assert!(last_path.starts_with("big/") && last_path.ends_with(".bin"));
    assert!(
        app_bytes >= P8_LIST_END,
        "the list was read through its end"
    );
    assert!(
        peak <= P8_MAX_FRAME + 4096,
        "decoder held {peak} bytes: more than one frame plus one unfinished entry"
    );
    assert!(
        peak * 20 < P8_LIST_END,
        "decoder held {peak} bytes, not small against a {P8_LIST_END}-byte list"
    );
}

/// The same 50,000 entries, decoded as one sequence and re-encoded with
/// one running state: 2.5 MB of compressed names, each against the entry
/// before it on the wire, reproduced byte for byte.
#[test]
fn b1_point8_fifty_thousand_entries_reencode_byte_for_byte() {
    let app = sender_app_stream("p8-dl");
    let opts = options(true, false);
    let lists = decode_inc_session(&app, &opts);
    assert_eq!(lists.len(), 2);
    assert_eq!(lists[1].entries.len(), P8_ENTRIES);
    assert_eq!(lists[1].end, P8_LIST_END);
    assert!(lists[1]
        .entries
        .iter()
        .all(|e| e.mode == 0o100_664 && e.size == 0 && e.mtime == MTIME));
    assert_lists_reencode(&app, &lists, &opts);
}

// ---------------------------------------------------------------------------
// B1 addendum captures (`capture/b1_wire_campaign.py`)
// ---------------------------------------------------------------------------

/// Every entry of a capture's session, all lists together.
fn session_entries(run: &str) -> Vec<FileListEntry> {
    let full = sender_app_stream(run);
    let app = &full[file_list_start(run, &full)..];
    let (opts, inc) = options_from_capture(run);
    decode_session(app, &opts, inc)
        .into_iter()
        .flat_map(|l| l.entries)
        .collect()
}

fn entry_named<'a>(entries: &'a [FileListEntry], path: &[u8]) -> &'a FileListEntry {
    entries
        .iter()
        .find(|e| e.path == path)
        .unwrap_or_else(|| panic!("no entry {:?}", String::from_utf8_lossy(path)))
}

/// Devices made by `finish_dv`: (name, mode, major, minor).
const DV_DEVICES: [(&[u8], u32, u32, u32); 5] = [
    (b"dv/cnull", 0o020_644, 1, 3),
    (b"dv/czero", 0o020_644, 1, 5),
    (b"dv/bloop", 0o060_644, 7, 0),
    (b"dv/cwide", 0o020_644, 4, 300),
    (b"dv/cbig", 0o020_644, 259, 70_000),
];

/// `-D` puts a device's number on the wire, and only a device's: at
/// protocol 31 a fifo and a socket carry none. The second major of 1 in a
/// row rides on XMIT_SAME_RDEV_MAJOR and still decodes to 1; a minor of 300
/// and a 259/70000 pair take multi-byte varints. Without `-D` the same
/// devices are still listed, with their modes, and no number. Without
/// `-l` the symlink is listed with no target. Measured on 3.2.7, and on
/// 3.1.3 with the classic flags.
#[test]
fn b1_device_numbers_and_link_targets_follow_the_flag_string() {
    for run in ["b1-dev-dl-full", "b1-dev-dl-noinc", "b1-313-dev-dl"] {
        let entries = session_entries(run);
        for (name, mode, major, minor) in DV_DEVICES {
            let e = entry_named(&entries, name);
            assert_eq!(e.mode, mode, "{run}");
            assert_eq!(e.rdev, Some(Rdev { major, minor }), "{run}");
        }
        for special in [&b"dv/fifo"[..], b"dv/sock"] {
            assert_eq!(entry_named(&entries, special).rdev, None, "{run}");
        }
        assert_eq!(
            entry_named(&entries, b"dv/ln").symlink_target.as_deref(),
            Some(&b"target.txt"[..]),
            "{run}"
        );
    }
    let entries = session_entries("b1-dev-dl-full");
    let wire_order: Vec<&[u8]> = entries.iter().map(|e| e.path.as_slice()).collect();
    let cnull = wire_order.iter().position(|p| *p == b"dv/cnull").unwrap();
    assert_eq!(wire_order[cnull + 1], b"dv/czero", "sent back to back");
    assert_ne!(entries[cnull + 1].flags & XMIT_SAME_RDEV_MAJOR, 0);

    let nodev = session_entries("b1-dev-dl-nodev");
    for (name, mode, _, _) in DV_DEVICES {
        let e = entry_named(&nodev, name);
        assert_eq!((e.mode, e.rdev), (mode, None), "b1-dev-dl-nodev");
    }
    let nolinks = session_entries("b1-dev-dl-nolinks");
    let ln = entry_named(&nolinks, b"dv/ln");
    assert_eq!((ln.mode, ln.symlink_target.as_ref()), (0o120_777, None));
    assert!(entry_named(&nolinks, b"dv/cnull").rdev.is_some());
}

/// A suffix above 255 bytes rides on XMIT_LONG_NAME as a varint (501 bytes
/// measured), and a path of 3,719 bytes decodes and re-encodes; the
/// round trip itself is in the all-captures test.
#[test]
fn b1_long_names_decode_on_both_servers() {
    for run in [
        "b1-long-dl",
        "b1-long-up",
        "b1-long-dl-noinc",
        "b1-313-long-dl",
    ] {
        let entries = session_entries(run);
        assert!(
            entries.iter().any(|e| e.flags & XMIT_LONG_NAME != 0),
            "{run}: some suffix needs LONG_NAME"
        );
        let longest = entries.iter().map(|e| e.path.len()).max().unwrap();
        assert_eq!(longest, 3 + 2 + 14 * 251 + 200, "{run}: ln/d/e.../f");
        assert!(longest < MAXPATHLEN);
    }
}

/// rsync sends a filename's bytes unchanged; a Latin-1 name is a legal
/// entry and the codec keeps it byte for byte.
#[test]
fn b1_non_utf8_names_are_kept_as_bytes() {
    let entries = session_entries("b1-nonutf8-dl");
    for name in [&b"nu/caf\xe9.txt"[..], b"nu/d\xff", b"nu/d\xff/x"] {
        let e = entry_named(&entries, name);
        assert!(std::str::from_utf8(&e.path).is_err());
        assert!(e.path_lossy().contains('\u{fffd}'));
    }
}

/// An unreadable directory makes the sender end the affected list with its
/// I/O error: `00 01` with varint flags, and `04 10 01`
/// (EXTENDED_FLAGS|IO_ERROR_ENDLIST, then the error) with the classic flags
/// of 3.1.3, which the decoder once read as an entry.
#[test]
fn b1_io_error_rides_on_the_terminator_in_both_flag_encodings() {
    for (run, terminator) in [
        ("b1-ioerr-dl", &[0x00, 0x01][..]),
        ("b1-ioerr-dl-noinc", &[0x00, 0x01][..]),
        ("b1-313-ioerr-dl", &[0x04, 0x10, 0x01][..]),
    ] {
        let full = sender_app_stream(run);
        let app = &full[file_list_start(run, &full)..];
        let (opts, inc) = options_from_capture(run);
        let lists = decode_session(app, &opts, inc);
        let failed: Vec<&DecodedList> = lists.iter().filter(|l| l.io_error != 0).collect();
        assert_eq!(
            failed.len(),
            1,
            "{run}: one list met the unreadable directory"
        );
        assert_eq!(failed[0].io_error, 1, "{run}");
        assert!(app[..failed[0].end].ends_with(terminator), "{run}");
        assert_eq!(
            encode_file_list_terminator(&opts, 1),
            terminator,
            "{run}: the encoder writes the same terminator"
        );
    }
}

/// The same tree sent by 3.1.3 (classic flags, no negotiated strings) and
/// by 3.2.7 (varint flags) decodes to the same entries: the flag encoding
/// changes the bytes, never the model.
#[test]
fn b1_classic_and_varint_flags_decode_to_the_same_list() {
    for (classic, varint) in [
        ("b1-313-p1-dl", "p1-dl"),
        ("b1-313-p1c-dl", "p1c-dl"),
        ("b1-313-p2-dl-noinc", "p2-dl-noinc"),
        ("b1-313-p7-dl-product", "p7-dl-product"),
    ] {
        assert!(!options_from_capture(classic).0.xfer_flags_as_varint);
        assert!(options_from_capture(varint).0.xfer_flags_as_varint);
        let key = |e: &FileListEntry| (e.path.clone(), e.mode, e.size, e.mtime, e.uid, e.gid);
        let a: Vec<_> = session_entries(classic).iter().map(key).collect();
        let b: Vec<_> = session_entries(varint).iter().map(key).collect();
        assert_eq!(a, b, "{classic} against {varint}");
    }
}

// ---------------------------------------------------------------------------
// Streaming decoder: boundaries and ceilings
// ---------------------------------------------------------------------------

/// Every capture whose file list B1 models end to end: the B0 campaign and
/// the B1 addendum (`b1-*`, devices, specials, links without `-l`,
/// LONG_NAME, non-UTF-8 names, io_error, and a 3.1.3 server's classic
/// flags).
const DECODABLE: [&str; 46] = [
    "b1-313-dev-dl",
    "b1-313-ioerr-dl",
    "b1-313-long-dl",
    "b1-313-p1-dl",
    "b1-313-p1-up",
    "b1-313-p1c-dl",
    "b1-313-p2-dl-noinc",
    "b1-313-p7-dl-product",
    "b1-dev-dl-full",
    "b1-dev-dl-noinc",
    "b1-dev-dl-nodev",
    "b1-dev-dl-nolinks",
    "b1-ioerr-dl",
    "b1-ioerr-dl-noinc",
    "b1-long-dl",
    "b1-long-dl-noinc",
    "b1-long-up",
    "b1-nonutf8-dl",
    "c10-dl-inc",
    "c10-dl-noinc",
    "c10-up",
    "c11-dl",
    "c12-up-dryrun",
    "c12-up-mkpath",
    "c13-up-delete",
    "p1-dl",
    "p1-up",
    "p1c-dl",
    "p2-dl-noinc",
    "p2-up-noinc",
    "p3-dl",
    "p3-up",
    "p4-dl",
    "p4-up",
    "p5-dl",
    "p5-up",
    "p7-dl-full",
    "p7-dl-product",
    "p7-up-full",
    "p7-up-product",
    "p8-dl",
    "p8-up",
    "p9-dl-inc",
    "p9-dl-noinc",
    "p9-up-inc",
    "p9-up-noinc",
];

/// Every decodable capture, with the options read from its own server
/// command: the whole list (all segments, one state) re-encodes byte for
/// byte, and fed ONE byte at a time the streaming decoder gives the same
/// entries and ends in the same state. Every byte boundary of real lists,
/// inside names, ACLs, checksums and id-bearing fields alike, is therefore
/// a recoverable "feed more", and a failed attempt never moves the state.
/// The two 50,000-entry lists go in 997-byte pieces, which still cut
/// entries at every offset.
#[test]
fn b1_every_decodable_capture_round_trips_and_streams_byte_by_byte() {
    for run in DECODABLE {
        eprintln!("b1 capture: {run}");
        let full = sender_app_stream(run);
        let app = &full[file_list_start(run, &full)..];
        let (opts, inc) = options_from_capture(run);
        let lists = decode_session(app, &opts, inc);
        assert_lists_reencode(app, &lists, &opts);

        let chunk = if run.starts_with("p8") { 997 } else { 1 };
        let (streamed, state) = stream_session(app, opts, inc, chunk);
        let expected: Vec<Vec<FileListEntry>> = lists.iter().map(|l| l.entries.clone()).collect();
        assert_eq!(streamed, expected, "{run}: streamed lists");
        let mut batch_state = FileListCodecState::new();
        for list in &lists {
            decode_list(app, list.start, &opts, &mut batch_state);
        }
        assert_eq!(state, batch_state, "{run}: final state");
    }
}

/// The captures B1 does not model say so with a typed error at the first
/// entry it cannot read, never with a misparse: xattr set references are
/// B2, hard-link followers are B4. (`p9big-*` is not here: its segments
/// interleave with transfer items, which is B3's driver, not the codec.)
#[test]
fn b1_captures_beyond_b1_are_refused_by_name() {
    for (run, expected) in [
        ("p6-dl", "hardlink"),
        ("p6-up", "hardlink"),
        ("p6-dl-noinc", "hardlink"),
        ("p6-up-noinc", "hardlink"),
        ("p6c-dl", "hardlink"),
    ] {
        let app = sender_app_stream(run);
        let (opts, inc) = options_from_capture(run);
        let mut state = FileListCodecState::new();
        let mut ndx_state = NdxState::new();
        let mut cursor = 0;
        let err = 'walk: loop {
            loop {
                match decode_file_list_entry(&app[cursor..], &opts, &mut state) {
                    Ok((FileListDecodeOutcome::Entry(_), n)) => cursor += n,
                    Ok((FileListDecodeOutcome::EndOfList { .. }, n)) => {
                        cursor += n;
                        break;
                    }
                    Err(e) => break 'walk e,
                }
            }
            assert!(inc, "{run}: a non-incremental list ended without a refusal");
            let (ndx, n) = decode_ndx(&app[cursor..], &mut ndx_state).expect("marker");
            assert!(
                ndx <= NDX_FLIST_OFFSET,
                "{run}: list ended without a refusal"
            );
            cursor += n;
        };
        match (expected, &err) {
            ("hardlink", RealWireError::HardlinkUnsupported { .. }) => {}
            _ => panic!("{run}: expected a {expected} refusal, got {err:?}"),
        }
    }
}

/// The entry ceiling is the caller's and it bites at the first entry past it.
#[test]
fn b1_streaming_entry_count_ceiling_is_enforced() {
    let app = sender_app_stream("p2-dl-noinc");
    let mut decoder =
        FileListStreamDecoder::new(options(true, false), FileListCodecState::new(), 8);
    decoder.feed(&app);
    for _ in 0..8 {
        assert!(matches!(
            decoder.next_outcome(),
            Ok(Some(FileListDecodeOutcome::Entry(_)))
        ));
    }
    assert_eq!(
        decoder.next_outcome(),
        Err(RealWireError::FileListTooManyEntries { limit: 8 })
    );
}

/// The ceiling never refuses a legal entry: a directory with every field
/// at the codec's own limits (longest path, two full ACLs of the longest
/// names, the most xattrs with the longest names and the largest inline
/// values) encodes under half of it, and streams through it intact.
#[test]
fn b1_streaming_largest_legal_entry_fits_the_ceiling() {
    let named = |i: usize| AclNamedEntry {
        id: u32::MAX - i as u32,
        principal: AclPrincipal::User,
        access: 7,
        name: Some("n".repeat(MAX_ACL_NAME_LEN)),
    };
    let full_acl = || {
        AclWireEntry::Literal(RsyncAcl {
            user_obj: Some(7),
            group_obj: Some(7),
            mask_obj: Some(7),
            other_obj: Some(7),
            names: (0..MAX_ACL_NAMED_ENTRIES).map(named).collect(),
        })
    };
    let xattrs = (0..MAX_XATTR_PAIRS)
        .map(|i| {
            let name = format!("user.{i:0>width$}", width = MAX_XATTR_NAME_LEN - 5);
            XattrPair::inline(name, vec![0xab; MAX_FULL_DATUM])
        })
        .collect::<Vec<_>>();
    let entry = FileListEntry {
        flags: XMIT_LONG_NAME | XMIT_USER_NAME_FOLLOWS | XMIT_GROUP_NAME_FOLLOWS | XMIT_MOD_NSEC,
        path: "d".repeat(4095).into_bytes(),
        size: i64::MAX,
        mtime: i64::MAX,
        mtime_nsec: Some(999_999_999),
        mode: DIR_MODE,
        uid: Some(i64::from(i32::MAX)),
        uid_name: Some("u".repeat(255)),
        gid: Some(i64::from(i32::MAX)),
        gid_name: Some("g".repeat(255)),
        checksum: Vec::new(),
        symlink_target: None,
        rdev: None,
        xattrs: Some(crate::aerorsync::real_wire::XattrWireEntry::Literal(xattrs)),
        acls: Some(FileListAcls {
            access: full_acl(),
            default: Some(full_acl()),
        }),
    };
    let opts = FileListDecodeOptions {
        preserve_acls: true,
        preserve_xattrs: true,
        ..options(true, false)
    };
    let bytes = encode_file_list_entry(&entry, &opts, &mut FileListCodecState::new());
    assert!(
        bytes.len() * 2 < MAX_FILE_LIST_ENTRY_BYTES,
        "largest legal entry is {} bytes, not under half the {MAX_FILE_LIST_ENTRY_BYTES} ceiling",
        bytes.len()
    );
    assert!(bytes.len() > 200_000, "the entry really is at the limits");

    let mut decoder = FileListStreamDecoder::new(opts, FileListCodecState::new(), 1);
    let mut decoded = None;
    for chunk in bytes.chunks(4096) {
        decoder.feed(chunk);
        if let Some(outcome) = decoder
            .next_outcome()
            .expect("a legal entry is not refused")
        {
            decoded = Some(outcome);
        }
    }
    assert_eq!(decoded, Some(FileListDecodeOutcome::Entry(entry)));
}

/// The per-entry ceiling bites on an entry that is still incomplete past
/// it. Every field has its own bound, so no entry reaches the default
/// ceiling; a caller-lowered one shows the mechanism on a legal entry with
/// a 3,000-byte path, fed in small chunks.
#[test]
fn b1_streaming_per_entry_byte_ceiling_is_enforced() {
    let entry = FileListEntry {
        path: "p".repeat(3000).into_bytes(),
        ..regular_entry(FILE_MODE, MTIME, XMIT_LONG_NAME)
    };
    let opts = options(false, false);
    let bytes = encode_file_list_entry(&entry, &opts, &mut FileListCodecState::new());
    let mut decoder =
        FileListStreamDecoder::new(opts, FileListCodecState::new(), 10).with_max_entry_bytes(1024);
    let mut fed = 0;
    for chunk in bytes.chunks(64) {
        decoder.feed(chunk);
        fed += chunk.len();
        match decoder.next_outcome() {
            Ok(None) => assert!(fed <= 1024, "waited past the ceiling ({fed} bytes)"),
            Err(RealWireError::FileListEntryTooLarge { limit, buffered }) => {
                assert_eq!(limit, 1024);
                assert!(buffered > 1024);
                return;
            }
            other => panic!("unexpected outcome {other:?}"),
        }
    }
    panic!(
        "a {}-byte entry went through a 1024-byte ceiling",
        bytes.len()
    );
}

/// rsync's `recv_file_entry` exits on `l1 + l2 >= MAXPATHLEN` before it
/// reads the name. So does the codec, as a refusal and not as a wait: a
/// stream decoder given only the declared length must fail at once,
/// instead of asking for 4 KiB more that could never make the entry legal.
/// One byte shorter is the longest legal path, and decodes.
#[test]
fn b1_path_at_maxpathlen_is_refused_before_its_bytes_arrive() {
    let opts = options(false, false);
    let mut head = encode_varint(XMIT_LONG_NAME as i32);
    head.extend_from_slice(&encode_varint(MAXPATHLEN as i32));
    assert_eq!(
        decode_file_list_entry(&head, &opts, &mut FileListCodecState::new()),
        Err(RealWireError::PathTooLong {
            field: "path",
            declared: MAXPATHLEN,
            max: MAXPATHLEN - 1,
        })
    );
    let mut decoder = FileListStreamDecoder::new(opts, FileListCodecState::new(), 1);
    decoder.feed(&head);
    assert!(matches!(
        decoder.next_outcome(),
        Err(RealWireError::PathTooLong { .. })
    ));

    // The same overflow reached through the prefix of the previous name.
    let mut state = FileListCodecState::new();
    let previous = FileListEntry {
        path: "q".repeat(200).into_bytes(),
        ..regular_entry(FILE_MODE, MTIME, XMIT_EXTENDED_FLAGS)
    };
    let mut bytes = encode_file_list_entry(&previous, &opts, &mut state.clone());
    let n = bytes.len();
    bytes.extend_from_slice(&encode_varint((XMIT_SAME_NAME | XMIT_LONG_NAME) as i32));
    bytes.push(200);
    bytes.extend_from_slice(&encode_varint((MAXPATHLEN - 200) as i32));
    decode_file_list_entry(&bytes, &opts, &mut state).expect("previous entry");
    assert!(matches!(
        decode_file_list_entry(&bytes[n..], &opts, &mut state),
        Err(RealWireError::PathTooLong { declared, .. }) if declared == MAXPATHLEN
    ));

    let longest = FileListEntry {
        path: "r".repeat(MAXPATHLEN - 1).into_bytes(),
        ..regular_entry(FILE_MODE, MTIME, XMIT_LONG_NAME)
    };
    let bytes = encode_file_list_entry(&longest, &opts, &mut FileListCodecState::new());
    let (outcome, _) = decode_file_list_entry(&bytes, &opts, &mut FileListCodecState::new())
        .expect("a path of MAXPATHLEN - 1 bytes is legal");
    assert_eq!(outcome, FileListDecodeOutcome::Entry(longest));
}

/// A symlink target must fit MAXPATHLEN with its NUL (`linkname_len =
/// read_varint30(f) + 1; if (linkname_len > MAXPATHLEN)` in rsync).
#[test]
fn b1_symlink_target_at_maxpathlen_is_refused_before_its_bytes_arrive() {
    let opts = options(false, false);
    let mut bytes = encode_varint(XMIT_EXTENDED_FLAGS as i32);
    bytes.push(1);
    bytes.push(b'l');
    bytes.extend_from_slice(&encode_varlong(0, 3));
    bytes.extend_from_slice(&encode_varlong(MTIME, 4));
    bytes.extend_from_slice(&0o120_777u32.to_le_bytes());
    bytes.extend_from_slice(&encode_varint(MAXPATHLEN as i32));
    assert_eq!(
        decode_file_list_entry(&bytes, &opts, &mut FileListCodecState::new()),
        Err(RealWireError::PathTooLong {
            field: "symlink_target",
            declared: MAXPATHLEN,
            max: MAXPATHLEN - 1,
        })
    );
}

/// Below protocol 30 rsync sends ids, times and device numbers in other
/// shapes; the codec says so instead of reading them as varints.
#[test]
fn b1_protocol_below_30_is_refused_by_name() {
    let opts = FileListDecodeOptions {
        protocol: 29,
        ..options(false, false)
    };
    assert_eq!(
        decode_file_list_entry(&[0x01, 0x01, b'f'], &opts, &mut FileListCodecState::new()),
        Err(RealWireError::FileListProtocolUnsupported { protocol: 29 })
    );
}

/// A suffix above 255 bytes needs XMIT_LONG_NAME; without it the length
/// would be cut to its low byte and the rest of the name read as fields.
#[test]
#[should_panic(expected = "needs XMIT_LONG_NAME")]
fn b1_encoder_refuses_a_long_suffix_without_long_name() {
    let entry = FileListEntry {
        path: "s".repeat(300).into_bytes(),
        ..regular_entry(FILE_MODE, MTIME, XMIT_EXTENDED_FLAGS)
    };
    encode_file_list_entry(
        &entry,
        &options(false, false),
        &mut FileListCodecState::new(),
    );
}

/// Without `-l` the peer expects no target: sending one would be read as
/// the next entry.
#[test]
#[should_panic(expected = "symlink target the session does not put on the wire")]
fn b1_encoder_refuses_a_symlink_target_without_links() {
    let entry = FileListEntry {
        symlink_target: Some(b"t".to_vec()),
        ..regular_entry(0o120_777, MTIME, XMIT_EXTENDED_FLAGS)
    };
    let opts = FileListDecodeOptions {
        preserve_links: false,
        ..options(false, false)
    };
    encode_file_list_entry(&entry, &opts, &mut FileListCodecState::new());
}

/// A device number under a session without `-D`, and a device with no
/// number under one with it, are both caller bugs.
#[test]
#[should_panic(expected = "device number the session does not put on the wire")]
fn b1_encoder_refuses_an_rdev_without_devices() {
    let entry = FileListEntry {
        rdev: Some(Rdev { major: 1, minor: 3 }),
        ..regular_entry(0o020_666, MTIME, XMIT_EXTENDED_FLAGS)
    };
    encode_file_list_entry(
        &entry,
        &options(false, false),
        &mut FileListCodecState::new(),
    );
}

#[test]
#[should_panic(expected = "negotiated -D but carries no device number")]
fn b1_encoder_refuses_a_device_without_rdev_under_devices() {
    let entry = regular_entry(0o020_666, MTIME, XMIT_EXTENDED_FLAGS);
    encode_file_list_entry(
        &entry,
        &options(true, false),
        &mut FileListCodecState::new(),
    );
}

#[test]
#[should_panic(expected = "sets XMIT_SAME_RDEV_MAJOR but its major differs")]
fn b1_encoder_refuses_a_same_rdev_major_flag_that_lies() {
    let entry = FileListEntry {
        rdev: Some(Rdev { major: 1, minor: 3 }),
        ..regular_entry(0o020_666, MTIME, XMIT_SAME_RDEV_MAJOR)
    };
    encode_file_list_entry(
        &entry,
        &options(true, false),
        &mut FileListCodecState::new(),
    );
}

// ---------------------------------------------------------------------------
// Point 3, 4, 5: session tables (12-multi-entry-wire-evidence.md §4, §5, §6)
// ---------------------------------------------------------------------------

/// Decode one capture's whole session into owned lists plus the app
/// stream they came from, so re-encode comparisons can slice it.
fn decode_capture_session(run: &str) -> (Vec<u8>, FileListDecodeOptions, Vec<DecodedList>) {
    let full = sender_app_stream(run);
    let start = file_list_start(run, &full);
    let app = full[start..].to_vec();
    let (opts, inc) = options_from_capture(run);
    let lists = decode_session(&app, &opts, inc);
    (app, opts, lists)
}

/// Every entry of every list of a session, in wire order.
fn wire_order_entries(lists: &[DecodedList]) -> Vec<&FileListEntry> {
    lists.iter().flat_map(|l| l.entries.iter()).collect()
}

/// One named ACL entry as the captures carry it: numeric id, user
/// principal, no inline name (ACL_NAME_FOLLOWS not set in these wires).
fn named_acl_entry(id: u32, access: u8) -> AclNamedEntry {
    AclNamedEntry {
        id,
        principal: AclPrincipal::User,
        access,
        name: None,
    }
}

/// §4: xattr sets resolve in wire order on `p3-up` and `p3-dl`: set 0 is
/// the empty set of the directory, set 1 `user.color=red`, set 2
/// `user.color=blue` + `user.tag=x`, and the last entry is a reference to
/// set 2 that resolves equal to it.
#[test]
fn b2_point3_xattr_sets_resolve_in_wire_order() {
    let red = vec![XattrPair::inline("user.color", "red".as_bytes())];
    let blue_tag = vec![
        XattrPair::inline("user.color", "blue".as_bytes()),
        XattrPair::inline("user.tag", "x".as_bytes()),
    ];
    for run in ["p3-up", "p3-dl"] {
        let (_app, _opts, lists) = decode_capture_session(run);
        let entries = wire_order_entries(&lists);
        assert_eq!(entries.len(), 4, "{run}");
        assert_eq!(entries[0].path, b"xa".as_slice(), "{run}");
        assert_eq!(entries[1].path, b"xa/f3".as_slice(), "{run}");
        assert_eq!(entries[2].path, b"xa/f1".as_slice(), "{run}");
        assert_eq!(entries[3].path, b"xa/f2".as_slice(), "{run}");
        // wire shapes: the directory is literal set 0 (empty), the last
        // entry is the only reference
        assert_eq!(entries[0].xattrs, Some(XattrWireEntry::Literal(vec![])));
        assert_eq!(entries[3].xattrs, Some(XattrWireEntry::Reference(2)));

        let mut tables = SessionTables::new();
        let resolved: Vec<_> = entries
            .iter()
            .map(|e| tables.resolve_entry(e).expect("{run}: resolve"))
            .collect();
        assert_eq!(resolved[0].xattrs, Some(vec![]), "{run}: set 0");
        assert_eq!(resolved[1].xattrs, Some(red.clone()), "{run}: set 1");
        assert_eq!(resolved[2].xattrs, Some(blue_tag.clone()), "{run}: set 2");
        assert_eq!(
            resolved[3].xattrs,
            Some(blue_tag.clone()),
            "{run}: reference to set 2 resolves equal to set 2"
        );
    }
}

/// §5: ACL references resolve in wire order on `p4-up` and `p4-dl` (same
/// shape, verified): `u:12345:rw-` plus the mask-covered group bits on
/// f3, `u:12345:r--` on f1, and f2's reference to access index 2 resolves
/// equal to f1's ACL.
#[test]
fn b2_point4_acl_references_resolve_in_wire_order() {
    let f3_acl = RsyncAcl {
        group_obj: Some(4),
        names: vec![named_acl_entry(12345, 6)],
        ..RsyncAcl::default()
    };
    let f1_acl = RsyncAcl {
        names: vec![named_acl_entry(12345, 4)],
        ..RsyncAcl::default()
    };
    for run in ["p4-up", "p4-dl"] {
        let (_app, _opts, lists) = decode_capture_session(run);
        let entries = wire_order_entries(&lists);
        assert_eq!(entries.len(), 4, "{run}");
        assert_eq!(entries[0].path, b"ac".as_slice(), "{run}");
        assert_eq!(entries[1].path, b"ac/f3".as_slice(), "{run}");
        assert_eq!(entries[2].path, b"ac/f1".as_slice(), "{run}");
        assert_eq!(entries[3].path, b"ac/f2".as_slice(), "{run}");
        assert_eq!(
            entries[3].acls.as_ref().expect("-A session").access,
            AclWireEntry::Reference(2),
            "{run}: f2 is the only access reference"
        );

        let mut tables = SessionTables::new();
        let resolved: Vec<_> = entries
            .iter()
            .map(|e| tables.resolve_entry(e).expect("{run}: resolve"))
            .collect();
        assert_eq!(
            resolved[0].acl_access,
            Some(RsyncAcl::empty()),
            "{run}: ac access"
        );
        assert_eq!(
            resolved[0].acl_default,
            Some(RsyncAcl::empty()),
            "{run}: ac default"
        );
        assert_eq!(resolved[1].acl_access, Some(f3_acl.clone()), "{run}: f3");
        assert_eq!(resolved[2].acl_access, Some(f1_acl.clone()), "{run}: f1");
        assert_eq!(
            resolved[3].acl_access,
            Some(f1_acl.clone()),
            "{run}: f2 via the reference to index 2"
        );
        assert_eq!(
            resolved[1].acl_default, None,
            "{run}: files carry no default ACL"
        );
    }
}

/// §6: `da/sub` resolves DEFAULT 0 after two access literals on `p5-up`
/// and `p5-dl`: access and default have separate index spaces.
#[test]
fn b2_point5_default_acl_has_its_own_index_space() {
    let da_default = RsyncAcl {
        user_obj: Some(7),
        group_obj: Some(7),
        mask_obj: Some(7),
        other_obj: Some(5),
        names: vec![named_acl_entry(12345, 7)],
    };
    let sub_access = RsyncAcl {
        names: vec![named_acl_entry(12345, 7)],
        ..RsyncAcl::default()
    };
    let f_access = RsyncAcl {
        group_obj: Some(7),
        names: vec![named_acl_entry(12345, 7)],
        ..RsyncAcl::default()
    };
    for run in ["p5-up", "p5-dl"] {
        let (_app, _opts, lists) = decode_capture_session(run);
        let entries = wire_order_entries(&lists);
        assert_eq!(entries.len(), 3, "{run}");
        assert_eq!(entries[0].path, b"da".as_slice(), "{run}");
        assert_eq!(entries[1].path, b"da/sub".as_slice(), "{run}");
        assert_eq!(entries[2].path, b"da/f".as_slice(), "{run}");
        assert_eq!(
            entries[1].acls.as_ref().expect("-A session").default,
            Some(AclWireEntry::Reference(0)),
            "{run}: da/sub default is a reference to default 0"
        );

        let mut tables = SessionTables::new();
        let resolved: Vec<_> = entries
            .iter()
            .map(|e| tables.resolve_entry(e).expect("{run}: resolve"))
            .collect();
        // two access literals were emitted (da, da/sub) before the
        // default reference: with one shared table, index 0 would resolve
        // to da's EMPTY access ACL instead of da's default
        assert_eq!(
            resolved[0].acl_access,
            Some(RsyncAcl::empty()),
            "{run}: da access"
        );
        assert_eq!(
            resolved[0].acl_default,
            Some(da_default.clone()),
            "{run}: da default"
        );
        assert_eq!(
            resolved[1].acl_access,
            Some(sub_access.clone()),
            "{run}: da/sub access"
        );
        assert_eq!(
            resolved[1].acl_default,
            Some(da_default.clone()),
            "{run}: da/sub default 0 in its own space"
        );
        assert_eq!(
            resolved[2].acl_access,
            Some(f_access.clone()),
            "{run}: da/f access"
        );
        assert_eq!(resolved[2].acl_default, None, "{run}: da/f has no default");
    }
}

/// The strong oracle: for every p3/p4/p5 capture, resolve each entry with
/// one receiving `SessionTables`, rebuild the wire entries from the
/// resolved metadata with `intern_acl`/`intern_xattrs` on a FRESH sending
/// `SessionTables`, and re-encode: the bytes must equal the capture. This
/// proves wire order, the two ACL index spaces and first-equal interning
/// together.
#[test]
fn b2_session_tables_reencode_every_capture_byte_for_byte() {
    for run in ["p3-dl", "p3-up", "p4-dl", "p4-up", "p5-dl", "p5-up"] {
        let (app, opts, lists) = decode_capture_session(run);
        let mut receive = SessionTables::new();
        let mut send = SessionTables::new();
        let mut state = FileListCodecState::new();
        for (i, list) in lists.iter().enumerate() {
            let mut bytes = Vec::new();
            for entry in &list.entries {
                let resolved = receive.resolve_entry(entry).expect("{run}: resolve");
                let mut rebuilt = entry.clone();
                if entry.acls.is_some() {
                    let access = send
                        .intern_acl(
                            AclTable::Access,
                            resolved.acl_access.clone().expect("access"),
                        )
                        .expect("{run}: intern access");
                    let default = resolved.acl_default.clone().map(|d| {
                        send.intern_acl(AclTable::Default, d)
                            .expect("{run}: intern default")
                    });
                    rebuilt.acls = Some(FileListAcls { access, default });
                }
                if entry.xattrs.is_some() {
                    rebuilt.xattrs = Some(
                        send.intern_xattrs(resolved.xattrs.clone().expect("xattrs"))
                            .expect("{run}: intern xattrs"),
                    );
                }
                bytes.extend_from_slice(&encode_file_list_entry(&rebuilt, &opts, &mut state));
            }
            bytes.extend_from_slice(&encode_file_list_terminator(&opts, list.io_error));
            assert_eq!(
                bytes,
                app[list.start..list.end],
                "{run}: list {i} rebuilt from resolved metadata must re-encode byte for byte"
            );
        }
    }
}

/// A reference past the end of the access ACL table, empty and non-empty.
#[test]
fn b2_acl_access_reference_out_of_range() {
    let mut tables = SessionTables::new();
    let mut entry = regular_entry(FILE_MODE, MTIME, XMIT_EXTENDED_FLAGS);
    entry.acls = Some(FileListAcls {
        access: AclWireEntry::Reference(0),
        default: None,
    });
    assert_eq!(
        tables.resolve_entry(&entry),
        Err(SessionTableError::ReferenceOutOfRange {
            table: SessionTable::AclAccess,
            index: 0,
            len: 0,
        })
    );
    // non-empty table: one literal (index 0), then Reference(1) is one
    // past the end; a "return the last element" bug would resolve it
    let mut tables = SessionTables::new();
    let mut literal_entry = regular_entry(FILE_MODE, MTIME, XMIT_EXTENDED_FLAGS);
    literal_entry.acls = Some(FileListAcls {
        access: AclWireEntry::Literal(RsyncAcl::empty()),
        default: None,
    });
    tables
        .resolve_entry(&literal_entry)
        .expect("the literal fills the table");
    let mut entry = regular_entry(FILE_MODE, MTIME, XMIT_EXTENDED_FLAGS);
    entry.acls = Some(FileListAcls {
        access: AclWireEntry::Reference(1),
        default: None,
    });
    assert_eq!(
        tables.resolve_entry(&entry),
        Err(SessionTableError::ReferenceOutOfRange {
            table: SessionTable::AclAccess,
            index: 1,
            len: 1,
        })
    );
}

/// A reference past the end of the DEFAULT ACL table, with the access
/// table non-empty: the two spaces are separate.
#[test]
fn b2_acl_default_reference_out_of_range_in_its_own_space() {
    let mut tables = SessionTables::new();
    let mut entry = regular_entry(FILE_MODE, MTIME, XMIT_EXTENDED_FLAGS);
    entry.acls = Some(FileListAcls {
        access: AclWireEntry::Literal(RsyncAcl::empty()),
        default: Some(AclWireEntry::Reference(0)),
    });
    assert_eq!(
        tables.resolve_entry(&entry),
        Err(SessionTableError::ReferenceOutOfRange {
            table: SessionTable::AclDefault,
            index: 0,
            len: 0,
        })
    );
    // non-empty default table: access literal + default literal, then an
    // entry with access Reference(0) (valid) and default Reference(1)
    let mut tables = SessionTables::new();
    let mut both_literal = regular_entry(FILE_MODE, MTIME, XMIT_EXTENDED_FLAGS);
    both_literal.acls = Some(FileListAcls {
        access: AclWireEntry::Literal(RsyncAcl::empty()),
        default: Some(AclWireEntry::Literal(RsyncAcl::empty())),
    });
    tables
        .resolve_entry(&both_literal)
        .expect("both literals fill their tables");
    let mut entry = regular_entry(FILE_MODE, MTIME, XMIT_EXTENDED_FLAGS);
    entry.acls = Some(FileListAcls {
        access: AclWireEntry::Reference(0),
        default: Some(AclWireEntry::Reference(1)),
    });
    assert_eq!(
        tables.resolve_entry(&entry),
        Err(SessionTableError::ReferenceOutOfRange {
            table: SessionTable::AclDefault,
            index: 1,
            len: 1,
        })
    );
}

/// A reference past the end of the xattr set table, empty and non-empty.
#[test]
fn b2_xattr_reference_out_of_range() {
    let mut tables = SessionTables::new();
    let mut entry = regular_entry(FILE_MODE, MTIME, XMIT_EXTENDED_FLAGS);
    entry.xattrs = Some(XattrWireEntry::Reference(5));
    assert_eq!(
        tables.resolve_entry(&entry),
        Err(SessionTableError::ReferenceOutOfRange {
            table: SessionTable::XattrSet,
            index: 5,
            len: 0,
        })
    );
    // non-empty table: one literal set (index 0), then Reference(1) is one
    // past the end; a "return the last element" bug would resolve it
    let mut tables = SessionTables::new();
    let mut literal_entry = regular_entry(FILE_MODE, MTIME, XMIT_EXTENDED_FLAGS);
    literal_entry.xattrs = Some(XattrWireEntry::Literal(vec![XattrPair::inline(
        "user.color",
        "red".as_bytes(),
    )]));
    tables
        .resolve_entry(&literal_entry)
        .expect("the literal fills the table");
    let mut entry = regular_entry(FILE_MODE, MTIME, XMIT_EXTENDED_FLAGS);
    entry.xattrs = Some(XattrWireEntry::Reference(1));
    assert_eq!(
        tables.resolve_entry(&entry),
        Err(SessionTableError::ReferenceOutOfRange {
            table: SessionTable::XattrSet,
            index: 1,
            len: 1,
        })
    );
}

/// The ACL table refuses the literal past its cap (`TableFull`).
#[test]
fn b2_acl_table_full_at_cap() {
    let mut tables = SessionTables::new();
    let mut entry = regular_entry(FILE_MODE, MTIME, XMIT_EXTENDED_FLAGS);
    entry.acls = Some(FileListAcls {
        access: AclWireEntry::Literal(RsyncAcl::empty()),
        default: None,
    });
    for _ in 0..MAX_SESSION_ACLS {
        tables.resolve_entry(&entry).expect("under the cap");
    }
    assert_eq!(
        tables.resolve_entry(&entry),
        Err(SessionTableError::TableFull {
            table: SessionTable::AclAccess,
            cap: MAX_SESSION_ACLS,
        })
    );
}

/// The xattr table refuses the set past its cap (`TableFull`).
#[test]
fn b2_xattr_table_full_at_cap() {
    let mut tables = SessionTables::new();
    let mut entry = regular_entry(FILE_MODE, MTIME, XMIT_EXTENDED_FLAGS);
    entry.xattrs = Some(XattrWireEntry::Literal(vec![XattrPair::inline(
        "user.color",
        "red".as_bytes(),
    )]));
    for _ in 0..MAX_SESSION_XATTR_SETS {
        tables.resolve_entry(&entry).expect("under the cap");
    }
    assert_eq!(
        tables.resolve_entry(&entry),
        Err(SessionTableError::TableFull {
            table: SessionTable::XattrSet,
            cap: MAX_SESSION_XATTR_SETS,
        })
    );
}

/// The id-name map refuses a new key past its cap (`TableFull`); an
/// existing id is overwritten without counting against the cap.
#[test]
fn b2_id_names_table_full_at_cap() {
    let mut tables = SessionTables::new();
    let big = IdNameList {
        names: (1..=MAX_SESSION_ID_NAMES as i64)
            .map(|id| (id, format!("u{id}")))
            .collect(),
        id0_name: None,
    };
    tables
        .record_id_names(&big, &IdNameList::default())
        .expect("at the cap");
    assert_eq!(tables.uid_name(MAX_SESSION_ID_NAMES as i64), Some("u50000"));
    let one_more = IdNameList {
        names: vec![(999_999, "one-more".to_string())],
        id0_name: None,
    };
    assert_eq!(
        tables.record_id_names(&one_more, &IdNameList::default()),
        Err(SessionTableError::TableFull {
            table: SessionTable::IdNames,
            cap: MAX_SESSION_ID_NAMES,
        })
    );
    // re-recording an existing id is an overwrite, not a new key
    let again = IdNameList {
        names: vec![(1, "renamed".to_string())],
        id0_name: None,
    };
    tables
        .record_id_names(&again, &IdNameList::default())
        .expect("overwrite under the cap");
    assert_eq!(tables.uid_name(1), Some("renamed"));
}

// ---------------------------------------------------------------------------
// B3.1: tree list reception
// ---------------------------------------------------------------------------

mod b3_1 {
    use super::*;
    use crate::aerorsync::events::CollectingSink;
    use crate::aerorsync::mock::{MockRemoteShellTransport, MockTransportConfig};
    use crate::aerorsync::native_driver::AerorsyncDriver;
    use crate::aerorsync::real_wire::{
        encode_ndx, encode_server_preamble, MuxHeader, MuxTag, ServerPreamble, XattrDatum,
    };
    use crate::aerorsync::remote_command::RemoteCommandSpec;
    use crate::aerorsync::transport::CancelHandle;
    use crate::aerorsync::tree_driver::{
        classify_tree_ndx, DirSegment, TreeListLimits, TreeListState, TreeNdx,
        DEFAULT_MAX_TREE_ENTRIES,
    };
    use crate::aerorsync::types::{AerorsyncErrorKind, SessionRole};

    pub(super) fn mux_frame(payload: &[u8]) -> Vec<u8> {
        let header = MuxHeader {
            tag: MuxTag::Data,
            length: payload.len() as u32,
        };
        let mut out = Vec::with_capacity(4 + payload.len());
        out.extend_from_slice(&header.encode());
        out.extend_from_slice(payload);
        out
    }

    pub(super) fn sample_entry(path: &str) -> FileListEntry {
        const XMIT_SAME_UID: u32 = 0x0008;
        const XMIT_SAME_GID: u32 = 0x0010;
        const XMIT_LONG_NAME: u32 = 0x0040;
        const XMIT_SAME_TIME: u32 = 0x0080;
        FileListEntry {
            flags: XMIT_LONG_NAME | XMIT_SAME_UID | XMIT_SAME_GID | XMIT_SAME_TIME,
            path: path.as_bytes().to_vec(),
            size: 1,
            mtime: 0,
            mtime_nsec: None,
            mode: FILE_MODE,
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

    /// A driver on a mock whose inbound is the capture's own server
    /// preamble followed by the sender's app stream, re-muxed into
    /// `MSG_DATA` frames of `chunk` bytes (arbitrary boundaries, as on a
    /// real socket).
    pub(super) async fn driver_for_capture(
        run: &str,
        chunk: usize,
    ) -> AerorsyncDriver<MockRemoteShellTransport> {
        let app = sender_app_stream(run);
        let (compat, _) = server_compat(run);
        let mut inbound = encode_server_preamble(&ServerPreamble {
            protocol_version: 31,
            compat_flags: compat as i32,
            checksum_algos: "xxh128".to_string(),
            compression_algos: "none".to_string(),
            checksum_seed: 0,
            consumed: 0,
        });
        for piece in app.chunks(chunk) {
            inbound.extend_from_slice(&mux_frame(piece));
        }
        let cfg = MockTransportConfig::healthy_download().with_raw_inbound(inbound);
        let mut d = AerorsyncDriver::new(MockRemoteShellTransport::new(cfg), CancelHandle::inert());
        d.session_role = Some(SessionRole::Receiver);
        let (opts, _) = options_from_capture(run);
        d.open_raw_stream_internal(&RemoteCommandSpec::capture_download("/remote/target"))
            .await
            .unwrap();
        d.perform_preamble_exchange(31, "xxh128", "none")
            .await
            .unwrap();
        // align the file-list checksum flag with what the capture's argv carried
        d.file_list_checksum_enabled = opts.always_checksum;
        d
    }

    /// A driver on a hand-built inbound stream (`preamble + frames`).
    pub(super) async fn hand_driver(
        frames: Vec<u8>,
        compat_flags: i32,
        preserve_xattrs: bool,
    ) -> AerorsyncDriver<MockRemoteShellTransport> {
        let mut inbound = encode_server_preamble(&ServerPreamble {
            protocol_version: 31,
            compat_flags,
            checksum_algos: "xxh128".to_string(),
            compression_algos: "none".to_string(),
            checksum_seed: 0,
            consumed: 0,
        });
        inbound.extend_from_slice(&frames);
        let cfg = MockTransportConfig::healthy_download().with_raw_inbound(inbound);
        let mut d = AerorsyncDriver::new(MockRemoteShellTransport::new(cfg), CancelHandle::inert());
        d.session_role = Some(SessionRole::Receiver);
        d.open_raw_stream_internal(&RemoteCommandSpec::capture_download("/remote/target"))
            .await
            .unwrap();
        d.perform_preamble_exchange(31, "xxh128", "none")
            .await
            .unwrap();
        d.file_list_checksum_enabled = false;
        d.effective_metadata.preserve_xattrs = preserve_xattrs;
        d
    }

    /// Read the next ndx from the state's buffered bytes, pulling frames
    /// only when the buffer runs dry. This is the B3.2 loop's read step.
    pub(super) async fn read_next_ndx(
        d: &mut AerorsyncDriver<MockRemoteShellTransport>,
        state: &mut TreeListState,
        sink: &mut CollectingSink,
    ) -> i32 {
        loop {
            if !state.buffered.is_empty() {
                match decode_ndx(&state.buffered, &mut d.inbound_ndx_state) {
                    Ok((n, consumed)) => {
                        state.buffered.drain(..consumed);
                        return n;
                    }
                    Err(RealWireError::NdxTruncated { .. }) => {}
                    Err(e) => panic!("ndx decode: {e}"),
                }
            }
            let payload = d.next_data_frame(sink).await.expect("a data frame");
            state.buffered.extend_from_slice(&payload);
        }
    }

    /// The test-side B3.2 loop: read ndx, decode announced segments, stop
    /// at the first item ndx or at NDX_FLIST_EOF.
    pub(super) async fn drive_segments(
        d: &mut AerorsyncDriver<MockRemoteShellTransport>,
        mut state: TreeListState,
        limits: TreeListLimits,
        sink: &mut CollectingSink,
    ) -> (Vec<DirSegment>, Option<i32>, bool) {
        let mut segments = Vec::new();
        let mut first_item = None;
        let mut saw_eof = false;
        loop {
            let ndx = read_next_ndx(d, &mut state, sink).await;
            match classify_tree_ndx(ndx) {
                TreeNdx::Segment { .. } => {
                    let seg = d
                        .receive_tree_segment(ndx, &mut state, limits, sink)
                        .await
                        .expect("segment decodes");
                    segments.push(seg);
                }
                TreeNdx::Item(n) => {
                    first_item = Some(n);
                    break;
                }
                TreeNdx::FlistEof => {
                    saw_eof = true;
                    break;
                }
                other => panic!("unexpected ndx case {other:?}"),
            }
        }
        (segments, first_item, saw_eof)
    }

    /// 1. p9-dl-inc: every segment of the capture, with ndx_start, dir_ndx,
    /// the A4 gaps, per-segment counts and the resolved wire names, all
    /// from `capture/decode_rsync_wire.py`, not from the code under test.
    #[tokio::test]
    async fn b3_1_p9_dl_inc_list_matches_the_capture() {
        let mut d = driver_for_capture("p9-dl-inc", 7).await;
        let mut sink = CollectingSink::default();
        let limits = TreeListLimits::default();
        let start = d
            .receive_initial_tree_list(limits, &mut sink)
            .await
            .expect("the initial list decodes");
        assert_eq!(start.initial.entries.len(), 1);
        assert_eq!(start.initial.ndx_start, 1);
        assert_eq!(start.initial.dir_ndx, None);
        let (extra, first_item, saw_eof) =
            drive_segments(&mut d, start.state, limits, &mut sink).await;
        let mut segments = vec![start.initial];
        segments.extend(extra);
        let starts: Vec<i32> = segments.iter().map(|s| s.ndx_start).collect();
        assert_eq!(starts, vec![1, 3, 8, 11, 14, 16]);
        let dirs: Vec<Option<i32>> = segments.iter().map(|s| s.dir_ndx).collect();
        assert_eq!(
            dirs,
            vec![None, Some(0), Some(1), Some(2), Some(4), Some(3)]
        );
        let gaps: Vec<i32> = segments.iter().map(|s| s.gap_ndx).collect();
        assert_eq!(
            gaps,
            vec![0, 2, 7, 10, 13, 15],
            "A4: ndx_start - 1 is the numbering gap"
        );
        let counts: Vec<usize> = segments.iter().map(|s| s.entries.len()).collect();
        assert_eq!(counts, vec![1, 4, 2, 2, 1, 0]);
        // entries are stored in SORTED order (NAS 3282, Q-a): ndx stays
        // contiguous inside each segment, now as the sorted ndx
        for seg in &segments {
            for (i, e) in seg.entries.iter().enumerate() {
                assert_eq!(e.ndx, seg.ndx_start + i as i32);
            }
        }
        let names: Vec<&[u8]> = segments
            .iter()
            .flat_map(|s| s.entries.iter().map(|e| e.entry.path.as_slice()))
            .collect();
        // sorted order per segment (was wire order before NAS 3282):
        // non-directories first, then directories, names byte for byte
        assert_eq!(
            names,
            vec![
                &b"r"[..],
                b"r/top.txt",
                b"r/d1",
                b"r/d2",
                b"r/d3",
                b"r/d1/f1",
                b"r/d1/f2",
                b"r/d2/f3",
                b"r/d2/sub",
                b"r/d2/sub/f4",
            ]
        );
        // the wire positions survive the sort: the capture's wire order of
        // segment dir_ndx=0 is top.txt, d1, d3, d2 (d3 before d2)
        let seg0 = &segments[1];
        let by_name = |n: &str| {
            seg0.entries
                .iter()
                .find(|e| e.entry.path == n.as_bytes())
                .unwrap()
        };
        assert_eq!(by_name("r/top.txt").wire_pos, 0);
        assert_eq!(by_name("r/d1").wire_pos, 1);
        assert_eq!(by_name("r/d3").wire_pos, 2);
        assert_eq!(by_name("r/d2").wire_pos, 3);
        // sorted ndx (old wire ndx in parentheses): d2 5 (6), d3 6 (5)
        assert_eq!(by_name("r/d2").ndx, 5);
        assert_eq!(by_name("r/d3").ndx, 6);
        // Q-a witness: r/d2/f3 must come out 11 (wire ndx 12)
        let seg2 = &segments[3];
        let f3 = seg2
            .entries
            .iter()
            .find(|e| e.entry.path == b"r/d2/f3")
            .unwrap();
        assert_eq!(f3.ndx, 11);
        assert_eq!(f3.wire_pos, 1);
        assert!(saw_eof, "p9-dl-inc ends the list with NDX_FLIST_EOF");
        assert_eq!(first_item, None);
    }

    /// 2. p9-dl-noinc: one list starting at ndx 0, then the uid/gid id
    /// lists with the CF_ID0_NAMES name of id 0.
    #[tokio::test]
    async fn b3_1_p9_dl_noinc_single_list_and_id_lists() {
        let mut d = driver_for_capture("p9-dl-noinc", 5).await;
        let mut sink = CollectingSink::default();
        let start = d
            .receive_initial_tree_list(TreeListLimits::default(), &mut sink)
            .await
            .expect("the single list decodes");
        assert_eq!(start.initial.ndx_start, 0);
        assert_eq!(start.initial.entries.len(), 10);
        assert_eq!(start.uid_names.id0_name.as_deref(), Some("root"));
        assert_eq!(start.gid_names.id0_name.as_deref(), Some("root"));
        let (extra, first_item, saw_eof) =
            drive_segments(&mut d, start.state, TreeListLimits::default(), &mut sink).await;
        assert!(
            extra.is_empty(),
            "no segment follows a non-incremental list"
        );
        assert!(
            first_item.is_some(),
            "the first item ndx follows the id lists"
        );
        assert!(!saw_eof);
    }

    /// 3. p8-dl: 50.000 entries (2.546.687 bytes) in ONE segment, received
    /// without any whole-list byte ceiling.
    #[tokio::test]
    async fn b3_1_p8_dl_fifty_thousand_entries_one_segment() {
        let mut d = driver_for_capture("p8-dl", 65_536).await;
        let mut sink = CollectingSink::default();
        let limits = TreeListLimits::default();
        let gate: usize = 50_001;
        assert!(
            DEFAULT_MAX_TREE_ENTRIES > gate,
            "the default ceiling must stay above the p8-dl gate ({gate} entries)"
        );
        let start = d
            .receive_initial_tree_list(limits, &mut sink)
            .await
            .expect("the initial list decodes");
        assert_eq!(start.initial.entries.len(), 1);
        let (extra, _, _) = drive_segments(&mut d, start.state, limits, &mut sink).await;
        assert_eq!(extra.len(), 1);
        assert_eq!(extra[0].entries.len(), 50_000);
        assert_eq!(extra[0].ndx_start, 3);
        assert_eq!(extra[0].dir_ndx, Some(0));
    }

    /// 4. p9big-dl-inc: the initial list plus the 18 lookahead segments
    /// arrive before the first item; the loop stops at its ndx.
    #[tokio::test]
    async fn b3_1_p9big_dl_inc_segments_before_the_first_item() {
        let mut d = driver_for_capture("p9big-dl-inc", 4_096).await;
        let mut sink = CollectingSink::default();
        let limits = TreeListLimits {
            max_entries: 100_000,
            ..Default::default()
        };
        let start = d
            .receive_initial_tree_list(limits, &mut sink)
            .await
            .expect("the initial list decodes");
        let mut state = start.state;
        let mut dirs: Vec<i32> = Vec::new();
        let mut segments: Vec<DirSegment> = Vec::new();
        let first_item = loop {
            let ndx = read_next_ndx(&mut d, &mut state, &mut sink).await;
            match classify_tree_ndx(ndx) {
                TreeNdx::Segment { .. } => {
                    let seg = d
                        .receive_tree_segment(ndx, &mut state, limits, &mut sink)
                        .await
                        .expect("a lookahead segment decodes");
                    dirs.push(seg.dir_ndx.expect("announced segment"));
                    segments.push(seg);
                }
                TreeNdx::Item(n) => break Some(n),
                TreeNdx::FlistEof => break None,
                other => panic!("unexpected ndx case {other:?}"),
            }
        };
        // decoder (decode_rsync_wire.py su b0c0/p9big-dl-inc, estratto NAS
        // 3112/3113): a 0x254c arriva ndx 2 (>= 0) dopo 18 segmenti extra
        // (dir_ndx 0..=17); il marker dir 18 (-119) e' a 0x2550, DOPO il
        // primo item. Quindi: initial + 18 extra = 19 segmenti, item 2.
        println!("segment dir_ndx sequence: {dirs:?}");
        assert_eq!(dirs, (0..=17).collect::<Vec<i32>>());
        assert_eq!(
            dirs.len() + 1,
            19,
            "the initial list plus 18 lookahead segments"
        );
        assert_eq!(
            first_item,
            Some(2),
            "the first ndx >= 0 after the lookahead (decoder riga 0x254c)"
        );
        // NAS 3113: il primo item e' proprio il buco del primo segmento
        // extra (ndx_start 3, gap 2): il sender usa il buco come item
        // ndx della directory del segmento.
        assert_eq!(segments[0].ndx_start, 3);
        assert_eq!(first_item, Some(segments[0].gap_ndx));
    }

    /// Fix 1 witness: the same non-incremental list decodes identically
    /// however the stream is chopped into mux frames.
    #[tokio::test]
    async fn b3_1_id_lists_decode_at_every_chunk_boundary() {
        for chunk in [1usize, 2, 3, 5, 7, 13, 64] {
            let mut d = driver_for_capture("p9-dl-noinc", chunk).await;
            let mut sink = CollectingSink::default();
            let start = d
                .receive_initial_tree_list(TreeListLimits::default(), &mut sink)
                .await
                .unwrap_or_else(|e| panic!("chunk {chunk}: {e}"));
            assert_eq!(start.initial.entries.len(), 10, "chunk {chunk}");
            assert_eq!(
                start.uid_names.id0_name.as_deref(),
                Some("root"),
                "chunk {chunk}"
            );
            assert_eq!(
                start.gid_names.id0_name.as_deref(),
                Some("root"),
                "chunk {chunk}"
            );
        }
    }

    /// 5a. The session entry ceiling is a typed, final error (InvalidFrame,
    /// never a retry): p8-dl carries 50.001 entries against the default
    /// ceiling of 50.000, and the big segment trips it.
    #[tokio::test]
    async fn b3_1_max_entries_trips_as_a_typed_error() {
        let mut d = driver_for_capture("p8-dl", 65_536).await;
        let mut sink = CollectingSink::default();
        let limits = TreeListLimits {
            max_entries: 1_000,
            ..Default::default()
        };
        let start = d
            .receive_initial_tree_list(limits, &mut sink)
            .await
            .expect("the small initial list is under the ceiling");
        let mut state = start.state;
        let ndx = read_next_ndx(&mut d, &mut state, &mut sink).await;
        assert!(matches!(classify_tree_ndx(ndx), TreeNdx::Segment { .. }));
        let err = d
            .receive_tree_segment(ndx, &mut state, limits, &mut sink)
            .await
            .expect_err("the 50.000-entry segment exceeds the explicit 1.000 ceiling");
        assert_eq!(err.kind, AerorsyncErrorKind::InvalidFrame);
        assert!(
            err.detail.contains("session limit exceeded: 1000"),
            "unexpected detail: {}",
            err.detail
        );
    }

    /// 5b. A single entry above max_entry_bytes fails in the decoder.
    #[tokio::test]
    async fn b3_1_entry_over_max_entry_bytes_fails_in_the_decoder() {
        let mut d = driver_for_capture("p9-dl-inc", 7).await;
        let mut sink = CollectingSink::default();
        let limits = TreeListLimits {
            max_entry_bytes: 8,
            ..Default::default()
        };
        let err = d
            .receive_initial_tree_list(limits, &mut sink)
            .await
            .expect_err("the first capture entry is far above 8 bytes");
        assert_eq!(err.kind, AerorsyncErrorKind::InvalidFrame);
        assert!(
            err.detail.contains("above the per-entry limit 8"),
            "unexpected detail: {}",
            err.detail
        );
    }

    /// 5c. An ACL/xattr back-reference past the table end, arriving in a
    /// segment AFTER the first, fails with ReferenceOutOfRange: the session
    /// tables really live across segments.
    #[tokio::test]
    async fn b3_1_reference_out_of_range_in_a_later_segment() {
        let (mut o, _) = options_from_capture("p9-dl-inc");
        o.preserve_xattrs = true;
        let mut e1 = sample_entry("a");
        e1.xattrs = Some(XattrWireEntry::Literal(vec![XattrPair {
            name: "user.a".to_string(),
            datum: XattrDatum::Inline(b"v".to_vec()),
        }]));
        let mut e2 = sample_entry("b");
        e2.xattrs = Some(XattrWireEntry::Reference(9));
        let mut codec = FileListCodecState::new();
        let mut payload = encode_file_list_entry(&e1, &o, &mut codec);
        payload.extend_from_slice(&encode_file_list_terminator(&o, 0));
        payload.extend_from_slice(&encode_ndx(NDX_FLIST_OFFSET, &mut NdxState::new()));
        payload.extend_from_slice(&encode_file_list_entry(&e2, &o, &mut codec));
        payload.extend_from_slice(&encode_file_list_terminator(&o, 0));
        let mut d = hand_driver(mux_frame(&payload), 0x80 | 1, true).await;
        let mut sink = CollectingSink::default();
        let limits = TreeListLimits::default();
        let start = d
            .receive_initial_tree_list(limits, &mut sink)
            .await
            .expect("the first segment decodes and interns xattr set 0");
        assert_eq!(start.initial.entries.len(), 1);
        let mut state = start.state;
        let ndx = read_next_ndx(&mut d, &mut state, &mut sink).await;
        assert!(matches!(classify_tree_ndx(ndx), TreeNdx::Segment { .. }));
        let err = d
            .receive_tree_segment(ndx, &mut state, limits, &mut sink)
            .await
            .expect_err("reference 9 is past the one-element xattr table");
        assert_eq!(err.kind, AerorsyncErrorKind::InvalidFrame);
        assert!(
            err.detail.contains("out of range"),
            "unexpected detail: {}",
            err.detail
        );
    }

    /// 6. Neutralita': the single-file frozen path is untouched: the same
    /// capture the B1 oracles pin still decodes to the same entries
    /// through the tree receiver, byte-identical names included (in the
    /// sorted order of NAS 3282 since then; the wire order is on
    /// `wire_pos`).
    #[tokio::test]
    async fn b3_1_single_file_capture_still_decodes_through_the_tree_path() {
        let mut d = driver_for_capture("p9-dl-noinc", 3).await;
        let mut sink = CollectingSink::default();
        let start = d
            .receive_initial_tree_list(TreeListLimits::default(), &mut sink)
            .await
            .expect("decodes");
        let app = sender_app_stream("p9-dl-noinc");
        let (opts, _) = options_from_capture("p9-dl-noinc");
        let reference = decode_list(&app, 0, &opts, &mut FileListCodecState::new());
        let got: Vec<&[u8]> = start
            .initial
            .entries
            .iter()
            .map(|e| e.entry.path.as_slice())
            .collect();
        // sorted order (was the wire order r, top.txt, d1, d3, d2, f1, f2,
        // sub, f3, f4 before NAS 3282): non-dirs first, then dirs, by name
        let want: Vec<&[u8]> = vec![
            &b"r"[..],
            b"r/top.txt",
            b"r/d1",
            b"r/d1/f1",
            b"r/d1/f2",
            b"r/d2",
            b"r/d2/f3",
            b"r/d2/sub",
            b"r/d2/sub/f4",
            b"r/d3",
        ];
        assert_eq!(got, want);
        assert_eq!(start.initial.entries.len(), reference.entries.len());
        // every entry still decodes byte-identical, only the storage order
        // changed: the wire positions reproduce the reference wire order
        let mut wire_pos: Vec<usize> = start.initial.entries.iter().map(|e| e.wire_pos).collect();
        wire_pos.sort_unstable();
        assert_eq!(wire_pos, (0..reference.entries.len()).collect::<Vec<_>>());
    }
}

// ---------------------------------------------------------------------------
// B3.1 (passo 4): session-wide references and the session entry ceiling
// ---------------------------------------------------------------------------

mod b3_1_coverage {
    use super::b3_1::{hand_driver, mux_frame, read_next_ndx, sample_entry};
    use super::*;
    use crate::aerorsync::events::CollectingSink;
    use crate::aerorsync::real_wire::{encode_ndx, XattrDatum};
    use crate::aerorsync::tree_driver::TreeListLimits;
    use crate::aerorsync::types::AerorsyncErrorKind;

    /// A back-reference in a LATER segment resolves against the one
    /// session table: Reference(0) in the second segment must decode Ok
    /// with the exact value interned by the first segment. Red with a
    /// per-segment SessionTables (B3M03).
    #[tokio::test]
    async fn b3_1_reference_resolves_across_segments() {
        let (mut o, _) = options_from_capture("p9-dl-inc");
        o.preserve_xattrs = true;
        let mut e1 = sample_entry("a");
        e1.xattrs = Some(XattrWireEntry::Literal(vec![XattrPair {
            name: "user.a".to_string(),
            datum: XattrDatum::Inline(b"v".to_vec()),
        }]));
        let mut e2 = sample_entry("b");
        e2.xattrs = Some(XattrWireEntry::Reference(0));
        let mut codec = FileListCodecState::new();
        let mut payload = encode_file_list_entry(&e1, &o, &mut codec);
        payload.extend_from_slice(&encode_file_list_terminator(&o, 0));
        payload.extend_from_slice(&encode_ndx(NDX_FLIST_OFFSET, &mut NdxState::new()));
        payload.extend_from_slice(&encode_file_list_entry(&e2, &o, &mut codec));
        payload.extend_from_slice(&encode_file_list_terminator(&o, 0));
        let mut d = hand_driver(mux_frame(&payload), 0x80 | 1, true).await;
        let mut sink = CollectingSink::default();
        let limits = TreeListLimits::default();
        let start = d
            .receive_initial_tree_list(limits, &mut sink)
            .await
            .expect("the first segment decodes");
        assert_eq!(start.initial.entries.len(), 1);
        let mut state = start.state;
        let ndx = read_next_ndx(&mut d, &mut state, &mut sink).await;
        let seg = d
            .receive_tree_segment(ndx, &mut state, limits, &mut sink)
            .await
            .expect("Reference(0) resolves from the session table, not a per-segment one");
        assert_eq!(seg.entries.len(), 1);
        let resolved = seg.entries[0]
            .resolved
            .xattrs
            .as_ref()
            .expect("resolved xattrs");
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].name, "user.a");
        assert_eq!(resolved[0].datum, XattrDatum::Inline(b"v".to_vec()));
    }

    /// The session entry ceiling counts ACROSS segments: with
    /// max_entries = 3 and two entries per segment, each segment stays
    /// under the decoder's own ceiling (2 <= 3) and only the session
    /// total (4 > 3) trips. Red with the session check removed (B3M07).
    /// The capture-based 5a does not cover this: at max_entries = 1_000
    /// the segment decoder's own FileListTooManyEntries fires first.
    #[tokio::test]
    async fn b3_1_session_ceiling_counts_across_segments() {
        let (o, _) = options_from_capture("p9-dl-inc");
        let mut codec = FileListCodecState::new();
        let mut payload = Vec::new();
        for name in ["a", "b"] {
            payload.extend_from_slice(&encode_file_list_entry(&sample_entry(name), &o, &mut codec));
        }
        payload.extend_from_slice(&encode_file_list_terminator(&o, 0));
        payload.extend_from_slice(&encode_ndx(NDX_FLIST_OFFSET, &mut NdxState::new()));
        for name in ["c", "d"] {
            payload.extend_from_slice(&encode_file_list_entry(&sample_entry(name), &o, &mut codec));
        }
        payload.extend_from_slice(&encode_file_list_terminator(&o, 0));
        let mut d = hand_driver(mux_frame(&payload), 0x80 | 1, false).await;
        let mut sink = CollectingSink::default();
        let limits = TreeListLimits {
            max_entries: 3,
            ..Default::default()
        };
        let start = d
            .receive_initial_tree_list(limits, &mut sink)
            .await
            .expect("the first segment (2 entries) is under the ceiling");
        assert_eq!(start.initial.entries.len(), 2);
        let mut state = start.state;
        let ndx = read_next_ndx(&mut d, &mut state, &mut sink).await;
        let err = d
            .receive_tree_segment(ndx, &mut state, limits, &mut sink)
            .await
            .expect_err("the session total 4 exceeds the ceiling 3");
        assert_eq!(err.kind, AerorsyncErrorKind::InvalidFrame);
        assert!(
            err.detail.contains("session limit exceeded: 3"),
            "unexpected detail: {}",
            err.detail
        );
    }
}

// ---------------------------------------------------------------------------
// B3.2 reactive replay (verdetto NAS 3179): the mock releases the capture's
// sender events only when the generator's own outbound earns them.
// ---------------------------------------------------------------------------

mod b3_2_reactive {
    use super::*;
    use crate::aerorsync::engine_adapter::CurrentDeltaSyncBridge;
    use crate::aerorsync::events::CollectingSink;
    use crate::aerorsync::native_driver::AerorsyncDriver;
    use crate::aerorsync::real_wire::{
        decode_item_flags, decode_sum_head, encode_server_preamble, MuxDemuxer, MuxHeader, MuxTag,
        ServerPreamble, NDX_DEL_STATS, NDX_DONE,
    };
    use crate::aerorsync::remote_command::RemoteCommandSpec;
    use crate::aerorsync::transport::{
        BidirectionalByteStream, CancelHandle, RawByteStream, RawRemoteShellTransport,
        RemoteCommandOutput, RemoteExecRequest, RemoteShellTransport, TransportProbe,
    };
    use crate::aerorsync::tree_driver::{TreeDownloadOptions, DEFAULT_MAX_TREE_ENTRIES};
    use crate::aerorsync::types::{AerorsyncError, SessionRole};
    use async_trait::async_trait;
    use std::collections::{HashSet, VecDeque};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    const CF_INC_RECURSE: i64 = 1;
    const CF_ID0_NAMES: i64 = 256;

    /// NAS 3201 point 1: the delta grammar follows the negotiated
    /// compression on both branches (raw int32 LE without `-z`, the shared
    /// single-file decoder with `-z`).
    #[test]
    fn tree_delta_grammar_follows_negotiated_compression() {
        use crate::aerorsync::native_driver::LiteralCompression;
        use crate::aerorsync::tree_driver::{tree_delta_grammar, TreeDeltaGrammar};
        assert_eq!(
            tree_delta_grammar(&LiteralCompression::None),
            TreeDeltaGrammar::Raw
        );
        assert_eq!(
            tree_delta_grammar(&LiteralCompression::Zstd),
            TreeDeltaGrammar::Compressed
        );
        assert_eq!(
            tree_delta_grammar(&LiteralCompression::Deflate),
            TreeDeltaGrammar::Compressed
        );
    }
    /// Stall timeout of the reactive mock: when the generator waits for an
    /// event the capture cannot release, the read fails with this error
    /// instead of hanging the test.
    const STALL: Duration = Duration::from_secs(10);

    #[derive(Debug)]
    enum ReplayEvent {
        /// Initial list (+ id lists when non-incremental): released eagerly.
        List(Vec<u8>),
        /// NDX_FLIST_EOF / NDX_DEL_STATS list-stream markers: eager.
        Marker(Vec<u8>),
        /// One announced file-list segment: released when the driver has
        /// sent at least as many NDX_DONE as echoes precede it in the
        /// capture's sender stream (the lookahead model, NAS 3282 Q2).
        Segment(Vec<u8>),
        /// The sender's answer to the generator's request for ndx: released
        /// only after our outbound carries that request.
        ItemAnswer(i32, Vec<u8>),
        /// One NDX_DONE echo: released after the k-th NDX_DONE we sent, never
        /// at the capture's offset (NAS 3282).
        Echo(Vec<u8>),
        /// SummaryFrame: released after our S dir dones + 4 pre-stats markers.
        Stats(Vec<u8>),
        /// Final NDX_DONE: released after our ACK (the 5th phase marker).
        FinalDone(Vec<u8>),
    }

    /// The client's (generator's) app stream from `capture_in.bin`.
    fn generator_app_stream(run: &str) -> Vec<u8> {
        let raw = capture_bytes(run, "capture_in.bin");
        let (compat, _) = server_compat(run);
        let negotiated = compat & CF_VARINT_FLIST_FLAGS != 0;
        let command = String::from_utf8(capture_bytes(run, "remote_command.txt")).unwrap();
        let compress = command
            .split_whitespace()
            .find(|a| a.starts_with('-') && !a.starts_with("--"))
            .is_some_and(|b| b.split('.').next().unwrap_or("").contains('z'));
        let mut at = 4usize;
        if negotiated {
            for _ in 0..if compress { 2 } else { 1 } {
                at += 1 + raw[at] as usize;
            }
        }
        reassemble_msg_data(&raw[at..])
            .expect("generator stream demultiplexes")
            .app_stream
    }

    /// Parse the generator's requests: ordered ndx list + sum_head count per
    /// data request + total NDX_DONE count.
    fn parse_generator_requests(
        gen_app: &[u8],
    ) -> (Vec<i32>, Vec<i32>, std::collections::HashMap<i32, i32>) {
        let mut pos = 0usize;
        let mut state = NdxState::default();
        let mut requests = Vec::new();
        let mut dones = Vec::new();
        let mut counts = std::collections::HashMap::new();
        while pos < gen_app.len() {
            let (ndx, c) = decode_ndx(&gen_app[pos..], &mut state).expect("gen ndx");
            pos += c;
            if ndx == NDX_DONE {
                dones.push(ndx);
                continue;
            }
            let (iflags, c2) = decode_item_flags(&gen_app[pos..]).expect("gen iflags");
            pos += c2;
            if iflags & 0x0100 != 0 {
                // ITEM_REPORT_XATTR: the generator's request section is a
                // run of skip varints closed by varint 0 (p10-dl), before
                // any sum_head.
                loop {
                    let (skip, c3) = decode_varint(&gen_app[pos..]).expect("gen xattr skip");
                    pos += c3;
                    if skip == 0 {
                        break;
                    }
                }
            }
            if iflags & 0x8000 != 0 {
                let (head, c3) = decode_sum_head(&gen_app[pos..]).expect("gen sum_head");
                pos += c3 + (head.count as usize) * (4 + head.checksum_length as usize);
                counts.insert(ndx, head.count);
            }
            requests.push(ndx);
        }
        (requests, dones, counts)
    }

    /// The client's filter list leads the generator's app stream (one int32
    /// length per rule, closed by a zero length); the request stream starts
    /// right after it.
    fn skip_client_filter_list(app: &[u8]) -> &[u8] {
        let mut pos = 0usize;
        loop {
            if pos + 4 > app.len() {
                return &app[pos..];
            }
            let len = i32::from_le_bytes(app[pos..pos + 4].try_into().unwrap());
            pos += 4;
            if len == 0 {
                return &app[pos..];
            }
            pos += len as usize;
        }
    }

    /// One request of a generator stream: ndx, iflags, and the sum_head
    /// count when ITEM_TRANSFER is set.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct GenRequest {
        ndx: i32,
        iflags: u16,
        sum_count: Option<i32>,
    }

    /// One event of our own outbound app stream.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum OutEvent {
        Req(GenRequest),
        Done,
    }

    /// Parse a request stream (the capture's generator stream after the
    /// filter list, or our own outbound app stream) into ordered events.
    /// A truncated tail (mid-write capture of our outbound) just stops.
    fn parse_events_seq(app: &[u8]) -> Vec<OutEvent> {
        let mut events = Vec::new();
        let mut pos = 0usize;
        let mut state = NdxState::default();
        while pos < app.len() {
            let Ok((ndx, c)) = decode_ndx(&app[pos..], &mut state) else {
                break;
            };
            pos += c;
            if ndx == NDX_DONE {
                events.push(OutEvent::Done);
                continue;
            }
            let Ok((iflags, c2)) = decode_item_flags(&app[pos..]) else {
                break;
            };
            pos += c2;
            if iflags & 0x0100 != 0 {
                loop {
                    let Ok((skip, c3)) = decode_varint(&app[pos..]) else {
                        break;
                    };
                    pos += c3;
                    if skip == 0 {
                        break;
                    }
                }
            }
            let mut sum_count = None;
            if iflags & 0x8000 != 0 {
                match decode_sum_head(&app[pos..]) {
                    Ok((head, c3)) => {
                        pos += c3 + (head.count as usize) * (4 + head.checksum_length as usize);
                        sum_count = Some(head.count);
                    }
                    Err(_) => break,
                }
            }
            events.push(OutEvent::Req(GenRequest {
                ndx,
                iflags,
                sum_count,
            }));
        }
        events
    }

    /// Requests and NDX_DONE count of a request stream, in order.
    fn parse_requests_seq(app: &[u8]) -> (Vec<GenRequest>, usize) {
        let mut reqs = Vec::new();
        let mut dones = 0usize;
        for ev in parse_events_seq(app) {
            match ev {
                OutEvent::Req(r) => reqs.push(r),
                OutEvent::Done => dones += 1,
            }
        }
        (reqs, dones)
    }

    /// Parse our own captured outbound raw stream into ordered events.
    fn parse_outbound_events(raw: &[u8]) -> Vec<OutEvent> {
        if raw.len() <= 4 {
            return Vec::new();
        }
        let Some(at) = app_stream_offset(raw) else {
            return Vec::new();
        };
        // Demux only the complete frames: while the driver is mid-write the
        // outbound ends inside a mux frame, and that partial tail must not
        // hide the requests already sent.
        let mut app = Vec::new();
        for frame in MuxDemuxer::new(&raw[at..]) {
            match frame {
                Ok((header, payload)) => {
                    if header.tag == MuxTag::Data {
                        app.extend_from_slice(payload);
                    }
                }
                Err(_) => break, // truncated trailing frame: ignore it
            }
        }
        parse_events_seq(&app)
    }

    /// Split the sender's app stream into replay events, guided by the
    /// generator's request list (an item answer exists only for a request).
    /// Also returns the session's list ranges as (ndx_start, entry count) in
    /// arrival order, for the NDX_DONE placement checks.
    /// The parsed sender stream: replay events, the total echo count, the
    /// list ranges (ndx_start, entry count) and each list's entries in wire
    /// order.
    struct ParsedSender {
        events: Vec<ReplayEvent>,
        echo_total: usize,
        lists: Vec<(i32, usize)>,
        entries: Vec<Vec<FileListEntry>>,
    }

    fn parse_sender_events(run: &str) -> ParsedSender {
        let app = sender_app_stream(run);
        let (compat, _) = server_compat(run);
        let inc = compat & CF_INC_RECURSE != 0;
        let (opts, _) = options_from_capture(run);
        let gen_app = generator_app_stream(run);
        let (gen_requests, _, _) = parse_generator_requests(&gen_app);
        let mut unanswered: Vec<i32> = gen_requests.clone();
        let mut events: Vec<ReplayEvent> = Vec::new();
        let mut ndx_state = NdxState::default();
        let mut codec = FileListCodecState::new();
        // Initial list (and the id lists of a non-incremental session).
        let mut dec = FileListStreamDecoder::new(opts, codec, DEFAULT_MAX_TREE_ENTRIES);
        dec.feed(&app);
        let mut initial_entries: Vec<FileListEntry> = Vec::new();
        while !dec.is_finished() {
            match dec.next_outcome() {
                Ok(Some(FileListDecodeOutcome::Entry(e))) => initial_entries.push(e),
                Ok(Some(_)) => {}
                Ok(None) => panic!("capture list truncated"),
                Err(e) => panic!("capture list decode: {e}"),
            }
        }
        let ndx_start0 = if inc { 1 } else { 0 };
        let n_entries = initial_entries.len();
        let mut lists: Vec<(i32, usize)> = vec![(ndx_start0, n_entries)];
        let mut all_entries: Vec<Vec<FileListEntry>> = vec![initial_entries];
        let mut next_start = ndx_start0 + n_entries as i32 + 1;
        let consumed = app.len() - dec.buffered();
        let mut list_bytes = app[..consumed].to_vec();
        let mut pos = consumed;
        codec = dec.into_parts().0;
        if !inc {
            // C4: id lists follow a non-incremental list when uid/gid (or
            // ACL) preservation is on and --numeric-ids is off. With
            // CF_ID0_NAMES each list ends with the id-0 name after the
            // terminating zero varint.
            let id0_names = compat & CF_ID0_NAMES != 0;
            let want_uids = opts.preserve_uid || opts.preserve_acls;
            let want_gids = opts.preserve_gid || opts.preserve_acls;
            if want_uids {
                if let Ok((_, c)) = decode_id_list(&app[pos..], id0_names) {
                    list_bytes.extend_from_slice(&app[pos..pos + c]);
                    pos += c;
                }
            }
            if want_gids {
                if let Ok((_, c)) = decode_id_list(&app[pos..], id0_names) {
                    list_bytes.extend_from_slice(&app[pos..pos + c]);
                    pos += c;
                }
            }
        }
        events.push(ReplayEvent::List(list_bytes));

        // Falena's table: a download sender stream carries exactly S+3
        // NDX_DONE echoes (S = extra segments), then the SummaryFrame, then
        // one final NDX_DONE. The trailing echoes and the stats frame's
        // leading 0x00 are byte-identical, so the tail is split by COUNT,
        // never by trying to parse the summary frame mid-run.
        let mut echoes_seen = 0usize;
        let tail = |events: &mut Vec<ReplayEvent>, pos: usize, app: &[u8]| {
            assert!(
                app.len() >= 1 && app[app.len() - 1] == 0x00,
                "capture ends with NDX_DONE"
            );
            events.push(ReplayEvent::Stats(app[pos..app.len() - 1].to_vec()));
            events.push(ReplayEvent::FinalDone(app[app.len() - 1..].to_vec()));
        };
        loop {
            if pos >= app.len() {
                break;
            }
            if unanswered.is_empty() && echoes_seen == (lists.len() - 1) + 3 {
                tail(&mut events, pos, &app);
                let echo_total = events
                    .iter()
                    .filter(|e| matches!(e, ReplayEvent::Echo(_)))
                    .count();
                return ParsedSender {
                    events,
                    echo_total,
                    lists,
                    entries: all_entries,
                };
            }
            let Ok((ndx, c)) = decode_ndx(&app[pos..], &mut ndx_state) else {
                break;
            };
            if ndx == NDX_DONE {
                echoes_seen += 1;
                events.push(ReplayEvent::Echo(app[pos..pos + c].to_vec()));
                pos += c;
                continue;
            }
            if ndx == NDX_FLIST_EOF || ndx == NDX_DEL_STATS {
                events.push(ReplayEvent::Marker(app[pos..pos + c].to_vec()));
                pos += c;
                continue;
            }
            if ndx >= 0 {
                if !unanswered.contains(&ndx) {
                    break; // past the answers: stats + final marker
                }
                unanswered.retain(|x| *x != ndx);
                let (iflags, c2) = decode_item_flags(&app[pos + c..]).expect("answer iflags");
                let mut end = pos + c + c2;
                if iflags & 0x0100 != 0 {
                    // ITEM_REPORT_XATTR: the sender's out-of-band xattr datum
                    // section (skip varints, each with len+datum, closed by
                    // varint 0) sits before any sum_head (p10-dl).
                    loop {
                        let (skip, c3) = decode_varint(&app[end..]).expect("answer oob skip");
                        end += c3;
                        if skip == 0 {
                            break;
                        }
                        let (ln, c4) = decode_varint(&app[end..]).expect("answer oob len");
                        end += c4 + ln as usize;
                    }
                }
                if iflags & 0x8000 != 0 {
                    // the sender echoes its own sum_head before the delta
                    let (_, csh) = decode_sum_head(&app[end..]).expect("answer sum_head");
                    end += csh;
                    if capture_compressed(run) {
                        // A `-z` capture frames the tokens with the compressed
                        // byte-tag grammar; the DEFLATED_DATA payloads stay
                        // opaque here (the driver inflates them), so the whole
                        // delta + checksum is skipped with the shared decoder.
                        let (_, cdel) = decode_delta_stream(&app[end..], 16, None)
                            .expect("answer compressed delta");
                        end += cdel;
                    } else {
                        // no-`-z` captures use int32 LE tokens: 0 end, >0 literal, <0 match
                        loop {
                            assert!(end + 4 <= app.len(), "raw token truncated");
                            let tag = i32::from_le_bytes(app[end..end + 4].try_into().unwrap());
                            end += 4;
                            if tag == 0 {
                                break;
                            }
                            if tag > 0 {
                                end += tag as usize;
                            }
                        }
                        end += 16; // whole-file checksum (xxh128)
                    }
                }
                events.push(ReplayEvent::ItemAnswer(ndx, app[pos..end].to_vec()));
                pos = end;
                continue;
            }
            if ndx <= NDX_FLIST_OFFSET {
                // Segment marker: the body runs to the next end-of-list.
                let mut dec = FileListStreamDecoder::new(opts, codec, DEFAULT_MAX_TREE_ENTRIES);
                dec.feed(&app[pos + c..]);
                let mut ok = false;
                let mut seg_entry_list: Vec<FileListEntry> = Vec::new();
                loop {
                    if dec.is_finished() {
                        ok = true;
                        break;
                    }
                    match dec.next_outcome() {
                        Ok(Some(FileListDecodeOutcome::Entry(e))) => seg_entry_list.push(e),
                        Ok(Some(_)) => {}
                        Ok(None) => break,
                        Err(_) => break,
                    }
                }
                if !ok {
                    break;
                }
                let seg_consumed = (app.len() - pos - c) - dec.buffered();
                let end = pos + c + seg_consumed;
                lists.push((next_start, seg_entry_list.len()));
                next_start += seg_entry_list.len() as i32 + 1;
                all_entries.push(seg_entry_list);
                events.push(ReplayEvent::Segment(app[pos..end].to_vec()));
                pos = end;
                codec = dec.into_parts().0;
                continue;
            }
            break;
        }
        // Tail: SummaryFrame + one final NDX_DONE (Falena's table), split by
        // the echo count, not by parsing the frame mid-run.
        tail(&mut events, pos, &app);
        let echo_total = events
            .iter()
            .filter(|e| matches!(e, ReplayEvent::Echo(_)))
            .count();
        ParsedSender {
            events,
            echo_total,
            lists,
            entries: all_entries,
        }
    }

    /// Parse our own captured outbound app stream: requested item ndx set and
    /// NDX_DONE count.
    /// Offset where the mux-framed app stream starts in a client-side raw
    /// stream: after the 4-byte protocol version and the client's negotiated
    /// strings (one or two, depending on whether compression was negotiated).
    /// A mux frame header has its tag in the high byte and a length that
    /// fits the buffer, which distinguishes it from a string's bytes.
    fn app_stream_offset(raw: &[u8]) -> Option<usize> {
        let mut at = 4usize;
        while at + 4 <= raw.len() {
            let hdr = u32::from_le_bytes(raw[at..at + 4].try_into().unwrap());
            let len = (hdr & 0x00FF_FFFF) as usize;
            let tag = (hdr >> 24) as u8;
            if tag >= 7 && at + 4 + len <= raw.len() {
                return Some(at);
            }
            at += 1 + raw[at] as usize;
        }
        None
    }

    fn parse_outbound(raw: &[u8], run: &str) -> (HashSet<i32>, i32) {
        let _ = run;
        let mut requested = HashSet::new();
        let mut dones = 0i32;
        for ev in parse_outbound_events(raw) {
            match ev {
                OutEvent::Req(r) => {
                    requested.insert(r.ndx);
                }
                OutEvent::Done => dones += 1,
            }
        }
        (requested, dones)
    }

    type SharedOutbound = Arc<Mutex<Vec<u8>>>;

    struct ReactiveStream {
        rx: tokio::sync::mpsc::Receiver<Vec<u8>>,
        pending: VecDeque<u8>,
        outbound: SharedOutbound,
    }

    #[async_trait]
    impl RawByteStream for ReactiveStream {
        async fn read_bytes(&mut self, max: usize) -> Result<Vec<u8>, AerorsyncError> {
            if self.pending.is_empty() {
                match tokio::time::timeout(STALL, self.rx.recv()).await {
                    Ok(Some(bytes)) => self.pending.extend(bytes),
                    Ok(None) => {
                        return Err(AerorsyncError::transport_clean_eof(
                            "reactive mock feeder finished",
                        ))
                    }
                    Err(_) => {
                        return Err(AerorsyncError::transport(
                            "REACTIVE MOCK STALL: the generator waited for an event the \
                             capture cannot release (no matching request or marker on \
                             the outbound)",
                        ))
                    }
                }
            }
            let take = max.min(self.pending.len());
            Ok(self.pending.drain(..take).collect())
        }
        async fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), AerorsyncError> {
            self.outbound
                .lock()
                .map_err(|_| AerorsyncError::transport("outbound mutex poisoned"))?
                .extend_from_slice(bytes);
            Ok(())
        }
        async fn shutdown(&mut self) -> Result<(), AerorsyncError> {
            Ok(())
        }
    }

    struct DummyStream;
    #[async_trait]
    impl BidirectionalByteStream for DummyStream {
        async fn write_frame(&mut self, _frame: &[u8]) -> Result<(), AerorsyncError> {
            Err(AerorsyncError::transport("dummy stream: unused"))
        }
        async fn read_frame(&mut self) -> Result<Vec<u8>, AerorsyncError> {
            Err(AerorsyncError::transport("dummy stream: unused"))
        }
        async fn shutdown(&mut self) -> Result<(), AerorsyncError> {
            Ok(())
        }
    }

    struct ReactiveTransport {
        rx: Arc<Mutex<Option<tokio::sync::mpsc::Receiver<Vec<u8>>>>>,
        outbound: SharedOutbound,
    }

    #[async_trait]
    impl RemoteShellTransport for ReactiveTransport {
        type Stream = DummyStream;
        async fn probe(&self) -> Result<TransportProbe, AerorsyncError> {
            Err(AerorsyncError::transport("probe unused in reactive mock"))
        }
        async fn exec(
            &self,
            _request: RemoteExecRequest,
        ) -> Result<RemoteCommandOutput, AerorsyncError> {
            Err(AerorsyncError::transport("exec unused in reactive mock"))
        }
        async fn open_stream(
            &self,
            _request: RemoteExecRequest,
        ) -> Result<Self::Stream, AerorsyncError> {
            Err(AerorsyncError::transport(
                "open_stream unused in reactive mock",
            ))
        }
        async fn cancel(&self) -> Result<(), AerorsyncError> {
            Ok(())
        }
    }

    #[async_trait]
    impl RawRemoteShellTransport for ReactiveTransport {
        type RawStream = ReactiveStream;
        async fn open_raw_stream(
            &self,
            _request: RemoteExecRequest,
        ) -> Result<Self::RawStream, AerorsyncError> {
            let rx = self
                .rx
                .lock()
                .unwrap()
                .take()
                .expect("open_raw_stream called once");
            Ok(ReactiveStream {
                rx,
                pending: VecDeque::new(),
                outbound: self.outbound.clone(),
            })
        }
    }

    /// The feeder: releases the preamble, then every event in capture order,
    /// gated on the generator's outbound actions (NAS 3282): the k-th echo
    /// unlocks at our k-th NDX_DONE (never at the capture's offset), and a
    /// segment unlocks when we have sent at least as many NDX_DONE as echoes
    /// precede it in the capture's sender stream (the lookahead model, Q2).
    async fn run_feeder(
        tx: tokio::sync::mpsc::Sender<Vec<u8>>,
        preamble: Vec<u8>,
        events: Vec<ReplayEvent>,
        echo_total: usize,
        outbound: SharedOutbound,
        run: &'static str,
    ) {
        let _ = tx.send(preamble).await;
        // echoes preceding each event in the capture's sender stream
        let mut echo_before = Vec::with_capacity(events.len());
        let mut echos = 0usize;
        for ev in &events {
            echo_before.push(echos);
            if matches!(ev, ReplayEvent::Echo(_)) {
                echos += 1;
            }
        }
        let mut echo_idx = 0usize;
        for (i, ev) in events.into_iter().enumerate() {
            match ev {
                ReplayEvent::List(bytes) | ReplayEvent::Marker(bytes) => {
                    let _ = tx.send(mux_frame_pub(&bytes)).await;
                }
                ReplayEvent::Segment(bytes) => {
                    let need = echo_before[i];
                    wait_for(&outbound, run, |_, dones| dones >= need as i32).await;
                    let _ = tx.send(mux_frame_pub(&bytes)).await;
                }
                ReplayEvent::ItemAnswer(n, bytes) => {
                    wait_for(&outbound, run, |req, _| req.contains(&n)).await;
                    let _ = tx.send(mux_frame_pub(&bytes)).await;
                }
                ReplayEvent::Echo(bytes) => {
                    echo_idx += 1;
                    let k = echo_idx;
                    wait_for(&outbound, run, |_, dones| dones >= k as i32).await;
                    let _ = tx.send(mux_frame_pub(&bytes)).await;
                }
                ReplayEvent::Stats(bytes) => {
                    // our S dir_flist dones + 4 pre-stats markers = S+4
                    let need = echo_total + 1; // echo_total = S+3
                    wait_for(&outbound, run, |_, dones| dones >= need as i32).await;
                    let _ = tx.send(mux_frame_pub(&bytes)).await;
                }
                ReplayEvent::FinalDone(bytes) => {
                    // our ACK (the 5th phase marker) after the summary: S+5
                    let need = echo_total + 2;
                    wait_for(&outbound, run, |_, dones| dones >= need as i32).await;
                    let _ = tx.send(mux_frame_pub(&bytes)).await;
                }
            }
        }
    }

    async fn wait_for(
        outbound: &SharedOutbound,
        run: &'static str,
        cond: impl Fn(&HashSet<i32>, i32) -> bool,
    ) {
        loop {
            {
                let raw = outbound.lock().unwrap().clone();
                if !raw.is_empty() {
                    let (req, dones) = parse_outbound(&raw, run);
                    if cond(&req, dones) {
                        return;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    fn mux_frame_pub(payload: &[u8]) -> Vec<u8> {
        let header = MuxHeader {
            tag: MuxTag::Data,
            length: payload.len() as u32,
        };
        let mut out = Vec::with_capacity(4 + payload.len());
        out.extend_from_slice(&header.encode());
        out.extend_from_slice(payload);
        out
    }

    fn preamble_bytes(run: &str) -> Vec<u8> {
        let (compat, _) = server_compat(run);
        encode_server_preamble(&ServerPreamble {
            protocol_version: 31,
            compat_flags: compat as i32,
            checksum_algos: "xxh128".to_string(),
            compression_algos: if capture_compressed(run) {
                "zstd".to_string()
            } else {
                "none".to_string()
            },
            checksum_seed: 0,
            consumed: 0,
        })
    }

    /// B32R1/B32R2/B32R3: drive_download_tree against the capture with the
    /// reactive mock. The mock can never release an item answer before a
    /// request exists; after the rewrite the run must complete and the
    /// outbound must satisfy the structural checks of NAS 3282 (the DONE
    /// offset is receiver-speed dependent, so it is NOT compared).
    #[tokio::test]
    async fn b3_2_reactive_replay_p9_dl_inc() {
        replay_run("p9-dl-inc").await;
    }

    #[tokio::test]
    async fn b3_2_reactive_replay_p9_dl_noinc() {
        replay_run("p9-dl-noinc").await;
    }

    #[tokio::test]
    async fn b3_2_reactive_replay_p9big_dl_inc() {
        replay_run("p9big-dl-inc").await;
    }

    /// NAS 3282 point 4: the p10-dl replay (cherry-picked capture), point
    /// (d): xattr written as per the list, the shared reference included,
    /// the 80-byte value complete.
    #[tokio::test]
    async fn b3_2_reactive_replay_p10_dl() {
        replay_run("p10-dl").await;
    }

    /// The p9z-dl-inc replay (cherry-picked -az capture): the same structural
    /// checks as p9-dl-inc, plus the served file contents must equal
    /// p9-dl-inc's (the generator requests are byte-identical), proving the
    /// compressed branch of the delta grammar and the zstd literal decode.
    #[tokio::test]
    async fn b3_2_reactive_replay_p9z_dl_inc() {
        let p9 = replay_run("p9-dl-inc").await;
        let p9z = replay_run("p9z-dl-inc").await;
        assert!(!p9.is_empty(), "p9-dl-inc served at least one file");
        assert_eq!(
            p9, p9z,
            "p9z-dl-inc must serve the same file contents as p9-dl-inc"
        );
    }

    /// NAS 3282 point 1, test (i): for every regular file of the four
    /// captures, the sorted ndx assigned by `flist_entry_cmp` equals the ndx
    /// the capture's generator requests (the ndx the sender delivers the
    /// content at). `r/d2/f3` must come out 11.
    #[test]
    fn b3_2_sorted_ndx_matches_capture_delivery() {
        use crate::aerorsync::tree_driver::flist_entry_cmp;
        for run in [
            "p9-dl-inc",
            "p9-dl-noinc",
            "p9big-dl-inc",
            "p10-dl",
            "p9z-dl-inc",
        ] {
            assert!(
                file_ndx_sets_conform(run, flist_entry_cmp),
                "{run}: the sorted ndx of every regular file must be the capture's delivery ndx"
            );
        }
        assert_eq!(
            sorted_ndx_of("p9-dl-inc", b"r/d2/f3", flist_entry_cmp),
            Some(11),
            "the Q-a witness: r/d2/f3 must come out 11"
        );
    }

    /// NAS 3282 point 1, test (ii): a dirs-first comparator and the wire
    /// order (identity) must fail the conformance check of (i).
    #[test]
    fn b3_2_sorted_ndx_mutations_fail() {
        use crate::aerorsync::tree_driver::flist_entry_cmp;
        use std::cmp::Ordering;
        let dirs_first = |a: &FileListEntry, b: &FileListEntry| {
            is_directory_mode(b.mode)
                .cmp(&is_directory_mode(a.mode))
                .then_with(|| a.path.cmp(&b.path))
        };
        let identity = |_a: &FileListEntry, _b: &FileListEntry| Ordering::Equal;
        let runs = [
            "p9-dl-inc",
            "p9-dl-noinc",
            "p9big-dl-inc",
            "p10-dl",
            "p9z-dl-inc",
        ];
        for run in runs {
            assert!(
                file_ndx_sets_conform(run, flist_entry_cmp),
                "{run}: the real comparator must conform"
            );
        }
        // The mutations must fail the conformance check of (i) overall; a
        // segment of only files (or only dirs) cannot distinguish them, so
        // the mixed segments of p9-dl-inc / p9-dl-noinc are the witnesses.
        assert!(
            runs.iter().any(|r| !file_ndx_sets_conform(r, dirs_first)),
            "a dirs-first comparator must NOT conform (witnessed by a mixed segment)"
        );
        assert!(
            runs.iter().any(|r| !file_ndx_sets_conform(r, identity)),
            "the wire order must NOT conform"
        );
        assert!(
            !file_ndx_sets_conform("p9-dl-inc", dirs_first),
            "p9-dl-inc: dirs-first must NOT conform (seg0 mixes top.txt with d1/d2/d3)"
        );
        assert!(
            !file_ndx_sets_conform("p9-dl-inc", identity),
            "p9-dl-inc: wire order must NOT conform (r/d2/f3 wire 12, sorted 11)"
        );
    }

    /// The sorted ndx the comparator under test assigns to one path.
    fn sorted_ndx_of(
        run: &str,
        path: &[u8],
        cmp: fn(&FileListEntry, &FileListEntry) -> std::cmp::Ordering,
    ) -> Option<i32> {
        let parsed = parse_sender_events(run);
        let (compat, _) = server_compat(run);
        let inc = compat & CF_INC_RECURSE != 0;
        let mut ndx_start = if inc { 1 } else { 0 };
        for entries in &parsed.entries {
            let mut es = entries.clone();
            es.sort_by(cmp);
            if let Some(pos) = es.iter().position(|e| e.path == path) {
                return Some(ndx_start + pos as i32);
            }
            ndx_start += entries.len() as i32 + 1;
        }
        None
    }

    /// True when the set of sorted ndx assigned to the regular files equals
    /// the set of file-request (ITEM_TRANSFER) ndx of the capture's
    /// generator stream.
    fn file_ndx_sets_conform(
        run: &str,
        cmp: fn(&FileListEntry, &FileListEntry) -> std::cmp::Ordering,
    ) -> bool {
        let parsed = parse_sender_events(run);
        let (compat, _) = server_compat(run);
        let inc = compat & CF_INC_RECURSE != 0;
        let gen_app_raw = generator_app_stream(run);
        let gen_app = skip_client_filter_list(&gen_app_raw);
        let (reqs, _) = parse_requests_seq(gen_app);
        let capture_file_reqs: HashSet<i32> = reqs
            .iter()
            .filter(|r| r.iflags & 0x8000 != 0)
            .map(|r| r.ndx)
            .collect();
        let mut ndx_start = if inc { 1 } else { 0 };
        let mut assigned = HashSet::new();
        for entries in &parsed.entries {
            let mut es = entries.clone();
            es.sort_by(cmp);
            for (pos, e) in es.iter().enumerate() {
                if is_regular_mode(e.mode) {
                    assigned.insert(ndx_start + pos as i32);
                }
            }
            ndx_start += entries.len() as i32 + 1;
        }
        assigned == capture_file_reqs
    }

    /// NAS 3282: the replay checks the NDX_DONE placement structurally, not
    /// by offset: total S+5, each dir_flist done after the last request of
    /// the list it closes, the 5 phase markers after the last item request.
    fn check_done_placement(events: &[OutEvent], lists: &[(i32, usize)]) {
        let s = lists.len().saturating_sub(1); // extra segments
        let done_idx: Vec<usize> = events
            .iter()
            .enumerate()
            .filter(|(_, e)| matches!(e, OutEvent::Done))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(done_idx.len(), s + 5, "total NDX_DONE must be S+5");
        let last_req_idx = events
            .iter()
            .rposition(|e| matches!(e, OutEvent::Req(_)))
            .expect("at least one request");
        assert!(
            done_idx[s] > last_req_idx,
            "the 5 phase markers must follow the last item request"
        );
        for k in 0..s {
            let (start, len) = lists[k];
            let last_of_list = events
                .iter()
                .enumerate()
                .filter(|(_, e)| match e {
                    OutEvent::Req(r) => r.ndx >= start && r.ndx < start + len as i32,
                    _ => false,
                })
                .map(|(i, _)| i)
                .max();
            if let Some(li) = last_of_list {
                assert!(
                    done_idx[k] > li,
                    "dir done {k} must come after the last request of its list"
                );
            }
        }
    }

    /// p10-dl point (d): the xattrs on disk match the list: the shared set on
    /// both f1 and f2, the full 80-byte user.long on f3, none on f4.
    fn check_p10_xattrs(dest: &std::path::Path) {
        use crate::aerorsync::real_wire::XattrPair;
        use crate::aerorsync::xattr_fs::read_user_xattrs;
        let as_map = |pairs: &[XattrPair]| -> Vec<(String, Vec<u8>)> {
            let mut v: Vec<(String, Vec<u8>)> = pairs
                .iter()
                .map(|p| (p.name.clone(), p.datum.bytes().unwrap_or(&[]).to_vec()))
                .collect();
            v.sort();
            v
        };
        let read = |rel: &str| read_user_xattrs(&dest.join(rel));
        let f1 = read("xm/d1/f1").unwrap_or_default();
        let f2 = read("xm/d1/f2").unwrap_or_default();
        let want_shared = vec![
            ("user.color".to_string(), b"blue".to_vec()),
            ("user.tag".to_string(), b"x".to_vec()),
        ];
        assert_eq!(as_map(&f1), want_shared, "f1 carries the literal set");
        assert_eq!(
            as_map(&f2),
            want_shared,
            "f2 carries the SAME set via the list reference"
        );
        let f3 = read("xm/d2/f3").unwrap_or_default();
        assert_eq!(
            as_map(&f3),
            vec![("user.long".to_string(), vec![b'L'; 80])],
            "f3's 80-byte value must arrive complete via the OOB section"
        );
        let f4 = read("xm/d2/f4");
        assert!(
            f4.as_ref().is_none_or(|p| p.is_empty()),
            "f4 carries no xattrs"
        );
    }

    fn events_summary(events: &[ReplayEvent]) -> Vec<String> {
        events
            .iter()
            .map(|e| match e {
                ReplayEvent::List(b) => format!("List({})", b.len()),
                ReplayEvent::Marker(b) => format!("Marker({})", b.len()),
                ReplayEvent::Segment(b) => format!("Segment({})", b.len()),
                ReplayEvent::ItemAnswer(n, b) => format!("Item({n},{})", b.len()),
                ReplayEvent::Echo(_) => "Echo".to_string(),
                ReplayEvent::Stats(b) => format!("Stats({})", b.len()),
                ReplayEvent::FinalDone(_) => "FinalDone".to_string(),
            })
            .collect()
    }

    /// Collect the served files (path relative to the destination root ->
    /// content) so a replay can compare contents across captures
    /// (p9z-dl-inc vs p9-dl-inc).
    fn collect_files(
        root: &std::path::Path,
        rel: PathBuf,
        out: &mut std::collections::BTreeMap<PathBuf, Vec<u8>>,
    ) {
        for entry in std::fs::read_dir(root.join(&rel)).unwrap() {
            let entry = entry.unwrap();
            let mut p = rel.clone();
            p.push(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                collect_files(root, p, out);
            } else {
                out.insert(p, std::fs::read(entry.path()).unwrap());
            }
        }
    }

    async fn replay_run(run: &'static str) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
        let parsed = parse_sender_events(run);
        let echo_total = parsed.echo_total;
        let lists = parsed.lists;
        let events = parsed.events;
        let gen_app_raw = generator_app_stream(run);
        let gen_app = skip_client_filter_list(&gen_app_raw);
        let (gen_reqs, gen_dones) = parse_requests_seq(gen_app);
        let (tx, rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
        let outbound: SharedOutbound = Arc::new(Mutex::new(Vec::new()));
        let feeder_outbound = outbound.clone();
        let preamble = preamble_bytes(run);
        let events_dbg = events_summary(&events);
        eprintln!("events: {:?}", events_dbg);
        tokio::spawn(async move {
            run_feeder(tx, preamble, events, echo_total, feeder_outbound, run).await;
        });
        let transport = ReactiveTransport {
            rx: Arc::new(Mutex::new(Some(rx))),
            outbound: outbound.clone(),
        };
        let mut d = AerorsyncDriver::new(transport, CancelHandle::inert());
        d.session_role = Some(SessionRole::Receiver);
        d.open_raw_stream_internal(&RemoteCommandSpec::capture_download("/remote/target"))
            .await
            .unwrap();
        let compression = if capture_compressed(run) {
            "zstd"
        } else {
            "none"
        };
        d.perform_preamble_exchange(31, "xxh128", compression)
            .await
            .unwrap();
        let (opts, _) = options_from_capture(run);
        d.file_list_checksum_enabled = opts.always_checksum;
        // the metadata flags of the capture's own argv (p10-dl carries -X)
        d.effective_metadata.preserve_xattrs = opts.preserve_xattrs;
        d.effective_metadata.preserve_acls = opts.preserve_acls;
        d.negotiated_xattrs = opts.preserve_xattrs;
        let dest = tempfile::tempdir().unwrap();
        let dl_opts = TreeDownloadOptions {
            preserve_xattrs: opts.preserve_xattrs,
            preserve_acls: opts.preserve_acls,
            preserve_uid: opts.preserve_uid,
            preserve_gid: opts.preserve_gid,
            ..Default::default()
        };
        let mut sink = CollectingSink::default();
        let res = d
            .drive_download_tree(dest.path(), dl_opts, &CurrentDeltaSyncBridge, &mut sink)
            .await;
        let outcome = match res {
            Ok(o) => o,
            Err(e) => {
                let raw = outbound.lock().unwrap().clone();
                let (req, dones) = parse_outbound(&raw, run);
                panic!(
                    "drive failed: {e:?}; outbound requests={req:?} dones={dones}; events={:?}",
                    events_dbg
                );
            }
        };
        assert!(!outcome.results.is_empty(), "at least one entry served");
        // NAS 3282: structural checks, NOT the byte offset (the real
        // generator's DONE position depends on receiver speed, Q8).
        let raw = outbound.lock().unwrap().clone();
        let events_out = parse_outbound_events(&raw);
        let our_reqs: Vec<GenRequest> = events_out
            .iter()
            .filter_map(|e| match e {
                OutEvent::Req(r) => Some(*r),
                _ => None,
            })
            .collect();
        let our_dones = events_out
            .iter()
            .filter(|e| matches!(e, OutEvent::Done))
            .count();
        // (a) the request sequence equals the capture's generator stream:
        // same ndx (sorted, Q-a), same iflags (dir items Q-c, the xattr
        // report bit), same sum_head counts, same order.
        assert_eq!(
            our_reqs, gen_reqs,
            "the request sequence must match the capture's generator stream"
        );
        // (b) total NDX_DONE = S+5, and the placement rules.
        assert_eq!(our_dones, gen_dones, "total NDX_DONE must be S+5");
        check_done_placement(&events_out, &lists);
        // p10-dl point (d): the xattrs on disk.
        if run == "p10-dl" {
            check_p10_xattrs(dest.path());
        }
        let mut served = std::collections::BTreeMap::new();
        collect_files(dest.path(), PathBuf::new(), &mut served);
        served
    }
}
