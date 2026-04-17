#!/bin/sh
# Normalize permissions on bind-mounted authorized_keys so sshd's
# StrictModes check succeeds. We must copy the file out of the bind
# mount to a writable location, then fix ownership + mode there.
#
# Without this, the host uid/gid and mode (typically 644 on a file
# owned by a dev user) cause sshd to reject the key with
# "Authentication refused: bad ownership or modes".

set -eu

SRC="/mnt/authorized_keys"
DEST="/home/testuser/.ssh/authorized_keys"

if [ -f "$SRC" ]; then
    cp "$SRC" "$DEST"
    chown testuser:testuser "$DEST"
    chmod 600 "$DEST"
fi

exec /usr/sbin/sshd -D -e
