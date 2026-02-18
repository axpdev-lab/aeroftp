# Security Evidence — v2.3.0

> Public release security evidence pack for AeroFTP.
> Tracks security claims, applied fixes, verification status, and acceptance gates.
>
> Status: Complete
> Date: 2026-02-19
> Owner: AeroFTP Development Team
> Reviewers: 4x Claude Opus 4.6 + GPT-5.3 Codex (automated audits)

---

## 1) Release Metadata

- Version: v2.3.0
- Previous version: v2.2.4
- Branch/Tag: `main` / `v2.3.0`
- Commit range: v2.2.4..v2.3.0
- Platform scope tested: Linux (Ubuntu 24.04, WebKitGTK)
- Security score claimed in release notes: N/A
- Score label: Estimated

Minimum completion criteria:
- [x] Commit range and tag are final
- [x] Platform test matrix is explicit (Linux primary, CI builds all platforms)
- [x] Score label matches real validation state (no overclaim)

---

## 2) Security-Relevant Changes

### 2.1 — Chat History SQLite Backend (New)

**Change**: Complete rewrite of chat history persistence from JSON flat-file to SQLite WAL mode with FTS5 full-text search. New `chat_history.rs` module with 18 Tauri commands operating on 4 tables (sessions, messages, branches, stats).

**Security controls applied**:
- **SQL injection prevention**: All queries use parameterized `?` placeholders — zero string interpolation in SQL
- **FTS5 query injection**: `sanitize_fts_query()` wraps user input in double quotes, preventing FTS5 operator injection (`AND`, `OR`, `NOT`, `NEAR`)
- **FTS5 XSS prevention**: `sanitize_fts_snippet()` applies HTML entity escaping first, then restores only `<mark>`/`</mark>` tags for highlight rendering
- **WAL mode**: `PRAGMA journal_mode=WAL` for concurrent read safety; single-writer model prevents corruption
- **File permissions**: Database created at `~/.config/aeroftp/chat_history.db` with directory permissions `0700`
- **In-memory fallback**: If SQLite file cannot be opened, falls back to `":memory:"` database — zero crash, zero data loss for current session
- **Clear-all command**: Dedicated `chat_history_clear_all` with explicit `DELETE FROM` across all 4 tables (not iterative delete)
- **Retention auto-apply**: `retentionAppliedRef` guard prevents double-cleanup per session; `cleanupHistory(days)` removes old data on mount

**Risk assessment**: Low — SQLite is a well-audited embedded database. All user inputs are parameterized. FTS5 outputs are XSS-sanitized before rendering.

### 2.2 — Chat History Manager UI (New)

**Change**: New `ChatHistoryManager.tsx` component with retention policies (7/30/90/180/365 days or unlimited), full-text search with highlighted snippets, session browser, and statistics dashboard.

**Security controls applied**:
- FTS5 search results rendered via `dangerouslySetInnerHTML` only after `sanitize_fts_snippet()` processing on the Rust side
- Retention enforcement runs on component mount, not on a timer (no race conditions)
- Statistics display (DB size, total tokens, total cost) read from aggregated queries — no raw data exposure

**Risk assessment**: Low — UI component with server-side sanitization of all dynamic content.

### 2.3 — Export/Import (Modified)

**Change**: Chat history export now exports from SQLite via `chat_history_export` command (JSON format). Import via `chat_history_import` validates structure before insertion.

**Security controls applied**:
- Export produces self-contained JSON with session metadata + messages
- Import validates expected fields before SQL insertion
- File I/O uses Tauri plugin-fs with existing scope restrictions

**Risk assessment**: Low — structured data exchange with validation.

---

## 3) Findings Ledger (Current Release)

55+ findings resolved across 5 independent auditors. Key findings by category:

| ID | Severity | Area | Description | Status | Linked Fix |
|----|----------|------|-------------|--------|------------|
| A1-SQL-01 | High | SQL | String interpolation in SQL queries | Fixed | All queries converted to parameterized `?` placeholders |
| A1-XSS-01 | High | XSS | FTS5 snippet output contains unsanitized HTML | Fixed | `sanitize_fts_snippet()` — escape HTML then restore `<mark>` |
| A1-FTS-01 | Medium | FTS5 | User input can inject FTS5 operators | Fixed | `sanitize_fts_query()` wraps input in double quotes |
| A2-WAL-01 | Medium | DB | Journal mode not set — default rollback journal | Fixed | `PRAGMA journal_mode=WAL` on connection open |
| A2-FALL-01 | Medium | DB | SQLite open failure causes panic | Fixed | In-memory fallback with `":memory:"` URI |
| A3-RET-01 | Medium | Logic | Retention policy not auto-applied on startup | Fixed | `retentionAppliedRef` + `useEffect` cleanup on mount |
| A3-CLR-01 | Medium | Logic | Clear-all iterates sessions (slow, partial failure) | Fixed | Dedicated `chat_history_clear_all` with direct `DELETE FROM` |
| A4-PERM-01 | Low | FS | DB file created without explicit permissions | Fixed | Directory `0700` via existing config dir creation |
| GPT-F2 | Medium | Logic | Retention auto-apply missing — stale data persists | Fixed | `retentionAppliedRef` guard + `cleanupHistory()` call |
| GPT-F4 | Medium | Logic | No dedicated clear-all command | Fixed | New `chat_history_clear_all` Rust command |

---

## 4) Applied Fixes Summary

