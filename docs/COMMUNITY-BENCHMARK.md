# Community Benchmark

> _Last updated: 2026-06-28_

Help AeroFTP build a real-world protocol comparison dataset without giving up
your privacy.

## Why this matters

When we say "protocol X is faster than protocol Y on provider Z", we want that
claim to be defensible. AeroFTP is developed and tested from a limited number of
connections, regions, operating systems, and provider accounts. That is not a
credible basis for a public protocol comparison page.

Speed and latency depend on your ISP, your distance from the provider region,
the time of day, the protocol stack, TLS handshake cost, provider throttling,
MTU, and local machine load. One developer cannot cover that space.

The Community Benchmark is our answer: a standard CLI command that any user can
run locally, against their own saved AeroFTP profiles, producing a sanitized JSON
report they can review and submit manually.

The goal is a public comparison page backed by community measurements, with
median, p95, standard deviation, sample counts, and honest caveats. Not marketing
claims. Not one fiber line in one country.

## What you run

The command is:

```bash
aeroftp-cli --profile "My Server" benchmark quick
```

The benchmark uses your existing saved profile. AeroFTP resolves credentials
from the encrypted vault internally. You never paste passwords or tokens into
the command line.

For a shareable report:

```bash
aeroftp-cli --profile "My Server" benchmark standard --consent-publish --report benchmark.json
```

`--consent-publish` does not upload anything automatically. It prints a
paste-ready JSON block that you can submit through GitHub after reviewing the
result.

## Benchmark Levels

| Level | Approximate time | Default payloads | Runs | Operations |
| ----- | ---------------- | ---------------- | ---- | ---------- |
| `quick` | About 30 seconds | 10 MB | 1 | upload, download |
| `standard` | About 5 minutes | 1 MB, 100 MB, 1 GB | 3 plus warm-up | upload, download, list, stat, delete |
| `deep` | About 30 minutes | 1 MB, 10 MB, 100 MB, 1 GB, 5 GB | 5 plus warm-up | upload, download, list, stat, delete |
| `custom` | You decide | configurable | configurable | configurable |

The actual duration depends on provider rate limits and your connection.

## Examples

Run the fastest local check:

```bash
aeroftp-cli --profile "SFTP Lab" benchmark quick
```

Write a private JSON report for your own archive:

```bash
aeroftp-cli --profile "SFTP Lab" benchmark quick --report ./bench-sftp.json
```

Prepare a community submission:

```bash
aeroftp-cli --profile "SFTP Lab" benchmark standard --consent-publish --report ./bench-sftp.json
```

Hide the provider hint as well:

```bash
aeroftp-cli --profile "SFTP Lab" benchmark standard --consent-publish --anonymize-extra
```

Run a small custom test:

```bash
aeroftp-cli --profile "SFTP Lab" benchmark custom --sizes 1M,10M --runs 2 --operations upload,download
```

Compare several saved profiles in one run (prints a combined comparison table):

```bash
aeroftp-cli benchmark standard --compare "S3 Backblaze,SFTP Lab,WebDAV Nextcloud"
```

Use `-i` to type the profiles interactively, or `--tui` for a full-screen
checklist.

Run only metadata operations:

```bash
aeroftp-cli --profile "SFTP Lab" benchmark custom --sizes 1M --runs 1 --operations list,stat,delete
```

Run a many-small-files workload (a separate axis: this is where per-file
overhead dominates and S3 typically beats WebDAV and FTP):

```bash
aeroftp-cli --profile "SFTP Lab" benchmark --file-count 1000 --file-size 64K
```

This adds `upload-all`, `list-dir`, `stat-all`, `download-all`, and `delete-all`
results, each reporting `files_per_second` and per-file latency. It runs
alongside the size sweep and is never averaged into the single-file numbers.

## What Happens Remotely

The benchmark creates temporary files under the profile's configured remote
base path:

```text
aeroftp-bench/<random-report-id>/
```

It uploads test payloads, downloads them back, measures metadata operations when
enabled, and then removes the temporary benchmark directory.

