#!/usr/bin/env bash
# Extreme cases against the servers of docker-compose.yml, after run-matrix.sh
# passed: the largest window and fan-out, the smallest and largest READ,
# downloads cancelled at random moments, parallel clients, a server restarted
# mid-transfer, a file replaced mid-transfer, names a path layer can get
# wrong, and a file the server cannot read. Every outcome is checked: bytes by
# sha256, refusals by exit code, and no temporary left behind.
#
# Usage: ./stress-matrix.sh <aeroftp-cli> <label> [server...]
# Results: results/<label>-stress.tsv and results/<label>-stress/ logs.
set -u
cd "$(dirname "$0")"
CLI=$1
LABEL=$2
shift 2
SERVERS=("$@")
[ ${#SERVERS[@]} -gt 0 ] || SERVERS=(openssh openssh-legacy openssh-maxsessions1 openssh-chroot sftpgo proftpd dropbear)
OUT=results/$LABEL-stress
mkdir -p "$OUT"
TSV=results/$LABEL-stress.tsv
[ -f "$TSV" ] || printf 'utc\tserver\tcase\tverdict\tdetail\n' > "$TSV"
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

port_of() {
    case $1 in
        openssh) echo 2231 ;; openssh-legacy) echo 2232 ;; openssh-maxsessions1) echo 2233 ;;
        openssh-chroot) echo 2234 ;; sftpgo) echo 2235 ;; proftpd) echo 2236 ;; dropbear) echo 2237 ;;
    esac
}
sum_of() { grep -F "  ./$1" generated/payload.sha256 | head -1 | cut -d' ' -f1; }
record() { printf '%s\t%s\t%s\t%s\t%s\n' "$(date -u +%H:%M:%SZ)" "$1" "$2" "$3" "$4" | tee -a "$TSV"; }
cli() { timeout "${TIMEOUT:-600}" "$CLI" --no-banner --key ssh_key --trust-host-key "$@"; }
leftovers() { find "$1" -name '*.aerotmp' -o -name '*.aeroardtmp' -o -name '*.aerosegtmp' -o -name '*.aerosrctmp' | wc -l; }

# A whole file, under the given flags, checked by sha256.
whole() { # server case remote payload-path flags...
    local server=$1 case=$2 remote=$3 rel=$4
    shift 4
    local url="sftp://testuser@127.0.0.1:$(port_of "$server")"
    rm -rf "$WORK/d" && mkdir -p "$WORK/d"
    cli "$@" get "$url" "$remote" "$WORK/d/out.bin" > "$OUT/$server-$case.log" 2>&1
    local rc=$? got
    got=$(sha256sum "$WORK/d/out.bin" 2>/dev/null | cut -d' ' -f1)
    if [ $rc = 0 ] && [ "$got" = "$(sum_of "$rel")" ]; then
        record "$server" "$case" PASS "sha256 ok"
    else
        record "$server" "$case" FAIL "rc=$rc $(tail -1 "$OUT/$server-$case.log" | cut -c1-160)"
    fi
}

for server in "${SERVERS[@]}"; do
    url="sftp://testuser@127.0.0.1:$(port_of "$server")"

    # Fan-out and depth at their limits.
    whole "$server" max-16x1024 /data/big.bin big.bin --multi-thread-streams 16 --multi-thread-cutoff 32M --sftp-readahead 1024
    # The smallest READ the read-ahead takes, and one far over any server's.
    whole "$server" chunk-4k /data/mid.bin mid.bin --buffer-size 4K
    whole "$server" chunk-4m /data/big.bin big.bin --multi-thread-streams 1 --buffer-size 4M
    # Names a path layer can get wrong.
    whole "$server" name-unicode "/data/special/naïve café 文件 #1 %20.bin" "special/naïve café 文件 #1 %20.bin" --multi-thread-streams 1
    whole "$server" name-spaces "/data/special/ leading and trailing .bin " "special/ leading and trailing .bin " --multi-thread-streams 1
    whole "$server" symlink /data/special/link-to-mid.bin mid.bin --multi-thread-streams 1

    # A file the server cannot read: refused, nothing left behind.
    rm -rf "$WORK/d" && mkdir -p "$WORK/d"
    cli get "$url" /data/denied/unreadable.bin "$WORK/d/out.bin" > "$OUT/$server-denied.log" 2>&1
    rc=$?
    if [ $rc != 0 ] && [ ! -e "$WORK/d/out.bin" ] && [ "$(leftovers "$WORK/d")" = 0 ]; then
        record "$server" denied PASS "refused rc=$rc, nothing left"
    else
        record "$server" denied FAIL "rc=$rc out=$([ -e "$WORK/d/out.bin" ] && echo present || echo absent) temps=$(leftovers "$WORK/d")"
    fi

    # Cancelled at random moments: no temporary left, the next download whole.
    bad=0
    for i in $(seq 12); do
        rm -rf "$WORK/d" && mkdir -p "$WORK/d"
        delay=$(awk -v s="$RANDOM" 'BEGIN { srand(s); printf "%.2f", 0.05 + rand() * 1.2 }')
        timeout -s INT "$delay" "$CLI" --no-banner --key ssh_key --trust-host-key --multi-thread-streams 4 \
            --multi-thread-cutoff 32M get "$url" /data/big.bin "$WORK/d/out.bin" > "$OUT/$server-cancel-$i.log" 2>&1
        rc=$?
        sleep 0.3
        if [ "$(leftovers "$WORK/d")" != 0 ]; then
            bad=$((bad + 1))
            left=$(find "$WORK/d" -type f -printf '%f:%s ' | cut -c1-120)
            echo "run $i: SIGINT after ${delay}s, rc=$rc, left: $left" >> "$OUT/$server-cancel-leftovers.txt"
        fi
    done
    if [ $bad = 0 ]; then
        record "$server" cancel-x12 PASS "no temporary left"
    else
        record "$server" cancel-x12 FAIL "$bad runs left a temporary: $(tail -1 "$OUT/$server-cancel-leftovers.txt")"
    fi
    whole "$server" after-cancels /data/big.bin big.bin --multi-thread-streams 4 --multi-thread-cutoff 32M

    # Eight clients at once on one server.
    rm -rf "$WORK/p" && mkdir -p "$WORK/p"
    for i in $(seq 8); do
        cli --multi-thread-streams 1 get "$url" /data/big.bin "$WORK/p/out$i.bin" > "$OUT/$server-parallel-$i.log" 2>&1 &
    done
    wait
    ok=0
    for i in $(seq 8); do
        [ "$(sha256sum "$WORK/p/out$i.bin" 2>/dev/null | cut -d' ' -f1)" = "$(sum_of big.bin)" ] && ok=$((ok + 1))
    done
    if [ $ok = 8 ]; then record "$server" parallel-8-clients PASS "8/8 sha256 ok"; else record "$server" parallel-8-clients FAIL "$ok/8 sha256 ok"; fi
