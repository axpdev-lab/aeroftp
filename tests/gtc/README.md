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
# Default run: all five protocols, CLI surfaces only (GTC-0 scope)
tests/gtc/parity_harness.sh

# Subset for fast iteration
tests/gtc/parity_harness.sh --protocols sftp,s3

# Dry-run: generate corpus, write plan, do not transfer
tests/gtc/parity_harness.sh --dry-run

# Override band (per-protocol speedup floor/ceiling, picks the
# tighter of spec vs flag)
tests/gtc/parity_harness.sh --band sftp:2.0:5.0
```

The run id is the UTC timestamp at start (`YYYYMMDDTHHMMSSZ`). Output
goes under `tests/gtc/reports/<run-id>/` and is gitignored. The
**baseline floor** for each protocol (single-stream wall-clock) gets
appended to
`docs/dev/roadmap/APPENDIX-GUI-TRANSFER-CONVERGENCE/baselines/legacy-floor.csv`
the first time it's recorded.

## What is and is not exercised in GTC-0

In: `cli-segments-1` (legacy single-stream baseline),
`cli-segments-n` (DAG range), `cli-parallel-n` (DAG batch),
byte-identity assertions, exit-code shape per CLI.

Out: `gui-single`/`gui-batch`/`gui-sync`/`gui-cross`. Those surfaces
are still legacy per the pin and would just re-measure
`cli-segments-1` while paying the Tauri IPC setup cost. They get
added in GTC-1 as the first surface (Panel batch) actually starts
using DAG.
