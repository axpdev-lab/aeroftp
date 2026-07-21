# DAG Transfer Engine

*Last updated: 2026-07-20. The engine is active in several production call
paths, but convergence is partial; this document records the paths that are
actually reachable, not every shape that the builder can represent.*

AeroFTP contains a shared, provider-agnostic transfer-DAG core. The core is
real and is used by the shaped single-file runner, the batch wrapper, the
non-dry-run sync wrapper, and the production range runner. It is not correct to
turn the existence of a builder, a capability flag, or a unit test into a
claim about production wire behavior.

The audit rule used here is:

1. a production call site must reach the builder and `execute_dag`;
2. the runner must bind the node to real provider I/O or an explicit, named
   adapter contract; and
3. wire-level parallelism is claimed only when the provider call can execute
   independently, not merely because several Tokio tasks or graph nodes exist.

## Production call-path matrix

| Operation | Active production call path | What the graph really does | Wire-level/default status |
|---|---|---|---|
| Single-file GUI `get` / `put` | `provider_commands::run_dag_*_leaf` → `execute_single_file_dag` (`src-tauri/src/provider_commands.rs:2289`, `:2438`) | `UploadFile`/`DownloadFile` bind to provider I/O; multipart binds begin/part/complete/abort; several structural nodes are no-ops | Shaped DAG is the normal network path, subject to the transfer router and explicit legacy override |
| Single-file CLI `get` / `put` | `run_single_file_transfer` → `execute_single_file_dag` (`src-tauri/src/bin/aeroftp_cli.rs:8748-8767`) | Same shaped-file runner and provider binding | DAG is selected by the router for normal network transfers; local-to-local or explicit legacy routes bypass it |
| Multi-file batch | `transfer_orchestrator::execute_batch` → `execute_batch_dag` (`src-tauri/src/transfer_orchestrator.rs:66-70`) | Streams the known entry list through a bounded backlog and a bounded active set (P2-04): each file's shaped subgraph is expanded only when admitted and dropped when done, never a full static graph. Graph from executor runtime capabilities (P1-01); capability-aware settings (P1-02); real per-part multipart wire I/O (P1-03) via shared `transfer_multipart` lifecycle | File-level parallelism for clone/session-pool providers; multipart batch files issue N wire `upload_part` calls with one begin/complete (or abort once after drain); `--max-backlog` bounds the pending-work queue |
| Non-dry-run sync | `sync_tree_core` → `execute_sync_dag` (`src-tauri/src/sync.rs:1223-1238`) | Scan/planning precede the graph; the precomputed transfer plan then streams per-file subgraphs through the same bounded frontier (P2-04) instead of one static plan graph; normal files use bounded independent clone workers, while delta retains the primary `DeltaBatch` lane | Clone-backed providers use their live session ceiling; locked or failed-clone providers and every delta request stay serial; dry-run stays on planning path |
| Segmented download | Provider/CLI adapters -> `run_concurrent_range_download` (`src-tauri/src/providers/multi_thread.rs:244`) | `shaped_ranges` drives real range requests and offset writes through `execute_dag`; the old `JoinSet` runner is test-only | Graph scheduling is the only production range scheduler; GUI Auto may still select one stream |
| Same-provider copy | GUI `provider_server_copy`, CLI `cp`, and CLI WebDAV `COPY` -> `execute_copy_dag` -> `TransferDagBuilder::shaped_copy` | One `ServerSideCopy` core, or observable `DownloadFile` -> `UploadFile`; recoverable native rejection emits typed fallback before the second shape | Native copy reports logical bytes with `wire_bytes=0` and `local_payload_bytes=0`; fallback reports both payload legs |
| Cross-profile transfer | `cross_profile_transfer::copy_one_file_with_options` (`src-tauri/src/cross_profile_transfer.rs:123-167`) | Source download → local temp file → destination upload, with optional SFTP delta and optional segmented source helper | Provider-owned/temp-file bridge; not one shared transfer DAG |

The MCP surface can reuse GUI command paths, but the engine does not make all
MCP, GUI, and CLI wire behavior identical. Surface adapters, provider routing,
and operation-specific fallbacks remain part of the runtime contract.

## What the engine is

The `transfer_dag` core schedules a per-operation directed acyclic graph of
typed nodes. A node runs only after its dependencies complete and its
`ResourceRequest` can be acquired from the operation's
`TransferResourceManager`. The core owns:

- `TransferDag`, node kinds, dependency validation, and ready-frontier
  dispatch;
- resource permits for file, checker, chunk, HTTP, API, disk-read,
  disk-write, hash, and **buffer-byte credits** (weighted quanta);
- directional disk requests: upload/upload-part reserve disk-read only;
  download/range reserve disk-write only; server-side copy reserves neither;
- the `AimdController` and congestion classifier (byte memory is a safety
  budget, not an AIMD congestion class);
- `DagObserver` lifecycle hooks and the executor summary.

