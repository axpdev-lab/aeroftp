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
    decode_file_list_entry, decode_id_list, decode_ndx, decode_varint, encode_file_list_entry,
    encode_file_list_terminator, encode_id_list, encode_varint, encode_varlong, is_directory_mode,
    reassemble_msg_data, AclNamedEntry, AclPrincipal, AclWireEntry, FileListAcls,
    FileListCodecState, FileListDecodeOptions, FileListDecodeOutcome, FileListEntry,
    FileListStreamDecoder, IdNameList, MuxPoll, MuxStreamReader, NdxState, Rdev, RealWireError,
    RsyncAcl, XattrPair, MAXPATHLEN, MAX_ACL_NAMED_ENTRIES, MAX_ACL_NAME_LEN,
    MAX_FILE_LIST_ENTRY_BYTES, MAX_FULL_DATUM, MAX_XATTR_NAME_LEN, MAX_XATTR_PAIRS, NDX_FLIST_EOF,
    NDX_FLIST_OFFSET, XMIT_EXTENDED_FLAGS, XMIT_GROUP_NAME_FOLLOWS, XMIT_LONG_NAME, XMIT_MOD_NSEC,
    XMIT_SAME_GID, XMIT_SAME_MODE, XMIT_SAME_NAME, XMIT_SAME_RDEV_MAJOR, XMIT_SAME_TIME,
    XMIT_SAME_UID, XMIT_TOP_DIR, XMIT_USER_NAME_FOLLOWS,
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
    let compress = command
        .split_whitespace()
        .find(|a| a.starts_with('-') && !a.starts_with("--"))
        .is_some_and(|b| b.split('.').next().unwrap_or("").contains('z'));
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
const DECODABLE: [&str; 44] = [
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
        ("p3-dl", "xattr"),
        ("p3-up", "xattr"),
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
            ("xattr", RealWireError::XattrAbbrevUnsupported { .. })
            | ("hardlink", RealWireError::HardlinkUnsupported { .. }) => {}
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
        xattrs: Some(xattrs),
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
