#!/usr/bin/env bash
# S8a byte-oracle harness. Builds the real-rsync Docker lane (port 2224),
# exercises upload + download via the host's own `rsync` client so that
# rsync protocol 31 wire bytes flow through the Python tee proxy
# (`rsync_proxy.py`) configured via sshd's `ForceCommand`, then copies
# the capture artifacts to `capture/artifacts_real/<freeze-ts>/`.
# Finally runs the Rust live test that asserts the lane is reachable
# and emits a non-empty greeting.
#
# `--checksum` is passed to the client so the transfer produces real
# delta traffic (block signatures, literal/matched bytes) on the wire,
# not a skip-by-mtime. The byte-oracle is only useful if it contains
# the full protocol trajectory.

set -euo pipefail
# Intentionally NOT using `set -o pipefail`: the harvest phase has
# benign pipes (`find | sort`, `docker exec | head`) where early-close
# from `head`/`sort` yields SIGPIPE on the upstream. With pipefail that
# turns into a script-level 141 exit, which is misleading here.

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)"
CAPTURE_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
SRC_TAURI_DIR="$ROOT_DIR"
WORKSPACE_DIR="$CAPTURE_DIR/workspace/real"
REAL_CAPTURE_SRC="$CAPTURE_DIR/workspace/real_capture"
ARTIFACTS_ROOT="$CAPTURE_DIR/artifacts_real"
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
  "$WORKSPACE_DIR/download/target.bin" \
  "$WORKSPACE_DIR/local/upload.bin" \
  "$WORKSPACE_DIR/local/download.bin" \
  "$WORKSPACE_DIR/local/expected-upload.bin" \
  "$WORKSPACE_DIR/local/expected-download.bin"

python3 - <<'PY' "$WORKSPACE_DIR"
from pathlib import Path
import sys

root = Path(sys.argv[1])

def make_payload(size: int) -> bytes:
    return bytes((i % 251 for i in range(size)))

def mutate_payload(basis: bytes, offset: int, patch: bytes) -> bytes:
    data = bytearray(basis)
    data[offset:offset + len(patch)] = patch
    return bytes(data)

basis = make_payload(256 * 1024)
upload_final = mutate_payload(basis, 8 * 1024, b"real-live-upload")
download_final = mutate_payload(basis, 8 * 1024, b"real-live-download")

(root / "upload").mkdir(parents=True, exist_ok=True)
(root / "download").mkdir(parents=True, exist_ok=True)
(root / "local").mkdir(parents=True, exist_ok=True)

(root / "upload" / "target.bin").write_bytes(basis)
(root / "download" / "target.bin").write_bytes(download_final)
(root / "local" / "upload.bin").write_bytes(upload_final)
(root / "local" / "download.bin").write_bytes(basis)
(root / "local" / "expected-upload.bin").write_bytes(upload_final)
(root / "local" / "expected-download.bin").write_bytes(download_final)
PY

