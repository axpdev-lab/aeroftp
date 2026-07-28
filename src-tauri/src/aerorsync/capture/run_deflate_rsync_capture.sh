#!/usr/bin/env bash
# Byte oracle for the pre-zstd peer family. Builds upstream rsync 3.1.3,
# forces a pinned 3.1.3 client onto zlibx, transfers a genuinely changed
# 256-KiB basis in both directions, and freezes both SSH exec streams.
#
# The guards are deliberately strict:
#   * server version must be exactly 3.1.3 / protocol 31;
#   * the captured server argv must contain --new-compress (zlibx);
#   * rsync --stats must report both literal and matched bytes;
#   * final SHA-256 must match while the seeded basis SHA-256 must differ.
# These prevent the useful-looking but false-green shapes paid for during
# the NAS investigation: wrong compressor, no-op basis, and zero tests.

set -euo pipefail

CAPTURE_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
SRC_TAURI_DIR="$(cd -- "$CAPTURE_DIR/../../.." && pwd)"
WORKSPACE_DIR="$CAPTURE_DIR/workspace/deflate-3.1.3"
REAL_CAPTURE_SRC="$CAPTURE_DIR/workspace/deflate_capture"
ARTIFACTS_ROOT="$CAPTURE_DIR/artifacts_deflate"
COMPOSE_FILE="$CAPTURE_DIR/docker-compose.rsync-3.1.3.yml"
COMPOSE_PROJECT="aeroftp-rsync-313-deflate"
CONTAINER_NAME="aeroftp-rsync-313-deflate"
IMAGE_NAME="aeroftp-rsync-313-deflate:local"
FREEZE_TS="$(date -u +%Y%m%d_%H%M%S)"
KEEP_STACK="${KEEP_STACK:-0}"

mkdir -p \
  "$WORKSPACE_DIR/upload" \
  "$WORKSPACE_DIR/download" \
  "$WORKSPACE_DIR/local" \
  "$REAL_CAPTURE_SRC" \
  "$ARTIFACTS_ROOT"

