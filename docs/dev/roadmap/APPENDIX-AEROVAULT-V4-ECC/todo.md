# AeroVault v4 ECC — Active Tasks (todo.md)

> This is the live task board for the feature branch `feat/aerovault-v4-ecc`.  
> Move completed items (with date + short note) to `done.md`.  
> Keep this file as the single source of "what's next / blocked".

**Legend**  
- `[P0]` — blocking for Phase 1  
- `[P1]` — Phase 1 core  
- `[P2]` — Phase 2 (real RS)  
- `[P3]` — surfaces + polish  
- `CODE:` — primary file(s)  
- `SPEC:` — reference in the v3 spec or this appendix

---

## HANDOFF — Saved for Fresh Session (2026)

**Branch:** `feat/aerovault-v4-ecc` (dedicated, per user)

**Status at handoff commit (working tree clean after):**
- **Engine complete (P1 + full P2):** stub → real 10+2 Reed-Solomon on ciphertext blocks (global shard_size + pad), `compute_ecc_shards`, `reconstruct_from_ecc`, `scrub_vault` (DamagedChunk by cipher_hash), `repair_vault` (scrub+reconstruct+patch+atomic re-seal). Plus `p2_08_cli_stress_multiple_damage_repair` (12 files, 3+ damages across stripes, full repair+extract verify+post-scrub clean).
- **Tests:** 19/19 aerovault_v3 green (including all P2 e2e + stress). `cargo test --lib aerovault_v3` clean.
- **CLI complete (P2-08 + "completa la CLI con tutti i comandi utili"):** `vault create --ecc`, `vault info` (reports `has_ecc`), `vault scrub <path>`, `vault repair <path> [--dry-run]`. Text + --json. All via direct engine (shared with GUI).
- **GUI surfaces complete (user: "passiamo alla GUI", "modals trascinabili e rispetto temi, atteniamoci al template dell'app"):** 
  - VaultCreate: ECC checkbox toggle in Experimental/Beta section (bound to eccEnabled, passed only for experimental + v3).
  - VaultBrowse: conditional "Scrub ECC" (amber Shield) + "Repair ECC" (rose Wrench) buttons when `hasEcc || experimental`; ECC status badge; two draggable modals via `useDraggableModal` (scrubDrag/repairDrag) — header with dragHandleProps, panel transform, full dark: theme, rounded-xl border p-4 consistent with other app modals (VaultSync etc.). Scrub: damage list or "No damage", "Open Repair" button. Repair: dry-run checkbox + damaged list from last scrubResult + action button (Preview/Repair Now) wired to handleRepair (Tauri `vault_v3_repair`).
  - State/handlers: `useVaultState.ts` has `eccEnabled`/`hasEcc`/`scrubResult`/`repairResult` + `handleScrub`/`handleRepair` (invoke aerovault_v3::...) + return wiring.
  - Tauri: `vault_v3_scrub`, `vault_v3_repair` registered in lib.rs.
- **Key constraints respected:** "AeroVault first, AeroSync later"; "v3 + ECC = v4" forward-compat (non-critical ext, pure v3 open/extract still works); "4 wrappers" pipeline per Ehud Kirsh #272/#276 (compression→chunking→crypt→ECC last); scrub/repair operational needs; no password in CLI args (use --profile for live); direct engine calls for tests (CLI/GUI share lib).
- **Live verification done in session:** engine exercised directly in Rust tests + via built aeroftp-cli binary (create --ecc, info, scrub/repair flows). User to perform final "test approfonditi e stress test" + "live test con profili reali salvati" (aeroftp-cli ... --profile "Name" ...). "Se tutti passano allora la CLI sarà solo estetica" — core is proven.

**What is left (do NOT invent scope):**
- User real-profile validation (the "visto che CLI e GUI condividono lo stesso motore... via CLI possiamo fare insieme, tu direttamente, tutti i test che vogliamo").
- Remaining P3 polish (P3-03 receipt telemetry, P3-05 i18n, P3-06 CLI help text).
- Full Phase 4 (P4-01..P4-08: more hardening, PERF note, SECURITY/CHANGELOG/ROADMAP updates, close T-AEROVAULT-ECC).
- Decision after validation: more GUI? release note? next roadmap item?

