# AeroVault v4 ECC — Completed Work (done.md)

> Historical record. Items are moved here from `todo.md` with date, session context, and outcome / commit-ish note.  
> This file exists so future sessions can resume without re-deriving the same understanding.

---

## Session: Decisions Approval + Naming Review from Discussion #276 (current)

**Approvals from user (point-by-point on the 7 open decisions):**
- 1. Module strategy → ok approvo (keep inside aerovault_v3.rs for now)
- 2. RS crate → ok approvo
- 3. critical flag → ok corretto (always false)
- 4. "ecc" in wrappers manifest → ok approvo
- 5. Password for repair → ok chiaro (required)
- 6. Performance on seal → ok concordo (recompute on seal)
- 7. Naming → discussed below (see dedicated update in AEROVAULT-V4-ECC.md)

**Key input for naming (7)**: User pointed to https://github.com/axpdev-lab/aeroftp/discussions/276 ("AeroVault Wrapper-Stack and Cryptography: Design Conversation").

**Analysis of #276 (fetched content)**:
- The thread (with heavy Ehud Kirsh contribution) treats the layers as **first-class wrappers**.
- Explicit pipeline (diagram + text): `compression -> chunking -> crypt -> ECC`.
- Language used: "Each box is a **first-class wrapper**", "Error-correction position", "ECC runs **last** in the pipeline", "ECC algorithm follow-ups now that the v3 pipeline slot is exercised end-to-end".
- ECC is positioned as the fourth wrapper, on-storage, after crypt (to protect stored bytes while preserving confidentiality).
- Already consistent with our existing code phrases ("extension directory for ECC", `ecc.reed-solomon` in the spec example).
- The thread is the canonical design anchor for the wrapper stack (checkpoint of decisions from the big Community Roadmap thread).

**Refined naming decision recorded in AEROVAULT-V4-ECC.md §7**:
- Technical / spec / receipt: "ECC wrapper" or "error-correction wrapper".
- CLI flag: `--ecc`.
- User-facing: "ECC wrapper (Reed-Solomon)" with reference to the pipeline.
- This keeps full continuity with the agreed model in #276.

All other decisions approved. We can now close D-01.

**Next**: Update todo/done, then (after final naming sign-off) start Phase 1 stub implementation.

**Date**: 2026 (start of dedicated work on `feat/aerovault-v4-ecc`)

**Branch created**:
```bash
git checkout -b feat/aerovault-v4-ecc
```
(Confirmed with `git branch --show-current`.)

**Appendix structure created**:
- `docs/dev/roadmap/APPENDIX-AEROVAULT-V4-ECC/`
  - `AEROVAULT-V4-ECC.md` (main design + executive plan + contract)
  - `todo.md` (live tasks, P0/P1/P2/P3 tagged, code pointers)
  - `done.md` (this file)

### Completed Items (moved from initial todo)

- [x] **Branch + working folder**
  - Dedicated feature branch following project convention (`feat/...`).
  - APPENDIX folder under `docs/dev/roadmap/` per the established pattern (see `APPENDIX-MULTI-USER/`).

- [x] **Spec contract capture**
  - Read `docs/AEROVAULT-V3-SPEC.md` (multiple passes: header layout, §6 Extension Directory with the exact `ecc.reed-solomon` JSON example, §9 Backward Compatibility rules, file layout diagram).
  - Verbatim quotes and implications recorded in the design doc.
  - Key sentence preserved: *"v3 + ECC = v4"* and the critical/non-critical rule.

- [x] **Full code audit of v3 foundation (the "we already have this" analysis)**
  - `VaultHeaderV3` already contains all four extension offset/len fields and serializes/deserializes them.
  - `ExtensionEntryV3` struct is a 1:1 match for the spec example.
  - `open_vault`: reads extension dir, deserializes, rejects only on `critical: true`.
  - `build_file_bytes` + `save_open_vault`: correctly places dir after manifest, sets payload_len=0, includes it in the atomic output.
  - `is_vault_v3`: magic + format byte == 3 (still works for future v4 files if we keep the byte at 3).
  - `vault_v3_security_info()` already mentions "extension directory for ECC" and the compatibility note.
  - **cipher_hash** computation (in `append_chunk`) and verification (before any AEAD decrypt in extract) is exactly the hook described in the test comment.

