#!/usr/bin/env python3
"""B1 addendum wire campaign: the file-list shapes B0 did not capture.

Measures what stock rsync puts on the wire for the cases the B1 codec had
left unverified, so each one is decoded against bytes instead of against a
reading of flist.c:

- devices (character and block, a minor above 255, a major repeated for
  XMIT_SAME_RDEV_MAJOR), fifos and sockets, with and without -D;
- a symlink listed without -l;
- names whose suffix passes 255 bytes (XMIT_LONG_NAME) and a path of about
  3,700 bytes;
- names that are not UTF-8;
- a list that ends with an I/O error (an unreadable directory);
- the classic, non-varint flags of a server older than 3.2 (upstream 3.1.3),
  on the same shapes.

It reuses `b0c0_wire_campaign.py` (trees, run, decode, freeze) and points it
at two containers started by hand, each with its own name, port and
workspace, so the B0 fixture and its port are neither reused nor removed:

    docker run -d --name aeroftp-rsync-327-b1 -p 127.0.0.1:2237:22 \\
        -v <capture>/keys:/keys:ro -v <capture>/workspace/b1-327:/workspace \\
        aeroftp-rsync-b0c0:latest
    docker run -d --name aeroftp-rsync-313-b1 -p 127.0.0.1:2236:22 \\
        -v <capture>/keys:/keys:ro -v <capture>/workspace/b1-313:/workspace \\
        <image built from Dockerfile.rsync-3.1.3-sshd>

Device nodes, the fifo and the socket are created inside the container as
root (the host session cannot mknod, and a socket path on the host is past
the 108-byte sun_path limit), then every mtime of the tree is pinned again.

Usage:
    python3 b1_wire_campaign.py [--only RUN ...] [--freeze] [--keep-going]
Output: workspace/b1-*/b0c0/out/<run>/ (gitignored); --freeze copies into
artifacts_real/frozen/b1/.
"""
from __future__ import annotations

import argparse
import os
import shutil
import sys
from pathlib import Path

import b0c0_wire_campaign as base

CAPTURE = base.CAPTURE
FROZEN_B1 = CAPTURE / "artifacts_real" / "frozen" / "b1"
NOINC = base.NOINC
MTIME = base.MTIME

# (container, port, workspace, capture root inside the workspace)
SERVERS = {
    "327": ("aeroftp-rsync-327-b1", 2237, CAPTURE / "workspace" / "b1-327", "real_capture"),
    "313": ("aeroftp-rsync-313-b1", 2236, CAPTURE / "workspace" / "b1-313", "deflate_capture"),
}

# A run that meets an unreadable directory ends with rsync's "some files
# could not be transferred" (23): that is the scenario, not a failure.
EXPECTED_RC = {"ioerr": 23}


# ---------------------------------------------------------------- trees

def tree_dv(r: Path):
    """Regular file and symlink on the host; the rest is made in the
    container by `finish_dv` (devices, fifo, socket)."""
    base.wfile(r / "dv" / "target.txt", b"target\n")
    os.symlink("target.txt", r / "dv" / "ln")


# name, type, major, minor. Two majors of 1 in a row give SAME_RDEV_MAJOR;
# minor 300 does not fit a byte; 259/70000 needs multi-byte varints.
DEVICES = [
    ("cnull", "c", 1, 3),
    ("czero", "c", 1, 5),
    ("bloop", "b", 7, 0),
    ("cwide", "c", 4, 300),
    ("cbig", "c", 259, 70000),
]


def finish_dv(container: str, tree_root_in_container: str):
    d = f"{tree_root_in_container}/dv"
    script = [f"mkfifo -m 0644 {d}/fifo",
              f"python3 -c \"import socket; socket.socket(socket.AF_UNIX).bind('{d}/sock')\""]
    for name, kind, major, minor in DEVICES:
        script.append(f"mknod -m 0644 {d}/{name} {kind} {major} {minor}")
    script.append(f"find {tree_root_in_container} -exec touch -h -d @{MTIME} {{}} +")
    base.sh(["docker", "exec", "-u", "root", container, "sh", "-c", " && ".join(script)])


def tree_ln(r: Path):
    long_a, long_b, leaf = "a" * 250, "b" * 250, "z" * 250
    base.wfile(r / "ln" / long_a / leaf, b"under a\n")
    base.wfile(r / "ln" / long_b / leaf, b"under b\n")
    deep = r / "ln" / "d"
    for _ in range(14):
        deep = deep / ("e" * 250)
    base.wfile(deep / ("f" * 200), b"deep\n")


def tree_nu(r: Path):
    root = bytes(r / "nu")
    os.makedirs(root + b"/d\xff", exist_ok=True)
    for p in (root + b"/caf\xe9.txt", root + b"/d\xff/x"):
        with open(p, "wb") as f:
            f.write(b"latin-1\n")


def tree_ioe(r: Path):
    base.wfile(r / "ioe" / "ok.txt", b"readable\n")
    base.wfile(r / "ioe" / "locked" / "hidden.txt", b"unreachable\n")


def lock_ioe(root: Path):
    os.chmod(root / "ioe" / "locked", 0)


