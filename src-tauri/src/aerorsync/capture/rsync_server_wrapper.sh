#!/usr/bin/env bash
# Byte-oracle wrapper for the real-rsync Docker lane. sshd's ForceCommand
# points at this script, which captures the stdin/stdout of the exec channel
# to a capture directory before delegating to the command the client
# asked for (typically `rsync --server ...`). stderr is also tee'd but
# still forwarded to the client, so rsync's diagnostics are preserved.
#
# Why a Python proxy instead of a `tee | cmd | tee` bash pipeline?
#   - `tee | cmd | tee` deadlocks on error: if `cmd` exits after partial
#     output, the upstream `tee` has no way to learn that its downstream
#     reader is gone, and it keeps blocking on `read(stdin)` until the
#     SSH client closes: which the client will not do until it gets a
#     reply, which it never will. Deadlock for up to io_timeout_ms.
#   - A FIFO-based bash variant exists but still has a fd-sharing race
#     between the main shell and the forked `bash -c`, which in practice
#     causes the remote rsync to see EOF on stdin immediately after
#     printing its 4-byte version greeting. Impossible to capture the
#     full handshake that way.
#   - `socat` would work but adds an apt dependency and its syntax for
#     dual-direction tee-with-file is awkward.
#   - Python is already in the debian:bookworm-slim base image and gives
#     us precise control over `os.read`, `os.write`, and `select.select`
#     on fd 0 and fd 1 of the wrapper, with no buffering surprises.

set -u

if [[ -z "${SSH_ORIGINAL_COMMAND:-}" ]]; then
  printf 'rsync_server_wrapper: SSH_ORIGINAL_COMMAND is empty\n' >&2
  exit 2
fi

TS="$(date -u +%Y%m%d_%H%M%S_%N)"
CAPTURE_DIR="/workspace/real_capture/$TS"
mkdir -p "$CAPTURE_DIR"

printf '%s\n' "$SSH_ORIGINAL_COMMAND" > "$CAPTURE_DIR/remote_command.txt"
date -Iseconds > "$CAPTURE_DIR/start.txt"

export AEROFTP_CAPTURE_DIR="$CAPTURE_DIR"
export AEROFTP_SSH_ORIGINAL_COMMAND="$SSH_ORIGINAL_COMMAND"
exec python3 /opt/rsync-tee/rsync_proxy.py