| Fix ID | Priority | Description | Files | Verification |
|--------|----------|-------------|-------|--------------|
| SQL-PARAM | P1 | Parameterized all SQL queries | `chat_history.rs` | Code review — zero string interpolation in SQL |
| FTS-XSS | P1 | XSS-safe FTS5 snippet rendering | `chat_history.rs` | Manual — `<script>` tags escaped, `<mark>` preserved |
| FTS-INJECT | P2 | FTS5 query injection prevention | `chat_history.rs` | Manual — operators in quotes rendered literal |
| WAL-MODE | P2 | WAL journal mode on connect | `chat_history.rs` | `PRAGMA journal_mode` returns "wal" |
| MEM-FALLBACK | P2 | In-memory fallback on open failure | `chat_history.rs` | Simulated permission denial — app continues |
| RET-APPLY | P2 | Retention auto-apply on mount | `AIChat.tsx` | Set 7-day retention, reload — old sessions removed |
| CLR-ALL | P2 | Dedicated clear-all command | `chat_history.rs` | Clear all — 4 tables emptied, DB size reduced |
| I18N-KEYS | P3 | 24 new keys in 47 languages | 47 locale files | `npm run i18n:validate` — 47/47 at 100% |

---

## 5) Security Tests and Results

### Automated

| Test Suite | Scope | Result | CI Job | Artifact |
|------------|-------|--------|--------|----------|
| `npm run build` | TypeScript compilation | Pass | Local | Zero errors |
| `npm run i18n:validate` | i18n key completeness | Pass | Local | 47/47 languages at 100% |
| `cargo check` | Rust compilation | Pass | Local | Zero errors/warnings |
| `npm run security:regression` | Security regression (5 checks) | Pass | Local | All checks green |

### Manual Validation

| Scenario | Expected | Result | Tester | Date |
|----------|----------|--------|--------|------|
| Save new chat session | Session persisted in SQLite | Pass | Dev | 2026-02-19 |
| Full-text search with `<script>` input | Script tags escaped, no XSS | Pass | Dev | 2026-02-19 |
| FTS5 search with `AND OR NOT` operators | Treated as literal text, no injection | Pass | Dev | 2026-02-19 |
| Set 7-day retention, reload app | Sessions >7 days removed | Pass | Dev | 2026-02-19 |
| Clear All chat history | All 4 tables emptied | Pass | Dev | 2026-02-19 |
| Delete SQLite file, restart app | In-memory fallback, no crash | Pass | Dev | 2026-02-19 |
| Export/import chat history | Round-trip preserves data | Pass | Dev | 2026-02-19 |
| Chat History Manager UI opens | Stats, search, sessions displayed | Pass | Dev | 2026-02-19 |

Known limitations:
- SQLite database is not encrypted at rest (covered by config directory 0700 permissions and optional master password vault)
- FTS5 index may contain sensitive chat content — same protection as the main database

---

## 6) Regression Watchlist

- [x] AI tool whitelist — unchanged (45 tools + clipboard_read_image)
- [x] Plugin execution model — unchanged
- [x] Credential storage and migration — unchanged
- [x] OAuth token handling — unchanged
- [x] Shell execute security controls — unchanged
- [x] CSP configuration — unchanged
- [x] Tauri capabilities scope — unchanged
- [x] TOTP 2FA for vault — unchanged
- [x] Remote vault security — unchanged
- [x] Path validation in AI tools — unchanged

---

## 7) Risk Acceptance (If Any)

| Risk ID | Severity | Reason accepted | Expiry date | Owner |
|---------|----------|-----------------|------------|-------|
| RISK-230-01 | Low | Chat history SQLite not encrypted at rest — acceptable given 0700 dir permissions and optional master password vault | v2.5.0 | Dev Team |
| RISK-230-02 | Info | FTS5 index stores tokenized chat content — same protection level as main DB, covered by RISK-230-01 | v2.5.0 | Dev Team |

---

## 8) Evidence Index

- Diffs:
  - `chat_history.rs` — Full SQLite backend with 18 commands, FTS5, WAL, in-memory fallback
  - `AIChat.tsx` — Integration hooks, retention auto-apply, ChatHistoryManager modal trigger
  - `ChatHistoryManager.tsx` — Manager UI with search, retention, stats
  - `lib.rs` — 18 new Tauri command registrations
  - 47 locale files — 24 new i18n keys (chatHistory.*)
- Audit reports:
  - Security audit (Claude Opus 4.6 agent #1) — SQL injection, XSS, path validation
  - Correctness audit (Claude Opus 4.6 agent #2) — Logic errors, edge cases, data integrity
  - Performance audit (Claude Opus 4.6 agent #3) — WAL mode, index optimization, connection pooling
  - Frontend/UX audit (Claude Opus 4.6 agent #4) — i18n, accessibility, theme compatibility
  - Counter-audit (GPT-5.3 Codex) — F2 retention auto-apply, F4 clear-all command
- CI runs:
  - GitHub Actions triggered on tag push (builds Linux/Windows/macOS)
- Test reports:
  - `npm run build` — zero errors
  - `npm run i18n:validate` — 47/47 at 100%
  - `cargo check` — zero errors
  - `npm run security:regression` — all checks pass

---

## 9) Security Sign-off

- Engineering owner sign-off: AeroFTP Dev Team
- Security reviewer sign-off: 4x Claude Opus 4.6 + GPT-5.3 Codex (automated)
- Release manager sign-off: AeroFTP Dev Team

Decision:
- [x] Approved for release
- [ ] Approved with accepted risks
- [ ] Blocked

---

## 10) Post-release Follow-up

- [ ] 24h monitoring completed
- [ ] 7-day regression check completed
- [ ] New findings triaged into roadmap

Follow-up issues:
- Optional SQLite encryption at rest (v2.5.0) — evaluate SQLCipher or application-level encryption
- CSP Phase 2 tightening (v2.4.0) — replace wildcard sources with specific origins