The executor's ready frontier is not a second process-wide scheduler. It is
dispatch-window bounded (`DEFAULT_DISPATCH_WINDOW = 256`, overridable via
`execute_dag_with_dispatch_window`), while the separate `DAG-P2-01` governor
applies the process-wide byte-memory and bandwidth caps. Only that many tasks
may be resident in the `JoinSet` at once. The indexed ready queue visits each
node and normalized edge once during dispatch; preprocessing uses
`sort_unstable`/`dedup` per dependency list, so setup is
O(V + sum(d_i log d_i)) rather than a strict O(V+E). Wide independent
frontiers therefore avoid both repeated full scans and unbounded task spawn.
Resource permits bound I/O classes and, as of `DAG-P0-06`, also bound owned
multipart part buffers via per-manager `buffer_bytes` credits (64 KiB quanta,
`acquire_many_owned`; env `AEROFTP_TRANSFER_BUFFER_BYTES`; default
`min(512 MiB, max(64 MiB, 10% MemAvailable))` on Linux, else 256 MiB). A
normal pool rounds its capacity down to whole quanta, so it never exceeds the
configured byte budget. A part whose rounded demand does not fit that usable
pool is admitted one-at-a-time through an explicit oversize lane. As of
`DAG-P2-01`, operationally completed by `DAG-P2-01b`, the buffer-byte pool is
**process-global**: a single hierarchical
governor (`transfer_dag/governor.rs`, reachable from the GUI `AppState`, the
CLI/MCP context, and the CLI TUI via `governor::global()`) owns ONE shared
quanta pool and ONE oversize lane, and every production job builds its manager
as a child via `governor::child_manager(budget)`. K concurrent jobs therefore
cannot each claim the full budget, and the single oversize allowance is
serialised across all jobs, not merely within one. A single job alone sees the
same budget and behaviour as before (the pool is sized by the same P0-06
policy).

As of `DAG-P2-05`, a multipart part is handed to the provider as a `PartBody`
(`transfer_multipart.rs`), not a pre-read `Vec<u8>`. Providers whose part upload
is a single send with a known length and no whole-part hashing (WebDAV/Nextcloud,
Dropbox, Drime, pCloud, Uploadcare, Google Drive, Yandex Disk, OneDrive,
OpenDrive, Cloudinary) stream the part from disk one bounded
`PART_STREAM_WINDOW_BYTES` (128 KiB) window at a time through a reusable window
pool, so those parts reserve only a window of `buffer_bytes` (not the whole part)
and many more fit the shared budget concurrently. Providers that must own the
whole part to hash, sign or encrypt it (S3 signed payload, B2/Box SHA-1, Azure,
MEGA, Filen) keep the full-part reservation and a full owned buffer: honest and
unchanged. The legacy non-DAG concurrent upload path in
`providers/multi_thread.rs` (`read_part_from_disk`, used only by B2, which needs
the whole part for its `x-bz-content-sha1` header) still allocates its full-part
buffers outside these credits. On the first node failure the executor cancels a graph-scoped token
(optionally a child of an external parent), stops new dispatch, and
terminates resident siblings within two seconds: cooperative cancel first,
then forced `JoinSet` abort, followed by one bounded drain. This bound
applies to async work that yields to Tokio; synchronous blocking code must
not run directly inside a DAG node because an async runtime cannot preempt
it. Optional per-node timeouts start at dispatch (including AIMD/resource
waits), are typed as `TransferErrorKind::Timeout`, and are distinct from
external cancel (`Cancelled`). Production keeps `node_timeout = None` so long
valid transfers are not cut by an arbitrary engine limit (`DAG-P0-05`).

Four production wrappers call the core:

- `transfer_dag_single_file` builds `shaped_file` and binds real single-file
  provider operations;
- `transfer_dag_batch` builds `from_batch_shaped` and adapts each file to the
  existing `TransferExecutor` session contract;
- `transfer_dag_sync` snapshots live capabilities after scan/planning, builds
  `from_sync_plan_shaped`, and owns clone workers, primary delta session,
  report aggregation, and progress replay explicitly.
- `transfer_dag_single_file::execute_copy_dag` builds `shaped_copy` for GUI,
  CLI `cp`, and the CLI WebDAV bridge.

The builder also exposes `shaped_ranges`. Since `DAG-P1-06`, the shared
concurrent range orchestrator always consumes that shape in production.

## The shapes and their status

| Shape | Builder | Current production status |
|---|---|---|
| Single-file core | `shaped_file(Download|Upload, caps, size)` | Active in normal GUI/CLI single-file network paths, with router and provider exceptions |
| Multipart single-file | `shaped_file(Upload, caps, size)` → `UploadPart × N` | Active when the single-file runner receives multipart capabilities; independent wire workers only for the provider set listed below |
| Batch | `from_batch_shaped(items, caps)` | Active graph wrapper; caps from `TransferExecutor::transfer_capabilities()`. Multipart files run real per-part wire I/O (DAG-P1-03); plain upload/download stay whole-file |
| Sync | `from_sync_plan_shaped(plan, live caps)` | Active for non-dry-run sync; normal files use the clone-backed cap, while delta is an exclusive primary-session lane |
| Copy | `shaped_copy(caps)` | Active for GUI copy, CLI `cp`, and CLI WebDAV `COPY`; native rejection fallback is observed and then runs an explicit two-node payload core |
| Segmented download | `shaped_ranges(N)` | Active for every shared concurrent range download; legacy `JoinSet` retained only in the equivalence test harness |

The usual seven-node envelope is a graph representation, not a guarantee that
each node performs I/O on every path:

`Discover` → `AcquireResource` → *transfer core* → `VerifyChecksum` →
`PreserveMetadata` → `CommitTemp` → `EmitProgress`.

In the current single-file runner, `Discover*`, `AcquireResource`, and
`EmitProgress` are structural no-ops. After DAG-P2-03, `VerifyChecksum` is a
real node on the durable native-multipart path: it runs after every part
receipt and before `CommitTemp`, so the remote object is not yet finalized. It
therefore makes no provider checksum claim and instead verifies, from durable
facts, that every part is present and the local source still matches the
identity that produced the uploaded parts, then records a durable `Verified`
fact. On the plain single-transfer-core path there is no journal, so it stays an
honest no-op that makes no claim. `CommitTemp` is real for multipart completion
and is fail-closed on that `Verified` fact; `PreserveMetadata` is real for
download mtime preservation. Progress start/byte/error events still come from the
surface adapters and provider callbacks; the executor summary does not yet
populate a complete engine-level byte/retry telemetry stream.

## Single-file multipart

This is the most complete DAG path. `execute_single_file_dag` binds
`UploadPart` nodes to the real provider lifecycle:

