#!/usr/bin/env python3
"""B0 + C0 wire campaign: one fixture, every multi-entry and flag scenario.

Measures what stock rsync 3.2.7 puts on the wire for the thirteen points of
`BATON-B0-C0-2026-09-04-executor.md` (appendix AERORSYNC), so the multi-entry
codec (B1-B3) and the crate flags (C1-C5) are written against bytes, not
against a reading of flist.c.

It reuses the real-rsync lane image (`Dockerfile.real-rsync-sshd`, the
`rsync_proxy.py` tee behind sshd's ForceCommand) but runs it as its own
container on its own port, so a lane-3 or real-lane container left running by
another checkout is neither reused nor removed. Point 8 (a 50,000-entry list)
is what exposed the proxy's blocking-write deadlock fixed alongside it.

Every scenario builds a deterministic tree (fixed mtimes, fixed modes, fixed
contents), runs the host `rsync` client once through the proxy, and keeps the
single session the proxy wrote. A run that produces zero or two sessions is a
hard failure: an ambiguous capture is worse than none.

Usage:
    python3 b0c0_wire_campaign.py [--only RUN ...] [--keep-stack]
Output: workspace/b0c0/out/<run>/ (gitignored). Freezing into
artifacts_real/frozen/b0c0/ is a separate, explicit step (--freeze).
"""
from __future__ import annotations

import argparse
import gzip
import hashlib
import json
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

CAPTURE = Path(__file__).resolve().parent
WS = CAPTURE / "workspace"
B0 = WS / "b0c0"
SRC = B0 / "src"
OUT = B0 / "out"
REAL_CAPTURE = WS / "real_capture"
FROZEN = CAPTURE / "artifacts_real" / "frozen" / "b0c0"

IMAGE = "aeroftp-rsync-b0c0:latest"
CONTAINER = "aeroftp-rsync-b0c0"
PORT = int(os.environ.get("B0C0_PORT", "2234"))
MTIME = 1_700_000_000
NAMED_UID = 12345
# Captures larger than this are frozen gzip-compressed.
GZIP_OVER = 256 * 1024


def sh(cmd, **kw):
    return subprocess.run(cmd, check=True, **kw)


# ---------------------------------------------------------------- trees

def wfile(p: Path, data: bytes, mode=0o644):
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_bytes(data)
    os.chmod(p, mode)


def fix_times(root: Path):
    for dirpath, dirnames, filenames in os.walk(root, topdown=False):
        for n in filenames:
            os.utime(Path(dirpath) / n, (MTIME, MTIME), follow_symlinks=False)
        os.utime(dirpath, (MTIME, MTIME))


def tree_p1(r: Path):
    wfile(r / "d" / "alpha.txt", b"a" * 10)
    wfile(r / "d" / "alpha2.txt", b"b" * 20)


def tree_p2(r: Path):
    wfile(r / "t" / "a.txt", b"top\n")
    (r / "t" / "b").mkdir(parents=True)
    wfile(r / "t" / "x" / "f1", b"one\n")
    wfile(r / "t" / "x" / "y" / "f2", b"two\n")
    wfile(r / "t" / "x" / "y" / "z" / "leaf.txt", b"leaf\n")


def tree_p3(r: Path):
    for n, attrs in (("f1", {"user.color": b"blue", "user.tag": b"x"}),
                     ("f2", {"user.color": b"blue", "user.tag": b"x"}),
                     ("f3", {"user.color": b"red"})):
        p = r / "xa" / n
        wfile(p, n.encode() * 4)
        for k, v in attrs.items():
            os.setxattr(p, k, v)


def tree_p4(r: Path):
    for n, perm in (("f1", "r--"), ("f2", "r--"), ("f3", "rw-")):
        p = r / "ac" / n
        wfile(p, n.encode() * 4)
        sh(["setfacl", "-m", f"u:{NAMED_UID}:{perm}", str(p)])


def tree_p5(r: Path):
    d = r / "da"
    d.mkdir(parents=True)
    sh(["setfacl", "-d", "-m", f"u:{NAMED_UID}:rwx", str(d)])
    (d / "sub").mkdir()  # inherits the default ACL from its parent
    wfile(d / "f", b"inherits\n")


