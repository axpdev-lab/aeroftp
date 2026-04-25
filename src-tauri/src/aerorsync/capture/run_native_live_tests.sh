#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)"
CAPTURE_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
SRC_TAURI_DIR="$ROOT_DIR"
WORKSPACE_DIR="$CAPTURE_DIR/workspace/native"
BIN_DIR="$CAPTURE_DIR/bin"
KEEP_STACK="${KEEP_STACK:-0}"

mkdir -p "$WORKSPACE_DIR/upload" "$WORKSPACE_DIR/download" "$WORKSPACE_DIR/local" "$BIN_DIR"
rm -f "$WORKSPACE_DIR"/upload/target.bin "$WORKSPACE_DIR"/download/target.bin "$WORKSPACE_DIR"/local/upload.bin "$WORKSPACE_DIR"/local/download.bin "$WORKSPACE_DIR"/local/expected-upload.bin "$WORKSPACE_DIR"/local/expected-download.bin

pushd "$SRC_TAURI_DIR" >/dev/null
cargo build --features aerorsync --bin aerorsync_serve
popd >/dev/null

cp "$SRC_TAURI_DIR/target/debug/aerorsync_serve" "$BIN_DIR/aerorsync_serve"
chmod +x "$BIN_DIR/aerorsync_serve"

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
upload_final = mutate_payload(basis, 8 * 1024, b"native-live-upload")
download_final = mutate_payload(basis, 8 * 1024, b"native-live-download")

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
    docker compose -f "$CAPTURE_DIR/docker-compose.native.yml" down --remove-orphans >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

docker compose -f "$CAPTURE_DIR/docker-compose.native.yml" down --remove-orphans >/dev/null 2>&1 || true

docker compose -f "$CAPTURE_DIR/docker-compose.native.yml" up -d --build

# Wait for sshd to accept TCP connections. The entrypoint generates host keys
# on first start, so we must not grab the fingerprint too early.
for attempt in $(seq 1 30); do
  if (exec 3<>/dev/tcp/127.0.0.1/2223) 2>/dev/null; then
    exec 3<&- 3>&-
    break
  fi
  sleep 1
done

# Extract the host's Ed25519 public key from inside the container and compute
# its SHA-256 fingerprint in the exact form that `ssh2::Session::host_key()`
# returns in-process (raw public_blob bytes, no text prefix). The public line
# at /etc/ssh/ssh_host_ed25519_key.pub is `ssh-ed25519 <base64> <comment>`,
# and that base64 is the exact public_blob we want to hash.
HOST_PUB_LINE="$(docker exec aeroftp-rsync-native cat /etc/ssh/ssh_host_ed25519_key.pub 2>/dev/null || true)"
if [[ -n "$HOST_PUB_LINE" ]]; then
  HOST_PUB_BASE64="$(echo "$HOST_PUB_LINE" | awk '{print $2}')"
  HOST_FINGERPRINT="$(echo -n "$HOST_PUB_BASE64" | base64 -d | sha256sum | awk '{print $1}')"
  echo "[harness] container ed25519 fingerprint: sha256:$HOST_FINGERPRINT"
  export RSNP_TEST_HOST_FINGERPRINT="$HOST_FINGERPRINT"
else
  echo "[harness] WARNING: could not extract host fingerprint; live tests will run with AcceptAny"
fi

export RSNP_TEST_HOST="127.0.0.1"
export RSNP_TEST_PORT="2223"
export RSNP_TEST_USER="testuser"
export RSNP_TEST_SSH_KEY="$CAPTURE_DIR/keys/id_ed25519"
export RSNP_TEST_MAX_FRAME_SIZE="$((32 * 1024 * 1024))"
export RSNP_TEST_REMOTE_UPLOAD_TARGET="/workspace/native/upload/target.bin"
export RSNP_TEST_REMOTE_DOWNLOAD_TARGET="/workspace/native/download/target.bin"
export RSNP_TEST_LOCAL_UPLOAD_FILE="$WORKSPACE_DIR/local/upload.bin"
export RSNP_TEST_LOCAL_DOWNLOAD_FILE="$WORKSPACE_DIR/local/download.bin"
export RSNP_TEST_EXPECT_UPLOAD_FILE="$WORKSPACE_DIR/upload/target.bin"
export RSNP_TEST_EXPECT_DOWNLOAD_FILE="$WORKSPACE_DIR/local/expected-download.bin"

pushd "$SRC_TAURI_DIR" >/dev/null
cargo test --features aerorsync live_tests -- --ignored --nocapture
popd >/dev/null