1. the first part lazily calls `begin_multipart_upload`;
2. the executor acquires the part's directional + buffer-byte lease, then
   hands the part to the provider as a `PartBody::DiskSlice` (DAG-P2-05) and
   calls `upload_part_body` while the lease is still held: streaming providers
   read a bounded window at a time, owning providers materialize the slice into
   an owned buffer;
3. receipts are collected and ordered by part number;
4. every successful receipt is atomically written to the transfer-versioned
   durable checkpoint before its node completes;
5. `VerifyChecksum` verifies the durable payload against a fresh observation of
   the local source and records the durable `Verified` fact; the durable state
   machine is `Prepared` → `Transferring` → `PayloadComplete` → `Verified` →
   `Committed`;
6. `CommitTemp` is fail-closed on `Verified`, calls `complete_multipart_upload`,
   then writes the durable `Committed` fact before the graph can report success;
7. a failed or cancelled durable upload keeps its session and valid receipts for
   restart, while a conservative TTL scavenger aborts only an expired matching
   session and removes its record only after that abort succeeds.

Part buffer sizing is shared: builder and runner use
`multipart_part_byte_len(file_size, part_index, part_count,
preferred_chunk_size)` so graph accounting cannot drift from the allocation.
The checkpoint identity binds local path/size/mtime, provider, endpoint account,
remote path and multipart layout. A matching restart restores only validated
receipts and dispatches only missing parts. Checkpoints are process-external
durable state; the P2-06 adaptive profile cache remains process-local and has
no ownership of them.

The runner first tries `clone_for_transfer()`. A clone-capable provider gets an
independent worker per part; otherwise the provider is taken through the
shared mutex. The current independent-worker set is:

- S3;
- Backblaze B2;
- Azure Blob;
- WebDAV Nextcloud chunked v2;
- **Drime** (`HttpClonePool`, max sessions 4), DAG-P1-05A;
- **Uploadcare** (`HttpClonePool`, max sessions 4), DAG-P1-05A;
- **Dropbox** (`HttpClonePool`, max sessions 4), DAG-P1-05B;
- **Box** (`HttpClonePool`, max sessions 4), DAG-P1-05B;
- **Filen native** (`HttpClonePool`, max sessions 4), DAG-P1-05D.

**pCloud** (DAG-P1-05C, 2026-07-19): multipart session API remains
**`LockedSingle`**. Live probes with independent workers on one `uploadid`
returned result **2068** ("Error writing to upload") on part 2 for every
attempt (including with `uploadsize=`). Serial multipart still completes
(with occasional 2068 + CLI retry). Result **4006** throttle is now mapped
to typed DAG `RateLimited`. Hints still advertise `multipart_max_parallel=2`
for a future re-attempt if the service contract changes.

**Filen native** (DAG-P1-05D, 2026-07-19): promoted. Transfer clones share
`Arc` config + auth/crypto snapshots (API key + master-key ring), seed only
root + current folder navigation (no full `dir_cache` / `file_key_cache`),
and never reconnect. `MultipartHandle` Debug redacts `upload_id` globally;
ingest transport errors scrub `uploadKey` / bearer material. Legacy
`upload()` fan-out stays at 4 and matches the shaped session ceiling (no
4×4 multiplication: sub-1 MiB files are one Filen chunk). Filen Desktop S3
and WebDAV bridge providers are unchanged.

A graph with N part nodes therefore means "N scheduled part operations",
not automatically "N concurrent network requests", unless the provider is
in the independent-worker set above.

OAuth providers that may rotate refresh tokens (including Dropbox and Box)
share one in-process `OAuth2Manager` refresh guard across primary and every
transfer clone. Cross-process serialization remains on `RefreshLease`.

### Evidence split (DAG-P1-05A + DAG-P1-05B)

| Layer | Drime | Uploadcare | Dropbox | Box |
|---|---|---|---|---|
| Lifecycle correctness (begin/complete/abort on primary; part uses opaque handle) | unit + local HTTP fixture | unit + local HTTP fixture | unit + local HTTP fixture (concurrent start empty body, close-before-finish, no-op abort) | unit + local HTTP fixture (Content-Range, chunk digest, sorted commit, abort DELETE) |
| Graph task overlap (builder cap >1) | shared multipart topology | shared multipart topology | shared multipart topology | shared multipart topology |
| Deterministic HTTP part overlap | peak ∈ (1, 4] on 4 workers | peak ∈ (1, 4] on 4 workers | barrier-backed peak = 4 on 4 workers | barrier-backed peak = 4 on 4 workers |
| Live WAN integrity (multipart ≥4 parts, download-back SHA-256, cleanup) | profile `Drime`, 22 MiB, byte-identical | profile `Uploadcare`, 22 MiB jpg, byte-identical | profile `My Dropbox`, 160 MiB, SHA-256 match, cleanup on **promoted debug CLI** after `profile-export --include-credentials` from prod → `profile-import` into dev vault | profile `MyBox`, 32 MiB (4×8 MiB plan), SHA-256 match, cleanup on **promoted debug CLI** (same export/import path) |
| Live WAN peak part overlap with **this** code | not claimed | not claimed | not instrumented on this run (integrity + cleanup only; first prod-only attempt saw one append_v2 drop during a 5G hotspot → WiFi reconnect) | not instrumented on this run |

### Evidence split (DAG-P1-05C pCloud)

| Layer | pCloud |
|---|---|
| Promotion decision | **Conservative: keep `LockedSingle`** |
| Concurrent live probe (experimental `HttpClonePool` debug CLI) | 20 MiB, ≥5×4 MiB parts: every attempt failed with `upload_write` result **2068** on part 2 ("Error writing to upload"); trial `uploadsize=` did not fix concurrent writes and was not retained |
| Serial live control (production CLI LockedSingle) | 20 MiB: success after occasional 2068 + CLI retry; download-back SHA-256 match; cleanup |
| Result 4006 | provider boundary maps to typed `TransferErrorKind::RateLimited` / AIMD congestion feedback |
| Wire contract retained | `upload_write?uploadid=&uploadoffset=`; bearer not in URL; offsets handle-derived |

