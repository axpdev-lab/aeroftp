# Native bulk transfers for external providers

A provider may transfer a bounded set of planned files in one native operation. This amortizes external-program startup without replacing the shared transfer DAG or changing file identities. The first adapter is Proton Drive CLI mode. Native Proton Drive profiles retain their existing client.

## Contract

`StorageProvider` exposes optional `upload_bulk` and `download_bulk` hooks. `bulk_upload` and `bulk_download` are separate live capabilities, unsupported by default. Older capability snapshots deserialize with both unsupported. Clone pools and native bulks are independent capabilities.

A bulk contains files only, has one positional outcome per submitted item and retains each original local/remote path. Outcomes distinguish completed, known failure, uncertain completion and not attempted. Never replay an entire partly completed bulk. Cancellation or a missing, malformed or incomplete native summary cannot prove that no upload reached the destination.

An item the adapter cannot carry (a name the native program would map, a source that changed since planning, a sibling in another folder or past the bounds, two names the local filesystem folds together) comes back as not attempted, and the caller runs its single-file path for it once. So does an item the native program reported as failed for a transient reason (a 429, a timeout, a network error); the single-file path keeps the adapter's own transient retries. Known permanent failures stay final, and an uncertain upload is never retried.

Groups share the relevant destination and remote parent and split on duplicate portable basenames. Files of one folder are gathered even when the caller interleaves folders (a sync plan, a tree walk), provided every destination in the batch is distinct; with a repeated destination only adjacent files are grouped, so the last write stays the caller's. Groups follow the order of their first file. Relative local paths are planned and passed in their absolute spelling. Each group is bounded to 256 files, 128 MiB of planned payload and a command-line budget: every file counts the longer of its absolute local path and its remote path (at least 64 bytes, for node-identifier sources), plus 1 KiB per command, against 24 KiB on Windows and 96 KiB elsewhere. Expanded native arguments are checked again. Large files, local paths needing CLI glob quoting and incompatible names retain the existing single-file path. CLI admission also preserves the independent worker path when grouping would not reduce native invocation overhead enough to justify a single-lease bulk.

The streaming DAG admits one bounded native operation under its existing resource, endpoint and disk leases. Completion, errors, batch accounting and progress events retain per-file identities. Unstarted groups stay unstarted on cancellation; a graph fault settles started files once. Timeouts retain the per-file budget for every submitted item rather than applying one file's timeout to a whole group.

## Proton CLI adapter

Uploads use multiple explicit local sources and preserve `create-new-revision` conflict handling. A source that disappeared or changed size fails independently; valid siblings can proceed. Mutating bulk commands are executed once. The adapter does not retry a whole bulk or automatically replay uncertain uploads.

Selected My Files downloads read the exact parent listing once, bind selected names to validated node UIDs and address only those UIDs. This avoids the official CLI's serial name resolution through repeated parent listings. It never replaces a selection with a recursive folder download. Other account sections retain the existing path resolution.

Downloads stage on the destination filesystem and publish only regular files the CLI confirmed and wrote. The planned size is not compared: without a claimed size Proton lists the encrypted size, which a correct download never matches. The fresh listing that binds My Files names to node UIDs records each file's claimed size, and a staged file of another size is not published. Integrity otherwise rests on the SDK behind the CLI, which checks the SHA-256 of every encrypted block and the manifest signature over all block hashes. The name probes that detect aliasing are removed before the CLI runs, so a staged file is always one the CLI wrote; a file it reports but does not write (Proton Docs and Sheets) fails with that explanation. Existing destinations are not removed ahead of publication. Temporary directories and owned child processes are released when a future is dropped, and scans and watchers never treat a staging folder as content. A complete JSON summary is required before success; an exit code alone is insufficient. A download whose outcome is unknown was only ever written to private staging, so it goes back to the single-file path; an upload whose outcome is unknown stays uncertain.

The official CLI supplies no stable streaming progress for individual files. Progress therefore reports the start and confirmed completion of each file, without inventing intermediate byte percentages or parsing debug logs that may contain tokens.

## Entry points

The shared batch DAG carries GUI folder and multi-select transfers and the CLI's `put -r`, `get -r`, glob `get`/`put` and `sync`; `execute_sync_dag` carries MCP `aeroftp_sync_tree`. MCP `aeroftp_upload_many` groups plain local-file items by remote folder through the same hook and keeps one result per item. `put` with `--no-clobber` or `--immutable` on a bulk-capable provider decides existence with one listing per remote folder instead of a probe per file, then sends the rest through the shared batch. AeroSync in the GUI groups eligible consecutive uploads or downloads in `remoteSyncRunner.ts`, with at most 256 files per chunk. Before sending, it archives each destination that versioned backup protects and captures missing local source state for index updates. A lone file, an unsupported bulk operation or an item reported as not attempted uses the single-file command; preparation is tracked separately so fallback neither skips nor repeats an archive. Failed or uncertain writes are not blindly replayed.

AeroCloud uses `cloud_service/bulk.rs` for eligible same-direction runs, grouping by folder through the shared planner and retaining per-file upload settlement and download timestamps. Directories, deletes, conflicts, renamed counterparts and providers or wrappers without the corresponding capability retain their existing path. Both routes retain per-file journal/index, verification and cancellation handling. Cross-profile `transfer` retains its separate single-file route.

## Resume and recovery boundaries

Providers declaring existing byte-resume support retain their established single-file path. Sync delta, checksum-dependent and error-correction sidecar work retains its existing execution path. Crypt and compression wrappers do not inherit raw inner-provider bulk capabilities.