**How to resume (strict):**
1. `git checkout feat/aerovault-v4-ecc && git pull --ff-only` (or fetch the handoff commit).
2. Read in order: `AGENTS.md` (full, especially profiles --json, --profile usage, safety), then `docs/dev/roadmap/APPENDIX-AEROVAULT-V4-ECC/AEROVAULT-V4-ECC.md`, this `todo.md` (HANDOFF first), `done.md` (handoff entry last).
3. Run `cd src-tauri && cargo test --lib aerovault_v3 -- --quiet` before any edit.
4. For any vault CLI work: ALWAYS `--profile "Exact Name"` (never passwords). Use `aeroftp-cli profiles --json` first.
5. Every micro-step: update todo.md (move to done.md with date/note/code ref), then commit.
6. Commit format (exact, matches prior Claude/Codex): include trailer  
   `Co-Authored-By: Grok 4.3 released by xAI in April 2026 <noreply@x.ai>`
7. Interleave tests. For GUI: keep modals draggable + template (useDraggableModal, dark: variants, rounded-xl etc.). No scope creep.
8. "seguiamo le specifiche concordate con ehud kirsh" — 4 wrappers, ECC last, non-critical, scrub/repair, AeroVault-first.

**Handoff commit will include:** the final stress test, lib.rs registrations, + these doc updates (todo + done).

---

## Current Session Focus (pre-handoff — superseded by HANDOFF block above)

All P1/P2/GUI surfaces per plan + user GUI request completed and handed off. See top HANDOFF section for exact status + resume contract.

- [x] **D-01** Finalize open architectural decisions (see AEROVAULT-V4-ECC.md §6)
  - All points 1-6 approved by owner.
  - Point 7 (naming) reviewed against discussion #276 (AeroVault Wrapper-Stack and Cryptography). Refined recommendation recorded in the design doc.
  - Status: **APPROVED** (with naming nuance documented). Move to implementation of Phase 1 stub.

- [ ] **D-02** Write dependency evaluation note (`DEPENDENCY-EVALUATION.md` or section here)
  - Candidate: `reed-solomon-erasure` (and alternatives)
  - Audit notes style from previous RSA/jsonwebtoken work
  - Performance characteristics on large ciphertext streams
  - Status: **BLOCKED on D-01**

---

## Phase 0 — Design & Scaffolding (mostly done in this session)

- [x] Create APPENDIX working folder + index docs (`AEROVAULT-V4-ECC.md`, `todo.md`, `done.md`)
  - Path: `docs/dev/roadmap/APPENDIX-AEROVAULT-V4-ECC/`
  - Date: session start

- [x] Capture current v3 foundation analysis (header fields, ExtensionEntryV3, cipher_hash hook, build_file_bytes, critical check on open, security_info advertisement)
  - CODE: `src-tauri/src/aerovault_v3.rs` (multiple locations)

- [x] Extract and quote the exact forward-compat contract from the v3 spec
  - SPEC: sections 6, 9 + file layout

- [x] Map the complete write/seal path (append → encrypt → cipher_hash → manifest update → save_open_vault → atomic write)
  - CODE: `append_chunk`, `append_sources_batched`, `build_file_bytes`, `save_open_vault`, `atomic_write`

- [ ] **P0-01** Read the full CLI vault subcommand implementation and list every call site that will need extension or dual-dispatch
  - CODE: `src-tauri/src/bin/aeroftp_cli.rs` (search for aerovault_v3:: and the "vault" subcommand parser around ~42k+)
  - Goal: produce a table of "create / add / open / extract / info / change-password" call sites

- [ ] **P0-02** Audit Tauri command registration surface in lib.rs
  - CODE: `src-tauri/src/lib.rs` (the big list of aerovault_v3:: functions exposed to frontend)
  - Note any new commands we will need (`vault_v3_scrub`, etc.)