Use a non-critical test profile if you are running `standard` or `deep` on a
free-tier provider. The CLI keeps the operation surface small, but provider rate
limits are controlled by the provider.

After a run, the scratch base directory is removed (guarded), honouring any
`--test-root-prefix` you passed. The scratch tree is created with parent
directories in one step, which fixes pCloud WebDAV refusing a nested collection
whose parent did not yet exist.

## Reading the run

A few presentation details make multi-profile runs easier to read:

- The per-profile header and the comparison tables carry a protocol / transport
  column, so it is clear which transport each row was measured over.
- A public-IP fairness snapshot flags a run as not-comparable if your public IP
  changed mid-sweep (for example, the connection dropped and reconnected on a
  different address), since that breaks the apples-to-apples comparison.
- Multi-profile runs show `[k/N]` progress per profile, plus the per-operation
  run `k/N` within each profile.
- On a Yandex endpoint error, the output adds a region hint.
- The connection-type field is clarified as downstream Mbps.

## Report Format

Reports use schema version 1. The authoritative field set is the serialized
`BenchmarkReport` struct in `src-tauri/src/bin/aeroftp_cli.rs`, but contributors
do not need to read the full spec.

At a high level, each report contains:

| Section | Meaning |
| ------- | ------- |
| `schema_version` | Always `1` for the current format |
| `report_id` | Random UUID used to deduplicate accidental double submissions |
| `generated_at` | UTC timestamp rounded to the hour |
| `cli` | AeroFTP CLI version, build target, Rust compiler when available |
| `level` | `quick`, `standard`, `deep`, or `custom` |
| `environment` | Coarse OS, architecture, CPU class, and time-of-day bucket |
| `consent` | Whether publish mode and extra anonymization were enabled |
| `results` | One entry per protocol, operation, and payload size |
| `summary` | Total runs, total bytes, duration, and warnings |

Each result entry records:

| Field | Meaning |
| ----- | ------- |
| `protocol` | Protocol family, for example `SFTP`, `WebDAV`, `S3`, `Google Drive` |
| `provider_hint` | Coarse provider hint, or `null` with `--anonymize-extra` |
| `operation` | Single-file: `upload`, `download`, `list`, `stat`, `delete`. Many-files: `upload-all`, `download-all`, `list-dir`, `stat-all`, `delete-all` |
| `payload_size_bytes` | Payload size for that measurement (per-file size for the many-files axis) |
| `runs` | Number of measured runs, excluding warm-up (files processed for the many-files axis) |
| `file_count` | Many-files axis only: number of files exercised by the batch operation |
| `files_per_second` | Many-files axis only: throughput in files/second over the whole batch |
| `throughput_mbps` | p50, p95, stddev, min, max for transfer operations |
| `latency_ms` | p50, p95, stddev, min, max for operation latency (per-file latency on the many-files axis; p50 is the mean per-file time) |
| `errors` | Transient and fatal operation counts |
| `raw_runs` | Per-run measurements so aggregation can be recomputed |

The schema intentionally does not report arithmetic mean. Mean is too easy to
distort on long-tail network measurements. The public comparison will use
median, p95, standard deviation, sample count, and outlier filtering.

## Privacy Model

The benchmark is designed so useful performance data can be shared without
revealing account details.

| Collected | Never collected |
| --------- | --------------- |
| Protocol family | Passwords, access keys, refresh tokens, OAuth secrets |
| Coarse provider hint, unless `--anonymize-extra` is used | Hostnames, IP addresses, MAC addresses |
| Payload size and operation type | Bucket names, drive names, container names |
| Throughput and latency statistics | Paths, directories, filenames |
| AeroFTP CLI version and build target | Usernames, emails, account IDs |
| OS family and CPU class | Exact CPU model, exact OS version, kernel |
| Time-of-day bucket | Exact timestamp, city, postal code |
| Optional coarse region selected in the GitHub Issue form | Local network topology |

