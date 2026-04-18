# Strada C — Capture Harnesses (Three Lanes)

This folder hosts the Docker + shell harnesses that produce the fixtures
the Strada C prototype is developed against. Three independent "lanes"
exist, each with its own port, image, and purpose.

Everything here is prototype-only and already covered by the repository
`.gitignore` for `src-tauri/src/rsync_native_proto/`.

## Lane matrix

| lane | port | stack file                        | container name               | role                                                                 |
|------|------|-----------------------------------|------------------------------|----------------------------------------------------------------------|
| 1    | 2222 | `docker-compose.yml`              | `aeroftp-rsync-capture`      | Wrapper baseline (rsync-over-ssh text oracle). **Frozen.**            |
| 2    | 2223 | `docker-compose.native.yml`       | `aeroftp-rsync-native`       | RSNP client ↔ RSNP server (Sinergie 6/7 live tests).                  |
| 3    | 2224 | `docker-compose.real-rsync.yml`   | `aeroftp-rsync-real`         | **S8a**: host rsync client ↔ real rsync server with byte-level oracle.|

Only lane 2 runs inside the Rust test process; lanes 1 and 3 are driven by
shell harnesses.

## Lane 1 — wrapper baseline (frozen)

Reference data for the current production wrapper (`rsync_over_ssh.rs`).
Captures stdout/stderr, `execve` chains, and SSH debug lines into
`artifacts/<timestamp>/`. **Do not modify** — the frozen artifact under
`artifacts/20260417_154800/` is the parity oracle referenced by
`fixtures.rs`.

```bash
./capture_wrapper_transcripts.sh
```

## Lane 2 — native RSNP live tests

Our dev-only `rsync_proto_serve` binary is cross-compiled from the host
and mounted into a debian:trixie-slim container. The Rust test process
(via `ssh2`) drives real SSH exec sessions against it and runs the five
live tests in `live_tests.rs` (probe / upload / download / host-key
mismatch / cancel-during-read).

sshd is pinned to Ed25519-only so the in-process fingerprint can be
extracted deterministically with `docker exec cat` and exported as
`RSNP_TEST_HOST_FINGERPRINT`.

```bash
./run_native_live_tests.sh
```

## Lane 3 — real rsync byte oracle (S8a)

A debian:bookworm-slim container running stock rsync 3.2.7. sshd's
`ForceCommand` points at `rsync_server_wrapper.sh`, which `exec`s
`rsync_proxy.py`. The Python proxy reads the wrapper's stdin/stdout
(the SSH exec channel), spawns `bash -c "$SSH_ORIGINAL_COMMAND"`, and
fans each direction to a file under
`/workspace/real_capture/<ts>/{capture_in.bin, capture_out.bin,
stderr.txt}` before forwarding the bytes on. The result is a byte-level
transcript of the real rsync protocol 31/32 as flown over SSH — the
oracle the wire work in S8b+ parses against.

Why a Python proxy and not a bash `tee | cmd | tee` pipeline: the naive
pipeline deadlocks on subprocess exit (upstream `tee` cannot learn that
the downstream reader is gone), and the FIFO-based bash variant has
fd-sharing bugs between the main shell and the forked `bash -c` that
cause rsync to see EOF on stdin immediately after its 4-byte version
greeting. Python `select.select` + `os.read`/`os.write` sidesteps both
issues with no buffering surprises.

The client is the host's own `rsync` binary (over SSH on port 2224).
Runs upload + download with `--checksum` so the transcript contains
real delta traffic, then copies the per-session capture dirs into
`artifacts_real/<FREEZE_TS>/{upload,download}/` and writes a
`summary.env`.

```bash
./run_real_rsync_capture.sh
```

One run is kept under `artifacts_real/frozen/` as the stable oracle
referenced from `fixtures.rs::RealRsyncBaselineByteTranscript::try_load_frozen`.

The harness also runs a dev-only Rust test
(`live_real_rsync_lane_emits_protocol_31_greeting`) that opens a raw
exec channel, reads the server's greeting bytes, and asserts the first
byte matches a known protocol version low byte (0x1F or 0x20). The test
uses the `RSNP_TEST_REAL_*` env namespace so it does not collide with
lane 2.

## Conventions

- Do not `docker compose build` two lanes in the same shell session
  without explaining why — they use different base images and the
  build caches can mask issues.
- `KEEP_STACK=1` keeps the lane's Docker stack alive after the script
  exits (useful for `docker exec` post-mortem). Defaults to 0 (teardown
  on EXIT).
- Ports, container names, and the artifact roots are designed not to
  overlap across lanes. If you need to run two lanes in parallel the
  only thing to watch is host memory.
