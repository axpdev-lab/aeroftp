#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Sourced by every capture script that talks to the rsync fixture container.
#
# The container authorises exactly one key, the one `entrypoint.sh` installs
# as `testuser`'s `authorized_keys` from the bind-mounted `keys/`. It is made
# here rather than kept in the tree: a private key committed to a public
# repository invites the reader to work out whether it opens anything real,
# and the answer being "only a container you could build yourself" is not
# worth the question. Regenerated only when absent, so a stack left up across
# runs keeps working. One definition for every script, so no script can go on
# expecting a key that nothing creates.
ensure_fixture_key() {
  local dir="$1/keys"
  mkdir -p "$dir"
  if [[ ! -f "$dir/id_ed25519" ]]; then
    ssh-keygen -q -t ed25519 -N '' -C aeroftp-rsync-test-fixture -f "$dir/id_ed25519"
  fi
  chmod 600 "$dir/id_ed25519"
  chmod 644 "$dir/id_ed25519.pub"
}
