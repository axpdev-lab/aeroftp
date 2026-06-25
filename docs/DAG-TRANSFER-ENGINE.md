# DAG Transfer Engine

*Last updated: 2026-06-22 (current as of the 4.0.x series; engine introduced in the v4.0.0 convergence).*

AeroFTP routes every file transfer through a shared, provider-agnostic
node-graph engine. This document describes what the engine is, why it
exists, how the production graph is shaped, and how providers plug into
it. It is the canonical technical reference for the engine after the
v4.0.0 convergence; the deeper architectural walk-through lives at
`docs.aeroftp.app/architecture/dag-transfer-engine.md`.

## What the engine is

The DAG transfer engine schedules a per-transfer directed acyclic
graph of typed nodes. Each node represents one structural step of a
transfer (discover the source, acquire the resource, move the bytes,
verify, preserve metadata, commit, emit progress); the executor only
dispatches a node when all of its predecessors completed and the
node's resource request can be satisfied from a shared per-session
budget. Topology is fixed at build time; concurrency comes from a
small, principled set of resource classes (file, chunk, http, disk
read, disk write, api).

Three layers compose the engine:

- **`transfer_dag` core** - pure, provider-free graph engine. Owns
  the executor, the graph and node types, the resource manager, the
  AIMD backpressure controller, and the observer pipeline.
- **`TransferDagBuilder`** - single source of truth for every shape
  the engine schedules: `single_file`, `from_batch`, `from_batch_shaped`,
  `from_sync_plan`, `from_sync_plan_shaped`, `shaped_file`, `shaped_copy`,
  `shaped_ranges`.
- **Three thin runners** - `transfer_dag_single_file`,
  `transfer_dag_batch`, `transfer_dag_sync`. Each is a bridge: it
  builds a graph through the builder, hands it to `execute_dag`, and
  binds each node kind to real provider I/O through a runner closure.

The CLI, the Tauri GUI commands, and the MCP server all schedule
transfers through the same runners, so the wire-level behavior is
identical across surfaces by construction.

## Why a DAG engine

Three converging needs justified the convergence:

1. **One observability surface.** Pre-DAG, each surface emitted
   slightly different progress / completion events through its own
   ad-hoc orchestrator. The shared engine produces one
   `DagObserver` stream that every surface consumes; the GUI
   `transfer_event` stream, the CLI exit-code-and-line semantics, and
   the MCP `notifications/progress` channel all derive from the same
   per-node lifecycle.
2. **Capability-aware shape.** The shaped builders read a provider's
   `TransferCapabilities` and pick the right transfer-core shape per
   transfer:
   - `multipart_upload` on the upload direction: the transfer core
     fans out into N `UploadPart` nodes, one per chunk, parallelized
     through the shared chunk budget.
   - `server_side_copy`: the copy graph collapses into a single
     `ServerSideCopy` node that holds only an `api_slot` (no disk
     I/O, no file slot).
   - `strict_concurrent_range_download`: the segmented download graph
     emits N `DownloadRange` nodes with no inter-segment dependencies.
   The same provider trait keeps the legacy fallback (no fan-out,
   single transfer node) for backends that do not advertise the
   capability.
3. **One scheduler, one place to fix.** The AIMD backpressure
   controller, the file / chunk / http / api budgets, the resource
   manager, the session pool - every scarce resource lives in
   `transfer_dag/resources` and is governed once for every transfer.
   Backends pick what they reserve (a one-line `ResourceRequest` per
   node kind); they do not own a scheduler of their own.

## The shapes

