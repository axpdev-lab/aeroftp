# Testing & Verification

This document describes how AeroFTP is tested and what is covered, protocol by protocol and provider by provider. It is meant to be honest about both strengths and gaps: where coverage is automated and where it relies on documented manual verification.

This page is a living document. It is updated continuously as coverage grows, and new tests are added over time, so the matrix below reflects the current state and is expected to improve release after release.

If you find a gap or a case we do not cover, please open an issue or a discussion. Community help with cross-platform and live-provider testing is welcome (see "Community testing" at the end).

---

## Testing layers

AeroFTP is tested at several layers, each catching a different class of problem.

| Layer | What it covers | What it cannot catch |
|-------|----------------|----------------------|
| Deterministic unit tests (Rust) | URL building, auth headers, response parsing, error mapping, crypto, error-correction, the sync state machine | Real network/API behavior, auth, rate limits |
| Frontend tests (vitest) | Sync planning, transfer queue, schema validation, hooks, formatting | Anything below the UI/logic layer |
| Automated live integration tests | Real transfers against lab servers (SFTP, FTP, FTPS, S3, WebDAV), Backblaze B2, AWS STS, and a Docker rsync fixture | Providers that need paid accounts or interactive OAuth |
| Cross-implementation / format golden | Our crypto and container formats checked against an independent implementation (e.g. the official `cryptomator-cli`) | Behavior outside the tested format |
| Manual live-test runs | Real round-trips against live cloud accounts, recorded with byte-identity (sha256) proof and the app version | Not re-run automatically on every release |

Current headline counts: about 2,700 Rust unit tests, about 25 Rust live-integration tests (credential or Docker gated), and about 175 frontend test files. All of these run; none are placeholders.

A note on honesty: a fully automated, "click to run before every release" suite is realistic for the deterministic tests and for the lab-backed protocols. It is not realistic to put 25 or more live OAuth cloud accounts into public CI without shipping credentials, hitting rate limits, and producing flaky results. For those providers we use documented manual verification per release cycle, plus community testers. The matrix below is explicit about which is which.

---

## Coverage matrix

Legend:

- **Unit**: deterministic offline tests exist for this provider's code paths.
- **Live (auto)**: an automated integration test exercises a real server in CI/lab.
- **Live (manual)**: most recent recorded manual round-trip (byte-identity verified), with the app version it was run against. "pending" means no current run on record.
- **Interop**: verified against an independent implementation or format spec.

### Transport protocols

| Protocol | Unit | Live (auto) | Live (manual) | Interop |
|----------|:----:|:-----------:|---------------|---------|
| SFTP | yes | yes (pool, pipeline, delta, 64 MiB WAN) | v4.0.5 | - |
| FTP | yes | yes (pool, 64 MiB WAN) | v4.0.5 | - |
| FTPS | yes | yes (segmented) | v4.0.5 | TLS |
| WebDAV | yes | yes (server-side copy) | v4.0.2 (Nextcloud) | RFC 4918 parse |
| S3 | yes | yes (multipart, copy, segmented, STS) | v4.0.0 | - |
| Azure Blob | yes | - | pending | - |
| OpenStack Swift | yes | - | pending | - |

### Native cloud providers

