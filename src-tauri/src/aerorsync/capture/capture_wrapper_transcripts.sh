#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")" && pwd)"
ARTIFACTS_ROOT="$ROOT_DIR/artifacts"
TIMESTAMP="$(date +%Y%m%d_%H%M%S)"
OUT_DIR="$ARTIFACTS_ROOT/$TIMESTAMP"
KEY_DIR="$ROOT_DIR/keys"
WORKSPACE_DIR="$ROOT_DIR/workspace"
KNOWN_HOSTS="$OUT_DIR/known_hosts"
KEEP_STACK="${KEEP_STACK:-0}"

mkdir -p "$OUT_DIR" "$KEY_DIR" "$WORKSPACE_DIR/remote" "$WORKSPACE_DIR/local"

if [[ ! -f "$KEY_DIR/id_ed25519" ]]; then
  ssh-keygen -q -t ed25519 -N '' -f "$KEY_DIR/id_ed25519"
fi

chmod 600 "$KEY_DIR/id_ed25519"
chmod 644 "$KEY_DIR/id_ed25519.pub"

cleanup() {
  if [[ "$KEEP_STACK" != "1" ]]; then
    docker compose -f "$ROOT_DIR/docker-compose.yml" down -v >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

docker compose -f "$ROOT_DIR/docker-compose.yml" up --build -d

SSH_BASE=(
  ssh
  -p 2222
  -i "$KEY_DIR/id_ed25519"
  -o StrictHostKeyChecking=accept-new
  -o UserKnownHostsFile="$KNOWN_HOSTS"
  -o BatchMode=yes
)

SSH_CAPTURE=(
  ssh
  -vvv
  -p 2222
  -i "$KEY_DIR/id_ed25519"
  -o StrictHostKeyChecking=accept-new
  -o UserKnownHostsFile="$KNOWN_HOSTS"
  -o BatchMode=yes
)

for _ in $(seq 1 30); do
  if "${SSH_BASE[@]}" testuser@127.0.0.1 true >/dev/null 2>&1; then
    break
  fi
  sleep 1
done

"${SSH_BASE[@]}" testuser@127.0.0.1 "mkdir -p /workspace/upload /workspace/download /workspace/notes"

BASE_FILE="$OUT_DIR/base.bin"
UPLOAD_FILE="$OUT_DIR/upload_candidate.bin"
DOWNLOAD_LOCAL_FILE="$OUT_DIR/download_existing.bin"

dd if=/dev/urandom of="$BASE_FILE" bs=1M count=8 status=none
cp "$BASE_FILE" "$UPLOAD_FILE"
cp "$BASE_FILE" "$DOWNLOAD_LOCAL_FILE"

dd if=/dev/urandom of="$UPLOAD_FILE" bs=4096 count=12 seek=32 conv=notrunc status=none
dd if=/dev/urandom of="$UPLOAD_FILE" bs=4096 count=12 seek=512 conv=notrunc status=none
dd if=/dev/urandom of="$UPLOAD_FILE" bs=4096 count=12 seek=1024 conv=notrunc status=none
touch -d '+5 seconds' "$UPLOAD_FILE"

RSYNC_SEED_SSH="ssh -p 2222 -i $KEY_DIR/id_ed25519 -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile=$KNOWN_HOSTS -o BatchMode=yes"

rsync -a -e "$RSYNC_SEED_SSH" "$BASE_FILE" testuser@127.0.0.1:/workspace/upload/target.bin
rsync -a -e "$RSYNC_SEED_SSH" "$BASE_FILE" testuser@127.0.0.1:/workspace/download/target.bin

"${SSH_BASE[@]}" testuser@127.0.0.1 \
  "dd if=/dev/urandom of=/workspace/download/target.bin bs=4096 count=12 seek=48 conv=notrunc status=none && \
   dd if=/dev/urandom of=/workspace/download/target.bin bs=4096 count=12 seek=768 conv=notrunc status=none && \
   dd if=/dev/urandom of=/workspace/download/target.bin bs=4096 count=12 seek=1280 conv=notrunc status=none"

sha256sum "$BASE_FILE" "$UPLOAD_FILE" "$DOWNLOAD_LOCAL_FILE" > "$OUT_DIR/local.sha256.txt"
"${SSH_BASE[@]}" testuser@127.0.0.1 \
  "sha256sum /workspace/upload/target.bin /workspace/download/target.bin" > "$OUT_DIR/remote.sha256.before.txt"

capture_case() {
  local name="$1"
  shift
  strace -f -e execve -s 400 -o "$OUT_DIR/${name}.execve.log" "$@" \
    > "$OUT_DIR/${name}.stdout.txt" \
    2> "$OUT_DIR/${name}.stderr.txt"
}

UPLOAD_CAPTURE_SSH="ssh -vvv -p 2222 -i $KEY_DIR/id_ed25519 -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile=$KNOWN_HOSTS -o BatchMode=yes"

capture_case \
  upload_actual \
  rsync -a -z --info=progress2 --stats -e "$UPLOAD_CAPTURE_SSH" \
  "$UPLOAD_FILE" \
  testuser@127.0.0.1:/workspace/upload/target.bin

capture_case \
  download_actual \
  rsync -a -z --info=progress2 --stats -e "$UPLOAD_CAPTURE_SSH" \
  testuser@127.0.0.1:/workspace/download/target.bin \
  "$DOWNLOAD_LOCAL_FILE"

grep -n "Sending command:" "$OUT_DIR/upload_actual.stderr.txt" > "$OUT_DIR/upload.remote_command.txt" || true
grep -n "Sending command:" "$OUT_DIR/download_actual.stderr.txt" > "$OUT_DIR/download.remote_command.txt" || true

"${SSH_BASE[@]}" testuser@127.0.0.1 \
  "sha256sum /workspace/upload/target.bin /workspace/download/target.bin" > "$OUT_DIR/remote.sha256.after.txt"
sha256sum "$UPLOAD_FILE" "$DOWNLOAD_LOCAL_FILE" > "$OUT_DIR/local.after.sha256.txt"

{
  echo "capture_dir=$OUT_DIR"
  echo "rsync=$(rsync --version | head -n 1)"
  echo "ssh=$(ssh -V 2>&1)"
  echo "upload_remote_command=$(tr '\n' ' ' < "$OUT_DIR/upload.remote_command.txt")"
  echo "download_remote_command=$(tr '\n' ' ' < "$OUT_DIR/download.remote_command.txt")"
} > "$OUT_DIR/summary.env"

echo "Artifacts written to: $OUT_DIR"