### Evidence split (DAG-P1-05D Filen native)

| Layer | Filen native |
|---|---|
| Promotion decision | **`HttpClonePool` ceiling 4** |
| Crypto/session ownership | `Arc<FilenConfig>` + `Arc<FilenAuthSnapshot>` (api_key + master_keys); replaced on connect/disconnect; workers never login/KDF/reconnect |
| Clone cache bounds | root + current path/folder only; empty `file_key_cache` |
| Deterministic HTTP part overlap | barrier-backed ingest fixture peak = 4; indexes 0..3; distinct AES-GCM nonces; body len = plain + 28 |
| Secret hygiene | global `MultipartHandle` Debug redacts `upload_id`; transport errors scrub `uploadKey`/bearer; API key stays Authorization header |
| 429/503 | `format_filen_error` + Retry-After → typed `RateLimited` / `ServiceUnavailable` |
| Live WAN integrity | profile **`Filen Dev`** (owner override: exact `Filen` requires interactive 2FA), promoted **debug** CLI with dev master (`AEROFTP_MASTER_PASSWORD`): 2×8 MiB + 256 KiB put/get, SHA-256 + byte identity, remote cleanup. Earlier same-day release CLI control also green. |
| Live WAN peak part overlap | not instrumented on this run (barrier fixture + live integrity) |

Clone failure stays fail-closed: runtime composition demotes
`file_parallel`/`session_pool` to single-lease and part I/O falls back to the
primary mutex.

For providers with `max_chunk_slots <= 1` (or missing → effective 1), every
capability-shaped builder chains parts in strict part-number order:

```text
AcquireResource -> part 1 -> part 2 -> ... -> part N
```

When `max_chunk_slots > 1`, parts fan out from acquire:

```text
AcquireResource -> {part 1, part 2, ... part N}
```

Single-file (`shaped_file`), batch (`from_batch_shaped`), and sync
(`from_sync_plan_shaped`) share one internal transfer-core helper
(`append_transfer_core`) so the topology cannot drift (DAG-P0-07). Different
files in a batch/sync graph stay independent: cap=1 serialises parts within
each file, not the whole job across files. This is protocol correctness for
ordering-sensitive upload sessions.

After `DAG-P1-03`, the batch runner executes real per-part wire I/O for shaped
multipart uploads: one lazy begin, N `upload_part` calls with exact
`multipart_part_byte_len` ranges, sorted complete, and at-most-once abort after
in-flight parts drain. Layout and once-guards live in the shared
`transfer_multipart` module used by the single-file path. Sync still does not
own a separate per-part batch contract beyond the shared topology helper.

## Batch and sync limitations

### Batch

`execute_batch_dag` is the current batch entry point, so the old hand-written
batch scheduler is gone. After `DAG-P1-01` / `DAG-P1-02` / `DAG-P1-03`:

- the graph is shaped from `executor.transfer_capabilities()` (the runtime
  snapshot owned by the provider executor after clone-probe composition), not
  from `TransferCapabilities::default()`;
- provider folder/batch entrypoints resolve settings with
  `resolve_provider_transfer_runtime`, which performs one live clone probe and
  then applies `resolve_transfer_settings_for_capabilities`, preserving
  `requested_max_concurrent` vs effective `max_concurrent`;
- clone/session-pool providers (S3, B2 when connected, Azure, SFTP/FTP pool
  kinds) can realize file-level concurrency bounded by the tighter of graph
  `file_slots` and the session-pool lease capacity;
- unknown, locked-single, failed clone probes, and non-pool kinds stay
  serial (`max_file_slots = 1`);
- multipart-shaped batch files use `TransferExecutor::{multipart_begin,
  multipart_upload_part, multipart_complete, multipart_abort}` (default
  conservative: unsupported). `ProviderUploadExecutor` implements the wire
  path when `multipart_upload` is available. One session-pool lease is held
  per multipart file for the whole lifecycle; chunk/disk-read budgets come
  from `TransferBatchConfig::transfer_budget_for_capabilities`;
- the whole-file `entry_transferred` dedupe seam is **removed**. Plain
  `UploadFile` / `DownloadFile` still use `execute_with_session`.

Honest distinctions:

- **multipart lifecycle correctness** (begin/part/complete/abort once) is
  active on the batch path for wire-capable executors;
- **graph task overlap** follows the shaped topology (cap=1 chain vs cap>1
  fan-out);
- **actual wire overlap** still requires independent workers
  (`clone_for_transfer`); otherwise parts serialise on the session mutex;
- a file error is recorded once in the batch snapshot **and** returned as
  `NodeOutcome::FileFailedButGraphContinues(TransferError)` so DAG
  accounting, observers and AIMD see a typed file-local failure without
  aborting unrelated files (`DAG-P1-04`). Multipart part nodes still drain as
  `Completed`; the single file-class congestion signal is applied at
  `CommitTemp` after complete/abort and lease release. Cancellation is never
  congestion.

### Sync

Non-dry-run sync reaches `execute_sync_dag`. Local and remote scans overlap,
which is a real benefit. The complete scan and plan are still resolved before
transfer (sync is scan-first, and its decision equivalence depends on that),
but the plan is now a plain vector, not a full executable node graph. The
transfer set streams through the same bounded frontier as batch (P2-04): each
planned transfer's shaped subgraph is expanded only when it is admitted into
the active set and dropped when it completes, so peak resident nodes stay
`O(active_file_cap x nodes_per_file)` instead of one static graph of N chains.
The global `DiscoverLocal` / `DiscoverRemote` / `Compare` prefix is gone from
the transfer graph because the scan/plan already ran; the per-file subgraph's
own discover node is a structural no-op.

