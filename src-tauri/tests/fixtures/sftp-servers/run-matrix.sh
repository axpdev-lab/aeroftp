#!/usr/bin/env bash
# Download scenarios against every server of docker-compose.yml with one
# aeroftp-cli binary. Each download is checked file by file against the
# payload's sha256; each run records exit code, wall time and peak RSS, and on
# the OpenSSH servers the SFTP channels the run opened (sshd at LogLevel
# VERBOSE logs one "Starting session" per channel: subsystem 'sftp', or the
# forced internal-sftp command in the chroot).
#
# Usage: ./run-matrix.sh <aeroftp-cli> <label> [server...]
# Results: results/<label>.tsv (one line per run) and results/<label>/ logs.
# Exits nonzero when any run failed or any file did not match.
set -u
cd "$(dirname "$0")"
CLI=$1
LABEL=$2
shift 2
SERVERS=("$@")
[ ${#SERVERS[@]} -gt 0 ] || SERVERS=(openssh openssh-legacy openssh-maxsessions1 openssh-chroot sftpgo proftpd dropbear)

port_of() {
    case $1 in
        openssh) echo 2231 ;; openssh-legacy) echo 2232 ;; openssh-maxsessions1) echo 2233 ;;
        openssh-chroot) echo 2234 ;; sftpgo) echo 2235 ;; proftpd) echo 2236 ;; dropbear) echo 2237 ;;
    esac
}

OUT=results/$LABEL
mkdir -p "$OUT"
TSV=results/$LABEL.tsv
[ -f "$TSV" ] || printf 'utc\tserver\tscenario\trc\tfiles_ok\tfiles\twall\tpeak_rss_kb\tsftp_channels\n' > "$TSV"
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
FAILED=0

channels() { # SFTP channels opened so far on an OpenSSH server, "-" elsewhere
    case $1 in
        openssh*) docker logs "aeroftp-sftp-$1" 2>&1 \
            | grep -cE "Starting session: (subsystem 'sftp'|forced-command \(config\) 'internal-sftp')" ;;
        *) echo - ;;
    esac
}

verify() { # prefix: the payload files under it, found under $WORK/dl with
    # their tree or by their (unique) name, and how many match their sha256
    local prefix=$1 ok=0 total=0
    while read -r sum path; do
        total=$((total + 1))
        local rel=${path#./}
        local got=$WORK/dl/$rel
        [ -f "$got" ] || got=$WORK/dl/$(basename "$rel")
        [ -f "$got" ] && [ "$(sha256sum "$got" | cut -d' ' -f1)" = "$sum" ] && ok=$((ok + 1))
    done < <(grep -F "  ./$prefix" generated/payload.sha256)
    echo "$ok $total"
}

run() { # server scenario prefix -- cli args...
    local server=$1 scenario=$2 prefix=$3
    shift 4
    rm -rf "$WORK/dl"
    mkdir -p "$WORK/dl"
    local before after rc wall rss
    before=$(channels "$server")
    /usr/bin/time -v -o "$OUT/$server-$scenario.time" timeout 900 "$CLI" --no-banner \
        --key ssh_key --trust-host-key "$@" > "$OUT/$server-$scenario.log" 2>&1
    rc=$?
    after=$(channels "$server")
    wall=$(awk -F': ' '/Elapsed \(wall/ {print $2}' "$OUT/$server-$scenario.time")
    rss=$(awk -F': ' '/Maximum resident/ {print $2}' "$OUT/$server-$scenario.time")
    read -r ok total < <(verify "$prefix")
    local opened=-
    [ "$before" != - ] && opened=$((after - before))
    printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$(date -u +%H:%M:%SZ)" "$server" "$scenario" \
        "$rc" "$ok" "$total" "$wall" "$rss" "$opened" | tee -a "$TSV"
    if [ "$rc" != 0 ] || [ "$ok" != "$total" ]; then FAILED=$((FAILED + 1)); fi
}

for server in "${SERVERS[@]}"; do
    url="sftp://testuser@127.0.0.1:$(port_of "$server")"
    run "$server" one-conn big.bin -- --multi-thread-streams 1 get "$url" /data/big.bin "$WORK/dl/big.bin"
    run "$server" four-conn big.bin -- --multi-thread-streams 4 --multi-thread-cutoff 32M get "$url" /data/big.bin "$WORK/dl/big.bin"
    run "$server" gui-fast-8x16 big.bin -- --multi-thread-streams 8 --multi-thread-cutoff 32M --sftp-readahead 16 get "$url" /data/big.bin "$WORK/dl/big.bin"
    run "$server" tree-600 many/ -- get -r "$url" /data/many "$WORK/dl"
    run "$server" edges edges/ -- get -r "$url" /data/edges "$WORK/dl"
    run "$server" small-100 small/ -- get -r "$url" /data/small "$WORK/dl"
done
if [ "$FAILED" != 0 ]; then
    echo "$FAILED run(s) failed" >&2
    exit 1
fi