def unlock_all(root: Path):
    """shutil.rmtree cannot enter a mode-000 directory."""
    if not root.exists():
        return
    for dirpath, dirnames, _ in os.walk(root):
        for d in dirnames:
            p = Path(dirpath) / d
            if not p.is_symlink():
                os.chmod(p, 0o755)
    locked = root / "ioe" / "locked"
    if locked.exists():
        os.chmod(locked, 0o755)


TREES = dict(base.TREES)
TREES.update({"dv": tree_dv, "ln": tree_ln, "nu": tree_nu, "ioe": tree_ioe})

# (run id, point, tree, direction, source path inside the tree, client
#  options, decoder extra args, server)
RUNS = [
    ("b1-dev-dl-full", 20, "dv", "dl", "dv", ["-a"], [], "327"),
    ("b1-dev-dl-nodev", 20, "dv", "dl", "dv", ["-rltp"], [], "327"),
    ("b1-dev-dl-nolinks", 20, "dv", "dl", "dv", ["-rtpD"], [], "327"),
    ("b1-dev-dl-noinc", 20, "dv", "dl", "dv", ["-a", NOINC], [], "327"),
    ("b1-long-dl", 21, "ln", "dl", "ln", ["-a"], [], "327"),
    ("b1-long-up", 21, "ln", "up", "ln", ["-a"], [], "327"),
    ("b1-long-dl-noinc", 21, "ln", "dl", "ln", ["-a", NOINC], [], "327"),
    ("b1-nonutf8-dl", 22, "nu", "dl", "nu", ["-a"], [], "327"),
    ("b1-ioerr-dl", 23, "ioe", "dl", "ioe", ["-a"], [], "327"),
    ("b1-ioerr-dl-noinc", 23, "ioe", "dl", "ioe", ["-a", NOINC], [], "327"),
    ("b1-313-p1-dl", 24, "p1", "dl", "d", ["-a"], [], "313"),
    ("b1-313-p1-up", 24, "p1", "up", "d", ["-a"], [], "313"),
    ("b1-313-p1c-dl", 24, "p1", "dl", "d", ["-a", "-c"], [], "313"),
    ("b1-313-p2-dl-noinc", 24, "p2", "dl", "t", ["-a", NOINC], [], "313"),
    ("b1-313-p7-dl-product", 24, "p7", "dl", "g", ["-rltp"], [], "313"),
    ("b1-313-dev-dl", 24, "dv", "dl", "dv", ["-a"], [], "313"),
    ("b1-313-long-dl", 24, "ln", "dl", "ln", ["-a"], [], "313"),
    ("b1-313-ioerr-dl", 24, "ioe", "dl", "ioe", ["-a"], [], "313"),
]


def point_at(server: str):
    """Aim the B0 campaign's globals at one of the two containers."""
    container, port, ws, capture_root = SERVERS[server]
    base.CONTAINER = container
    base.PORT = port
    base.WS = ws
    base.B0 = ws / "b0c0"
    base.SRC = base.B0 / "src"
    base.OUT = base.B0 / "out"
    base.REAL_CAPTURE = ws / capture_root
    base.REAL_CAPTURE.mkdir(parents=True, exist_ok=True)
    base.FROZEN = FROZEN_B1
    base.TREES = TREES


def build_trees(server: str, runs):
    container = SERVERS[server][0]
    for tree in sorted({r[2] for r in runs}):
        root = base.SRC / tree
        if root.exists():
            if tree == "dv":
                # Root-owned nodes from the previous build.
                base.sh(["docker", "exec", "-u", "root", container, "rm", "-rf",
                         f"/workspace/b0c0/src/{tree}"])
            unlock_all(root)
            if root.exists():
                shutil.rmtree(root)
        root.mkdir(parents=True)
        TREES[tree](root)
        base.fix_times(root)
        if tree == "dv":
            finish_dv(container, f"/workspace/b0c0/src/{tree}")
        if tree == "ioe":
            lock_ioe(root)


def base_run(run):
    """The 7-tuple shape the B0 functions take."""
    return run[:7]


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--only", nargs="*", default=[])
    ap.add_argument("--freeze", action="store_true", help="only copy out/ into frozen/b1/")
    ap.add_argument("--keep-going", action="store_true",
                    help="report a decoder stop and go on with the next run (investigation only; freeze still refuses it)")
    a = ap.parse_args()
    selected = [r for r in RUNS if not a.only or r[0] in a.only]
    key = CAPTURE / "keys" / "id_ed25519"
    for server in ("327", "313"):
        runs = [r for r in selected if r[7] == server]
        if not runs:
            continue
        point_at(server)
        base.RUNS = [base_run(r) for r in runs]
        if a.freeze:
            base.freeze([r[0] for r in runs])
            continue
        build_trees(server, runs)
        for run in runs:
            expected_rc = next((rc for k, rc in EXPECTED_RC.items() if k in run[0]), 0)
            try:
                dest = base.run_one(base_run(run), key)
            except SystemExit as stop:
                dest = base.OUT / run[0]
                rc = (dest / "client.rc.txt").read_text().strip() if (dest / "client.rc.txt").exists() else "?"
                if expected_rc and rc == str(expected_rc) and (dest / "capture_out.bin").exists():
                    (dest / "point.txt").write_text(f"{run[1]}\n")
                else:
                    raise stop
            try:
                base.decode(base_run(run), dest)
            except SystemExit as stop:
                if not a.keep_going:
                    raise
                print(f"[b1] {run[0]}: {stop}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