The sync runner snapshots `transfer_capabilities()` and allocates clone workers
only when the provider's existing `clone_for_transfer` contract realizes its
advertised session ceiling. The DAG `file_slots` use that same cap. Worker I/O
returns recorded progress and outcomes to `drive_sync_transfers`, the sole
owner of the caller sink and `SyncReport`, so completion order does not affect
counters or per-file start/done integrity. A clone allocation failure demotes
the operation to the former primary serial path.

A requested delta sync always retains the primary provider and one
`DeltaBatch`; it is dispatched only by the exclusive serial lane. Normal clone
workers never borrow that session. Deletes still start only after every graph
file node has drained, and dry-run remains on the legacy planning path.

## Streaming multi-file frontier (DAG-P2-04)

Multi-file production no longer builds one static `TransferDag` for the entire
job. A `WorkSource` (`src-tauri/src/transfer_dag/work_source.rs`) emits
`TransferWorkItem`s; a bounded backlog buffers pending items and pauses the
source when full (backpressure); and at most `active_file_cap` file subgraphs
are materialized and executing at once. Each admitted file's shaped subgraph is
built with `TransferDagBuilder::shaped_file`, run through `execute_dag` with the
job-wide resource manager, AIMD controller, and session pool, then dropped. Peak
resident nodes are therefore `O(active_file_cap x nodes_per_file)`, not
`O(total_files x nodes_per_file)`.

- **Batch** streams the known entry list (`BatchEntriesWorkSource`); every
  existing behavior is preserved because the per-file node runners
  (`run_whole_file_node`, `run_multipart_part_node`, `run_multipart_commit_node`)
  are reused verbatim, bound to a one-element entry slice.
- **Sync** streams the precomputed transfer plan (`SyncTransfersWorkSource`);
  each subgraph hands its transfer to the same `drive_sync_transfers` driver,
  which still owns the bounded worker pool, the exclusive delta lane, and
  out-of-order report aggregation. Sync remains scan-first: the plan is a plain
  vector (a plan iterator), but the graph is streamed.
- **Backlog knob**: the CLI `--max-backlog` (default 10000) maps to the batch
  `TransferBatchConfig::max_backlog` and thence the engine backlog cap. Sync
  uses the engine default; a smaller value than the active window is clamped up
  because a backlog below the active set cannot fill it.
- **Bounds proof**: a deterministic gate streams 10,000,000 synthetic items and
  asserts peak materialized nodes stay within `active_file_cap x template`;
  the assertion fails on the old build-full-graph-first approach.

`--check-first` / audit modes that need a complete preview may still materialize
a full plan (a work-item inventory), never a full executable node graph for
millions of files.

## Segmented downloads

The range primitive preallocates a temporary file, writes validated ranges at
offsets, handles servers that ignore Range, and cleans up on cancellation.
`run_concurrent_range_download` now calls `run_ranges_via_graph` directly,
which builds `shaped_ranges` and binds each `DownloadRange` node to a real
request and offset write. There is no environment switch or production
legacy branch.

The former `JoinSet` runner is compiled only under `#[cfg(test)]`. Its
equivalence matrix gates byte identity, outcome and typed-error parity,
progress totals, range boundaries, panic propagation, fail-fast cancellation,
and the same `max_parallel` ceiling. The GUI's Auto value remains conservative
and can still resolve to a single stream.

## Server-side copy and cross-profile copy

Normal same-provider copy is a production `shaped_copy` graph. GUI
`provider_server_copy`, CLI `cp`, and the CLI WebDAV `COPY` handler call
`execute_copy_dag`, which snapshots `transfer_capabilities()` and builds one
of two transfer cores:

1. `ServerSideCopy`, reserving an API slot and no file/disk payload resource;
2. `DownloadFile` -> `UploadFile`, sharing one temporary path and the existing
   directional file/disk resource model.

The native node calls `StorageProvider::server_side_copy`, so S3 and B2 retain
their provider-owned multipart server copy above 5 GiB. A recoverable
capability-boundary rejection is classified only by
`should_attempt_copy_fallback`, completed as an observed copy fallback, and
then followed by the explicit payload graph. Permission, 404, auth, quota,
transient transport, I/O, and cancel errors fail the `ServerSideCopy` node at
`FailureScope::File` and do not dispatch either payload leg.

The combined copy metric snapshot separates `logical_bytes`, `wire_bytes`,
and `local_payload_bytes`. Native copy reports zero for both local data-path
fields. Download-upload reports one logical object, two wire legs, and one
locally materialized temporary payload. The legacy
`server_side_copy_with_fallback` helper remains behavior-compatible for
external/library callers and its policy regression tests, but no production
copy endpoint calls it.

Cross-profile transfer is a different operation: it downloads the source into
a local temporary file and uploads it to the destination. It may select the
segmented source helper or SFTP delta upload, but it does not call the shared
single-file DAG runner for both legs.

## AIMD, observers, and resources

`AimdController` is real and can be passed to `execute_dag`. It is useful on
the paths that expose actual class-level concurrency, especially batch
transfers, single-file multipart, and the production range graph. It is not a
global governor and cannot tune a serial file slot into parallelism; its live
dispatch permits are always per operation. The process-global cap that spans
operations is the separate `DAG-P2-01` governor (byte-memory pool + bandwidth
token bucket), not AIMD. Batch file-level failures use the typed, non-fatal
`FileFailedButGraphContinues` outcome: D2 congestion reduces the File-class
target while unrelated file subgraphs continue. Sync still uses one file slot
and does not yet expose this batch feedback contract.

