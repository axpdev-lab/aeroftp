# GTC parity harness

Drives the GUI<->CLI parity oracle for the GUI-Transfer-Convergence
filone. See:

- `docs/dev/roadmap/APPENDIX-GUI-TRANSFER-CONVERGENCE.md` (Piano)
- `docs/dev/roadmap/APPENDIX-GUI-TRANSFER-CONVERGENCE/baselines/2026-05-19_parity-oracle-spec.md`
- `docs/dev/roadmap/APPENDIX-GUI-TRANSFER-CONVERGENCE/baselines/2026-05-19_legacy-call-chains-pin.md`

## Prerequisites

- `aeroftp-cli` >= 3.8.3 on PATH (installed system-wide from any
  recent release; the harness `--check` step prints the resolved
  version).
- The five axpbuntu lab profiles in the vault, ids verified by
  `aeroftp-cli profiles --format json --show name,type,host`:
  - `srv_1778600830336_6pvrx7450` (SFTP admin)
  - `srv_axpbuntu_ftp_plain`
  - `srv_axpbuntu_ftps_explicit`
  - `srv_axpbuntu_s3_minio`
  - `srv_axpbuntu_webdav_vanilla`
- Vault unlocked (OS keyring on the desktop session OR
  `AEROFTP_MASTER_PASSWORD` env var for headless).

## Usage

```bash
# Default: CLI surfaces only (back-compat). Five protocols, segments-1
# / segments-n / parallel-n cells per protocol.
tests/gtc/parity_harness.sh

# CLI subset for fast iteration
tests/gtc/parity_harness.sh --protocols sftp,s3

# GUI surfaces only (cargo-test driven)
tests/gtc/parity_harness.sh --suite gui
# equivalent shortcut
tests/gtc/parity_harness.sh --gui-only

# Full matrix: CLI + GUI in one run
tests/gtc/parity_harness.sh --suite all

# Subset of GUI cells
tests/gtc/parity_harness.sh --suite gui \
    --gui-cells gui-single-sftp,gui-cross-sftp-s3

# Dry-run: generate corpus, write plan, do not transfer
tests/gtc/parity_harness.sh --dry-run

# Override band (per-protocol speedup floor/ceiling for CLI cells, OR
# per-cell-key for GUI cells; repeatable)
tests/gtc/parity_harness.sh --band sftp:2.0:5.0
tests/gtc/parity_harness.sh --suite gui --band gui-single-sftp:2.2:4.5
```

The run id is the UTC timestamp at start (`YYYYMMDDTHHMMSSZ`). Output
goes under `tests/gtc/reports/<run-id>/` and is gitignored. The
**baseline floor** for each protocol (single-stream wall-clock) gets
appended to
`docs/dev/roadmap/APPENDIX-GUI-TRANSFER-CONVERGENCE/baselines/legacy-floor.csv`
the first time it's recorded.

## CLI cells (GTC-0 scope)

In: `cli-segments-1` (legacy single-stream baseline),
`cli-segments-n` (DAG range), `cli-parallel-n` (DAG batch),
byte-identity assertions, exit-code shape per CLI.

| protocol | floor | ceiling |
|----------|-------|---------|
| sftp     | 1.5   | 6.0     |
| ftp      | 1.5   | 5.0     |
| ftps     | 1.3   | 5.0     |
| s3       | 2.0   | 6.0     |
| webdav   | 1.3   | 4.0     |

## GUI cells (GTC-6, added 2026-05-20)

GUI surfaces don't run through `aeroftp-cli`: they're driven via the
gated `integration_gtc_wan_segmented.rs` cargo test binary which
exercises each GUI Tauri entry-point directly against the axpbuntu
lab. The harness invokes the corresponding test fn, parses the
single-line summary it emits on stderr, and produces one parity cell.

Test binary requirements (same as the cargo integration suite):
- axpbuntu lab credentials in the vault (admin SFTP + FTP + S3/MinIO).
- ~64 MiB free at `$TMPDIR` for the round-trip files.
- WAN reachability to `49.13.171.110` (no firewall blocking SSH/FTP/S3).

| cell key            | engine                            | protocol  | floor | ceiling | rationale                                       |
|---------------------|-----------------------------------|-----------|-------|---------|-------------------------------------------------|
| `gui-single-sftp`   | `provider_download_file`+ segs    | sftp      | 2.0   | 5.0     | Empirical median 2.98x on axpbuntu, 64 MiB.     |
| `gui-single-s3`     | `provider_download_file`+ segs    | s3 (MinIO)| 0.9   | 5.0     | MinIO on the lab is too fast for 64 MiB to amortize segmenting; single-stream already lands in ~7-10s and run-to-run jitter dominates (observed 1.79x then 0.96x back-to-back). Floor is no-regression-ish; correctness is the gate, speedup is the signal. |
| `gui-single-ftp`    | `provider_download_file`+ segs    | ftp       | 0.8   | 5.0     | Same problem as S3 cell: single-stream finishes in ~8-10s on the lab and jitter dominates. Three back-to-back runs measured 1.32x / 1.74x / 0.90x. No-regression-ish floor; byte-identity is the real gate. |
| `gui-sync-sftp`     | `sync_download_transfer` (GTC-3)  | sftp      | 1.8   | 4.0     | AeroSync overlay narrows ceiling vs. raw.       |
| `gui-cross-sftp-s3` | `transfer_orchestrator::execute_batch` (GTC-4) | sftp -> s3 | 2.5 | 5.0   | DAG fan-out across two providers, 4x16 MiB.     |

Bands are policy decisions taken 2026-05-20. The first two GUI cells
were widened from the initial 1.8 / 1.8 after three consecutive live
`--suite all` runs on axpbuntu (run ids `20260520T150140Z`,
`20260520T151623Z`, `20260520T152506Z`):

- `gui-single-s3` floor 1.8 -> 0.9: three runs measured 1.79x / 0.96x
  / 1.13x. MinIO on the lab is too fast for 64 MiB to amortize
  segmenting (single-stream already finishes in 7-10s) and WAN jitter
  dominates.
- `gui-single-ftp` floor 1.8 -> 0.8: same root cause. Three runs
  measured 1.32x / 1.74x / 0.90x on vsftpd.

For both cells the floor sits below 1.0 as a "no-regression" guard,
not a speedup target. Byte-identity on the round-trip remains the
real gate. To get back to a meaningful speedup floor on those two
cells, either move to a much larger fixture file (>= 256 MiB) or run
on a slower upstream than Hetzner CPX22.

The bands protect against:
1. Regressions below the empirical median minus WAN jitter (floor).
2. Anomalous "too good" runs from caching / fixture corruption (ceiling).

Override on the command line with `--band <cell-key>:<floor>:<ceiling>`.

### Out of GTC-6 scope

- `gui-batch-sftp` / `gui-batch-s3`: the FTP GUI batch path runs on
  `FtpDownloadExecutor`, not the `ProviderDownloadExecutor` that the
  segments engine targets (no-double-pool invariant). It would
  re-measure the legacy floor (0.9 .. 1.2x) and is excluded by design.
- `rclone_crypt_*` downloads: full-buffer-in-memory path, doesn't
  touch the segmented helper. Wiring requires a separate slice that
  also has to handle decrypt streaming.
- AeroVault overlay extract: same family as crypt, same exclusion.
