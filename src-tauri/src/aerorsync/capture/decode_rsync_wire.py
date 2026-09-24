#!/usr/bin/env python3
"""Line-by-line decoder for one captured rsync session (protocol 30+).

Input is a capture directory written by `rsync_proxy.py`
(`remote_command.txt`, `capture_in.bin` = client -> server,
`capture_out.bin` = server -> client). Output is a text listing: handshake,
every multiplex frame, and the application stream of each direction decoded
field by field with its offset, so a wire-evidence document can quote bytes
and their meaning instead of a description.

Scope is the B0/C0 wire campaign (`12-multi-entry-wire-evidence.md`,
`13-filters-and-flags-wire-evidence.md`): file-list entries, incremental
file-list segments, NDX values, filter lists, files-from, item flags and the
per-file transfer framing without compression. It is an instrument, not a
reimplementation: when it meets a byte it does not understand it stops and
prints where, which is itself a finding. It never guesses past a stop.

Usage: decode_rsync_wire.py <capture-dir> [--max-entries N]
"""
from __future__ import annotations

import argparse
import gzip
import shlex
import stat
import sys
from dataclasses import dataclass, field
from pathlib import Path

MPLEX_BASE = 7
MSG_NAMES = {
    0: "MSG_DATA", 1: "MSG_ERROR_XFER", 2: "MSG_INFO", 3: "MSG_ERROR",
    4: "MSG_WARNING", 5: "MSG_ERROR_SOCKET", 6: "MSG_LOG", 7: "MSG_CLIENT",
    8: "MSG_ERROR_UTF8", 9: "MSG_REDO", 10: "MSG_STATS", 20: "MSG_FLIST",
    21: "MSG_FLIST_EOF", 22: "MSG_IO_ERROR", 33: "MSG_IO_TIMEOUT",
    42: "MSG_NOOP", 86: "MSG_ERROR_EXIT", 100: "MSG_SUCCESS",
    101: "MSG_DELETED", 102: "MSG_NO_SEND",
}

XMIT = [
    (1 << 0, "TOP_DIR"), (1 << 1, "SAME_MODE"), (1 << 2, "EXTENDED_FLAGS"),
    (1 << 3, "SAME_UID"), (1 << 4, "SAME_GID"), (1 << 5, "SAME_NAME"),
    (1 << 6, "LONG_NAME"), (1 << 7, "SAME_TIME"),
    (1 << 8, "SAME_RDEV_MAJOR|NO_CONTENT_DIR"), (1 << 9, "HLINKED"),
    (1 << 10, "USER_NAME_FOLLOWS"), (1 << 11, "GROUP_NAME_FOLLOWS"),
    (1 << 12, "HLINK_FIRST|IO_ERROR_ENDLIST"), (1 << 13, "MOD_NSEC"),
    (1 << 14, "SAME_ATIME"),
]
CF = [
    (1, "INC_RECURSE"), (2, "SYMLINK_TIMES"), (4, "SYMLINK_ICONV"),
    (8, "SAFE_FLIST"), (16, "AVOID_XATTR_OPTIM"), (32, "CHKSUM_SEED_FIX"),
    (64, "INPLACE_PARTIAL_DIR"), (128, "VARINT_FLIST_FLAGS"), (256, "ID0_NAMES"),
]
# rsync.h; ITEM_REPORT_XATTR = 0x0100 cross-checked against native_driver.rs.
ITEM = [
    (1 << 0, "REPORT_ATIME"), (1 << 1, "REPORT_CHANGE"), (1 << 2, "REPORT_SIZE|TIMEFAIL"),
    (1 << 3, "REPORT_TIME"), (1 << 4, "REPORT_PERMS"), (1 << 5, "REPORT_OWNER"),
    (1 << 6, "REPORT_GROUP"), (1 << 7, "REPORT_ACL"), (1 << 8, "REPORT_XATTR"),
    (1 << 10, "REPORT_CRTIME"), (1 << 11, "BASIS_TYPE_FOLLOWS"),
    (1 << 12, "XNAME_FOLLOWS"), (1 << 13, "IS_NEW"), (1 << 14, "LOCAL_CHANGE"),
    (1 << 15, "TRANSFER"),
]
ITEM_REPORT_XATTR = 1 << 8
ITEM_BASIS_TYPE_FOLLOWS = 1 << 11
ITEM_XNAME_FOLLOWS = 1 << 12
ITEM_TRANSFER = 1 << 15