`DAG-P2-06` makes the controller endpoint and workload aware across successive
jobs in one process. A bounded, process-local `AdaptiveProfileRegistry` caches
the safe target a controller converged to, keyed by an `AdaptiveProfileKey`:
the `DAG-P2-01b` endpoint identity plus a workload class (batch/sync file,
shaped single/multipart, or segmented range). A new controller seeds each class
from the intersection of the effective budget, the operator max, and the cached
safe target, so a first or expired use starts at the honest ceiling exactly as
before; the typed congestion and healthy lifecycle then mirrors moves back to
the same key only. It is a seed/feedback record, never a shared semaphore: the
cache has a finite TTL (600 s) and key cap (256, least-recently-used eviction),
holds no secret, path, or object name, and owns none of the `DAG-P2-02`
checkpoint state.

The resource manager's slot classes (file, checker, chunk, HTTP, API,
disk-read, disk-write, hash) are per operation. Its byte-credit pool for
multipart buffers is no longer per operation: as of `DAG-P2-01` it is the
process-global governor's shared pool. The endpoint level of the hierarchy
(a concurrency sub-cap keyed by protocol + host + account) and a bandwidth
token bucket also live in that governor. The bounded local-copy fallback and
the non-pipelined SFTP copy loops reserve bucket credits before moving each
chunk, so concurrent jobs honour one configured cap
(`AEROFTP_GLOBAL_BANDWIDTH_BPS`; unset = unlimited, no throttle).

`DAG-P2-01b` completes the job-level hierarchy without a second scheduler:
foreground single/copy/range jobs and background batch/sync jobs acquire an
RAII endpoint lease. Foreground is preferred, but after eight bypasses a
waiting background job receives the next endpoint slot. `StorageProvider`
supplies a canonical endpoint identity, with exact host/account overrides for
FTP and SFTP; the batch executor and shared segmented-range helper carry that
identity through to the lease. The same lease boundary resolves local physical
devices (Unix `st_dev`, conservative nearest-existing-path fallback elsewhere)
and reserves independent read/write slots per device
(`AEROFTP_DISK_DEVICE_SLOTS`, default 4). Multiple local paths are sorted and
deduplicated before acquisition, so a cross-device copy cannot invert lock
order. Cancellation drops queued or held RAII leases without leaving a phantom
priority turn. Upload/download resource requests still reserve only their
per-operation disk direction (`DAG-P0-06`); the device lease is the additional
process-wide cap.

`DagObserver` provides a shared node lifecycle abstraction. The GUI's
`GuiDagObserver` is used on the shaped single-file path, but surface start,
byte-progress, and error events still come from `TransferEventSink` and
provider callbacks, not from DAG node lifecycle. Byte progress is therefore
still adapter-derived, not DAG-derived.

GUI progress pressure is governed by the shared `ProgressGovernor`
(DAG-P0-08) behind `AppHandleSink` / `emit_gui_transfer_event`:

- `transfer_event` with `event_type == "progress"` is capped at ≤10 Hz per
  `transfer_id` (latest-sample coalescing; first sample immediate);
- `transfer_batch_progress` is capped independently at ≤10 Hz per
  `batch_id`;
- `start` / per-file `file_start` explicitly open a lane; terminals flush the
  latest pending sample, remove all lane state, and reject late callbacks
  until a new lifecycle start (no retained tombstones);
- cross-profile `file_*` events are immediate children of the aggregate
  transfer and do not close its lane; only aggregate complete/error/cancelled
  are terminal there;
- a bounded same-lane ordering stripe covers routing plus IPC emission, so an
  already-admitted concurrent callback cannot overtake its terminal event;
- concurrent multipart callbacks cannot double-claim one 100 ms slot;
- CLI `indicatif`, MCP progress notifications, and archive/sync-scan
  throttles remain separate product domains.

Copy populates logical, wire, local-payload, and copy-fallback metrics. As of
`DAG-P2-07` blocks A-D the other runners also attest bytes, wait/run timing,
slot peak, TTFB (on covered paths), and whole-file retries; see the telemetry
section below for the exact coverage and the remaining gaps.

## Engine telemetry (DAG-P2-07)

Blocks A-D populate the metric core; blocks E, F, G add the unified stats
surface, the slow optimization loop, and the nightly CI benchmark. The
paragraphs below describe the whole wave.

`TransferDagMetrics` (`src-tauri/src/transfer_dag/metrics.rs`) carries the
logical/wire/local-payload byte triple, backpressure, and fallback counters,
and now also `wait_nanos_total` (dispatch to runner start: AIMD permit plus
resource lease wait), `run_nanos_total` (runner execution), `slot_peak`
(high-water mark of filled dispatch slots, sampled after each dispatch fill),
and `ttfb_nanos_total` with `ttfb_samples`. All new fields are additive with
serde defaults, and `absorb()` merges per-file subgraph metrics into job-level
totals across the streaming batch/sync frontier (sums; max for `slot_peak`).
The executor attributes timing per node through `NodeTiming`; cancel and
timeout wrapper arms attest wait/run up to the abort via a shared
`RunnerStartSlot` stamp instead of fabricating durations, and the `JoinError`
force-abort arm contributes nothing.

Byte attestation is explicit per runner. The shaped single-file runner reports
through a shared `AtomicU64` and `SingleFileMetricsObserver` (download:
on-disk size; upload: file size; multipart: `layout.total_size` once at
commit). Batch folds per-file `FileMetricCounters`, with multipart commit
bytes recorded inside the once-guard of `claim_account`. Sync sends a
driver-attested per-file `SyncByteAttestation` (logical and wire) through
`TransferJob.ack` (`oneshot::Sender<SyncByteAttestation>`): on the rsync delta
path the wire figure is the real delta payload (`delta_stats.bytes_sent`), not
the whole file, and paths without an honest wire figure fall back to the
logical size. The range graph returns
`(ConcurrentRangeOutcome, DagExecutionSummary)` from
`providers/multi_thread.rs`: the byte triple on `Completed`, zeroed counters
on `ServerIgnoredRange`.

