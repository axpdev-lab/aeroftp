#!/usr/bin/env bash
set -euo pipefail

if [[ ! -f /keys/id_ed25519.pub ]]; then
  echo "missing /keys/id_ed25519.pub" >&2
  exit 1
fi

install -d -m 700 -o testuser -g testuser /home/testuser/.ssh
install -m 600 -o testuser -g testuser /keys/id_ed25519.pub /home/testuser/.ssh/authorized_keys
chown -R testuser:testuser /workspace

exec /usr/sbin/sshd -D -e
