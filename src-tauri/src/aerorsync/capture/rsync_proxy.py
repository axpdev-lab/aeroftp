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

    # Readable sources: client stdin (to feed into proc), proc stdout, proc
    # stderr. Remove a source on EOF; close the corresponding downstream to
    # propagate EOF faithfully.
    sources = {client_in_fd, proc_out_fd, proc_err_fd}

    # If the SIGPIPE default were inherited, a Python write() to a closed
    # client would raise BrokenPipeError mid-shuttle and kill us. Catch
    # BrokenPipeError explicitly instead and unwind cleanly.
    signal.signal(signal.SIGPIPE, signal.SIG_DFL)

    try:
        while sources:
            readable, _, _ = select.select(list(sources), [], [], 1.0)
            if not readable:
                # Poll proc liveness: if it's gone and no output is left,
                # drain and exit. Without this we could block in select()
                # indefinitely when all three sources are gone but the
                # OS hasn't yet delivered EOF.
                if proc.poll() is not None and not any(
                    fd in sources for fd in (proc_out_fd, proc_err_fd)
                ):
                    break
                continue

            for fd in readable:
                try:
                    data = os.read(fd, CHUNK)
                except OSError:
                    data = b""

                if not data:
                    # EOF on this source. Remove it and propagate downstream.
                    sources.discard(fd)
                    if fd == client_in_fd:
                        try:
                            proc.stdin.close()
                        except Exception:
                            pass
                    continue

                if fd == client_in_fd:
                    capture_in.write(data)
                    try:
                        os.write(proc_in_fd, data)
                    except BrokenPipeError:
                        sources.discard(fd)
                elif fd == proc_out_fd:
                    capture_out.write(data)
                    try:
                        os.write(client_out_fd, data)
                    except BrokenPipeError:
                        sources.discard(fd)
                elif fd == proc_err_fd:
                    capture_err.write(data)
                    try:
                        os.write(client_err_fd, data)
                    except BrokenPipeError:
                        sources.discard(fd)

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
