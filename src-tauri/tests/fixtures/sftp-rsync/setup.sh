#!/usr/bin/env bash
# Generate an ed25519 SSH keypair for the delta-sync integration fixture.
# Idempotent: if ssh_key already exists, skip.
#
# Keys live in the fixture directory and are gitignored via
# src-tauri/tests/fixtures/sftp-rsync/.gitignore.

set -euo pipefail
cd "$(dirname "$0")"

if [ -f ssh_key ] && [ -f ssh_key.pub ]; then
    echo "Fixture key already present. Skipping generation."
    exit 0
fi

# -N "" → no passphrase; tests use BatchMode=yes and cannot prompt.
# -C → identity label embedded in the key for easier auditing.
ssh-keygen -t ed25519 -f ssh_key -N "" -C "aeroftp-delta-sync-fixture" -q

chmod 600 ssh_key
chmod 644 ssh_key.pub

echo "Fixture key generated:"
echo "  private: $(pwd)/ssh_key"
echo "  public:  $(pwd)/ssh_key.pub"