def tree_p6(r: Path):
    h = r / "h"
    wfile(h / "a", b"shared-a\n")
    os.link(h / "a", h / "b")
    wfile(h / "c", b"shared-c\n")
    (h / "sub").mkdir()
    os.link(h / "a", h / "sub" / "d")
    os.link(h / "c", h / "sub" / "e")
    wfile(h / "z", b"alone\n")


def tree_p7(r: Path):
    wfile(r / "g" / "f1", b"one\n")
    wfile(r / "g" / "f2", b"two\n")


def tree_p8(r: Path):
    # Hash-like names defeat the same-name prefix compression, so the list
    # really grows past 2 MiB instead of collapsing into a few bytes per entry.
    d = r / "big"
    d.mkdir(parents=True)
    for i in range(50_000):
        (d / (hashlib.sha1(str(i).encode()).hexdigest() + ".bin")).touch()


def tree_p9(r: Path):
    wfile(r / "r" / "top.txt", b"top\n")
    wfile(r / "r" / "d1" / "f1", b"1\n")
    wfile(r / "r" / "d1" / "f2", b"2\n")
    wfile(r / "r" / "d2" / "f3", b"3\n")
    wfile(r / "r" / "d2" / "sub" / "f4", b"4\n")
    (r / "r" / "d3").mkdir()


def tree_p9big(r: Path):
    # Past rsync's file-count lookahead (about 1,000 entries) the sender stops
    # front-loading segments and interleaves them with file transfers.
    for d in range(40):
        for f in range(60):
            wfile(r / "w" / f"d{d:02d}" / f"f{f:02d}", b"x")


def tree_c(r: Path):
    wfile(r / "fl" / "keep" / "a.txt", b"a\n")
    wfile(r / "fl" / "keep" / "b.log", b"b\n")
    wfile(r / "fl" / "tmp" / "t.txt", b"t\n")
    wfile(r / "fl" / "x.log", b"x\n")
    wfile(r / "fl" / "y.txt", b"y\n")


TREES = {"p1": tree_p1, "p2": tree_p2, "p3": tree_p3, "p4": tree_p4, "p5": tree_p5,
         "p6": tree_p6, "p7": tree_p7, "p8": tree_p8, "p9": tree_p9, "p9big": tree_p9big, "c": tree_c}