Retries are metered only where a real retry loop exists, and those loops live
in the executors, not in the DAG runners. `TransferExecutor::
take_transfer_attempts` defaults to `None` (unmetered, reported as zero
retries, never guessed); the provider and FTP whole-file executors
(`provider_transfer_executor.rs`, `ftp_transfer_executor.rs`) implement it
with a per-entry attempt map (first try included, retries = attempts - 1,
consumed once). Unmetered residuals: `CrossProfileExecutor`, http_retry
attempts for the range family, and provider-internal retry loops
(`yandex_disk.rs`, `s3.rs`, `internxt.rs`, `filen/mod.rs`, `webdav.rs`,
`mega_native.rs`) that sit below the metrics surface.

Time-to-first-byte is recorded by `transfer_dag/ttfb.rs`: a process-global
registry scoped by `tokio::runtime::Id`, with job-level attribution. At most
one recorder is active per runtime: the first `TtfbRecorder::install` owns
it, and any later install on the same runtime NESTS (owns nothing, folds
zero). The batch and sync streaming runners install the owning guard around
the whole streaming frontier, so every per-file `execute_dag` subgraph nests
and each real HTTP sample is counted exactly once per job; per-file subgraph
metrics therefore report `ttfb_samples == 0` and the exact total lands on the
owning job run. Single-file and copy runs are themselves outermost, so they
own the recorder and fold at their own single finalize as before. A second
independent top-level job started on the same runtime while one is active
nests and reports zero TTFB, and concurrent unrelated `send_with_retry`
traffic on the same runtime during a job is attributed to that job; both are
documented shared-runtime attribution, never relabeled. Foreign runtimes
cannot pollute a run. The single instrumented choke point is
`providers/http_retry.rs::send_with_retry`: a sample is the honest first byte
(the reqwest future resolving with headers received), the timer starts after
the tpslimit toll so throttle wait is not counted, a transport error records
nothing, and each retried attempt that reaches headers is its own sample. On
the force-abort drain-ceiling path (executor drain timeout) a sample that
resolves after the fold is recorded late and not counted: an honest
under-count, never a fabricated value. This
covers every DAG production path whose provider routes through
`send_with_retry` (15 provider files). Coverage is per call site, not per
provider: within one provider only the operations that flow through
`send_with_retry` record TTFB. Example, verified live on Backblaze B2 and
MinIO: the S3 provider signs downloads through `s3_request` (which uses
`send_with_retry`, TTFB recorded) but issues uploads through raw
`client.send` (no TTFB), so an S3 download job reports `ttfb_samples == file
count` while an upload job honestly reports zero. Paths
that record nothing, keeping their latency inside `run_nanos_total`: FTP/FTPS
and SFTP (non-HTTP), HTTP providers that bypass `send_with_retry` with raw
`client.execute`/`send` (webdav, dropbox, google_drive, onedrive, box,
cloudinary, drime_cloud, google_photos, imagekit, immich, opendrive,
uploadcare, yandex_disk, mega_native, oauth helpers, and the non-`s3_request`
S3 operations above), and MEGAcmd subprocess
providers.

The process resource sampler (`src-tauri/src/proc_stats.rs`: CPU user/system
nanoseconds, RSS bytes, FD count from Linux `/proc/self`, compile-time `None`
stub elsewhere) is wired at job start/end (block E): the batch and sync runners
bracket the whole job with a `ResourceSampleGuard`, so the delta is present on
Linux and honestly `None` (never a zeroed guess) elsewhere.

Block E adds one engine-level stats source, `EngineTransferStats`
(`metrics.rs`): the folded job `TransferDagMetrics`, the real wall-clock
milliseconds (not the summed `run_nanos_total`, which double-counts concurrent
nodes), the optional `ProcessResourceDelta`, and a `cancelled` flag (additive,
serde-defaulted to `false` for older payloads) so a torn-down job's real
folded stats are still surfaced but honestly labeled, letting MCP/GUI
consumers tell it from a completed one. It is built once at job end,
replacing the former `tracing::debug` of the batch/sync totals, and every
surface reads that one struct. The CLI `--json` folder and sync results carry
it in-band under an additive `stats` key (threaded through the shared batch
runners, so a legacy non-pool-backed transfer honestly reports no stats rather
than a stale one). The GUI receives one `transfer_engine_stats` event at job
end. The MCP `aeroftp_transfer_stats` accessor reads the most recent job from a
one-slot process-global store (`transfer_dag/engine_stats.rs`); it reports
`{available:false}` when none has run, and its description states honestly that
the MCP server's own `aeroftp_transfer`/`transfer_tree` tools use the direct
cross-profile path and do not run the DAG engine, so they are not counted.

Block F is the slow optimization loop. After a job drains fully successfully
(never on cancellation, never with per-file failures, and never while
`--aimd-disable` is set), the batch/sync runner
distills the populated job telemetry into a `JobThroughputObservation`
(aggregate throughput against the transfer-phase wall clock, which brackets
the streaming frontier and excludes scan/plan and per-runner setup rather
than spanning the whole job, dispatch-wait ratio, concurrency high-water) and feeds
it to `AdaptiveProfileRegistry::observe_job` under the same
`AdaptiveProfileKey` (endpoint + workload) the P2-06 seed/feedback path uses.
The loop is decrease-biased: a plateau (more concurrency without more
throughput) or a wait-dominated dispatch shaves one slot off the learned safe
target; only a job that beats the throughput baseline by more than 5% at low
wait steps the target up by one toward the concurrency that achieved it
(bounded recovery). It shares the registry's key isolation, TTL, capacity and
LRU eviction, and injectable clock, so it stays bounded and never poisons a
profile across a cancellation.