NDX_DONE = -1
NDX_FLIST_EOF = -2
NDX_DEL_STATS = -3
NDX_FLIST_OFFSET = -101

# int_byte_extra[] from io.c, indexed by first byte / 4.
INT_BYTE_EXTRA = [0] * 32 + [1] * 16 + [2] * 8 + [3] * 4 + [4] * 2 + [5] + [6]

CSUM_LEN = {"xxh128": 16, "xxh3": 8, "xxh64": 8, "md5": 16, "md4": 16, "sha1": 20, "none": 0}


def names(bits: int, table) -> str:
    out = [n for b, n in table if bits & b]
    rest = bits & ~sum(b for b, _ in table)
    if rest:
        out.append(f"0x{rest:x}?")
    return "|".join(out) if out else "0"


class Stop(Exception):
    """The decoder met something it does not model. Never swallowed silently."""


class Reader:
    def __init__(self, buf: bytes, base: int = 0):
        self.buf = buf
        self.pos = 0
        self.base = base

    def left(self) -> int:
        return len(self.buf) - self.pos

    def take(self, n: int) -> bytes:
        if self.pos + n > len(self.buf):
            raise Stop(f"truncated: need {n} bytes at 0x{self.pos:x}, have {self.left()}")
        b = self.buf[self.pos:self.pos + n]
        self.pos += n
        return b

    def byte(self) -> int:
        return self.take(1)[0]

    def int32(self) -> int:
        return int.from_bytes(self.take(4), "little", signed=True)

    def shortint(self) -> int:
        return int.from_bytes(self.take(2), "little")

    def varint(self) -> int:
        ch = self.byte()
        extra = INT_BYTE_EXTRA[ch // 4]
        if not extra:
            return ch
        bit = 1 << (8 - extra)
        b = bytearray(self.take(extra))
        b.append(ch & (bit - 1))
        return int.from_bytes(bytes(b), "little", signed=True)

    def varlong(self, min_bytes: int) -> int:
        b2 = self.take(min_bytes)
        ch = b2[0]
        u = bytearray(b2[1:])
        extra = INT_BYTE_EXTRA[ch // 4]
        if extra:
            bit = 1 << (8 - extra)
            u += self.take(extra)
            u.append(ch & (bit - 1))
        else:
            u.append(ch)
        return int.from_bytes(bytes(u), "little", signed=True)

    def vstring(self) -> bytes:
        n = self.byte()
        if n & 0x80:
            n = ((n & 0x7F) << 8) | self.byte()
        return self.take(n)


@dataclass
class NdxState:
    prev_pos: int = -1
    prev_neg: int = 1

    def read(self, r: Reader) -> int:
        b = r.byte()
        if b == 0:
            return NDX_DONE
        neg = False
        if b == 0xFF:
            neg = True
            b = r.byte()
        if b == 0xFE:
            b0, b1 = r.take(2)
            if b0 & 0x80:
                rest = r.take(2)
                num = int.from_bytes(bytes([b1, rest[0], rest[1], b0 & 0x7F]), "little")
            else:
                num = (b0 << 8) + b1 + (self.prev_neg if neg else self.prev_pos)
        else:
            num = b + (self.prev_neg if neg else self.prev_pos)
        if neg:
            self.prev_neg = num
            return -num
        self.prev_pos = num
        return num


@dataclass
class Opts:
    sender_is_server: bool
    short: str
    long_opts: list
    caps: str
    compat: int = 0
    protocol: int = 0
    csum_len: int = 16

    def has(self, c: str) -> bool:
        return c in self.short

    @property
    def varint_flags(self) -> bool:
        return bool(self.compat & 128)

    @property
    def inc_recurse(self) -> bool:
        return bool(self.compat & 1)


@dataclass
class FlistState:
    lastname: bytes = b""
    mode: int = 0
    mtime: int = 0
    uid: int = 0
    gid: int = 0
    ndx_start: int = 0
    entries: list = field(default_factory=list)
    all_entries: dict = field(default_factory=dict)
    acl_count: dict = field(default_factory=lambda: {"access": 0, "default": 0})
    xattr_count: int = 0
    # dir_flist of flist.c: every directory in the order it was sent. The
    # segment announcement NDX_FLIST_OFFSET - n indexes THIS list, not ndx.
    dirs: list = field(default_factory=list)
    dirs_sorted: list = field(default_factory=list)
    # Transfer-phase ndx index the list AFTER flist_sort_and_clean.
    sorted_entries: dict = field(default_factory=dict)


def parse_remote_command(cmd: str):
    argv = shlex.split(cmd)
    short = ""
    caps = ""
    longs = []
    for a in argv[2:]:
        if a.startswith("--"):
            longs.append(a)
        elif a.startswith("-") and a != "-":
            s = a[1:]
            if "e." in s:
                s, caps = s.split("e.", 1)
                caps = caps
            short += s
    return short, longs, caps


class Out:
    def __init__(self):
        self.lines: list[str] = []

    def __call__(self, s: str = ""):
        self.lines.append(s)


def hexs(b: bytes, limit: int = 24) -> str:
    h = b[:limit].hex(" ")
    return h + (" ..." if len(b) > limit else "")


def field_line(out: Out, r: Reader, start: int, label: str):
    raw = r.buf[start:r.pos]
    out(f"  {r.base + start:07x}  {hexs(raw):<40} {label}")


def decode_acl(out: Out, r: Reader, st: FlistState, kind: str):
    s = r.pos
    ndx = r.varint()
    if ndx != 0:
        field_line(out, r, s, f"acl[{kind}] REFERENCE to index {ndx - 1} (wire value {ndx} = index+1)")
        return
    field_line(out, r, s, f"acl[{kind}] literal follows, will be index {st.acl_count[kind]}")
    st.acl_count[kind] += 1
    s = r.pos
    flags = r.byte()
    field_line(out, r, s, f"  acl flags 0x{flags:02x}")
    for bit, nm in ((1, "user_obj"), (2, "group_obj"), (4, "mask_obj"), (8, "other_obj")):
        if flags & bit:
            s = r.pos
            v = r.varint()
            field_line(out, r, s, f"  {nm} = {v:o} (octal perms)")
    if flags & 16:
        s = r.pos
        count = r.varint()
        field_line(out, r, s, f"  name list count = {count}")
        for _ in range(count):
            s = r.pos
            ident = r.varint()
            access = r.varint()
            label = f"  id={ident} access=0x{access:x} (perms {access >> 2:o}, {'user' if access & 2 else 'group'})"
            if access & 1:
                n = r.byte()
                label += f" name={r.take(n).decode(errors='replace')!r}"
            field_line(out, r, s, label)


def decode_xattr(out: Out, r: Reader, st: FlistState):
    s = r.pos
    ndx = r.varint()
    if ndx != 0:
        field_line(out, r, s, f"xattr REFERENCE to set {ndx - 1} (wire value {ndx} = index+1)")
        return
    field_line(out, r, s, f"xattr literal set follows, will be set {st.xattr_count}")
    st.xattr_count += 1
    s = r.pos
    count = r.varint()
    field_line(out, r, s, f"  xattr count = {count}")
    for _ in range(count):
        s = r.pos
        nlen = r.varint()
        dlen = r.varint()
        name = r.take(nlen)
        body = r.take(16 if dlen > 32 else dlen)
        field_line(out, r, s, f"  name={name!r} datum_len={dlen} {'md5 digest' if dlen > 32 else 'value'}={body!r}")


def decode_entry(out: Out, r: Reader, o: Opts, st: FlistState, ndx: int) -> bool:
    """Decode one entry or the end marker. Returns False at end of list."""
    s = r.pos
    if o.varint_flags:
        xf = r.varint()
    else:
        xf = r.byte()
        if xf & 4:
            xf |= r.byte() << 8
    if xf == 0:
        field_line(out, r, s, "flags 0 = END OF LIST")
        if o.varint_flags:
            s = r.pos
            io = r.varint()
            field_line(out, r, s, f"io_error = {io}")
        return False
    field_line(out, r, s, f"flags 0x{xf:x} = {names(xf, XMIT)}")
    l1 = 0
    if xf & (1 << 5):
        s = r.pos
        l1 = r.byte()
        field_line(out, r, s, f"same-name prefix length l1 = {l1} ({st.lastname[:l1]!r})")
    s = r.pos
    l2 = r.varint() if xf & (1 << 6) else r.byte()
    field_line(out, r, s, f"name suffix length l2 = {l2} ({'varint30, LONG_NAME' if xf & (1 << 6) else 'byte'})")
    s = r.pos
    suffix = r.take(l2)
    name = st.lastname[:l1] + suffix
    field_line(out, r, s, f"name suffix {suffix!r} -> full name {name!r}")
    hl_short = False
    first = None
    if (xf & (1 << 9)) and not (xf & (1 << 12)) and o.protocol >= 30:
        s = r.pos
        fh = r.varint()
        first = st.all_entries.get(fh)
        in_this = fh >= st.ndx_start
        hl_short = in_this
        field_line(out, r, s, f"first_hlink_ndx = {fh} ({first['name'] if first else '?'!r}); "
                   f"{'same segment: remaining attrs copied from it' if in_this else 'EARLIER segment: attrs follow in full'}")
    if hl_short:
        mode, mtime = first["mode"], first["mtime"]
        size = first["size"]
    else:
        s = r.pos
        size = r.varlong(3)
        field_line(out, r, s, f"file_length = {size} (varlong min 3)")
        mtime = st.mtime
        if not xf & (1 << 7):
            s = r.pos
            mtime = r.varlong(4)
            field_line(out, r, s, f"mtime = {mtime} (varlong min 4)")
        if xf & (1 << 13):
            s = r.pos
            ns = r.varint()
            field_line(out, r, s, f"mtime_nsec = {ns}")
        mode = st.mode
        if not xf & (1 << 1):
            s = r.pos
            mode = r.int32() & 0xFFFFFFFF
            field_line(out, r, s, f"mode = 0{mode:o} ({stat.filemode(mode)})")
        if o.has("o") and not xf & (1 << 3):
            s = r.pos
            st.uid = r.varint()
            lab = f"uid = {st.uid}"
            if xf & (1 << 10):
                n = r.byte()
                lab += f", user name {r.take(n)!r}"
            field_line(out, r, s, lab)
        if o.has("g") and not xf & (1 << 4):
            s = r.pos
            st.gid = r.varint()
            lab = f"gid = {st.gid}"
            if xf & (1 << 11):
                n = r.byte()
                lab += f", group name {r.take(n)!r}"
            field_line(out, r, s, lab)
        if o.has("D") and (stat.S_ISCHR(mode) or stat.S_ISBLK(mode)):
            raise Stop("device entry: rdev not modelled")
        if o.has("l") and stat.S_ISLNK(mode):
            s = r.pos
            ln = r.varint()
            target = r.take(ln)
            field_line(out, r, s, f"symlink target {target!r}")
    # Measured (p6c-dl): a non-first hard link whose leader is in the same
    # segment carries no file checksum, the next byte is the next entry.
    if o.has("c") and stat.S_ISREG(mode) and not hl_short:
        s = r.pos
        csum = r.take(o.csum_len)
        field_line(out, r, s, f"file checksum ({o.csum_len} bytes) {csum.hex()}")
    if o.has("A") and not stat.S_ISLNK(mode):
        decode_acl(out, r, st, "access")
        if stat.S_ISDIR(mode):
            decode_acl(out, r, st, "default")
    if o.has("X"):
        decode_xattr(out, r, st, )
    st.lastname = name
    st.mode, st.mtime = mode, mtime
    e = {"ndx": ndx, "name": name.decode(errors="replace"), "mode": mode, "mtime": mtime, "size": size, "flags": xf}
    st.entries.append(e)
    st.all_entries[ndx] = e
    if stat.S_ISDIR(mode):
        st.dirs.append(e["name"])
    out(f"  => entry ndx {ndx}: {e['name']!r} {stat.filemode(mode)} size={size}")
    return True


def sort_key(e: dict):
    """f_name_cmp for protocol >= 29, as measured: inside one directory every
    non-directory sorts before every directory, a directory before its own
    contents, names compared as bytes."""
    parts = e["name"].encode().split(b"/")
    key = tuple((1, c) for c in parts[:-1])
    return key + (((1 if stat.S_ISDIR(e["mode"]) else 0), parts[-1]),)


def read_id_lists(out: Out, r: Reader, o: Opts):
    """Without INC_RECURSE the names do not travel inline: after the list,
    one uid list and one gid list, each varint(id) byte(len) name, ended by
    varint 0 and (with CF_ID0_NAMES) the name of id 0."""
    for flag, kind in (("o", "uid"), ("g", "gid")):
        if not o.has(flag) or "--numeric-ids" in o.long_opts:
            continue
        out(f"-- {kind} list (sent after the file list because INC_RECURSE is off)")
        while True:
            s = r.pos
            ident = r.varint()
            if ident == 0:
                lab = f"{kind} list end (varint 0)"
                if o.compat & 256:
                    n = r.byte()
                    lab += f", CF_ID0_NAMES: name of id 0 = {r.take(n)!r}"
                field_line(out, r, s, lab)
                break
            n = r.byte()
            field_line(out, r, s, f"{kind} {ident} name {r.take(n)!r}")


def decode_flist(out: Out, r: Reader, o: Opts, st: FlistState, title: str, max_entries: int):
    out(f"-- file list segment: {title}, ndx_start = {st.ndx_start}")
    st.entries = []
    ndx = st.ndx_start
    shown = 0
    while True:
        quiet = shown >= max_entries
        target = Out() if quiet else out
        if not decode_entry(target, r, o, st, ndx):
            if quiet:
                out(f"  ... {shown - max_entries} entries not printed ...")
                out("\n".join(target.lines))
            break
        ndx += 1
        shown += 1
    out(f"-- end of segment: {len(st.entries)} entries, app-stream bytes so far 0x{r.pos:x}")
    ordered = sorted(st.entries, key=sort_key)
    moved = [e for i, e in enumerate(ordered) if e is not st.entries[i]]
    for i, e in enumerate(ordered):
        st.sorted_entries[st.ndx_start + i] = e
    if moved:
        out(f"   wire order differs from sorted order; transfer ndx use the sorted order:")
        for i, e in enumerate(ordered[:max_entries]):
            out(f"     sorted ndx {st.ndx_start + i} = {e['name']!r} (wire position ndx {e['ndx']})")
    else:
        out("   wire order already equals sorted order")
    for e in ordered:
        if stat.S_ISDIR(e["mode"]):
            st.dirs_sorted.append(e["name"])
    return len(st.entries)


def demux(raw: bytes, start: int, out: Out, label: str, max_frames: int = 40):
    """Split a multiplexed stream into (app_data, frames, messages)."""
    app = bytearray()
    frames = []
    pos = start
    while pos + 4 <= len(raw):
        hdr = int.from_bytes(raw[pos:pos + 4], "little")
        tag = (hdr >> 24) - MPLEX_BASE
        ln = hdr & 0xFFFFFF
        body = raw[pos + 4:pos + 4 + ln]
        if tag not in MSG_NAMES:
            raise Stop(f"{label}: unsupported mux tag {tag} at raw offset 0x{pos:x}")
        if pos + 4 + ln > len(raw):
            raise Stop(f"{label}: frame at raw offset 0x{pos:x} declares {ln} bytes, "
                       f"only {len(raw) - pos - 4} left")
        frames.append((pos, tag, ln, len(app)))
        if tag == 0:
            app += body
        pos += 4 + ln
    out(f"== {label}: {len(frames)} mux frames from raw offset 0x{start:x}, {len(app)} bytes of MSG_DATA")
    sizes = [f[2] for f in frames if f[1] == 0]
    if sizes:
        out(f"   MSG_DATA frame sizes: max {max(sizes)}, count {len(sizes)}, first {sizes[:12]}")
    for i, (p, tag, ln, appoff) in enumerate(frames):
        if tag != 0 or i < max_frames:
            nm = MSG_NAMES.get(tag, f"tag{tag}")
            extra = ""
            if tag != 0:
                body = raw[p + 4:p + 4 + ln]
                extra = f" body={body[:60]!r}"
            out(f"   raw 0x{p:07x}  hdr {raw[p:p+4].hex(' ')}  {nm:<14} len {ln:<6} app@0x{appoff:x}{extra}")
    if pos != len(raw):
        raise Stop(f"{label}: {len(raw) - pos} trailing raw bytes do not form a frame")
    return bytes(app), frames


def handshake_client(out: Out, r: Reader, o: Opts):
    s = r.pos
    v = r.int32()
    field_line(out, r, s, f"client protocol version {v}")
    return v


def handshake_server(out: Out, r: Reader, o: Opts):
    s = r.pos
    v = r.int32()
    field_line(out, r, s, f"server protocol version {v}")
    s = r.pos
    o.compat = r.varint()
    field_line(out, r, s, f"compat flags 0x{o.compat:x} = {names(o.compat, CF)}")
    return v


def negotiated(out: Out, r: Reader, o: Opts, who: str, compress: bool):
    s = r.pos
    cs = r.vstring()
    field_line(out, r, s, f"{who} checksum list {cs.decode()!r}")
    lists = [cs.decode().split()]
    if compress:
        s = r.pos
        cz = r.vstring()
        field_line(out, r, s, f"{who} compress list {cz.decode()!r}")
    return lists[0]


def read_filter_list(out: Out, r: Reader, title: str):
    out(f"-- filter list ({title})")
    while True:
        s = r.pos
        n = r.int32()
        if n == 0:
            field_line(out, r, s, "int32 0 = end of filter list")
            return
        rule = r.take(n)
        field_line(out, r, s, f"int32 len {n} + rule {rule!r}")


def read_files_from(out: Out, r: Reader):
    out("-- files-from names (client -> server, inside MSG_DATA right after the filter list)")
    s = r.pos
    buf = bytearray()
    while True:
        b = r.byte()
        buf.append(b)
        if buf.endswith(b"\0\0"):
            break
    field_line(out, r, s, f"names NUL-separated, double NUL ends: {bytes(buf)!r}")


def try_stats_tail(out: Out, r: Reader) -> bool:
    """A server sender ends with handle_stats(): five varlong30(x, 3) values
    (total_read, total_written, total_size, flist_buildtime, flist_xfertime,
    protocol >= 29) and then one final NDX_DONE. Accept it only if it
    consumes the stream exactly, otherwise leave the reader untouched."""
    probe = Reader(r.buf[r.pos:], r.base + r.pos)
    try:
        vals = [probe.varlong(3) for _ in range(5)]
        last = probe.byte()
    except Stop:
        return False
    if last != 0 or probe.left():
        return False
    s = r.pos
    r.take(probe.pos - 1)
    field_line(out, r, s, "stats: total_read={} total_written={} total_size={} "
               "flist_buildtime={} flist_xfertime={} (varlong min 3 each)".format(*vals))
    s = r.pos
    r.take(1)
    field_line(out, r, s, "final NDX_DONE")
    return True


def transfer_phase(out: Out, r: Reader, o: Opts, st: FlistState, role: str, max_items: int):
    """Walk the post-list stream of one side. role is 'sender' or 'generator'."""
    ndxs = NdxState()
    out(f"-- {role} stream after the initial file list")
    printed = 0
    counts = {"file": 0, "done": 0, "segments": 0}
    while r.left():
        s = r.pos
        ndx = ndxs.read(r)
        quiet = printed >= max_items
        tgt = Out() if quiet else out
        if ndx == NDX_DONE:
            counts["done"] += 1
            field_line(out, r, s, f"NDX_DONE (#{counts['done']})")
            if role == "sender" and o.sender_is_server and try_stats_tail(out, r):
                break
            continue
        if ndx == NDX_FLIST_EOF:
            field_line(out, r, s, "NDX_FLIST_EOF (no more file-list segments)")
            continue
        if ndx == NDX_DEL_STATS:
            field_line(out, r, s, "NDX_DEL_STATS")
            for nm in ("files", "dirs", "symlinks", "devices", "specials"):
                s2 = r.pos
                v = r.varint()
                field_line(out, r, s2, f"  deleted {nm} = {v}")
            continue
        if ndx <= NDX_FLIST_OFFSET:
            dir_ndx = NDX_FLIST_OFFSET - ndx
            counts["segments"] += 1
            parent = st.dirs_sorted[dir_ndx] if dir_ndx < len(st.dirs_sorted) else "?"
            wire_parent = st.dirs[dir_ndx] if dir_ndx < len(st.dirs) else "?"
            field_line(out, r, s, f"ndx {ndx} = NDX_FLIST_OFFSET - {dir_ndx}: file-list segment for "
                       f"dir_flist[{dir_ndx}] (sorted-order dir {parent!r}; wire-order dir {wire_parent!r})")
            if role != "sender":
                raise Stop("segment announced on the generator stream")
            prev_end = st.ndx_start + len(st.entries)
            st.ndx_start = prev_end + 1
            # Measured: ndx_start - 1 is not a file, it names the segment's
            # parent directory (rsync.c read_ndx_and_attrs: files[-1] ->
            # dir_flist->files[parent_ndx]).
            st.sorted_entries[st.ndx_start - 1] = {"name": f"{parent} (segment gap = parent dir)"}
            decode_flist(out, r, o, st, f"segment for dir_flist[{dir_ndx}] {parent!r}", 12)
            if st.entries and not all(e["name"].startswith(parent + "/") for e in st.entries):
                out(f"   !! segment content does not live under {parent!r}: dir_flist order hypothesis wrong")
            continue
        if ndx < 0:
            raise Stop(f"unexpected negative ndx {ndx}")
        counts["file"] += 1
        printed += 1
        if ndx not in st.sorted_entries:
            # An ndx no list defines means the walk is misaligned: never
            # label it and move on (that is how the stats tail was once read
            # as four phantom items).
            raise Stop(f"ndx {ndx} at 0x{s:x} is not in any decoded segment")
        e = st.sorted_entries.get(ndx, {})
        field_line(tgt, r, s, f"ndx {ndx} ({e.get('name', '?')!r})")
        s = r.pos
        iflags = r.shortint()
        field_line(tgt, r, s, f"iflags 0x{iflags:04x} = {names(iflags, ITEM)}")
        if iflags & ITEM_BASIS_TYPE_FOLLOWS:
            s = r.pos
            field_line(tgt, r, s, f"basis type {r.byte()}")
        if iflags & ITEM_XNAME_FOLLOWS:
            s = r.pos
            field_line(tgt, r, s, f"xname {r.vstring()!r}")
        if iflags & ITEM_REPORT_XATTR and o.has("X"):
            # Out-of-band xattr section (06-xattr-oob-wire-evidence.md):
            # varint(skip) [+ varint(len) datum on the sender] ... varint 0.
            while True:
                s = r.pos
                skip = r.varint()
                if skip == 0:
                    field_line(tgt, r, s, "xattr out-of-band section end (varint 0)")
                    break
                if role == "sender":
                    ln = r.varint()
                    r.take(ln)
                    field_line(tgt, r, s, f"xattr oob skip={skip} len={ln}")
                else:
                    field_line(tgt, r, s, f"xattr oob request skip={skip}")
        if not iflags & ITEM_TRANSFER:
            continue
        if o.has("n"):
            # Measured (c12-up-dryrun): with --dry-run both sides exchange
            # ndx + iflags only, no sum_head and no file data.
            continue
        s = r.pos
        count, blen, s2len, rem = r.int32(), r.int32(), r.int32(), r.int32()
        field_line(tgt, r, s, f"sum_head count={count} blength={blen} s2length={s2len} remainder={rem}")
        if role == "generator":
            if count:
                s = r.pos
                r.take(count * (4 + s2len))
                field_line(tgt, r, s, f"{count} block signatures")
        else:
            if o.has("z"):
                raise Stop("compressed token stream (-z) is not modelled by this decoder; "
                           "07-deflate-token-wire-evidence.md covers it")
            while True:
                s = r.pos
                tok = r.int32()
                if tok == 0:
                    field_line(tgt, r, s, "token 0 = end of file data")
                    break
                if tok > 0:
                    lit = r.take(tok)
                    field_line(tgt, r, s, f"literal {tok} bytes {lit[:16]!r}")
                else:
                    field_line(tgt, r, s, f"match block {-(tok + 1)}")
            s = r.pos
            field_line(tgt, r, s, f"whole-file checksum {r.take(o.csum_len).hex()}")
    if printed > max_items:
        out(f"  ... {printed - max_items} further file items not printed ...")
    out(f"-- {role} summary: {counts['file']} file items, {counts['segments']} extra segments, "
        f"{counts['done']} NDX_DONE")


def read_capture(p: Path) -> bytes:
    """Frozen captures above 256 KiB are stored gzip-compressed."""
    if p.exists():
        return p.read_bytes()
    return gzip.decompress(p.with_name(p.name + ".gz").read_bytes())


def first_ndx_start(o: Opts) -> int:
    """Measured on the frozen upload: with INC_RECURSE negotiated the first
    file of the first segment is ndx 1, not 0 (flist.c starts the numbering
    at 1 so that a segment boundary always leaves a gap)."""
    return 1 if o.inc_recurse else 0


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("capture_dir", type=Path)
    ap.add_argument("--max-entries", type=int, default=40)
    ap.add_argument("--max-items", type=int, default=12)
    ap.add_argument("--files-from", action="store_true",
                    help="client sends a files-from name list after the filter list")
    a = ap.parse_args()
    d = a.capture_dir
    cmd = (d / "remote_command.txt").read_text().strip()
    cin = read_capture(d / "capture_in.bin")
    cout = read_capture(d / "capture_out.bin")
    short, longs, caps = parse_remote_command(cmd)
    o = Opts(sender_is_server="--sender" in longs, short=short, long_opts=longs, caps=caps)
    out = Out()
    out(f"# {d.name}")
    out(f"remote command: {cmd}")
    out(f"server short options: {short!r}  capabilities: {caps!r}  long: {longs}")
    out(f"raw bytes: client->server {len(cin)}, server->client {len(cout)}")
    out("")
    compress = "z" in short
    try:
        rc = Reader(cin)
        rs = Reader(cout)
        out("== handshake, server -> client")
        sv = handshake_server(out, rs, o)
        out("== handshake, client -> server")
        cv = handshake_client(out, rc, o)
        o.protocol = min(sv, cv)
        out(f"   negotiated protocol {o.protocol}")
        if "v" in caps:
            out("== negotiated strings")
            negotiated(out, rc, o, "client", compress)
            srv = negotiated(out, rs, o, "server", compress)
            cli = rc.buf[:rc.pos]
        s = rs.pos
        seed = rs.int32()
        field_line(out, rs, s, f"checksum seed {seed}")
        # First common algorithm in the client's order wins (compat.c).
        cli_list = cin[5:5 + cin[4]].decode().split() if "v" in caps else ["md5"]
        winner = next((x for x in cli_list if x in srv), "md5") if "v" in caps else "md5"
        o.csum_len = CSUM_LEN.get(winner, 16)
        out(f"   checksum winner {winner!r} -> {o.csum_len} bytes")
        out("")
        st = FlistState()
        if o.sender_is_server:
            # Measured on the frozen download (2026-09-24): the receiving
            # client multiplexes from the first byte after the negotiated
            # strings, so its filter list travels inside MSG_DATA.
            out(f"   client raw handshake ends at 0x{rc.pos:x}")
            out("")
            app_s, _ = demux(cout, rs.pos, out, "server -> client (sender)")
            app_c, _ = demux(cin, rc.pos, out, "client -> server (generator)")
            out("")
            rg = Reader(app_c)
            read_filter_list(out, rg, "client -> server, first bytes of MSG_DATA")
            if a.files_from:
                read_files_from(out, rg)
            out("")
            st.ndx_start = first_ndx_start(o)
            ra = Reader(app_s)
            decode_flist(out, ra, o, st, "initial", a.max_entries)
            if not o.inc_recurse:
                read_id_lists(out, ra, o)
            transfer_phase(out, ra, o, st, "sender", a.max_items)
            out("")
            transfer_phase(out, rg, o, st, "generator", a.max_items)
        else:
            out(f"   client raw handshake ends at 0x{rc.pos:x}")
            out("")
            app_c, _ = demux(cin, rc.pos, out, "client -> server (sender)")
            app_s, _ = demux(cout, rs.pos, out, "server -> client (generator)")
            out("")
            ra = Reader(app_c)
            # With --delete or --prune-empty-dirs the sending client ships the
            # filter list to the receiving server, inside the multiplexed stream.
            if "--delete" in " ".join(longs) or any(x.startswith("--delete") for x in longs):
                read_filter_list(out, ra, "client -> server, inside MSG_DATA, receiver wants it")
            st.ndx_start = first_ndx_start(o)
            decode_flist(out, ra, o, st, "initial", a.max_entries)
            if not o.inc_recurse:
                read_id_lists(out, ra, o)
            transfer_phase(out, ra, o, st, "sender", a.max_items)
            out("")
            transfer_phase(out, Reader(app_s), o, st, "generator", a.max_items)
    except Stop as e:
        out(f"!! STOP: {e}")
        print("\n".join(out.lines))
        return 3
    print("\n".join(out.lines))
    return 0


if __name__ == "__main__":
    sys.exit(main())