done

# Disruptions, on the main OpenSSH server only (tc netem slows its side of the
# link so the event lands mid-transfer).
if printf '%s\n' "${SERVERS[@]}" | grep -qx openssh; then
    C=aeroftp-sftp-openssh
    url="sftp://testuser@127.0.0.1:2231"
    slow() { docker exec "$C" tc qdisc replace dev eth0 root netem delay 40ms; }

    # The server restarted mid-transfer: the download ends, nothing is
    # published and no temporary is left.
    slow
    rm -rf "$WORK/d" && mkdir -p "$WORK/d"
    TIMEOUT=180 cli --multi-thread-streams 4 --multi-thread-cutoff 32M get "$url" /data/big.bin "$WORK/d/out.bin" > "$OUT/openssh-restart.log" 2>&1 &
    pid=$!
    sleep 2
    docker restart -t 0 "$C" > /dev/null
    started=$(date +%s)
    wait $pid
    rc=$?
    took=$(( $(date +%s) - started ))
    if [ $rc != 0 ] && [ ! -e "$WORK/d/out.bin" ] && [ "$(leftovers "$WORK/d")" = 0 ]; then
        record openssh restart-mid-transfer PASS "rc=$rc after ${took}s, nothing published or left"
    elif [ $rc = 0 ] && [ "$(sha256sum "$WORK/d/out.bin" | cut -d' ' -f1)" = "$(sum_of big.bin)" ]; then
        record openssh restart-mid-transfer PASS "recovered: rc=0, sha256 ok after ${took}s"
    else
        record openssh restart-mid-transfer FAIL "rc=$rc after ${took}s out=$([ -e "$WORK/d/out.bin" ] && echo present || echo absent) temps=$(leftovers "$WORK/d")"
    fi
    sleep 2

    # The file replaced mid-transfer: refused, or one whole version, never a mix.
    slow
    docker exec "$C" sh -c 'head -c 67108864 /data/big.bin > /tmp/moving.bin && chmod 644 /tmp/moving.bin'
    old=$(docker exec "$C" sha256sum /tmp/moving.bin | cut -d' ' -f1)
    rm -rf "$WORK/d" && mkdir -p "$WORK/d"
    cli --multi-thread-streams 4 --multi-thread-cutoff 32M get "$url" /tmp/moving.bin "$WORK/d/out.bin" > "$OUT/openssh-replaced.log" 2>&1 &
    pid=$!
    sleep 1.5
    docker exec "$C" sh -c 'head -c 67108864 /dev/urandom > /tmp/moving.new && mv /tmp/moving.new /tmp/moving.bin'
    new=$(docker exec "$C" sha256sum /tmp/moving.bin | cut -d' ' -f1)
    wait $pid
    rc=$?
    got=$(sha256sum "$WORK/d/out.bin" 2>/dev/null | cut -d' ' -f1)
    if [ $rc != 0 ] && [ ! -e "$WORK/d/out.bin" ]; then
        record openssh replaced-mid-transfer PASS "refused rc=$rc: $(grep -m1 -oE 'changed while[^.]*' "$OUT/openssh-replaced.log" | cut -c1-80)"
    elif [ $rc = 0 ] && { [ "$got" = "$old" ] || [ "$got" = "$new" ]; }; then
        record openssh replaced-mid-transfer PASS "one whole version: $([ "$got" = "$old" ] && echo old || echo new)"
    else
        record openssh replaced-mid-transfer FAIL "rc=$rc, published bytes match neither version"
    fi
    docker exec "$C" tc qdisc del dev eth0 root 2>/dev/null
fi
