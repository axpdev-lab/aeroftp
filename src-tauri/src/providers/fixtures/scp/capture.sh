# Builds the same folder on any POSIX system and captures the listing commands
# the SCP engine runs. $1 = output dir, $2 = prefix (busybox or gnu).
# The folder path is fixed because the fixtures print it. `mkdir` without -p
# fails when it exists, so a second run never wipes a folder in use.
out=$1; pre=$2; d=/tmp/scpfx
mkdir "$d" || { echo "$d exists: another capture is running, or remove it" >&2; exit 1; }
trap 'rm -rf "$d"' EXIT
cd "$d" || exit 1
printf 'hello' > ./"a b"
: > ./"it's"
: > ./-dash
: > ./"$(printf 'new\nline')"
: > ./"$(printf 'tab\tx')"
: > ./.hidden
: > ./"café"
: > ./"$(printf 'raw\377byte')"
: > ./"x -> y"
: > ./"$(printf 'spoof\n-rw-r--r--    1 0        0            9999 2001-01-01 00:00:00 +0000 a b')"
printf '0123456789' > ./ten
ln -s "a b" link
ln -s /nonexistent dangling
mkdir dir sub
chmod 0750 sub
chmod 4755 ten
touch -d @1700000000 ./ten
LC_ALL=C ls -lan --full-time -- "$d" > "$out/$pre-ls-lan.txt" 2> "$out/$pre-ls-lan.err"
LC_ALL=C find "$d" -mindepth 1 -maxdepth 1 -print0 > "$out/$pre-find.txt"
LC_ALL=C ls -land --full-time -- "$d/$(printf 'new\nline')" > "$out/$pre-ls-land-newline.txt"
LC_ALL=C ls -land --full-time -- "$d/link" "$d/dir" "$d/ten" > "$out/$pre-ls-land-misc.txt"
LC_ALL=C ls -land --full-time -- /dev/null /dev/zero > "$out/$pre-ls-land-dev.txt"
LC_ALL=C ls -land --full-time -- "$d/nope" > "$out/$pre-ls-missing.out" 2> "$out/$pre-ls-missing.err"; echo "rc=$?" >> "$out/$pre-ls-missing.err"
readlink -- "$d/link" > "$out/$pre-readlink.txt"
(ls --version 2>&1 | head -1; uname -a) > "$out/$pre-version.txt"
