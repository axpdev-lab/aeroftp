#!/usr/bin/env python3
"""Byte-oracle proxy for the real-rsync Docker lane.

Invoked by sshd's ForceCommand via `rsync_server_wrapper.sh`. Reads
`$AEROFTP_SSH_ORIGINAL_COMMAND` from the environment, spawns it as a
subprocess, and shuttles bytes between:

    wrapper stdin  (from SSH)  -->  subprocess stdin
    subprocess stdout          -->  wrapper stdout (back to SSH)
    subprocess stderr          -->  wrapper stderr (back to SSH)

While shuttling, it tees each direction to a file under
`$AEROFTP_CAPTURE_DIR/{capture_in.bin, capture_out.bin, stderr.txt}` so
S8b and later sinergie have a deterministic byte-level transcript of the
real rsync protocol.

Why this over a bash pipeline: bash `tee | cmd | tee` deadlocks on
subprocess exit because the upstream `tee` cannot learn that downstream
is gone. Bash FIFO variants have fd-sharing bugs between main shell and
forked `bash -c`. Python with `select.select` and explicit `os.read` /
`os.write` has no buffering surprises and exits cleanly as soon as the
subprocess exits, without waiting on a phantom stdin EOF.
"""
from __future__ import annotations

import os
import select
import signal
import subprocess
import sys
from pathlib import Path


CHUNK = 64 * 1024


def main() -> int:
    capture_dir = Path(os.environ["AEROFTP_CAPTURE_DIR"])
    cmd = os.environ["AEROFTP_SSH_ORIGINAL_COMMAND"]

    capture_in = open(capture_dir / "capture_in.bin", "wb", buffering=0)
    capture_out = open(capture_dir / "capture_out.bin", "wb", buffering=0)
    capture_err = open(capture_dir / "stderr.txt", "ab", buffering=0)

    # Use /bin/bash so `$SSH_ORIGINAL_COMMAND` parses with the same quoting
    # rules OpenSSH would apply when no ForceCommand is set. Inherit no
    # extra env beyond what we already have.
    proc = subprocess.Popen(
        ["/bin/bash", "-c", cmd],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        close_fds=True,
    )

    client_in_fd = sys.stdin.fileno()
    client_out_fd = sys.stdout.fileno()
    client_err_fd = sys.stderr.fileno()
    proc_in_fd = proc.stdin.fileno()
    proc_out_fd = proc.stdout.fileno()
    proc_err_fd = proc.stderr.fileno()

    # Each source feeds exactly one sink through a pending buffer, and the
    # loop only writes to a sink that select() reports writable. A blocking
    # os.write here deadlocks as soon as both directions carry more than a
    # pipe buffer at once: measured 2026-09-24 on a 50,000-entry download
    # (2.5 MB file list one way, the generator stream the other), where the
    # proxy sat in write() to the client while the client sat in write() to
    # the proxy. Single-file transfers never filled both pipes together.
    route = {client_in_fd: proc_in_fd, proc_out_fd: client_out_fd, proc_err_fd: client_err_fd}
    tee = {client_in_fd: capture_in, proc_out_fd: capture_out, proc_err_fd: capture_err}
    pending = {sink: bytearray() for sink in route.values()}
    # A sink closes once its source hit EOF and its buffer drained; for the
    # server's stdin that close is how EOF propagates to rsync.
    closing = set()
    for fd in route.values():
        os.set_blocking(fd, False)

    sources = {client_in_fd, proc_out_fd, proc_err_fd}
    dead_sinks = set()

    # If the SIGPIPE default were inherited, a Python write() to a closed
    # client would raise BrokenPipeError mid-shuttle and kill us. Catch
    # BrokenPipeError explicitly instead and unwind cleanly.
    signal.signal(signal.SIGPIPE, signal.SIG_DFL)

    def finish_sink(sink):
        if sink == proc_in_fd:
            try:
                proc.stdin.close()
            except Exception:
                pass
        dead_sinks.add(sink)

    try:
        while sources or any(pending[k] for k in pending if k not in dead_sinks):
            # Back-pressure: stop reading a source while its sink is backed up.
            readable_set = [fd for fd in sources if len(pending[route[fd]]) < 4 * CHUNK]
            writable_set = [k for k, buf in pending.items() if buf and k not in dead_sinks]
            readable, writable, _ = select.select(readable_set, writable_set, [], 1.0)
            if not readable and not writable:
                # Poll proc liveness: if it's gone and no output is left,
                # drain and exit. Without this we could block in select()
                # indefinitely when all three sources are gone but the
                # OS hasn't yet delivered EOF.
                # Leave only once nothing queued is still owed to a live
                # sink: bytes read from the server but not yet accepted by
                # a busy client would otherwise be dropped here, and the
                # client would see a truncated stream (the capture files
                # would still be complete, since the tee runs at read time).
                if proc.poll() is not None and not any(
                    fd in sources for fd in (proc_out_fd, proc_err_fd)
                ) and not any(buf for k, buf in pending.items() if k not in dead_sinks):
                    break
                continue

            for sink in writable:
                try:
                    n = os.write(sink, pending[sink])
                    del pending[sink][:n]
                except BlockingIOError:
                    pass
                except BrokenPipeError:
                    pending[sink].clear()
                    finish_sink(sink)
                    for src, dst in route.items():
                        if dst == sink:
                            sources.discard(src)
                if sink in closing and not pending[sink]:
                    finish_sink(sink)

            for fd in readable:
                try:
                    data = os.read(fd, CHUNK)
                except OSError:
                    data = b""
                sink = route[fd]
                if not data:
                    # EOF on this source. Remove it and propagate downstream
                    # once everything already read has been delivered.
                    sources.discard(fd)
                    if fd == client_in_fd:
                        if pending[sink]:
                            closing.add(sink)
                        else:
                            finish_sink(sink)
                    continue
                tee[fd].write(data)
                if sink not in dead_sinks:
                    pending[sink] += data

        proc.wait()
        rc = proc.returncode
    finally:
        capture_in.close()
        capture_out.close()
        capture_err.close()
        try:
            proc.kill()
        except Exception:
            pass

    end_marker = capture_dir / "end.txt"
    end_marker.write_text(os.popen("date -Iseconds").read())
    return rc if rc is not None and rc >= 0 else 128 - rc


if __name__ == "__main__":
    sys.exit(main())
