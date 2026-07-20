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
| Multi-file batch | `transfer_orchestrator::execute_batch` → `execute_batch_dag` (`src-tauri/src/transfer_orchestrator.rs:66-70`) | Graph from executor runtime capabilities (P1-01); capability-aware settings (P1-02); real per-part multipart wire I/O (P1-03) via shared `transfer_multipart` lifecycle | File-level parallelism for clone/session-pool providers; multipart batch files issue N wire `upload_part` calls with one begin/complete (or abort once after drain) |
| Non-dry-run sync | `sync_tree_core` → `execute_sync_dag` (`src-tauri/src/sync.rs:1223-1238`) | Scan/planning precede the graph; normal files use bounded independent clone workers, while delta retains the primary `DeltaBatch` lane | Clone-backed providers use their live session ceiling; locked or failed-clone providers and every delta request stay serial; dry-run stays on planning path |
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

The executor is not globally bounded by process or endpoint. The ready
frontier is dispatch-window bounded (`DEFAULT_DISPATCH_WINDOW = 256`,
overridable via `execute_dag_with_dispatch_window`): only that many tasks may
be resident in the `JoinSet` at once. The indexed ready queue visits each node
and normalized edge once during dispatch; preprocessing uses
`sort_unstable`/`dedup` per dependency list, so setup is
O(V + sum(d_i log d_i)) rather than a strict O(V+E). Wide independent
frontiers therefore avoid both repeated full scans and unbounded task spawn.
Resource permits bound I/O classes and, as of `DAG-P0-06`, also bound owned
multipart part buffers via per-manager `buffer_bytes` credits (64 KiB quanta,
`acquire_many_owned`; env `AEROFTP_TRANSFER_BUFFER_BYTES`; default
`min(512 MiB, max(64 MiB, 10% MemAvailable))` on Linux, else 256 MiB). A
normal pool rounds its capacity down to whole quanta, so it never exceeds the
configured byte budget. A part whose rounded demand does not fit that usable
pool is admitted one-at-a-time through an explicit oversize lane. This is
**not** a process-global memory governor
(`DAG-P2-01`); the legacy non-DAG concurrent upload path in
`providers/multi_thread.rs` (`read_part_from_disk`) remains outside these
credits. On the first node failure the executor cancels a graph-scoped token
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

In the current single-file runner, `Discover*`, `AcquireResource`,
`VerifyChecksum`, and `EmitProgress` are structural no-ops. `CommitTemp` is
real for multipart completion, and `PreserveMetadata` is real for download
mtime preservation. Progress start/byte/error events still come from the
surface adapters and provider callbacks; the executor summary does not yet
populate a complete engine-level byte/retry telemetry stream.

## Single-file multipart

This is the most complete DAG path. `execute_single_file_dag` binds
`UploadPart` nodes to the real provider lifecycle:

1. the first part lazily calls `begin_multipart_upload`;
2. the executor acquires the part's directional + buffer-byte lease, then
   each part reads its local slice (`read_chunk` → `vec![0u8; len]`) and
   calls `upload_part` while the lease is still held;
3. receipts are collected and ordered by part number;
4. `CommitTemp` calls `complete_multipart_upload`;
5. a failed graph makes a best-effort `abort_multipart_upload` call.

Part buffer sizing is shared: builder and runner use
`multipart_part_byte_len(file_size, part_index, part_count,
preferred_chunk_size)` so graph accounting cannot drift from the allocation.

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
which is a real benefit. The complete scan and plan are then materialized
before graph construction. `DiscoverLocal`, `DiscoverRemote`, and `Compare`
describe that plan but do not perform the scan/planning work themselves.

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
global governor, is rebuilt per operation, and cannot tune a serial file slot
into parallelism. Batch file-level failures use the typed, non-fatal
`FileFailedButGraphContinues` outcome: D2 congestion reduces the File-class
target while unrelated file subgraphs continue. Sync still uses one file slot
and does not yet expose this batch feedback contract.

The resource manager is also per operation. It has file, checker, chunk, HTTP,
API, disk-read, disk-write, and hash classes, but no process/endpoint governor
and no byte-credit pool for multipart buffers. Upload/download resource
requests currently reserve both disk directions in the generic whole-file
profile. These are follow-up tasks, not guarantees of the present engine.

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

Copy now populates logical, wire, local-payload, and copy-fallback metrics.
Other executor byte and retry fields remain incomplete until P2 telemetry.

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
| `src-tauri/src/transfer_dag/adaptive.rs` | AIMD controller and congestion classification |
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