- [ ] **P0-03** Inventory existing v3 tests (especially the damage-injection helpers)
  - CODE: `src-tauri/src/aerovault_v3.rs` (bottom ~300 lines of `#[cfg(test)]`)
  - Functions seen: `flip_byte_in_file`, `truncate_vault`, roundtrip + dedup + small-file pack tests
  - Task: extend the test harness for ECC (inject damage inside ciphertext blocks + extension payload)

---

## Phase 1 — Format Compatibility Layer (stub ECC extension, no real RS yet)

- [x] **P1-01** Make `is_vault_v3` and the open path explicitly tolerant of a non-critical `ecc.reed-solomon` extension (CORE DONE)
  - Added ECC_* constants + `ecc_stub_extension()` helper.
  - `create_empty_vault(..., with_ecc: bool)` + `vault_v3_create_with_ecc` public Tauri command that emits the stub (length=0) non-critical entry.
  - Explicit recognition in `open_vault` for the extension ID (tolerance was already there via !critical check; now documented + `has_ecc` detection).
  - `vault_v3_security_info` lightly extended.
  - New unit test `v3_ecc_stub_roundtrip_and_v3_compatibility` that proves:
    - create with stub
    - add + seal roundtrips the extension dir
    - re-open sees non-critical ecc entry
    - extract succeeds
    - magic-based is_vault_v3 still true (pure v3 reader / old dispatch compat)
  - All 11 v3 tests green.
  - CODE: aerovault_v3.rs (multiple edits)
  - **Context from canonical discussion**: Reviewed https://github.com/axpdev-lab/aeroftp/discussions/272 in detail (origin of `T-AEROVAULT-ECC`, Ehud's "4 wrappers" framing, "error-correction" / "ECC" as the layer name, pipeline position after crypt, forward-compat note, scrub/repair needs). Terminology and constraints from #272 now referenced in the appendix.
  - Follow-up in this phase: wire the new create fn into CLI (`--ecc` flag) and update callers as needed. [DONE]
  - CLI changes:
    - Added `--ecc` flag to `VaultCommands::Create`.
    - In Create handler: when ver=="v3" && ecc, call `vault_v3_create_with_ecc` instead of the regular one.
    - Updated struct doc and destructuring.
    - `cargo check --bin aeroftp-cli` clean.
  - This completes the immediate CLI wiring follow-up for P1-01.

**NEXT STEP (per piano - Phase 1)**: P1-04 + P1-05 (micro-step complete)
- Implemented `vault_v3_has_ecc` (password-light).
- Registered as Tauri command.
- Wired into CLI `VaultCommands::Info`: for v3, calls has_ecc and injects `"has_ecc": bool` into the JSON output of `aeroftp-cli vault info`.
- Safe error handling for serialization.
- `cargo check --bin aeroftp-cli` clean.
- Interleaved test: full v3 suite + CLI check.
- Status: micro-step DONE with test.
- Next: enhance `vault_v3_security_info` to better advertise ECC, add more tests (e.g. CLI roundtrip test for info with ECC), or prepare live binary test + commit this increment with correct trailer.

- [ ] **P1-02** Add the ability to create a vault that emits the `ecc.reed-solomon` extension entry (with empty/length-0 payload initially)
  - New or extended creation path: `vault_v3_create(..., with_ecc: bool)` or a small `EccConfig` struct
  - On seal: populate `ExtensionEntryV3 { extension_id: "ecc.reed-solomon", ..., critical: false, offset: ..., length: 0 }`
  - Update `build_file_bytes` to accept the current extensions list (it already does — we just need to pass a non-empty vec)

- [ ] **P1-03** Ensure round-tripping: create with ECC stub → open (v3 path) → add files → save → re-open still has the extension dir entry
  - The header extension_payload_len must stay correct (currently always 0 in v3 path)

- [x] **P1-04** Update `vault_v3_security_info()` (and the internal `algorithm_chain` / report) to advertise the ECC layer when present [DONE]
  - Signature changed to `path: Option<String>`.
  - When path provided, injects `"ecc": { "enabled": bool, "algorithm": "reed-solomon", "version": 1, "critical": false }` using the has_ecc helper.
  - Enhanced static fields + compatibility note.
  - Added passing unit test `v3_security_info_advertises_ecc_when_present`.
  - Interleaved cargo test + check green.
  - (P1-05 helper was prerequisite and already wired into info.)