- [x] **Write / seal path mapping (complete)**
  - `append_chunk` → compress → encrypt_with_aad → `blake3::hash(encrypted)` as cipher_hash → append len + ciphertext to flat `data` → insert `ChunkRecordV3`.
  - `append_sources_batched`, `append_file_at`, small-file packing.
  - `compact_live_chunks` on deletes.
  - `save_open_vault` → `build_file_bytes` (re-encrypt manifest every time) → atomic tempfile + fsync + parent dir sync.
  - This flow must be the attachment point for ECC recompute on every seal.

- [x] **Damage / test harness discovery**
  - Existing helpers: `flip_byte_in_file`, `truncate_vault`.
  - Existing tests already exercise cipher_hash mismatch path and expect the exact error message.
  - Small-file batching, dedup, directory ops, password change, pack tests all present.

- [x] **Dispatch surfaces identified (high level)**
  - Tauri: `lib.rs` — `is_vault_v3` guard + version dispatch (3 vs 2), then long list of `aerovault_v3::vault_v3_*` functions registered as commands.
  - CLI: `bin/aeroftp_cli.rs` — direct calls to `aerovault_v3::vault_v3_create`, `vault_v3_add_files`, `vault_v3_open`, `vault_v3_extract_entry` (and the vault subcommand parser).
  - Detailed call-site inventory moved to open task **P0-01**.

- [x] **Crypto / dependency context**
  - Current relevant crates visible in Cargo.toml slice: `blake3` (keyed + regular), `aes-gcm-siv`, `aes-kw`, `hmac`, `sha2`, `rand` (0.8 with 0.10 alias), `secrecy`, `hex`, `serde_json`.
  - Historical pattern for new deps (RSA removal, jsonwebtoken switch to aws-lc-rs, audit.toml entries) is well documented in CHANGELOG and security evidence files. Any RS crate addition will follow the same rigor.

- [x] **Design doc populated**
  - Full "Current State — What v3 Already Gave Us".
  - Recommended architecture (separate `aerovault_v4.rs` module preferred).
  - 5-phase piano esecutivo.
  - Risk register.
  - Explicit "How to Resume This Work in Future Sessions" section.
  - Open decisions list (the 7 questions that must be answered before heavy coding).

- [x] **todo.md + done.md** created with the agreed format (status tags, CODE: / SPEC: pointers, phase numbers, move-to-done discipline).

### Key Insights Captured (for future readers)

1. We are **not** inventing the extension mechanism — it was deliberately left as `[]` + reserved header fields + critical flag by the v3/Ehud design.
2. The `cipher_hash` being computed on the **ciphertext** and checked **before** decrypt is the load-bearing hook for any scrub/repair.
3. Because `save_open_vault` always rewrites the whole file (header + data + manifest + extensions), ECC can be recomputed on every seal without changing the surrounding atomic-write contract.
4. The hardest constraint is **"a pure v3 reader must still be able to open a v4+ECC file and extract data"** (as long as the extension is non-critical). This rules out many tempting shortcuts.

### Tests Baseline at End of Session
- The v3 test suite was not run in this initial design pass (will be first action of next coding session).
- Recommendation for anyone resuming: `cargo test --lib aerovault_v3 -- --nocapture` (or the specific damage/roundtrip tests) before any edit.

---

## Later Sessions (append here when moving items)

