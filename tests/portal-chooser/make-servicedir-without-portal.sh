#!/usr/bin/env bash
# Build a D-Bus service directory that mirrors the system one but omits every
# xdg-desktop-portal service.
#
# Why this exists: a bus with no service directory at all models "no portal" but
# also removes gvfs, the keyring and everything else, so a chooser that fails to
# appear could be blamed on the missing portal when something else broke it. This
# keeps every other service activatable and takes away only the portal, which is
# the machine we actually want to imitate: one where xdg-desktop-portal is not
# installed.
set -euo pipefail
DEST="${1:?usage: make-servicedir-without-portal.sh <dir>}"
mkdir -p "$DEST"
count=0
skipped=0
# The user directory is resolved defensively: under `set -u` a bare "$HOME" kills
# the script before a single service is linked, and CI steps do occasionally run
# without HOME set. Deliberately NOT XDG_DATA_HOME: the caller points that at a
# private empty root for the run, and mirroring that would scan nothing.
USER_SVC="${HOME:-/nonexistent}/.local/share/dbus-1/services"
for dir in /usr/share/dbus-1/services /usr/local/share/dbus-1/services "$USER_SVC"; do
  [ -d "$dir" ] || continue
  for f in "$dir"/*.service; do
    [ -e "$f" ] || continue
    base="$(basename "$f")"
    case "$base" in
      *portal*|*Portal*) skipped=$((skipped + 1)); continue ;;
    esac
    [ -e "$DEST/$base" ] || ln -s "$f" "$DEST/$base"
    count=$((count + 1))
  done
done
echo "service dir $DEST: $count services linked, $skipped portal service(s) omitted"