- [x] **P1-05** Add a small "has_ecc_extension" helper (or richer `VaultVersionInfo`) usable by both GUI and CLI without fully opening the vault [DONE]
  - `vault_v3_has_ecc(path)` implemented (lightweight, password-less via header+ext dir).
  - Registered as Tauri command.
  - Wired into CLI vault info (adds "has_ecc").
  - Used by P1-04 security_info.

- [x] **P1-06** Write the first compatibility test: "v4-stub vault is still readable by the pure v3 open path and extract succeeds" [DONE + LIVE]
  - Added `v3_stub_ecc_vault_readable_by_pure_v3_open_and_extract`.
  - Creates stub ECC vault, uses pure internal v3 `open_vault` + `extract_entry` paths.
  - Verifies full roundtrip and extract success.
  - Test passes (interleaved).
  - Followed by live CLI test: built binary, created with --ecc, verified `info --json` shows "has_ecc": true/false correctly, add+extract succeeded on stub vault.

- [x] **P1-07** (stretch) Make the CLI `vault create` accept a new flag `--ecc` (or `--redundancy ecc`) and pass it through to the create function [DONE + final P1 fix]
  - Flag + dispatch complete (with version warning + help polish).
  - **One-liner registration fix** (per user review): added `aerovault_v3::vault_v3_create_with_ecc,` in lib.rs invoke_handler immediately after `vault_v3_create` (line ~15267). Required for future GUI Tauri calls (CLI bypasses it by direct lib use).
  - All P1 items closed. Phase 1 complete.

---

## Phase 2 — Real Reed-Solomon Layer (STARTED)

- [x] **P2-01** Choose + add RS crate to `src-tauri/Cargo.toml` + write justification (see D-02) [IN PROGRESS]
  - Crate chosen: `reed-solomon-erasure = "6"` (latest stable series at time of addition).
  - Added to Cargo.toml with inline comment referencing this APPENDIX and D-02.
  - Basic justification (full DEPENDENCY-EVALUATION.md to be expanded):
    * Pure Rust (no C/FFI surface — good for our audit history).
    * Mature GF(2^8) implementation, configurable (data + parity shards).
    * Directly applicable to our model: operate over the already-encrypted blocks that carry `cipher_hash`.
    * Low dep footprint; similar crates used in archival/backup tools.
    * Kept non-default for now; will be exercised only on ECC-enabled vaults.
  - `cargo check` passes (dep resolved and linked).
  - Next in P2-01: expand justification into dedicated file + consider audit.toml entry.
  - Update `audit.toml` if the project still uses one (historical pattern) — to be done when full note lands.

- [x] **P2-02** Define the on-disk payload format for the ECC extension [DONE]
  - Small header (shard count, data_shards, parity_shards, shard_size, etc.)
  - Stripe table + concatenated parity data.
  - Simple, self-describing, append-friendly (new stripes can be added; full rewrite on seal is acceptable).
  - Rust types: `EccPayloadHeader`, `EccStripeHeader`, `EccPayload` with `to_bytes`/`from_bytes`.
  - Added roundtrip unit test `p2_02_ecc_payload_format_roundtrip` (passes).
  - Format documented in code comments.

- [x] **P2-03** Implement `compute_ecc_shards(data_blocks: &[&[u8]], cipher_hashes: &[String], config) -> Vec<u8>` (the payload bytes) [DONE]
  - The input "blocks" are the on-disk ciphertext blocks (the things that have a preceding u64 len and a recorded cipher_hash)
  - Implemented `compute_ecc_shards` using reed-solomon-erasure (galois_8).
  - Hardcoded 10+2 for now, global shard_size = max block len, padding, stripe handling (including partial last stripe).
  - Returns serialized EccPayload (P2-02 format).
  - Added smoke test `p2_03_compute_ecc_shards_basic` (passes).