**2026 (this session)** — D-01 closed with user approvals (1-6 full, 7 with reference to discussion #276 for wrapper-stack terminology consistency).
- All decisions recorded in AEROVAULT-V4-ECC.md.
- Baseline tests confirmed green (10/10 aerovault_v3).
- CLI + lib.rs call-site inventory completed (see previous session notes).
- Naming refined to treat ECC as "first-class wrapper" per #276 language ("ECC", "error-correction wrapper", pipeline position last after crypt).

**P1-01 Start completed (core engine + CLI wiring)**:
- Implemented stub support for emitting + roundtripping the non-critical `ecc.reed-solomon` extension.
- New `vault_v3_create_with_ecc` Tauri command.
- Explicit handling + dedicated unit test proving v3 compatibility.
- 11/11 aerovault_v3 tests green.
- Changes isolated to `src-tauri/src/aerovault_v3.rs` on `feat/aerovault-v4-ecc`.

**CLI wiring step (next per plan)**:
- Added `--ecc` flag to `VaultCommands::Create` (with doc referencing T-AEROVAULT-ECC and the APPENDIX).
- Handler logic: when creating v3 and `--ecc` is set, dispatch to `vault_v3_create_with_ecc`.
- `cargo check --bin aeroftp-cli` succeeded cleanly.
- This allows `aeroftp-cli vault create --ecc ...` (and with --vault-version v3).

**Important context added (user request)**: Full review of the canonical discussion https://github.com/axpdev-lab/aeroftp/discussions/272 (the permanent [ROADMAP] Wrappers/Overlays thread). This is where `T-AEROVAULT-ECC` was formally proposed by Ehud Kirsh and codified. Key terminology and constraints from there:
- Preferred terms: "Error correction layer", "error-correction", "ECC" (as the name of the 4th first-class wrapper).
- Pipeline: compression → chunking → crypt → **error-correction / ECC**.
- "4 wrappers" framing.
- Forward-compat explicitly called out ("v3 + ECC = v4", v3 not blocked, ECC in v4 track).
- Operational needs (scrub/repair) and UX profiles already anticipated.

Appendix docs (AEROVAULT-V4-ECC.md + this file) now cite #272 as primary source for the feature and terminology. Our current approach (non-critical extension, "ECC wrapper", `--ecc`, stub in v3 engine, recovery before decrypt) is already aligned with the discussion.

Next per plan: P1-04 + P1-05 (enhance ECC visibility + has_ecc helper) — advanced.
- `vault_v3_has_ecc` helper implemented (lightweight, no password required).
- Tauri command registered.
- Previous local commit amended for correct trailer format (Co-Authored-By: ... <email> matching Claude/Codex style).
- P1-04 completed: `vault_v3_security_info(path: Option<String>)` now reports per-vault ECC status using the helper when path is given.
- Enhanced JSON with "ecc" object (enabled, algorithm, version, critical) + updated general fields.
- Matches the plan item: "advertise the ECC layer when present".
- Added + passing unit test `v3_security_info_advertises_ecc_when_present`.
- Interleaved full test runs green.

- P1-06 completed: Added dedicated compatibility test `v3_stub_ecc_vault_readable_by_pure_v3_open_and_extract`.
- Proves v4-stub (ECC extension) vaults remain fully readable/extractable via pure v3 internal paths (`open_vault`, `extract_entry`).
- Test passes cleanly.
- Interleaved with cargo test.

- P1-07 (stretch) completed:
  - --ecc flag fully implemented in parser + handler.
  - Dispatches correctly to with_ecc variant.
  - Added version warning + help text polish.
  - Verified in previous live build/test runs (create --ecc worked, info showed has_ecc).

- Live CLI test executed successfully (repeated for P1-07 verification):
  - Used --ecc explicitly.

**P1 final fix (user-flagged one-liner)**:
- Registered `aerovault_v3::vault_v3_create_with_ecc` in `src-tauri/src/lib.rs` invoke_handler (immediately after `vault_v3_create` at the Tauri command list).
- Required for GUI (Tauri invoke); CLI already calls the lib function directly, so it worked without it.
- `cargo check` clean.

**Phase 2 started (P2-01)**:
- Selected `reed-solomon-erasure = "6"` (pure-Rust, mature, fits our ciphertext-block + cipher_hash model).
- Added to Cargo.toml with justification comment (full D-02 note + audit.toml entry to follow in this phase).
- `cargo check` passes after dep resolution.
- Tracking updated in todo.md.
- All P1 items (including the registration) now closed. Phase 1 complete.

All per plan, step-by-step with tests + live verification. Phase 2 engines lit. 🚀
  - All flows green.

All per plan, step-by-step with tests. Ready for Phase 2 (RS crate).

**P2-02 completed**:
- Defined the on-disk ECC extension payload format.
- `EccPayloadHeader`, `EccStripeHeader`, `EccPayload` with binary to_bytes/from_bytes.
- Stripe-based layout (header + stripe table + concatenated parity data).
- Simple, self-describing, suitable for append (new stripes) and full rewrite on seal.
- Roundtrip test `p2_02_ecc_payload_format_roundtrip` added and passing.
- Format documented in source comments.

**P2-03 completed**:
- Implemented `compute_ecc_shards`.
- Uses the RS crate to produce parity for stripes of on-disk blocks.
- Returns ready-to-store payload bytes.
- Smoke test passes.
- Interleaved with `cargo test`.

**P2-04 completed**:
- Implemented `reconstruct_from_ecc`.
- Correctly recovers damaged data blocks given the ECC payload and list of bad indices.
- Handles the crate's Option-based reconstruction and partial stripes.
- Test with simulated single-block corruption + successful repair passes.
- Interleaved tests green.

**P2-05 completed**:
- Wired `compute_ecc_shards` into the seal path (`save_open_vault`) and `build_file_bytes`.
- On every save for ECC-enabled vaults, parities are recomputed from current on-disk blocks and stored in the extension payload area.
- Initial support in create path too.
- Recompute cost noted (fine for the feature; only happens for --ecc vaults).
- All prior P2 tests continue to pass.

**P2-06 completed**:
- Added `scrub_vault` primitive + `DamagedChunk` struct.
- Verifies every chunk's cipher_hash against the stored ciphertext.
- Returns list with records and exact on-disk byte ranges.
- Test with intentional tamper passes and reports correct range.
- Interleaved with broader module tests (green).

**P2-07 + P2-08 completed (as requested)**:
- Repair primitive `repair_vault` implemented: scrub + reconstruct + patch data + atomic re-seal via save.
- Exposed `vault_v3_scrub` and `vault_v3_repair` as Tauri commands (registered).
- Dry-run support, reports.
- Full CLI commands added: `aeroftp-cli vault scrub <path> [-p pw]`, `aeroftp-cli vault repair <path> [--dry-run] [-p pw]` with text/JSON output.
- Strong e2e + stress tests (multiple damages across stripes, 12+ files, full repair + extract verify + post-scrub clean).
- 18+ tests green.
- Engine (shared by CLI/GUI) is thoroughly validated directly. CLI is now complete for ECC operations; further GUI will be wiring on proven core.

Phase 2 (ECC core + primitives + full CLI exposure) complete. Ready for GUI surfaces or user final live tests with real saved profiles + stress.

Rocket engines lit. Proceeding step-by-step. 🚀

**GUI ECC surfaces (P2) - completed per user request**:
- Draggable modals via useDraggableModal (headers with dragHandleProps, panel with transform), full theme support (dark: everywhere), stick to app template (rounded-xl, borders, p-4, lucide, consistent with AeroSync dialogs/other modals, no new globals).
- VaultCreate: ECC toggle in experimental (Beta) section.
- Browse: buttons, badge, modals (scrub shows list; repair has dry-run + enhanced list from scrubResult).
- State/handlers wired to Tauri commands.
- Typecheck & 19 tests green.
- Live CLI proxy + Rust e2e cover the flows.

Ready for user's real-profile tests. Estetica maestro mode engaged. 🚀

---

## HANDOFF COMMIT (user-requested: "salvare un handhoff e mi dai prompt breve per nuova finestra fresca")

**When:** Immediately after GUI surfaces + P2-08 finalize (stress test + Tauri registrations) + all prior P1/P2/CLI per plan.

**Branch:** `feat/aerovault-v4-ecc`

**Commit (this one):** Includes:
- Rust: `p2_08_cli_stress_multiple_damage_repair` (12-file multi-damage across stripes) + `vault_v3_scrub` / `vault_v3_repair` registrations in lib.rs invoke_handler.
- Docs: this entry + prominent HANDOFF block injected at top of todo.md (with exact resume instructions, status, constraints, "seguiamo le specifiche concordate con ehud kirsh").
- All changes committed together so fresh window starts from clean tree.

**Delivered exactly as tracked + user directives:**
- P1-01 to P1-07 (stub, compat, has_ecc, security_info, --ecc flag, one-liner registration).
- P2-01 to P2-08 (reed-solomon-erasure=6, EccPayload* format, compute_ecc_shards (10+2, global shard, pad), reconstruct fix (Option + trim), wiring into build_file_bytes/save, scrub_vault, repair_vault atomic, Tauri+CLI exposure + stress e2e).
- "completa la CLI con tutti i comandi utili": scrub + repair + info has_ecc + create --ecc done (no extras invented).
- GUI (user explicit "passiamo alla GUI", "modals trascinabili e rispetto temi, atteniamoci al template dell'app"): full surfaces in VaultCreate + VaultBrowse + useVaultState (toggle, conditional amber/rose buttons, badge, two draggable modals with full damage list + dry-run + invoke handlers, theme/dark consistent, no deviation from existing estetica/hooks).
- 19 tests green. Engine (direct) + CLI live exercised. "se tutti passano allora la CLI sarà solo estetica" — passed.
- Strict adherence: AeroVault first; v3 forward-compat (non-critical ecc.reed-solomon); ECC last wrapper (4-wrappers pipeline from #272/#276 Ehud Kirsh); update todo/done every step; commits with correct Co-Authored trailer; --profile only; interleaved tests; no password on CLI; no scope creep.

**Post-handoff next (per conversation record — do not assume):**
1. User performs real "live test con profili reali salvati" + approfonditi stress (using aeroftp-cli --profile + the new scrub/repair).
2. If green: CLI estetica only.
3. Then: remaining P3 items (receipt fields, i18n, help text) or jump to P4 docs/CHANGELOG or close item.
4. Or user directs "next roadmap topic".

**Key files for any resume (read first):**
- AGENTS.md (CLI agent rules, profiles, safety, --json, exit codes)
- docs/dev/roadmap/APPENDIX-AEROVAULT-V4-ECC/AEROVAULT-V4-ECC.md (contract, 4-wrappers, decisions)
- docs/dev/roadmap/APPENDIX-AEROVAULT-V4-ECC/todo.md (HANDOFF section + current focus)
- docs/dev/roadmap/APPENDIX-AEROVAULT-V4-ECC/done.md (this + full history)
- src-tauri/src/aerovault_v3.rs (engine: ~234 ECC consts, compute ~392, reconstruct ~470, scrub ~542, repair ~592, tests bottom)
- src-tauri/src/bin/aeroftp_cli.rs (VaultCommands + handlers ~426xx)
- src-tauri/src/lib.rs (registrations)
- src/components/vault/{useVaultState.ts, VaultCreate.tsx, VaultBrowse.tsx} (GUI state + surfaces + modals)

**Test baseline to run immediately on resume:**
```bash
cd src-tauri && cargo test --lib aerovault_v3 -- --quiet
# expect: 19 passed
```

**Commit trailer used (and to keep using):**
Co-Authored-By: Grok 4.3 released by xAI in April 2026 <noreply@x.ai>

Handoff complete. Fresh window prompt will be provided to user separately. All core work per APPENDIX + user "via libera" steps is captured and clean. 🚀

(End of handoff entry)