Block G is a schedule-only nightly workflow
(`.github/workflows/nightly-telemetry.yml`): it runs the deterministic DAG
telemetry suites plus the process-sampler suite as gates, then one local
(no-WAN) benchmark cell (`src-tauri/tests/telemetry_nightly.rs`, `#[ignore]`d
outside the nightly) that moves bytes through a temp file and serializes an
`EngineTransferStats` envelope as an uploaded JSON artifact. It never runs on
push or pull request and never needs lab hardware. WAN and competitor cells
stay manual in `tests/gtc/parity_harness.sh`. This wave adds no WAN benchmark
numbers; evidence is the deterministic test suite listed in the appendix
tracker (`docs/dev/drafts/TRACKER-DAG-P2-07-DRAFT-telemetry-benchmark.md`).

Residuals carried forward (unchanged from blocks A-D): unmetered retry sites
(`CrossProfileExecutor`, http_retry for the range family, provider-internal
loops in `yandex_disk`/`s3`/`internxt`/`filen`/`webdav`/`mega_native`) and
TTFB not recorded on FTP/SFTP or on HTTP providers that bypass
`send_with_retry`.

## Capability contract

`StorageProvider::transfer_capabilities()` is consumed by the shaped
single-file runner before `shaped_file` is built. The batch runner (DAG-P1-01)
uses `TransferExecutor::transfer_capabilities()`, a runtime snapshot composed
from the live provider plus clone/pool feasibility
(`compose_runtime_transfer_capabilities` / `finalize_capabilities_for_session_model`).
Provider batch settings resolve through
`resolve_provider_transfer_runtime` and its single
`resolve_transfer_settings_for_capabilities` pass (DAG-P1-02). The sync runner
uses a live capability snapshot and demotes failed clone allocation to one file
slot. The copy
runner consumes the live provider snapshot before `shaped_copy`; the native
node calls `server_side_copy`.

The trait methods below describe available provider primitives; they do not
by themselves prove that every production operation invokes them:

| Method | Purpose |
|---|---|
| `begin_multipart_upload` | Open an upload session |
| `upload_part` | Send one multipart part |
| `complete_multipart_upload` | Commit the collected parts |
| `abort_multipart_upload` | Release an incomplete session |
| `server_side_copy` / `server_copy` | Provider-native copy primitive |
| `supports_server_side_copy` / `supports_server_copy` | Provider capability gates |

The default implementations return `NotSupported`. A provider can implement a
primitive while still using a provider-owned or legacy call path for a given
operation.

## File map

| File | Role |
|---|---|
| `src-tauri/src/transfer_dag/mod.rs` | Public engine exports |
| `src-tauri/src/transfer_dag/builder.rs` | Graph shape constructors and shape tests |
| `src-tauri/src/transfer_dag/executor.rs` | Ready-frontier execution, resource arbitration, observer summary |
| `src-tauri/src/transfer_dag/capabilities.rs` | `TransferCapabilities` and capability states |
| `src-tauri/src/transfer_dag/resources.rs` | Per-operation budgets and resource permits |
| `src-tauri/src/transfer_dag/adaptive.rs` | AIMD controller, congestion classification, and the endpoint/workload adaptive profile registry (`DAG-P2-06`) |
| `src-tauri/src/transfer_dag/observer.rs` | Observer abstraction and adapters |
| `src-tauri/src/transfer_dag_single_file.rs` | Real shaped single-file and same-provider copy runners |
| `src-tauri/src/transfer_dag_batch.rs` | Batch graph wrapper and file-session adapter |
| `src-tauri/src/transfer_dag_sync.rs` | Non-dry-run sync graph wrapper, bounded normal worker ownership, and exclusive delta lane |
| `src-tauri/src/providers/multi_thread.rs` | Shared graph range scheduler and test-only legacy equivalence runner |
| `src-tauri/src/copy_fallback.rs` | Authoritative copy-fallback classifier and legacy compatibility helper |
| `src-tauri/src/cross_profile_transfer.rs` | Temp-file cross-profile bridge |

`provider_transfer_executor.rs` remains active for batch session execution,
segmented-download eligibility, and the provider session model. Its presence
is an explicit reminder that the transfer architecture is transitional.

## v4.0.0 convergence, stated accurately

| Before | Current implementation |
|---|---|
| Rollout flags for the early DAG phases | The three `AEROFTP_TRANSFER_ENGINE_DAG_*` flags were removed; the single-file router still has an explicit legacy override |
| Hand-written `JoinSet` batch orchestrator | `execute_batch_dag` is the batch entry point |
| Provider-owned multipart in the old single-file paths | The shaped single-file runner owns the multipart lifecycle where its call path reaches it |
| Copy behavior spread across provider call sites | GUI, CLI `cp`, and WebDAV `COPY` share `execute_copy_dag`; native copy and both fallback payload legs are observable nodes |
| Range graph migration | `shaped_ranges` is the only production concurrent range scheduler; the old `JoinSet` is test-only |
| One fully converged engine for every surface | Shared core plus selected wrappers; batch/sync/copy/cross-profile still have documented adapters and limits |

For the planned next steps (complete telemetry and global resource governance), see
the audit appendix at
`docs/dev/roadmap/APPENDIX-DAG-ENGINE_Parallel-Transfers-Audit.md`.
Bounded dispatch (P0-04), typed outcomes (P0-03), and graph-scoped
fail-fast cancel/timeout (P0-05) are already in the core executor.

## See also

- `docs/PROVIDER-INTEGRATION-GUIDE.md` - provider primitives and capability
  advertisement.
- `docs/CLI-GUIDE.md` - capability discovery and command-specific transfer
  options.
- `docs/THREAT-MODEL.md` - security analysis for transfer operations.