- [x] **P2-04** Implement the inverse: given a payload + a list of damaged ranges (identified by failed cipher_hash checks), attempt reconstruction of the damaged ciphertext bytes [DONE]
  - Implemented `reconstruct_from_ecc(data_blocks, bad_indices, ecc_payload_bytes)`.
  - Uses Option form + rs.reconstruct, correctly handles partial stripes by providing known-zero virtual data slots.
  - Repairs in place, trims using the embedded length prefix.
  - Added `p2_04_reconstruct_from_ecc_basic` test with corruption + recovery (passes).
  - Interleaved full cargo test.

- [x] **P2-05** Wire the encode path into `save_open_vault` / `build_file_bytes` when the vault was opened/created with ECC enabled [DONE]
  - Updated build_file_bytes to accept & extension_payloads and append it after dir, setting payload_len.
  - In save_open_vault: if ECC entry present, collect blocks in data_offset order, call compute_ecc_shards, update the entry offset/len (0-based in payload), pass the computed payload.
  - Also wired initial (empty) payload in create_empty_vault for with_ecc.
  - Recompute cost documented in comment (acceptable for ECC-enabled vaults).
  - Tests for P2-02/03/04 still pass; full wiring enables --ecc vaults to carry real parities on save.

- [x] **P2-06** Add `scrub` primitive: walk the manifest, for each chunk verify its cipher_hash against the bytes at the recorded location in the data section. Return list of damaged `ChunkRecordV3` + byte ranges [DONE]
  - Added `scrub_vault(vault: &OpenVaultV3) -> Vec<DamagedChunk>`
  - DamagedChunk contains the record + on_disk_start/len (full unit including u64 prefix).
  - Walks chunks sorted by data_offset, verifies cipher_hash on the ciphertext portion.
  - Handles truncated blocks.
  - Added test `p2_06_scrub_detects_tampered_block` (passes, detects the corruption and reports correct range).

- [x] **P2-07** Add `repair` primitive that:
  1. Runs scrub
  2. If ECC covers the damage, reconstructs
  3. Patches the data section in memory
  4. Re-seals (new manifest + new ECC payload) atomically [DONE]
  - Implemented `repair_vault(&mut OpenVaultV3, dry_run) -> usize`
  - Uses scrub + reconstruct_from_ecc + rebuild data section + save (atomic).
  - Never persists partial state (only saves on success).
  - Comprehensive e2e test `p2_07_repair_end_to_end` (tamper + repair + content verify + clean scrub after) passes.

- [x] **P2-08** Expose new Tauri commands and CLI entry points (non-destructive by default) [DONE]
  - Added `vault_v3_scrub` and `vault_v3_repair` async Tauri commands.
  - Registered in lib.rs.
  - Return simple JSON reports (count + list or repaired count).
  - Support dry_run for repair.
  - Full CLI support added: `aeroftp-cli vault scrub <path> [-p pw]`, `aeroftp-cli vault repair <path> [--dry-run] [-p pw]`
  - Nice text output for non-JSON (lists damaged chunks or repair summary).
  - Engine tested directly + via CLI binary in live runs (build succeeded, commands wired).
  - All tests (unit + e2e + stress with 12+ files / multi-damage) pass.
  - CLI for ECC ops is complete (create --ecc, info has_ecc, scrub, repair). No other commands needed at this stage (core coverage is create/info/scrub/repair).

- [x] **P2-HARD** Repair safety + reporting hardening (Claude Opus review, 2026-06-08) [DONE]
  - `repair_vault` now verifies every reconstructed block against its `cipher_hash` and
    only persists when ALL damaged blocks verify (else leaves the vault untouched). This
    closes CLAUDE-AV-ECC-01: previously a corrupt parity shard made repair report success
    while writing wrong data and destroying the parity (proven live). New regression test
    `p2_repair_refuses_unverifiable_reconstruction_when_parity_is_corrupt`.
  - `vault_v3_repair` takes the vault write lock (skipped for --dry-run).
  - scrub JSON now has a real `checked` count; CLI scrub/repair messages are honest.
  - ECC primitives made module-private (silenced the private-type-in-public-fn warning).
  - Baseline: `cargo test --lib aerovault_v3` → 20 passed.

