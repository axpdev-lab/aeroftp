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

## Current Session Focus (as of creation)

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

- [ ] **P1-07** (stretch) Make the CLI `vault create` accept a new flag `--ecc` (or `--redundancy ecc`) and pass it through to the create function
  - CODE: the vault subcommand parser + the call site at ~42661

---

## Phase 2 — Real Reed-Solomon Layer

- [ ] **P2-01** Choose + add RS crate to `src-tauri/Cargo.toml` + write justification (see D-02)
  - Update `audit.toml` if the project still uses one (historical pattern)

- [ ] **P2-02** Define the on-disk payload format for the ECC extension
  - Small header (shard count, data_shards, parity_shards, shard_size, hash of the shard table, etc.)
  - Then the raw shards (or striped)
  - Keep it simple and append-only friendly

- [ ] **P2-03** Implement `compute_ecc_shards(data_blocks: &[&[u8]], cipher_hashes: &[String], config) -> Vec<u8>` (the payload bytes)
  - The input "blocks" are the on-disk ciphertext blocks (the things that have a preceding u64 len and a recorded cipher_hash)

- [ ] **P2-04** Implement the inverse: given a payload + a list of damaged ranges (identified by failed cipher_hash checks), attempt reconstruction of the damaged ciphertext bytes

- [ ] **P2-05** Wire the encode path into `save_open_vault` / `build_file_bytes` when the vault was opened/created with ECC enabled
  - Recompute on every seal (document the cost)

- [ ] **P2-06** Add `scrub` primitive: walk the manifest, for each chunk verify its cipher_hash against the bytes at the recorded location in the data section. Return list of damaged `ChunkRecordV3` + byte ranges

- [ ] **P2-07** Add `repair` primitive that:
  1. Runs scrub
  2. If ECC covers the damage, reconstructs
  3. Patches the data section in memory
  4. Re-seals (new manifest + new ECC payload) atomically
  - Must never persist a partially repaired state

- [ ] **P2-08** Expose new Tauri commands and CLI entry points (non-destructive by default)
  - `vault_v3_scrub(vault_path, password) -> ScrubReport`
  - `vault_v3_repair(vault_path, password, options) -> RepairReport`
  - CLI: `aeroftp-cli vault repair ... --dry-run`

---

## Phase 3 — User Surfaces & Polish (GUI + CLI)

- [ ] **P3-01** GUI create dialog: add ECC / redundancy option (under Experimental or a new "Reliability" section). Wire to the new create path.
- [ ] **P3-02** GUI vault browser / properties: show "ECC: Reed-Solomon (2 parity)" badge + "Last scrubbed" + "Run scrub / Repair" actions.
- [ ] **P3-03** Enhance the technical receipt (VaultReport) with ECC fields (shards_generated, bytes_protected, repair_events, etc.).
- [ ] **P3-04** CLI parity:
  - `aeroftp-cli vault create --profile ... --ecc`
  - `aeroftp-cli vault info --json` (include extensions + ecc status)
  - `aeroftp-cli vault repair <path> [--dry-run] [--force]`
  - `aeroftp-cli vault scrub <path>`
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