rm -rf "$REAL_CAPTURE_SRC"/*
rm -f \
  "$WORKSPACE_DIR/upload/target.bin" \
  "$WORKSPACE_DIR/download/source.bin" \
  "$WORKSPACE_DIR/local/upload.bin" \
  "$WORKSPACE_DIR/local/download.bin" \
  "$WORKSPACE_DIR/local/expected-upload.bin" \
  "$WORKSPACE_DIR/local/expected-download.bin"

# Same seed, size, and mutation offsets as the NAS live regression lane.
# The expected files and their bases are written separately and checked
# before rsync starts, so a prior successful run cannot turn this into a
# no-op.
python3 - "$WORKSPACE_DIR" <<'PY'
from pathlib import Path
import random
import sys

root = Path(sys.argv[1])
rng = random.Random(20260728)
basis = bytearray(rng.getrandbits(8) for _ in range(256 * 1024))
final = bytearray(basis)
for offset in (4096, 60000, 200000):
    final[offset:offset + 512] = bytes(
        value ^ 0xA5 for value in final[offset:offset + 512]
    )

(root / "upload").mkdir(parents=True, exist_ok=True)
(root / "download").mkdir(parents=True, exist_ok=True)
(root / "local").mkdir(parents=True, exist_ok=True)

(root / "upload" / "target.bin").write_bytes(basis)
(root / "local" / "upload.bin").write_bytes(final)
(root / "local" / "expected-upload.bin").write_bytes(final)

(root / "download" / "source.bin").write_bytes(final)
(root / "local" / "download.bin").write_bytes(basis)
(root / "local" / "expected-download.bin").write_bytes(final)
PY

assert_different() {
  local basis="$1"
  local expected="$2"
  if cmp -s "$basis" "$expected"; then
    echo "[deflate-harness] FATAL: basis equals expected result: $basis" >&2
    exit 10
  fi
}

assert_different \
  "$WORKSPACE_DIR/upload/target.bin" \
  "$WORKSPACE_DIR/local/expected-upload.bin"
assert_different \
  "$WORKSPACE_DIR/local/download.bin" \
  "$WORKSPACE_DIR/local/expected-download.bin"

if docker compose version >/dev/null 2>&1; then
  STACK_MODE="compose"
else
  STACK_MODE="docker"
fi

stop_stack() {
  if [[ "$STACK_MODE" == "compose" ]]; then
    docker compose -p "$COMPOSE_PROJECT" -f "$COMPOSE_FILE" down \
      >/dev/null 2>&1 || true
  elif docker container inspect "$CONTAINER_NAME" >/dev/null 2>&1; then
    docker rm -f "$CONTAINER_NAME" >/dev/null
  fi
}

cleanup() {
  if [[ "$KEEP_STACK" != "1" ]]; then
    stop_stack
  fi
}
trap cleanup EXIT

# OpenSSH rejects this checked-in fixture key when a fresh worktree gives
# it group/other read bits.
chmod 600 "$CAPTURE_DIR/keys/id_ed25519"

stop_stack
if [[ "$STACK_MODE" == "compose" ]]; then
  HOST_UID="$(id -u)" HOST_GID="$(id -g)" \
    docker compose -p "$COMPOSE_PROJECT" -f "$COMPOSE_FILE" up -d --build
else
  docker build \
    --build-arg "TESTUSER_UID=$(id -u)" \
    --build-arg "TESTUSER_GID=$(id -g)" \
    -f "$CAPTURE_DIR/Dockerfile.rsync-3.1.3-sshd" \
    -t "$IMAGE_NAME" \
    "$CAPTURE_DIR"
  docker run -d \
    --name "$CONTAINER_NAME" \
    --restart unless-stopped \
    -p 2225:22 \
    -v "$CAPTURE_DIR/keys:/keys:ro" \
    -v "$CAPTURE_DIR/workspace:/workspace" \
    "$IMAGE_NAME" \
    >/dev/null
fi

ready=0
for _ in $(seq 1 30); do
  if (exec 3<>/dev/tcp/127.0.0.1/2225) 2>/dev/null; then
    exec 3<&- 3>&-
    ready=1
    break
  fi
  sleep 1
done
if [[ "$ready" != "1" ]]; then
  echo "[deflate-harness] FATAL: rsync 3.1.3 sshd did not become ready" >&2
  exit 11
fi

SERVER_VERSION_RAW="$(docker exec "$CONTAINER_NAME" rsync --version)"
SERVER_VERSION="${SERVER_VERSION_RAW%%$'\n'*}"
if [[ "$SERVER_VERSION" != "rsync  version 3.1.3  protocol version 31" ]]; then
  echo "[deflate-harness] FATAL: unexpected server: $SERVER_VERSION" >&2
  exit 12
fi

SSH_OPTS=(
  -i /keys/id_ed25519
  -p 2225
  -o StrictHostKeyChecking=no
  -o UserKnownHostsFile=/dev/null
  -o BatchMode=yes
  -o ConnectTimeout=5
)

run_pinned_rsync() {
  docker run --rm \
    --network host \
    --entrypoint /usr/local/bin/rsync \
    -e LC_ALL=C \
    -v "$CAPTURE_DIR/keys:/keys:ro" \
    -v "$CAPTURE_DIR/workspace:/workspace" \
    "$IMAGE_NAME" \
    "$@"
}

echo "[deflate-harness] capturing zlibx upload"
run_pinned_rsync -av \
  --protocol=31 \
  --compress \
  --new-compress \
  --checksum \
  --stats \
  -e "ssh ${SSH_OPTS[*]}" \
  /workspace/deflate-3.1.3/local/upload.bin \
  "testuser@127.0.0.1:/workspace/deflate-3.1.3/upload/target.bin" \
  >"$REAL_CAPTURE_SRC/.upload.stdout" \
  2>"$REAL_CAPTURE_SRC/.upload.stderr"

echo "[deflate-harness] capturing zlibx download"
run_pinned_rsync -av \
  --protocol=31 \
  --compress \
  --new-compress \
  --checksum \
  --stats \
  -e "ssh ${SSH_OPTS[*]}" \
  "testuser@127.0.0.1:/workspace/deflate-3.1.3/download/source.bin" \
  /workspace/deflate-3.1.3/local/download.bin \
  >"$REAL_CAPTURE_SRC/.download.stdout" \
  2>"$REAL_CAPTURE_SRC/.download.stderr"

sha256sum -c < <(
  printf '%s  %s\n' \
    "$(sha256sum "$WORKSPACE_DIR/local/expected-upload.bin" | awk '{print $1}')" \
    "$WORKSPACE_DIR/upload/target.bin"
  printf '%s  %s\n' \
    "$(sha256sum "$WORKSPACE_DIR/local/expected-download.bin" | awk '{print $1}')" \
    "$WORKSPACE_DIR/local/download.bin"
)

assert_nonzero_stat() {
  local stats_file="$1"
  local label="$2"
  local value
  value="$(
    awk -F': ' -v key="$label" \
      '$1 == key {gsub(/[^0-9]/, "", $2); print $2}' \
      "$stats_file"
  )"
  if [[ ! "$value" =~ ^[0-9]+$ ]] || (( value == 0 )); then
    echo "[deflate-harness] FATAL: $label is not positive in $stats_file: ${value:-missing}" >&2
    exit 13
  fi
}

for stats_file in "$REAL_CAPTURE_SRC/.upload.stdout" "$REAL_CAPTURE_SRC/.download.stdout"; do
  assert_nonzero_stat "$stats_file" "Literal data"
  assert_nonzero_stat "$stats_file" "Matched data"
done

DEST_DIR="$ARTIFACTS_ROOT/$FREEZE_TS"
mkdir -p "$DEST_DIR"

UPLOAD_SESSION=""
DOWNLOAD_SESSION=""
while read -r session; do
  [[ -z "$session" ]] && continue
  session_dir="$REAL_CAPTURE_SRC/$session"
  [[ -s "$session_dir/remote_command.txt" ]] || continue
  cmd="$(cat "$session_dir/remote_command.txt")"
  if [[ "$cmd" == *"--sender"* ]]; then
    DOWNLOAD_SESSION="$session"
  else
    UPLOAD_SESSION="$session"
  fi
done < <(
  find "$REAL_CAPTURE_SRC" -maxdepth 1 -mindepth 1 -type d -printf '%f\n' \
    | LC_ALL=C sort
)

if [[ -z "$UPLOAD_SESSION" || -z "$DOWNLOAD_SESSION" ]]; then
  echo "[deflate-harness] FATAL: could not classify both capture sessions" >&2
  exit 14
fi

for session in "$UPLOAD_SESSION" "$DOWNLOAD_SESSION"; do
  command_file="$REAL_CAPTURE_SRC/$session/remote_command.txt"
  if ! grep -q -- '--new-compress' "$command_file"; then
    echo "[deflate-harness] FATAL: zlibx did not reach server argv: $(cat "$command_file")" >&2
    exit 15
  fi
done

cp -r "$REAL_CAPTURE_SRC/$UPLOAD_SESSION" "$DEST_DIR/upload"
cp -r "$REAL_CAPTURE_SRC/$DOWNLOAD_SESSION" "$DEST_DIR/download"
cp "$REAL_CAPTURE_SRC/.upload.stdout" "$DEST_DIR/upload/client.stdout.txt"
cp "$REAL_CAPTURE_SRC/.upload.stderr" "$DEST_DIR/upload/client.stderr.txt"
cp "$REAL_CAPTURE_SRC/.download.stdout" "$DEST_DIR/download/client.stdout.txt"
cp "$REAL_CAPTURE_SRC/.download.stderr" "$DEST_DIR/download/client.stderr.txt"

CLIENT_VERSION_RAW="$(
  docker run --rm --entrypoint /usr/local/bin/rsync "$IMAGE_NAME" --version
)"
CLIENT_VERSION="${CLIENT_VERSION_RAW%%$'\n'*}"
cat >"$DEST_DIR/summary.env" <<EOF
freeze_ts=$FREEZE_TS
client_rsync=$CLIENT_VERSION
server_rsync=$SERVER_VERSION
compress_choice=zlibx
upload_expected_sha256=$(sha256sum "$WORKSPACE_DIR/local/expected-upload.bin" | awk '{print $1}')
upload_final_sha256=$(sha256sum "$WORKSPACE_DIR/upload/target.bin" | awk '{print $1}')
download_expected_sha256=$(sha256sum "$WORKSPACE_DIR/local/expected-download.bin" | awk '{print $1}')
download_final_sha256=$(sha256sum "$WORKSPACE_DIR/local/download.bin" | awk '{print $1}')
upload_bytes_in=$(stat -c '%s' "$DEST_DIR/upload/capture_in.bin")
upload_bytes_out=$(stat -c '%s' "$DEST_DIR/upload/capture_out.bin")
download_bytes_in=$(stat -c '%s' "$DEST_DIR/download/capture_in.bin")
download_bytes_out=$(stat -c '%s' "$DEST_DIR/download/capture_out.bin")
EOF

echo "[deflate-harness] checking captured tokens with AeroRsync real_wire"
(
  cd "$SRC_TAURI_DIR"
  AEROFTP_DEFLATE_CAPTURE="$DEST_DIR" \
    cargo test --features aerorsync --lib \
      rsync_3_1_3_deflate_byte_oracle_matches_real_wire_decoder -- \
      --ignored --nocapture
)

echo "[deflate-harness] byte oracle written to $DEST_DIR"
