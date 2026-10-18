# AeroAgent 2.0 roadmap: delivered foundations and remaining work

Code snapshot: `318be8459` on `main`, reconciled 2026-10-18. AeroAgent's product
version is still **1.7**, independently of AeroFTP's version. This document
closes the roadmap reconciliation in Requested item 50 of
[#1202](https://github.com/axpdev-lab/aeroftp/issues/1202); it does not announce a
2.0 release or promise that the proposals below will all ship together.

The September MCP-client and coding-agent plans predate AeroAgent 1.6 and 1.7.
Use this snapshot and queue when continuing those plans. Their June task cards,
handoffs and September execution order are historical evidence, not instructions
to implement the delivered features again. Task IDs below retain those plans'
identities; new extensions have suffixes so the original completed scope stays
closed.

## Delivered foundations

“Delivered” here means present on this main snapshot. The linked source and
existing tests are inspection evidence; this documentation change does not claim
a fresh live test of each subsystem.

| Plan ID / area | Present behavior and its boundary | Source evidence |
|---|---|---|
| CAG-10..14, CAG-20..31: coding foundation | Project rules and mentions, workspace context, contextual coding prompts and plan artifacts, patch preview/apply, checkpoints and restore, Git inspection/stage/commit, checks/verify, diagnostics and search already exist. The specialized handlers require a GUI context. An explicit user-selected Coding/Plan entry point is a separate decision. | [Rules](../src-tauri/src/coding_rules.rs), [workspace](../src/components/DevTools/aiChatCodingWorkspace.ts), [handlers](../src-tauri/src/ai_core/coding_tools.rs), [patch tests](../src/components/DevTools/aiChatCodingPatch.test.ts) |
| AA26-01: capability truth | Known-model capability metadata carries provenance; unknown models have an explicit conservative context budget rather than an assumed 4K window. Registry freshness remains maintenance work. | [Registry](../src/types/aiModelRegistry.ts), [tests](../src/types/aiModelRegistry.test.ts) |
| AA26-02: foreground Responses adapter | OpenAI Responses text/image/tool requests, streaming and scoped native turn state are implemented. Requests use `store: false`; this is not durable response chaining or resumable background execution. Other provider transports remain separate. | [Adapter](../src-tauri/src/openai_responses.rs), [streaming](../src-tauri/src/ai_stream.rs), [routing](../src-tauri/src/ai_native.rs) |
| AA26-03: provider-neutral tool discovery | The registry distinguishes built-in, plugin, macro and MCP provenance. `tool_search` loads additional callable schemas; discovery grants no permission. This local selection contract is already usable without a provider-hosted search service. | [Registry and selector](../src/components/DevTools/aiChatToolRegistry.ts), [tests](../src/components/DevTools/aiChatToolRegistry.test.ts) |
| AA26-10A/B; MCLIENT-01..06 foundations: MCP client | STDIO lifecycle/framing, namespaced discovery, schema validation, vault-held configuration and secrets, server settings, per-call approvals, cancellation and remote Streamable HTTP with backend OAuth all exist. Enablement is per server; ready snapshots offer every accepted tool. Per-tool policy controls remain below. | [STDIO supervisor](../src-tauri/src/mcp_client_transport.rs), [bridge](../src-tauri/src/mcp_client_bridge.rs), [HTTP](../src-tauri/src/mcp_client_http_transport.rs), [OAuth lifecycle](../src-tauri/src/mcp_client_oauth_lifecycle.rs), [settings](../src/components/AISettings/McpServersPanel.tsx), [snapshots](../src/components/DevTools/aiChatMcp.ts) |
| MCLIENT-07: reviewed preset foundation | The DeepWiki HTTP preset installs by backend-owned ID and starts disabled. Managed local install validation also exists. Downloaded Fetch/Git preset artifacts are still separate work, not a missing MCP client. | [Presets](../src-tauri/src/mcp_client_presets.rs), [managed installs](MCP-MANAGED-INSTALL.md), [installer](../src-tauri/src/mcp_client_install.rs) |
| AA26-30: scoped read-only workers | The chat can delegate bounded local-folder work and work on pinned S3 profiles. Workers share a foreground reservation ledger, budgets and cancellation. This does not provide arbitrary provider access, writes, nested delegation or jobs that survive an app restart. | [Coordinator](../src-tauri/src/ai_core/runner/worker_coordinator.rs), [worker run](../src-tauri/src/ai_core/runner/worker_coordinator/worker_run.rs), [S3 boundary](../src-tauri/src/ai_core/runner/worker_remote.rs), [ledger](../src-tauri/src/ai_core/runner/ledger.rs), [chat wiring](../src/components/DevTools/AIChat.tsx) |
| AeroAgent 1.7: GUI control | Bounded state and semantic intents cover navigation, selection, saved-profile connection, supported settings and AeroTools panels. The app shows control and provides pause/stop; session-bound backend approval remains required. This is not a general browser or unrestricted desktop driver. | [Broker and supported intents](../src-tauri/src/gui_controller.rs), [controller](../src/gui/controller.ts), [chat dispatch](../src/components/DevTools/aiChatToolPipeline.ts) |
| AeroAgent 1.7: MCP capability decisions | The community benchmark and additional remote tools are reachable from the GUI agent. A parity test accounts for each MCP tool as directly reachable, covered by another tool or deliberately declined with a reason. Parity does not mean every MCP name is exposed unchanged. | [Allowlist and parity tests](../src-tauri/src/ai_tools.rs), [benchmark](../src-tauri/src/ai_core/benchmark_tool.rs), [result formatting tests](../src/components/DevTools/aiChatToolResultFormat.test.ts) |

Local MCP execution has a specific platform limit: the production STDIO sandbox
launches on Linux and fails closed on Windows and macOS. Linux grants are
explicit and read-only; network access requires a separate consent. Remote HTTP
servers have their own transport and authentication path. See
[the launch boundary and sandbox tests](../src-tauri/src/mcp_client_sandbox.rs).
Neither “MCP client complete” nor the presence of a preset implies portable,
unrestricted local execution.

## Remaining queue for 2.0 evaluation

These are proposals with concrete completion criteria, not newly confirmed
defects or promises of a release date. The existing approvals and credential
boundaries apply to every extension.

| ID | Remaining scope | Acceptance before calling it done |
|---|---|---|
| AA26-03B | Optional provider-native deferred/hosted tool search beyond the shipped provider-neutral selector. | Negotiate the adapter capability, preserve tool identity and tool/result replay, keep the backend approval boundary, and test a deterministic local-selector fallback when the provider path is unavailable. |
| AA26-04 | Recoverable, compactable conversation state beyond the current foreground budget ledger and chat history. | Define a versioned event/replay contract; recover a stopped multi-step turn without repeating a completed write; preserve tool/result pairing across compaction; test cancellation and redacted persistence. Evaluate provider response chaining only after deciding retention and deletion policy. |
| AA26-11 | Progressive skill discovery and activation. The project-rules loader is not a `SKILL.md` runtime. | Load only skill metadata during discovery, instructions on activation and referenced resources when needed; preserve provenance and trust; test hostile paths, oversized content and tool approvals. |
| AA26-20 | Bind a workspace to exact saved deploy-profile IDs and produce a read-only deploy/verify plan. Existing remote tools alone do not provide that binding. | Resolve credentials by backend handle, identify staging/production and destination root, show the artifact plan and verify stat/hash/quota without writing. Test a changed or removed profile and a failed lookup. |
| AA26-20B | Approved deployment and provider-specific rollback. | Build on AA26-20; approve the actual upload/sync plan, capture outcomes, optionally run configured HTTP checks, and document which successful changes each provider can roll back. Never imply transactional rollback across providers. |
| AA26-12 | Browser and visual verification artifacts beyond the app's semantic GUI controller. | Define the supported browser/MCP adapter and grants; capture bounded screenshots, console/network evidence and failures; test cancellation and keep credentials out of artifacts. |
| AA26-30B | Extend the delivered workers to additional provider types or write-capable tasks. | Keep the read-only local/S3 implementation as the baseline. For each extension prove root/profile confinement, budget sharing, cancellation and approval behavior before enabling it; nested delegation needs its own policy. |
| AA26-31 | Durable/background jobs, with isolated coding worktrees only after persistence is defined. | Persist run state; survive process exit with an honest completed/failed/interrupted outcome; resume without duplicate writes; cancel every child and release credentials and filesystem resources. Depends on AA26-04. |
| AA26-40 | A maintained multi-step evaluation and trace suite across providers and surfaces. Existing unit/fixture coverage is not this end-to-end suite. | Publish reproducible scenarios for discovery, approvals, context limits, provider fallback, MCP failure, workers and cross-profile integrity, with redacted traces and explicit pass/fail criteria. Build fixtures alongside each lane. |
| CAG-02 / Block 3 | Decide whether a distinct Coding/Plan entry point is desired in addition to the shipped contextual activation. | Owner-approved interaction contract, localized entry point and onboarding, with tests showing the selected mode and its approval rules. Do not reimplement existing plan artifacts or coding tools. |
| MCLIENT-02/04B | Tool inspection and individual tool enablement/policy controls beyond the shipped server toggle. The current ready snapshot marks every accepted tool enabled. | Persist and enforce a per-tool refusal in the backend as well as discovery; show the supplying server and schema; revalidate policy revisions before execution; a user override must never lower the approval floor. |
| MCLIENT-07B | Fetch/Git presets downloaded at install time, tracked separately as Requested item 26 of #1202. | Review the supply-chain design, pin artifact versions and SHA-256, gate OS/architecture/Python ABI, reject a bad digest and interrupted install, and install disabled. DeepWiki remains the delivered preset. |
| MCLIENT-05B | Native STDIO sandbox support on Windows and macOS. | Demonstrate equivalent process/filesystem/network restrictions and explicit grants on each OS; until then keep local launch refused there. |

## Execution order and verification

1. Keep capability metadata, tool parity and MCP session/security regressions
   maintained on the shipped foundation. Do not reopen AA26-01/02/03/10/30 as
   blank-slate implementations.
2. Specify AA26-04 and its failure/replay fixtures. AA26-11 and the read-only
   AA26-20 binding can be designed independently; neither needs another MCP
   client implementation.
3. Extend to AA26-20B after the deploy plan is reviewable. Build AA26-31 after
   the persistence contract; evaluate worker extensions and browser adapters
   against the shared approval, trace and cancellation rules.
4. Treat the preset supply chain, native OS sandboxes and explicit Coding/Plan
   UX as separate design and platform gates. There is no decision here to ship
   them together or change the product version.

For implementation PRs, run formatting and focused tests for the touched path,
typecheck for TypeScript and the translation gates for locale changes. GitHub
Actions is the authoritative full PR gate; do not repeat the complete local
pre-push suite for each PR push. Use the full local gate where project policy
requires it, including direct main/release work or unavailable PR CI.

Keep this snapshot's source revision explicit when updating it. A source link
establishes implementation evidence, not a passing platform-specific live test.