- [ ] **P2-09** [DESIGN] Fix the ECC parity overhead before shipping
  - `compute_ecc_shards` uses one global `shard_size` = largest on-disk block and pads each
    stripe to 10 data shards, storing 2 full-size parity shards per stripe. With CDC bounds
    (min 256 KiB / avg 1 MiB) real vaults have few large chunks, so overhead is FAR above the
    nominal 20%. Measured: 300 KB single-chunk vault → ~600 KB parity (≈200%, 902 KB file).
  - Options: per-stripe shard_size (max within the stripe), or split large chunks into N
    sub-shards so RS(N, parity) gives parity/N overhead. Changes the on-disk payload format
    (bump `ECC_PAYLOAD_VERSION`); still pre-release so no migration needed.
  - BLOCKS: marking ECC as production-ready / any release note claiming efficient redundancy.

---

## Phase 3 — User Surfaces & Polish (GUI + CLI)

**HANDOFF NOTE:** Core requested surfaces + full CLI done and committed in handoff (see done.md HANDOFF entry + todo HANDOFF block above). Implemented exactly: P3-01 (create toggle), P3-02 (browse buttons + badge + modals), P3-04 (CLI scrub/repair/info/create --ecc + profile usage). Remaining are polish only.

- [x] **P3-01** GUI create dialog: add ECC / redundancy option (under Experimental or a new "Reliability" section). Wire to the new create path.  → DONE in handoff (VaultCreate.tsx experimental toggle + state.eccEnabled + conditional create path).
- [x] **P3-02** GUI vault browser / properties: show "ECC: Reed-Solomon (2 parity)" badge + "Last scrubbed" + "Run scrub / Repair" actions. → DONE in handoff (VaultBrowse: conditional amber/rose buttons, badge, draggable modals with lists + dry-run + Open Repair; useDraggableModal + full dark: template match).
- [ ] **P3-03** Enhance the technical receipt (VaultReport) with ECC fields (shards_generated, bytes_protected, repair_events, etc.).
- [x] **P3-04** CLI parity: create --profile ... --ecc ; info --json has_ecc ; repair/scrub commands. → DONE (full P2-08 + handoff finalize; all via --profile).
- [ ] **P3-05** Add i18n keys for new strings (follow existing vault telemetry pattern).
- [ ] **P3-06** Update the "vault" help text and man-page style output in the CLI.

---

## Phase 4 — Tests, Hardening, Documentation, Release

- [ ] **P4-01** Extend the damage test helpers (`flip_byte_in_file` etc.) to target ciphertext blocks and the extension payload area.
- [ ] **P4-02** Property / round-trip tests:
  - Create with ECC → inject N bit flips in different blocks → scrub detects → repair succeeds → extract matches original.
  - Same after delete + compact (live chunks change).
- [ ] **P4-03** Compatibility matrix tests:
  - Old v3 binary (simulated by using only the v3 code paths) can open + extract from a v4+ECC file (non-critical ext).
  - v4 reader can open a pure historical v3 file.
- [ ] **P4-04** Performance / large vault notes (document in the appendix or a new PERF note).
- [ ] **P4-05** Security delta review (even lightweight) + update SECURITY.md and the appendix.
- [ ] **P4-06** Update public docs:
  - ROADMAP.md (move from In Flight → Just Shipped when ready)
  - SECURITY.md (table of vault formats)
  - AEROVAULT-V3-SPEC.md (add a small "v4 evolution" note)
  - CLI-GUIDE.md and any provider/vault docs
- [ ] **P4-07** Add entry to CHANGELOG.md under the next version (with credit to Ehud + this appendix).
- [ ] **P4-08** Close the T-AEROVAULT-ECC item (update any tracking in issue #162 if accessible).

---

## Blocked / Needs Input

- All of Phase 2 is blocked on crate choice + payload format decision.
- Any change that would make a v4 file unreadable by current v3 open path is **forbidden** until we have a migration story (we don't want one).

---

## How to Pick Up Work

1. Read `AEROVAULT-V4-ECC.md` (the contract and current state analysis).
2. Read this file, pick the next non-blocked item with the lowest phase number.
3. Run `cargo test --lib aerovault_v3` (or the specific test module) before touching code.
4. When an item is done, move it (with date + one-line result) to `done.md` and update this list.

Keep the design doc and these two files in sync with reality.