Proton CLI mode does not claim byte-offset resume. After interruption or a process/power failure, recovery is at file level with fresh source/destination state; a file with an uncertain result may already exist remotely. The official CLI can recognize unchanged upload content. Existing durable queue and metadata behavior is retained; bulk success is never inferred for an entire batch from one process exit.

A hard process or power failure can leave unpublished temporary data, as distinct from a valid destination. General crash recovery, journal replay and stale-part cleanup across all providers belong to the separate transfer-resilience audit.

## Other wrappers

MEGAcmd accepts multiple upload sources, but arbitrary selected downloads are not represented by its one-source `get` syntax. Its background daemon also outlives an individual wrapper client. Bulk capabilities remain disabled until owned transfer tags, per-file terminal proof and tag-scoped cancellation are implemented and verified. Killing a wrapper process does not by itself prove cancellation of daemon work.

Filen Desktop S3 and WebDAV are persistent local HTTP gateways. They do not incur Proton's per-file program startup, and the protocols do not provide a generic multi-file upload request. Their performance must be measured through connection reuse, concurrency and request overhead. An upload acknowledgement from a write-back cache is measured separately from native-API cloud visibility and independent cloud readback.

## Validation

The focused tests cover bounded grouping, old capability snapshots, independent per-file events and accounting, partial failure without replay, malformed native summaries, source changes, selected UID binding, staging aliases and owned-child cleanup on cancellation. A shim-backed sync integration verifies that 200 planned uploads use one native command. Native Windows and the full cross-platform build remain PR CI responsibilities.

### Controlled Proton comparison, 20 October 2026

The fixture contains 200 distinct 4 KiB files (819,200 bytes). Three alternating direct-CLI/shared-DAG pairs ran sequentially on the same authenticated account, followed by one four-worker per-file baseline. All seven downloads were byte-identical. The baseline calls the existing single-file adapter directly with four workers, reproducing the previous wrapper mechanism; it is not a timed checkout of an older release. The generated remote tree was permanently removed after the test.

| Direction | Previous per-file mechanism, 1 run | Bulk DAG median, 3 runs | Official CLI median, 3 runs | Baseline / bulk |
| --- | ---: | ---: | ---: | ---: |
| Upload | 123.61 s | 24.36 s | 18.97 s | 5.07× |
| Download | 135.03 s | 22.13 s | 22.89 s | 6.10× |

Bulk upload range: 16.03–28.05 s; direct CLI: 17.78–20.14 s. Bulk download range: 21.00–23.69 s; direct CLI: 22.59–31.86 s. Upload medians still differ by about 28%; this small cloud sample does not establish exact parity or a stable residual overhead. The download medians are close, and the reduced native process count removes the demonstrated startup bottleneck.

The direct upload passes all 200 explicit local files in one invocation. The direct download passes the whole fixture folder; the wrapper passes only the 200 planned UID sources. This folder reference is valid because the fixture has no unselected siblings. Production bulk downloads never broaden a selection to a folder. Upload timing is acknowledged by the later byte-identical download; it is not an independent durability claim.

Proton Drive CLI: `cli-drive@0.9.0+75612299`; SDK: `js@0.22.2+75612299`. Linux x86_64, debug Rust wrapper. HTTP gateway measurements ran on the independent Filen account; no second Proton contender ran concurrently. Raw timing samples are in [the benchmark data](benchmarks/2026-10-20-proton-bulk.json).

To reproduce, use an authenticated official CLI and an exclusive account window, then opt in explicitly:

```sh
AEROFTP_BULK_LIVE=1 AEROFTP_BULK_BENCH_OUTPUT=/tmp/proton-bulk.json \
  cargo test --lib providers::proton::cli_sequence_tests::bulk_live_benchmark \
  -- --exact --ignored --nocapture
```

The live test creates and permanently removes only its own UUID-named fixture. Do not interrupt the test parent before cleanup. An earlier owner-requested interruption was cleaned successfully and is excluded from these completed measurements. Historical four-way benchmark numbers are also excluded.

### Whole-command check, 22 October 2026

The released 4.2.4 CLI kept relative local paths and interleaved sync plans on one process per file, and its path budget split 200 files of a deep local folder into two launches. One serial run on the same test account, 200 distinct 4 KiB files and an 80-file tree in five folders, every download compared byte for byte:

| Command | 4.2.4 | After the fixes | Native launches after |
| --- | ---: | ---: | --- |
| `put -r` absolute path | 42.6 s | 25.4 s | 1 |
| `put -r ./dir` | 115.9 s | 26.5 s | 1 |
| `get -r` to an absolute path | 29.4 s | 27.2 s | 1 |
| `get -r` to `.` | 122.6 s | 22.1 s | 1 |
| `put -r --no-clobber`, every file present | 88.6 s | 14.7 s | 2 listings |
| `sync` upload, 80 files in 5 folders | 70.0 s | 33.5 s | 5 |
| `sync` download, same tree | 66.6 s | 37.7 s | 5 |

These are single whole-command samples with setup included, a debug wrapper build, not repeated medians.

### MEGAcmd and Filen follow-up

The PR records their independent live comparisons separately. They do not become bulk-capable as a consequence of Proton's result. MEGAcmd native multi-source upload/folder download is compared with four direct per-file workers, not represented as an implemented MEGAcmd adapter. Filen gateway comparisons include per-process isolated password-vault startup for AeroFTP, persistent credential configuration for rclone, and independent native API cloud readback; they cannot be relabeled as measurements of the Filen desktop GUI.

Filen S3 requires gateway-compatible rclone settings: no directory markers, no bucket creation check and no post-PUT HEAD. The latter changes what upload acknowledgement proves, so independent full cloud readback remains required. Native API visibility waits include polling, authentication and listing overhead; they are observed upper bounds, not an exact internal cache flush duration.