# ---------------------------------------------------------------- runs
# (run id, point, tree, direction, source path inside the tree, client options,
#  decoder extra args). Direction "up" = client sender, server receiver;
#  "dl" = server sender (--sender in the remote command).
NOINC = "--no-inc-recursive"
RUNS = [
    ("p1-up", 1, "p1", "up", "d", ["-a"], []),
    ("p1-dl", 1, "p1", "dl", "d", ["-a"], []),
    ("p1c-dl", 1, "p1", "dl", "d", ["-a", "-c"], []),
    ("p2-up-noinc", 2, "p2", "up", "t", ["-a", NOINC], []),
    ("p2-dl-noinc", 2, "p2", "dl", "t", ["-a", NOINC], []),
    ("p3-up", 3, "p3", "up", "xa", ["-aX"], []),
    ("p3-dl", 3, "p3", "dl", "xa", ["-aX"], []),
    ("p4-up", 4, "p4", "up", "ac", ["-aA"], []),
    ("p4-dl", 4, "p4", "dl", "ac", ["-aA"], []),
    ("p5-up", 5, "p5", "up", "da", ["-aA"], []),
    ("p5-dl", 5, "p5", "dl", "da", ["-aA"], []),
    ("p6-up", 6, "p6", "up", "h", ["-aH"], []),
    ("p6-dl", 6, "p6", "dl", "h", ["-aH"], []),
    ("p6-up-noinc", 6, "p6", "up", "h", ["-aH", NOINC], []),
    ("p6-dl-noinc", 6, "p6", "dl", "h", ["-aH", NOINC], []),
    ("p6c-dl", 6, "p6", "dl", "h", ["-aHc"], []),
    ("p7-up-full", 7, "p7", "up", "g", ["-a"], []),
    ("p7-dl-full", 7, "p7", "dl", "g", ["-a"], []),
    ("p7-up-product", 7, "p7", "up", "g", ["-rltp"], []),
    ("p7-dl-product", 7, "p7", "dl", "g", ["-rltp"], []),
    ("p8-up", 8, "p8", "up", "big", ["-a"], ["--max-entries", "6", "--max-items", "4"]),
    ("p8-dl", 8, "p8", "dl", "big", ["-a"], ["--max-entries", "6", "--max-items", "4"]),
    ("p9-up-inc", 9, "p9", "up", "r", ["-a"], []),
    ("p9-dl-inc", 9, "p9", "dl", "r", ["-a"], []),
    ("p9-up-noinc", 9, "p9", "up", "r", ["-a", NOINC], []),
    ("p9-dl-noinc", 9, "p9", "dl", "r", ["-a", NOINC], []),
    ("p9big-up-inc", 9, "p9big", "up", "w", ["-a"], ["--max-entries", "4", "--max-items", "3"]),
    ("p9big-dl-inc", 9, "p9big", "dl", "w", ["-a"], ["--max-entries", "4", "--max-items", "3"]),
    ("c10-dl-inc", 10, "c", "dl", "fl",
     ["-a", "--include=keep/b.log", "--exclude=*.log", "--filter=- tmp/"], []),
    ("c10-dl-noinc", 10, "c", "dl", "fl",
     ["-a", NOINC, "--include=keep/b.log", "--exclude=*.log", "--filter=- tmp/"], []),
    ("c10-up", 10, "c", "up", "fl",
     ["-a", "--include=keep/b.log", "--exclude=*.log", "--filter=- tmp/"], []),
    ("c11-dl", 11, "c", "dl", "fl/", ["-a", "--files-from=@LIST@"], ["--files-from"]),
    ("c12-up-mkpath", 12, "c", "up", "fl/y.txt", ["-a", "--mkpath"], []),
    ("c12-up-dryrun", 12, "c", "up", "fl", ["-a", "--dry-run"], []),
    ("c13-up-delete", 13, "c", "up", "fl", ["-a", "--delete"], []),
]


def ssh_e(key: Path) -> str:
    return (f"ssh -i {key} -p {PORT} -o StrictHostKeyChecking=no "
            "-o UserKnownHostsFile=/dev/null -o BatchMode=yes -o ConnectTimeout=5 "
            "-o LogLevel=ERROR")


def stack_up():
    sh(["bash", "-c", f'source "{CAPTURE}/fixture_key.sh" && ensure_fixture_key "{CAPTURE}"'])
    sh(["docker", "build", "-q", "-t", IMAGE, "-f", str(CAPTURE / "Dockerfile.real-rsync-sshd"),
        "--build-arg", f"TESTUSER_UID={os.getuid()}", "--build-arg", f"TESTUSER_GID={os.getgid()}",
        str(CAPTURE)], stdout=subprocess.DEVNULL)
    subprocess.run(["docker", "rm", "-f", CONTAINER], capture_output=True)
    WS.mkdir(exist_ok=True)
    REAL_CAPTURE.mkdir(parents=True, exist_ok=True)
    sh(["docker", "run", "-d", "--name", CONTAINER, "-p", f"127.0.0.1:{PORT}:22",
        "-v", f"{CAPTURE / 'keys'}:/keys:ro", "-v", f"{WS}:/workspace", IMAGE],
       stdout=subprocess.DEVNULL)
    for _ in range(30):
        r = subprocess.run(["rsync", "-e", ssh_e(CAPTURE / "keys" / "id_ed25519"), "--list-only",
                            "testuser@127.0.0.1:/workspace/"], capture_output=True)
        if r.returncode == 0:
            break
        time.sleep(1)
    else:
        raise SystemExit("fixture did not answer on port %d" % PORT)
    ver = subprocess.run(["docker", "exec", CONTAINER, "rsync", "--version"],
                         capture_output=True, text=True).stdout.splitlines()[0]
    print(f"[b0c0] server: {ver}")
    print(f"[b0c0] client: {subprocess.run(['rsync', '--version'], capture_output=True, text=True).stdout.splitlines()[0]}")
    # The probe above went through the proxy too: clear what it captured.
    clear_sessions()