| Provider | Unit | Live (auto) | Live (manual) | Notes |
|----------|:----:|:-----------:|---------------|-------|
| Backblaze B2 | yes | yes (multipart 250 MB, 5.1 GB rename) | env-gated CI | Best automated cloud coverage |
| Google Drive | yes | - | v4.0.0 (multipart 500 MiB) | |
| Dropbox | yes | - | v4.0.0 (upload sessions) | |
| OneDrive | yes | - | v4.0.0 (session upload) | |
| Box | yes | - | v4.0.0 (chunked 100 MiB) | |
| pCloud | yes | - | v4.0.0 (chunked 500 MiB) | |
| MEGA | yes | - | v4.0.0 (buffered multipart) | Crypto layer unit-tested |
| FileLu | yes | - | v4.0.0 (API, S3, WebDAV) | 3 endpoints byte-identical |
| Filen | yes | - | partial | E2E round-trip not yet closed |
| Koofr | yes | - | pending (v4 re-test) | Recent upload-error fix pinned |
| Yandex Disk | yes | - | pending | |
| kDrive | yes | - | pending | |
| OpenDrive | yes | - | pending | Recent size-limit fix pinned |
| Zoho WorkDrive | yes | - | pending | |
| Jottacloud | yes | - | pending | |
| Internxt | yes | - | pending | E2E encrypted |
| 4shared | yes | - | pending | |
| Drime | yes | - | pending | |
| GitHub | yes | - | pending | |
| GitLab | yes | - | pending | |
| Immich | yes | - | pending | |
| ImageKit | yes | - | n/a | No multipart by design |
| Uploadcare | yes | - | pending | |
| Cloudinary | yes | - | pending | |

### Cross-cutting features

| Area | Coverage |
|------|----------|
| Cryptomator format | Unit tests plus forward and reverse interop against the official `cryptomator-cli` (cryptofs 2.8.0), plus an edge-case sweep (empty file, unicode names, deep nesting, empty folders, over-long names, symlink skip) |
| AeroVault containers + error correction | Unit tests, byte-identical golden against the independent `aerovault` crate, and a live create / scrub / corrupt / repair / extract round-trip; Reed-Solomon error correction passes a 1,897-case seeded stress run and fails closed beyond budget |
| AeroCrypt overlay | Unit tests plus live encrypt/decrypt round-trips (byte-identical, zero-knowledge at rest verified) |
| rclone-crypt interoperability | Unit tests for the rclone crypt format |
| Sync engine | State machine, journal, retry, and conflict-mode tests plus a Docker rsync-over-SSH integration fixture |
| Transfer engine (DAG) | Single-file runner plus batch/sync wrappers; live multipart, direct server-side copy, and segmented-download coverage are path/provider-specific (DAG range is opt-in) |
| Profile imports | Tests for importing from rclone, FileZilla, PuTTY, WinSCP, Cyberduck, and more |
| Archive compression | Live round-trip across zip, 7z, tar, tar.gz, tar.xz, tar.bz2 with password and level variations |
| Provider offline parsers (v4.1.0) | A deterministic offline parser plus an HTTP-status-to-error test net now covers all 16 unit-only storage providers (Azure, Koofr, Yandex, Cloudinary, Uploadcare, GitLab, Filen, 4shared, kDrive, Jottacloud, Zoho, Internxt, Drime, Immich; GitHub was already covered; Swift is skipped, as its only profile, Blomp, is inactive) |

---

## Running the tests

Deterministic tests (no credentials, run anywhere):

```bash
# Rust unit tests
cd src-tauri && cargo test

# Frontend tests
npm run test
```

Live integration tests are gated behind credentials or Docker fixtures and are marked `#[ignore]`, so they do not run by default. They require lab servers and/or provider credentials that are not part of the public repository.

---

## Release cadence

Before tagging a release we run the deterministic suite (Rust and frontend) and the lab-backed integration tests, and CI builds on Linux, Windows, and macOS must be green before the tag is pushed. Live cloud verification for the providers marked "Live (manual)" is performed per release cycle and recorded with byte-identity proof.

We are expanding this in two directions: a single pre-release smoke entrypoint that runs the deterministic and lab suites and reports a pass/skip matrix, and systematic deterministic parsing and error-mapping tests for the providers that currently have unit coverage only. The goal is that a change on a provider's side or in a third-party dependency is caught by a failing test rather than by a user. Because this is ongoing work, the coverage matrix above is updated continuously as new tests land.

---

## Community testing

Some coverage can only come from real accounts and real platforms we do not have. If you would like to help test a specific provider or platform (macOS, Windows, a Linux distro, or a cloud account you use), please say so in the discussions. Contributions are credited in the changelog and the contributors list.