| Shape           | Builder method                    | When it applies                                   | Core nodes (excluding structural anchors)          |
| --------------- | --------------------------------- | ------------------------------------------------- | -------------------------------------------------- |
| Single file     | `shaped_file(Download, …)`        | Any download leaf                                 | `DownloadFile`                                     |
| Single file     | `shaped_file(Upload, …)`          | Upload below one chunk OR no multipart capability | `UploadFile`                                       |
| Multipart fan-out | `shaped_file(Upload, caps, size)` | Upload above one chunk AND multipart capability | N × `UploadPart`                                   |
| Batch           | `from_batch_shaped(items, caps)`  | Every multi-file transfer                         | Per-file single-core or multipart fan-out          |
| Sync            | `from_sync_plan_shaped(plan, caps)` | Every non-dry-run sync                          | Global `DiscoverLocal`+`DiscoverRemote`→`Compare`, per-file chain |
| Copy            | `shaped_copy(caps)`               | Cross-bucket / cross-folder copy                  | `ServerSideCopy` OR `DownloadFile`+`UploadFile`    |
| Segmented dl    | `shaped_ranges(N)`                | Intra-file Range-download fan-out                 | N × `DownloadRange`                                |

Every shape carries the same seven-node structural envelope:

`Discover(Local|Remote)` → `AcquireResource` → *transfer core* →
`VerifyChecksum` → `PreserveMetadata` → `CommitTemp` → `EmitProgress`.

The structural nodes hold no scarce resources; only transfer-core
nodes reserve `file_slots` / `chunk_slots` / `disk_read_slots` /
`disk_write_slots` / `http_slots` / `api_slots`. The executor enforces
the budget before dispatching a node, so the same `ResourceRequest`
that a node lists is what the scheduler arbitrates against.

## Multipart orchestration

The multipart fan-out shape is the most active part of the runner.
For an upload above one preferred chunk on a multipart-capable
provider (S3, B2, …), the runner allocates a per-transfer
`MultipartCtx`:

- An `Arc<Mutex<Option<MultipartHandle>>>` initialized lazily: the
  first `UploadPart` invocation that wins the mutex opens the session
  via `StorageProvider::begin_multipart_upload(remote, total_size,
  content_type, local_source_path)`. Subsequent invocations observe an initialized
  handle and skip the call.
- An `Arc<Mutex<Vec<UploadedPart>>>` that collects per-part receipts.
- An `Arc<HashMap<usize, u32>>` mapping each `UploadPart` node id to
  its 1-based part number (matching the S3 / B2 contract).

The terminal `CommitTemp` node sorts the receipts by `part_number`
ascending and submits them through
`StorageProvider::complete_multipart_upload(handle, parts)`. On any
runner-level failure the engine spawns a best-effort
`abort_multipart_upload(handle)` so the provider does not accumulate
orphan upload IDs.

The graph's `VerifyChecksum` node joins every `UploadPart` node before
it can run, so the commit only fires once every part lands. The
shared chunk budget governs how many parts upload in parallel;
provider-specific protocol caps (S3 / B2 both ceiling at 10000 parts)
are enforced by the builder profile, not the runner.

## Server-side copy

Copies between two keys on the same provider (`S3` `x-amz-copy-source`,
`B2` `b2_copy_file`, WebDAV `COPY`, ImageKit `copyFile`, and the 14
other native providers that advertise the capability) route through
the `shaped_copy` graph. The transfer core collapses to a single
`ServerSideCopy` node that reserves only an `api_slot`, so the engine
never schedules disk I/O for a server-side copy. The capability gate
is `TransferCapabilities::server_side_copy.is_available()`.

When the capability is absent the graph degrades honestly: a
`DownloadFile` followed by an `UploadFile` (two real transfer nodes,
two file slots, two real round-trips). The shape is fixed at build
time so the executor never has to second-guess the legacy fallback at
runtime.

## AIMD backpressure

Every shape runs under the same `AimdController`. The controller
shrinks the per-class dispatch target on a real congestion signal
(HTTP 429 / 503, network timeout, connection reset, SFTP channel
disconnect) and grows it linearly when transfers complete cleanly.
The ceiling is the budget the resource manager was constructed with,
so a no-congestion transfer dispatches identically to the ceiling
ceiling.

The controller is per-transfer (constructed once at the top of
`execute_dag` and dropped when the transfer ends); persistence across
runs remains out of scope.

## Provider trait surface

A provider participates in the engine by advertising its capabilities
and implementing the matching trait methods. The capability snapshot
is read once from `StorageProvider::transfer_capabilities()` before
the graph is built; the runner reads it on the engine side and the
builder shapes the graph accordingly. Three families of methods
matter:

| Trait method                              | Purpose                                     |
| ----------------------------------------- | ------------------------------------------- |
| `begin_multipart_upload(remote, total_size, content_type, local_source_path)` | Open a multipart session. |
| `upload_part(&handle, part_number, data)` | Upload one part of a multipart session.     |
| `complete_multipart_upload(handle, parts)` | Finalize a multipart session.              |
| `abort_multipart_upload(handle)`          | Release session state on failure.           |
| `server_side_copy(from, to)`              | Native server-side copy on the same backend.|
| `supports_server_side_copy()`             | Capability gate for the `ServerSideCopy` node. |

The default trait implementations return `ProviderError::NotSupported`,
so a provider that never advertises the capability never reaches them.
A provider that advertises a capability MUST implement the matching
methods or the runner will surface the `NotSupported` error at the
first dispatch.

## File map

| File                                      | Role                                                   |
| ----------------------------------------- | ------------------------------------------------------ |
| `src-tauri/src/transfer_dag/mod.rs`       | Public exports of the engine surface.                  |
| `src-tauri/src/transfer_dag/builder.rs`   | Every shape constructor (`shaped_file`, `from_batch_shaped`, `shaped_copy`, `shaped_ranges`, …). |
| `src-tauri/src/transfer_dag/executor.rs`  | `execute_dag` + resource arbitration + observer pipeline. |
| `src-tauri/src/transfer_dag/capabilities.rs` | `TransferCapabilities` + `Capability` enum.          |
| `src-tauri/src/transfer_dag/resources.rs` | `TransferBudget` + `ResourceRequest` + `TransferResourceManager`. |
| `src-tauri/src/transfer_dag/adaptive.rs`  | `AimdController` + congestion classifier.              |
| `src-tauri/src/transfer_dag/observer.rs`  | `DagObserver` + Journal / Ordered / Gui observers.     |
| `src-tauri/src/transfer_dag/probe.rs`     | `SessionProbeCache` + `resolve_session_model`.         |
| `src-tauri/src/transfer_dag_single_file.rs` | Single-file runner (CLI + GUI single transfer).      |
| `src-tauri/src/transfer_dag_batch.rs`     | Multi-file batch runner.                               |
| `src-tauri/src/transfer_dag_sync.rs`      | Sync session runner.                                   |
| `src-tauri/src/providers/multi_thread.rs` | Segmented download runner.                             |

The `provider_transfer_executor.rs` module still hosts the
`TransferExecutor` implementations (`ProviderDownloadExecutor`,
`ProviderUploadExecutor`) the batch runner consumes through
`execute_with_session`, the segmented download eligibility gate, and
the `ProviderListSessionModel` enum the scan layer reads. Its full
removal is deferred to a post-convergence cleanup window.

## What changed in v4.0.0

| Before v4.0.0                                          | After v4.0.0                                          |
| ------------------------------------------------------ | ----------------------------------------------------- |
| Flag-gated DAG path: `AEROFTP_TRANSFER_ENGINE_DAG_*`   | DAG path unconditional, three env vars removed.       |
| Hand-rolled `JoinSet` sliding-window batch orchestrator | `execute_batch_dag` is the only batch path.          |
| Multipart upload was internal to `provider.upload()`   | Multipart is an engine concern: N `UploadPart` nodes governed by the shared chunk budget. |
| Server-side copy was an ad-hoc per-provider method     | One `ServerSideCopy` node, one `api_slot`, one shape. |
| Five distinct routing shims (`if dag_enabled { … }`)   | Zero shims; the graph engine is the production path.  |

## See also

- `docs.aeroftp.app/architecture/dag-transfer-engine.md` - long-form
  architectural walk-through, design rationale, performance numbers.
- `docs/PROVIDER-INTEGRATION-GUIDE.md` - how to plug a new storage
  backend into the engine (capability advertisement, trait methods,
  session pool integration).
- `docs/THREAT-MODEL.md` - STRIDE analysis for the engine surface.