cleanup() {
  if [[ "$KEEP_STACK" != "1" ]]; then
    docker compose -f "$CAPTURE_DIR/docker-compose.real-rsync.yml" down --remove-orphans >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

docker compose -f "$CAPTURE_DIR/docker-compose.real-rsync.yml" down --remove-orphans >/dev/null 2>&1 || true
docker compose -f "$CAPTURE_DIR/docker-compose.real-rsync.yml" up -d --build

for _ in $(seq 1 30); do
  if (exec 3<>/dev/tcp/127.0.0.1/2224) 2>/dev/null; then
    exec 3<&- 3>&-
    break
  fi
  sleep 1
done

HOST_PUB_LINE="$(docker exec aeroftp-rsync-real cat /etc/ssh/ssh_host_ed25519_key.pub 2>/dev/null || true)"
if [[ -z "$HOST_PUB_LINE" ]]; then
  echo "[harness] FATAL: could not extract Ed25519 public key from container" >&2
  exit 1
fi
HOST_PUB_BASE64="$(echo "$HOST_PUB_LINE" | awk '{print $2}')"
HOST_FINGERPRINT="$(echo -n "$HOST_PUB_BASE64" | base64 -d | sha256sum | awk '{print $1}')"
echo "[harness] real-rsync lane Ed25519 fingerprint: sha256:$HOST_FINGERPRINT"

SSH_OPTS=(
  -i "$CAPTURE_DIR/keys/id_ed25519"
  -p 2224
  -o StrictHostKeyChecking=no
  -o UserKnownHostsFile=/dev/null
  -o BatchMode=yes
  -o ConnectTimeout=5
)

# Real rsync upload: host client -> containerised server, delta via --checksum.
echo "[harness] capturing rsync upload (real protocol)"
rsync -avz --stats --checksum \
  -e "ssh ${SSH_OPTS[*]}" \
  "$WORKSPACE_DIR/local/upload.bin" \
  "testuser@127.0.0.1:/workspace/real/upload/target.bin" \
  >"$REAL_CAPTURE_SRC/.upload.stdout" 2>"$REAL_CAPTURE_SRC/.upload.stderr" \
  || {
    echo "[harness] real-rsync upload failed; stderr:" >&2
    cat "$REAL_CAPTURE_SRC/.upload.stderr" >&2
    exit 2
  }

echo "[harness] capturing rsync download (real protocol)"
rsync -avz --stats --checksum \
  -e "ssh ${SSH_OPTS[*]}" \
  "testuser@127.0.0.1:/workspace/real/download/target.bin" \
  "$WORKSPACE_DIR/local/download.bin" \
  >"$REAL_CAPTURE_SRC/.download.stdout" 2>"$REAL_CAPTURE_SRC/.download.stderr" \
  || {
    echo "[harness] real-rsync download failed; stderr:" >&2
    cat "$REAL_CAPTURE_SRC/.download.stderr" >&2
    exit 3
  }

echo "[harness] harvesting capture artifacts"
DEST_DIR="$ARTIFACTS_ROOT/$FREEZE_TS"
mkdir -p "$DEST_DIR"

ORDERED_SESSIONS=$(find "$REAL_CAPTURE_SRC" -maxdepth 1 -mindepth 1 -type d -printf '%f\n' 2>/dev/null | LC_ALL=C sort)

if [[ -z "$ORDERED_SESSIONS" ]]; then
  echo "[harness] FATAL: tee wrapper wrote no per-session capture dirs" >&2
  exit 4
fi

UPLOAD_SESSION=""
DOWNLOAD_SESSION=""
while read -r session; do
  [[ -z "$session" ]] && continue
  session_dir="$REAL_CAPTURE_SRC/$session"
  [[ -s "$session_dir/remote_command.txt" ]] || continue
  cmd=$(cat "$session_dir/remote_command.txt")
  if [[ "$cmd" == *"--sender"* ]]; then
    DOWNLOAD_SESSION="$session"
  else
    UPLOAD_SESSION="$session"
  fi
done <<< "$ORDERED_SESSIONS"

if [[ -z "$UPLOAD_SESSION" || -z "$DOWNLOAD_SESSION" ]]; then
  echo "[harness] FATAL: could not classify upload/download sessions" >&2
  echo "  sessions found: $ORDERED_SESSIONS" >&2
  exit 5
fi

cp -r "$REAL_CAPTURE_SRC/$UPLOAD_SESSION"   "$DEST_DIR/upload"
cp -r "$REAL_CAPTURE_SRC/$DOWNLOAD_SESSION" "$DEST_DIR/download"

cp "$REAL_CAPTURE_SRC/.upload.stdout"   "$DEST_DIR/upload/client.stdout.txt"
cp "$REAL_CAPTURE_SRC/.upload.stderr"   "$DEST_DIR/upload/client.stderr.txt"
cp "$REAL_CAPTURE_SRC/.download.stdout" "$DEST_DIR/download/client.stdout.txt"
cp "$REAL_CAPTURE_SRC/.download.stderr" "$DEST_DIR/download/client.stderr.txt"

HOST_RSYNC=unknown
if HOST_RAW=$(rsync --version 2>/dev/null); then
  HOST_RSYNC=$(printf '%s\n' "$HOST_RAW" | head -1)
fi
printf '%s\n' "$HOST_RSYNC" > "$DEST_DIR/host_rsync_version.txt"

SERVER_RSYNC=unknown
if SERVER_RAW=$(docker exec aeroftp-rsync-real rsync --version 2>/dev/null); then
  SERVER_RSYNC=$(printf '%s\n' "$SERVER_RAW" | head -1)
fi
printf '%s\n' "$SERVER_RSYNC" > "$DEST_DIR/server_rsync_version.txt"

cat > "$DEST_DIR/summary.env" <<EOF
freeze_ts=$FREEZE_TS
host_fingerprint=sha256:$HOST_FINGERPRINT
host_rsync=$HOST_RSYNC
server_rsync=$SERVER_RSYNC
upload_bytes_in=$(stat -c '%s' "$DEST_DIR/upload/capture_in.bin" 2>/dev/null || echo 0)
upload_bytes_out=$(stat -c '%s' "$DEST_DIR/upload/capture_out.bin" 2>/dev/null || echo 0)
download_bytes_in=$(stat -c '%s' "$DEST_DIR/download/capture_in.bin" 2>/dev/null || echo 0)
download_bytes_out=$(stat -c '%s' "$DEST_DIR/download/capture_out.bin" 2>/dev/null || echo 0)
EOF

echo "[harness] artifacts written to $DEST_DIR"

# Dev-only live test against the lane. Uses RSNP_TEST_REAL_* namespace so
# the native lane's tests stay unaffected.
export RSNP_TEST_REAL_HOST="127.0.0.1"
export RSNP_TEST_REAL_PORT="2224"
export RSNP_TEST_REAL_USER="testuser"
export RSNP_TEST_REAL_SSH_KEY="$CAPTURE_DIR/keys/id_ed25519"
export RSNP_TEST_REAL_MAX_FRAME_SIZE="$((32 * 1024 * 1024))"
export RSNP_TEST_REAL_HOST_FINGERPRINT="$HOST_FINGERPRINT"
export RSNP_TEST_REAL_REMOTE_UPLOAD_TARGET="/workspace/real/upload/target.bin"

pushd "$SRC_TAURI_DIR" >/dev/null
cargo test --features aerorsync \
  aerorsync::live_tests::live_real_rsync_lane_emits_protocol_31_greeting \
  -- --ignored --nocapture
popd >/dev/null

echo "[harness] S8a capture + live test complete"
