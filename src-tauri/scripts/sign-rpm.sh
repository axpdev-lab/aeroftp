#!/usr/bin/env bash
# Sign the release rpm with the project's OpenPGP key, then verify it
# against the public key shipped in the package. rpm 6 (Fedora 45) refuses
# unsigned packages. rpmsign, not the bundler's own signer: the bundler's
# signature reads as "SIGNATURES NOT OK" on rpm 4.14 and 4.16 (EL8, EL9).
# Usage: sign-rpm.sh <public-key.asc> <rpm>...
# Env: RPM_SIGNING_KEY (armored secret key), RPM_SIGNING_KEY_PASSPHRASE.
set -euo pipefail
pub="$1"; shift
if [ -z "${RPM_SIGNING_KEY:-}" ]; then
  case "${GITHUB_REF:-}" in
    refs/tags/*) echo "::error::RPM_SIGNING_KEY is not set: a release rpm must be signed"; exit 1 ;;
  esac
  echo "::warning::RPM_SIGNING_KEY is not set: rpm left unsigned (not a release)"
  exit 0
fi
[ "$#" -gt 0 ] || { echo "::error::no rpm to sign"; exit 1; }
GNUPGHOME="$(mktemp -d)"; export GNUPGHOME
DB="$(mktemp -d)"
trap 'rm -rf "$GNUPGHOME" "$DB"' EXIT
chmod 700 "$GNUPGHOME"
printf '%s' "$RPM_SIGNING_KEY_PASSPHRASE" > "$GNUPGHOME/pass"
printf '%s\n' "$RPM_SIGNING_KEY" | gpg --batch --import
fpr="$(gpg --batch --with-colons --list-secret-keys | sed -n 's/^fpr:\{9\}\([0-9A-F]*\):.*/\1/p' | head -n 1)"
[ -n "$fpr" ] || { echo "::error::no secret key imported"; exit 1; }
# rpm on Ubuntu points %__gpg at /usr/bin/gpg2, which Ubuntu does not ship.
gpg_bin="$(command -v gpg)"
for f in "$@"; do
  rpmsign --addsign \
    --define "__gpg $gpg_bin" \
    --define "_gpg_name $fpr" \
    --define "_gpg_sign_cmd_extra_args --batch --pinentry-mode loopback --passphrase-file $GNUPGHOME/pass" \
    "$f"
done
rpm --dbpath "$DB" --import "$pub"
for f in "$@"; do
  out="$(rpm --dbpath "$DB" -K "$f")"
  echo "$out"
  case "$out" in
    *"signatures OK"*) ;;
    *) echo "::error::$f is not signed by $pub"; exit 1 ;;
  esac
done