The CLI runs a sanitization sweep before writing or printing the report. If it
detects common credential prefixes, email addresses, local paths, or IP address
patterns, it refuses to write the report.

If the CLI has to choose between producing a useful report and avoiding a leak,
it must avoid the leak.

## Extra Anonymization

Use:

```bash
aeroftp-cli --profile "My Server" benchmark standard --consent-publish --anonymize-extra
```

With `--anonymize-extra`, the report replaces `provider_hint` with `null` and
stores only a short per-report hash. This lets maintainers group results within
one submission without showing which provider you used.

This is useful if you want to contribute measurements but do not want the public
Issue to reveal "this account tested S3" or "this account tested WebDAV".

## How To Submit

1. Update to the AeroFTP release that includes `aeroftp-cli benchmark`.
2. Pick a saved profile you are comfortable testing.
3. Run:

```bash
aeroftp-cli --profile "My Server" benchmark standard --consent-publish --report benchmark.json
```

4. Review the JSON if you want.
5. Open the benchmark report Issue template:

```text
https://github.com/axpdev-lab/aeroftp/issues/new?template=benchmark-report.yml
```

6. Paste the block between `BEGIN BENCHMARK REPORT` and `END BENCHMARK REPORT`.
7. Select a coarse region and connection type.
8. Submit.

Please do not hand-edit the JSON. If something looks wrong, open a normal bug
report and attach the exact CLI error instead.

## What We Do With Reports

Maintainers will manually validate early submissions, check the schema version,
deduplicate accidental repeats by `report_id`, and aggregate compatible results.

The first public output will be a docs page under:

```text
https://docs.aeroftp.app/test-reports/
```

The page will report results in cautious language, for example:

```text
In the community dataset for SFTP uploads, N=18, median throughput was X Mbps
and p95 latency was Y ms across Z coarse regions.
```

It will not claim universal winners from a small sample.

## Bias And Limitations

This dataset is voluntary. That means it has selection bias.

People who run benchmarks often have better-than-average connections, newer
machines, more technical confidence, and a higher chance of using fiber or data
center links. The published comparison page will say that clearly.

We especially value reports from:

- slow or metered connections
- mobile and 4G/5G connections
- corporate proxies
- rural links
- regions far from provider data centers
- self-hosted WebDAV, SFTP, FTP, and FTPS servers

Those reports make the dataset more honest.

## Phase Gate

This project deliberately starts small.

Phase 1 uses manual GitHub Issue submissions and manual aggregation. There is no
automatic upload endpoint and no live dashboard.

If the community submits fewer than 10 usable reports in the 2 months after the
benchmark release, we will close the initiative as `wontfix` and publish a
manual qualitative protocol comparison instead.

If 10 or more useful reports arrive, Phase 2 can open with a dedicated
submission endpoint, static dashboard, filters, and automated aggregation.

That gate prevents us from maintaining empty infrastructure.

## Good Benchmark Hygiene

- Close large downloads, cloud sync clients, video calls, and game launchers.
- Avoid running `standard` or `deep` during a provider outage.
- If possible, run the same profile at different times of day and submit each
  report separately.
- Prefer real saved profiles over synthetic local-only endpoints.
- Use `quick` first if you are unsure about quota or rate limits.
- Use `standard` for the most useful community submission.
- Use `deep` only on non-critical accounts with enough bandwidth and quota.

## Troubleshooting

If the command says the report failed sanitization, do not bypass it. Open a bug
report and include the error message. The sweep is intentionally strict.

If cleanup fails, delete the `aeroftp-bench/` directory manually from that
profile (under the profile's base path, or under your `--test-root-prefix`)
before running another benchmark.

If a provider returns rate-limit or quota errors, retry later with `quick` or a
smaller custom run:

```bash
aeroftp-cli --profile "My Server" benchmark custom --sizes 1M,10M --runs 1 --operations upload,download
```

If authentication fails, re-authorize the saved profile in AeroFTP first. The
benchmark does not ask for credentials and does not accept credentials on the
command line.