def stack_down():
    subprocess.run(["docker", "rm", "-f", CONTAINER], capture_output=True)


def clear_sessions():
    for p in REAL_CAPTURE.iterdir():
        shutil.rmtree(p) if p.is_dir() else p.unlink()


def build_trees(only):
    wanted = {r[2] for r in RUNS if not only or r[0] in only}
    for t in sorted(wanted):
        root = SRC / t
        if root.exists():
            shutil.rmtree(root)
        root.mkdir(parents=True)
        TREES[t](root)
        fix_times(root)


# The p8 scenarios take a few minutes each; anything far past that is a stall
# (the proxy deadlock p8 once exposed), and must fail with diagnostics instead
# of blocking the campaign forever.
CLIENT_TIMEOUT_S = 1200


def build_argv(run, key: Path, dest: Path) -> list:
    """The client argv of one run. Pure: freezing re-derives it without
    re-running the scenario."""
    rid, _, tree, direction, rel, opts, _ = run
    opts = [o.replace("@LIST@", str(dest / "files-from.txt")) for o in opts]
    remote = "testuser@127.0.0.1:/workspace/b0c0"
    if direction == "up":
        if rid == "c12-up-mkpath":
            return ["rsync", "-e", ssh_e(key), *opts, str(SRC / tree / rel),
                    f"{remote}/up/{rid}/new/deep/"]
        return ["rsync", "-e", ssh_e(key), *opts, str(SRC / tree / rel), f"{remote}/up/{rid}/"]
    return ["rsync", "-e", ssh_e(key), *opts, f"{remote}/src/{tree}/{rel}",
            str(B0 / "dl" / rid) + "/"]


def write_argv(dest: Path, argv: list):
    # JSON, not " ".join: `--filter=- tmp/` must stay one argument.
    (dest / "client.argv.json").write_text(json.dumps(argv, indent=1) + "\n")


def run_one(run, key: Path) -> Path:
    rid, point, tree, direction, rel, opts, _ = run
    dest = OUT / rid
    if dest.exists():
        shutil.rmtree(dest)
    dest.mkdir(parents=True)
    clear_sessions()
    if any("@LIST@" in o for o in opts):
        (dest / "files-from.txt").write_text("keep/a.txt\ny.txt\n")
    target = B0 / ("up" if direction == "up" else "dl") / rid
    if target.exists():
        shutil.rmtree(target)
    target.mkdir(parents=True)
    if rid == "c13-up-delete":
        wfile(target / "fl" / "stale.txt", b"to be deleted\n")
        fix_times(target)
    if rid == "c12-up-mkpath":
        shutil.rmtree(target)  # --mkpath must create up/<rid>/new/deep/
    argv = build_argv(run, key, dest)
    write_argv(dest, argv)
    try:
        r = subprocess.run(argv, capture_output=True, timeout=CLIENT_TIMEOUT_S)
    except subprocess.TimeoutExpired as e:
        (dest / "client.stdout.txt").write_bytes(e.stdout or b"")
        (dest / "client.stderr.txt").write_bytes(e.stderr or b"")
        (dest / "client.rc.txt").write_text("timeout\n")
        raise SystemExit(f"[b0c0] {rid}: rsync did not finish in {CLIENT_TIMEOUT_S} s "
                         f"(stalled proxy or server?); partial output in {dest}")
    (dest / "client.stdout.txt").write_bytes(r.stdout)
    (dest / "client.stderr.txt").write_bytes(r.stderr)
    (dest / "client.rc.txt").write_text(f"{r.returncode}\n")
    # The proxy writes end.txt after the server exits; wait for it.
    for _ in range(50):
        sessions = [p for p in REAL_CAPTURE.iterdir() if p.is_dir()]
        if sessions and all((p / "end.txt").exists() for p in sessions):
            break
        time.sleep(0.1)
    sessions = [p for p in REAL_CAPTURE.iterdir() if p.is_dir()]
    if len(sessions) != 1:
        raise SystemExit(f"[b0c0] {rid}: expected exactly one proxied session, got {len(sessions)}")
    for f in sessions[0].iterdir():
        shutil.copy2(f, dest / f.name)
    if r.returncode != 0:
        raise SystemExit(f"[b0c0] {rid}: rsync exited {r.returncode}: {r.stderr.decode(errors='replace')}")
    (dest / "point.txt").write_text(f"{point}\n")
    return dest


