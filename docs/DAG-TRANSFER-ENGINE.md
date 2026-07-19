# DAG Transfer Engine

*Last updated: 2026-07-19. The engine is active in several production call
paths, but convergence is partial; this document records the paths that are
actually reachable, not every shape that the builder can represent.*

AeroFTP contains a shared, provider-agnostic transfer-DAG core. The core is
real and is used by the shaped single-file runner, the batch wrapper, the
non-dry-run sync wrapper, and the opt-in range runner. It is not correct to
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
| Multi-file batch | `transfer_orchestrator::execute_batch` → `execute_batch_dag` (`src-tauri/src/transfer_orchestrator.rs:66-70`) | A graph is built and executed, but the runner uses default capabilities; `entry_transferred` is a whole-file contract | No capability-driven cloud fan-out claim; generic settings currently clamp file concurrency |
| Non-dry-run sync | `sync_tree_core` → `execute_sync_dag` (`src-tauri/src/sync.rs:1223-1238`) | Scan and planning happen before the graph; the graph wraps a precomputed plan and a serial file driver | DAG wrapper is active; file transfer remains serial by design; dry-run stays on the planning path |
| Segmented download | `run_provider_segmented_download` (`src-tauri/src/providers/multi_thread.rs:290-310`) | Default is the legacy `JoinSet` range scheduler; `shaped_ranges` calls `execute_dag` only on the opt-in graph branch | Range I/O is real; DAG range scheduling requires `AEROFTP_RANGE_GRAPH=1`; GUI Auto may use one stream |
| Same-provider copy | GUI/CLI copy commands → `server_side_copy_with_fallback` (`src-tauri/src/provider_commands.rs:5011`, `src-tauri/src/bin/aeroftp_cli.rs:32161`) | Native provider copy or download → upload fallback | Native copy avoids local payload bytes; normal copy is not orchestrated by `shaped_copy` |
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
terminates resident siblings within two seconds — cooperative cancel first,
then forced `JoinSet` abort, followed by one bounded drain. This bound
applies to async work that yields to Tokio; synchronous blocking code must
not run directly inside a DAG node because an async runtime cannot preempt
it. Optional per-node timeouts start at dispatch (including AIMD/resource
waits), are typed as `TransferErrorKind::Timeout`, and are distinct from
external cancel (`Cancelled`). Production keeps `node_timeout = None` so long
valid transfers are not cut by an arbitrary engine limit (`DAG-P0-05`).

Three production wrappers call the core:

- `transfer_dag_single_file` builds `shaped_file` and binds real single-file
  provider operations;
- `transfer_dag_batch` builds `from_batch_shaped` and adapts each file to the
  existing `TransferExecutor` session contract;
- `transfer_dag_sync` builds `from_sync_plan_shaped` after scan/planning and
  adapts graph nodes to a serial per-file driver.

The builder also exposes `shaped_copy` and `shaped_ranges`. Those shapes are
useful and tested, but their presence is not evidence that the normal copy
command or default range path uses them.

## The shapes and their status

| Shape | Builder | Current production status |
|---|---|---|
| Single-file core | `shaped_file(Download|Upload, caps, size)` | Active in normal GUI/CLI single-file network paths, with router and provider exceptions |
| Multipart single-file | `shaped_file(Upload, caps, size)` → `UploadPart × N` | Active when the single-file runner receives multipart capabilities; independent wire workers only for the provider set listed below |
| Batch | `from_batch_shaped(items, caps)` | Active graph wrapper, but it passes `TransferCapabilities::default()` and does not provide a per-part batch I/O contract |
| Sync | `from_sync_plan_shaped(plan, caps)` | Active for non-dry-run sync, with default capabilities, precomputed scan/plan, and serial file execution |
| Copy | `shaped_copy(caps)` | Builder/test/forward-compatible runner shape; normal copy commands use `server_side_copy_with_fallback` directly |
| Segmented download | `shaped_ranges(N)` | Active only from the `AEROFTP_RANGE_GRAPH=1` branch; default remains the `JoinSet` scheduler |

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
- WebDAV Nextcloud chunked v2.

Dropbox, Box, pCloud, Filen, Drime, and Uploadcare can expose multipart
capability or part APIs, but the current audit does not promote them to
wire-level DAG fan-out without an independent worker and a provider-specific
live gate. A graph with N part nodes therefore means “N scheduled part
operations”, not automatically “N concurrent network requests”.

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
ordering-sensitive upload sessions; batch/sync runners still deduplicate
per-file whole-file I/O until P1-03 lands the real per-part contract.

## Batch and sync limitations

### Batch

`execute_batch_dag` is the current batch entry point, so the old hand-written
batch scheduler is gone. The batch runner nevertheless has three important
limits:

- it constructs the graph with `TransferCapabilities::default()` at
  `src-tauri/src/transfer_dag_batch.rs:104`, rather than the connected
  provider snapshot;
- the generic settings resolver currently clamps the effective file
  concurrency to one (`src-tauri/src/transfer_settings.rs:137`);
- `entry_transferred` is a whole-file `execute_with_session` contract. When a
  shaped batch graph contains multiple part nodes, the first node dispatches
  the whole file and the remaining nodes become no-ops
  (`src-tauri/src/transfer_dag_batch.rs:129`).

