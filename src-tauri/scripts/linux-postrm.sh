#!/bin/bash
# Post-remove cleanup for dispatcher-owned Linux aliases.

set -e

ROOT="${AEROFTP_PACKAGE_ROOT:-}"
root_path() {
    printf '%s%s\n' "$ROOT" "$1"
}

ACTION="${1:-remove}"
case "$ACTION" in
    remove|purge|0)
        rm -f "$(root_path /usr/bin/aftp)"
        rm -f "$(root_path /usr/bin/aeroftp-cli)"
        rm -f "$(root_path /usr/lib/aeroftp/aeroftp.bin)"
        ;;
esac

exit 0