def decode(run, dest: Path):
    extra = run[6]
    r = subprocess.run([sys.executable, str(CAPTURE / "decode_rsync_wire.py"), str(dest), *extra],
                       capture_output=True, text=True)
    (dest / "decoded.txt").write_text(r.stdout + r.stderr)
    status = "ok" if r.returncode == 0 else f"STOP rc={r.returncode}"
    last = [l for l in r.stdout.splitlines() if l.startswith("!! STOP")]
    print(f"[b0c0] {run[0]:<16} point {run[1]:>2}  {dest.name}: decode {status} {last[0] if last else ''}")
    if r.returncode != 0:
        raise SystemExit(f"[b0c0] {run[0]}: decoder exited {r.returncode}, capture rejected")


def sanitize_argv_json(data: bytes) -> bytes:
    """The station's absolute paths (home, key) do not belong in the
    repository; the argv shape is what the evidence needs. Replace on the
    decoded strings, not on the serialized bytes: json.dumps escapes
    non-ASCII, backslashes and quotes, so a byte-level match could miss the
    path and publish it."""
    argv = [a.replace(str(CAPTURE), "<capture>") for a in json.loads(data)]
    if any(str(CAPTURE) in a for a in argv):
        raise SystemExit("[b0c0] argv still carries the checkout path after sanitizing")
    return (json.dumps(argv, indent=1) + "\n").encode()


def freeze(only):
    """Copy the captures into the versioned frozen tree. Big ones are gzipped."""
    FROZEN.mkdir(parents=True, exist_ok=True)
    for run in RUNS:
        if only and run[0] not in only:
            continue
        src = OUT / run[0]
        if not src.exists():
            continue
        # An out/ tree can outlive the run that checked it: decode again
        # here and refuse to publish anything the decoder does not finish.
        check = subprocess.run(
            [sys.executable, str(CAPTURE / "decode_rsync_wire.py"), str(src), *run[6]],
            capture_output=True, text=True)
        if check.returncode != 0:
            raise SystemExit(f"[b0c0] {run[0]}: refusing to freeze, decoder exited {check.returncode}")
        dst = FROZEN / run[0]
        if dst.exists():
            shutil.rmtree(dst)
        dst.mkdir()
        for f in sorted(src.iterdir()):
            if f.name in ("start.txt", "end.txt", "decoded.txt", "client.argv.txt"):
                # Timestamps carry no protocol content; decoded.txt is
                # regenerated from the capture by decode_rsync_wire.py.
                continue
            data = f.read_bytes()
            if f.name == "client.argv.json":
                data = sanitize_argv_json(data)
            if f.suffix == ".bin" and len(data) > GZIP_OVER:
                (dst / (f.name + ".gz")).write_bytes(gzip.compress(data, mtime=0))
            else:
                (dst / f.name).write_bytes(data)
    print(f"[b0c0] frozen into {FROZEN}")


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--only", nargs="*", default=[])
    ap.add_argument("--keep-stack", action="store_true")
    ap.add_argument("--freeze", action="store_true", help="only copy out/ into frozen/")
    a = ap.parse_args()
    if a.freeze:
        freeze(a.only)
        return 0
    build_trees(a.only)
    stack_up()
    key = CAPTURE / "keys" / "id_ed25519"
    try:
        for run in RUNS:
            if a.only and run[0] not in a.only:
                continue
            dest = run_one(run, key)
            decode(run, dest)
    finally:
        if not a.keep_stack:
            stack_down()
    return 0


if __name__ == "__main__":
    sys.exit(main())