The batch graph is therefore active as an orchestration wrapper, but it is not
yet a capability-aware, per-part, wire-parallel cloud scheduler. A file error
is also recorded in the batch snapshot while the node currently returns
`Completed` (`src-tauri/src/transfer_dag_batch.rs:270`), so the documented AIMD
and failure semantics must not be stronger than that implementation.

### Sync

Non-dry-run sync reaches `execute_sync_dag`. Local and remote scans overlap,
which is a real benefit. The complete scan and plan are then materialized
before graph construction. `DiscoverLocal`, `DiscoverRemote`, and `Compare`
describe that plan but do not perform the scan/planning work themselves.

The sync builder receives default capabilities at
`src-tauri/src/transfer_dag_sync.rs:587-592`, the resource profile has one
file slot at `:615`, and `drive_sync_transfers` consumes jobs serially. This is
why the sync DAG should be described as a graph wrapper around a serial file
driver, not as parallel sync orchestration. Dry-run remains on the legacy
planning path by design.

## Segmented downloads

The range primitive itself is production code: it preallocates a temporary
file, writes validated ranges at offsets, handles servers that ignore Range,
and cleans up on cancellation. The default selection in
`providers/multi_thread.rs:290-310` still chooses the `JoinSet` scheduler.

When `AEROFTP_RANGE_GRAPH=1`, the same module builds `shaped_ranges` and calls
`execute_dag`; that branch binds each `DownloadRange` node to a real range
request and offset write. This is an opt-in migration path, not the default
production scheduler. The GUI's Auto value is conservative and can resolve to
a single stream.

## Server-side copy and cross-profile copy

The normal same-provider copy feature is real, but it is not currently a
`shaped_copy` production graph. `server_side_copy_with_fallback` is the shared
entry point for GUI and CLI copy call sites. It first tries the provider's
native `server_copy` when `supports_server_copy()` is true, then falls back to
download → upload only for the explicitly recoverable capability failures.
Authentication, missing-source, quota, and other hard errors remain errors.

The `shaped_copy` builder and the `ServerSideCopy` runner branch model the
desired one-API-slot graph, and the builder has unit tests for both native and
fallback shapes. The normal copy commands do not construct that graph, so the
engine must not claim a unified copy node or S3 `UploadPartCopy` fan-out.

Cross-profile transfer is a different operation: it downloads the source into
a local temporary file and uploads it to the destination. It may select the
segmented source helper or SFTP delta upload, but it does not call the shared
single-file DAG runner for both legs.

## AIMD, observers, and resources

`AimdController` is real and can be passed to `execute_dag`. It is useful on
the paths that expose actual class-level concurrency, especially single-file
multipart and the opt-in range graph. It is not a global governor, is rebuilt
per operation, and cannot tune a serial file slot into parallelism. Batch
file-level failures are currently hidden behind a completed node, and sync
uses one file slot, so neither path supplies the full feedback loop implied by
the original convergence description.

The resource manager is also per operation. It has file, checker, chunk, HTTP,
API, disk-read, disk-write, and hash classes, but no process/endpoint governor
and no byte-credit pool for multipart buffers. Upload/download resource
requests currently reserve both disk directions in the generic whole-file
profile. These are follow-up tasks, not guarantees of the present engine.

`DagObserver` provides a shared node lifecycle abstraction. The GUI's
`GuiDagObserver` is used on the shaped single-file path, but surface start,
byte-progress, and error events still come from `TransferEventSink` and
provider callbacks — not from DAG node lifecycle. Byte progress is therefore
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

The executor's bytes and retries fields remain zero until the corresponding
bindings are wired (P2 telemetry).

## Capability contract

`StorageProvider::transfer_capabilities()` is consumed by the shaped
single-file runner before `shaped_file` is built. The batch and sync runners
currently use `TransferCapabilities::default()` at their documented call
sites. The normal copy helper uses the provider's `supports_server_copy()`
and `server_copy()` methods rather than `shaped_copy`'s capability snapshot.

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
| `src-tauri/src/transfer_dag_single_file.rs` | Real shaped single-file runner |
| `src-tauri/src/transfer_dag_batch.rs` | Batch graph wrapper and file-session adapter |
| `src-tauri/src/transfer_dag_sync.rs` | Non-dry-run sync graph wrapper and serial driver |
| `src-tauri/src/providers/multi_thread.rs` | Default range scheduler and opt-in DAG range runner |
| `src-tauri/src/copy_fallback.rs` | Normal native-copy/fallback policy |
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
| Copy behavior spread across provider call sites | Native copy/fallback policy is shared by `server_side_copy_with_fallback`, but is not yet a production `ServerSideCopy` DAG node |
| Range graph migration | `shaped_ranges` exists and is real, but the default remains the `JoinSet` scheduler |
| One fully converged engine for every surface | Shared core plus selected wrappers; batch/sync/copy/cross-profile still have documented adapters and limits |

For the planned next steps—resource credits, capability-aware batch, real
sync concurrency, production shaped copy, and global resource governance—see
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
