# Security Evidence — v2.2.3

> Public release security evidence pack for AeroFTP.
> Tracks security claims, applied fixes, verification status, and acceptance gates.
>
> Status: Complete
> Date: 2026-02-17
> Owner: AeroFTP Development Team
> Reviewers: Claude Opus 4.6 (automated audit)

---

## 1) Release Metadata

- Version: v2.2.3
- Previous version: v2.2.2
- Branch/Tag: `main` / `v2.2.3`
- Commit range: v2.2.2..v2.2.3
- Platform scope tested: Linux (Ubuntu 24.04, WebKitGTK)
- Security score claimed in release notes: N/A (incremental release)
- Score label: Estimated

Minimum completion criteria:
- [x] Commit range and tag are final
- [x] Platform test matrix is explicit (Linux primary, CI builds all platforms)
- [x] Score label matches real validation state (no overclaim)

---

## 2) Security-Relevant Changes

### 2.1 — `shell_execute` Backend Tool (New)

**Change**: Replaced frontend-only `terminal_execute` (which dispatched commands to the PTY without capturing output) with a new `shell_execute` Rust backend tool that executes commands via `std::process::Command` and returns structured `stdout`, `stderr`, and `exit_code`.

**Security controls applied**:
- 30-second timeout via `tokio::time::timeout` — prevents runaway processes
- 1MB output limit — prevents memory exhaustion from large command output
- Danger level: `high` — requires explicit user approval before execution
- Existing destructive command denylist inherited from `terminal_execute`
- Path validation via `validate_path()` for working directory argument
- Tool whitelist updated from 44 to 45 entries in `ai_tools.rs`

**Risk assessment**: Medium — shell execution is inherently powerful but gated behind user approval (danger: high) and bounded by timeout + output limits.

### 2.2 — i18n Error Path Fix

**Change**: Fixed 9 incorrect i18n key paths in `formatProviderError()` (e.g. `ai.errors.network` → `ai.providerErrors.network`). These caused error messages to display raw key strings instead of translated text.

**Security impact**: Low — UX issue only. Error messages were still displayed, just not localized. No information leakage or security degradation.

### 2.3 — i18n Structural Audit

**Change**: Removed 1188 leaked/duplicate keys across 46 locale files. Rewrote validation script with 8 integrity checks.

**Security impact**: None — data cleanup only. No functional changes to application behavior.

### 2.4 — Welcome Screen Redesign

**Change**: Replaced emoji-based empty state in AIChat with Lucide icons, 3x3 capability grid, API key setup banner, and clickable quick prompts.

**Security impact**: None — UI-only change. Quick prompts inject text into the chat input (not executed automatically). API key banner opens existing settings dialog.

---

## 3) Findings Ledger (Current Release)

| ID | Severity | Area | Description | Status | Linked Fix |
|----|----------|------|-------------|--------|------------|
| SE-223-01 | Low | Tools | `shell_execute` output not HTML-escaped before display | Fixed | XSS pipeline applies `escapeHtml()` to all tool results before rendering |
| SE-223-02 | Info | i18n | 1188 leaked keys in locale files | Fixed | Structural audit + cleanup script |

---

## 4) Applied Fixes Summary

| Fix ID | Priority | Description | Files | Verification |
|--------|----------|-------------|-------|--------------|
| SE-223-01 | P3 | Shell output goes through existing XSS pipeline | `aiChatUtils.ts` | Manual — verified `escapeHtml` applied to all tool results |
| SE-223-02 | P3 | Locale file structural cleanup | 46 locale files | `npm run i18n:validate` — 47/47 at 100% |

---

## 5) Security Tests and Results

### Automated

| Test Suite | Scope | Result | CI Job | Artifact |
|------------|-------|--------|--------|----------|
| `npm run build` | TypeScript compilation | Pass | Local | Zero errors |
| `npm run i18n:validate` | i18n key completeness | Pass | Local | 47/47 languages at 100% |
| `cargo check` | Rust compilation | Pass | Local | Zero errors/warnings |

### Manual Validation

| Scenario | Expected | Result | Tester | Date |
|----------|----------|--------|--------|------|
| shell_execute with benign command (`ls`) | stdout captured, displayed | Pass | Dev | 2026-02-17 |
| shell_execute requires approval | Tool approval dialog shown | Pass | Dev | 2026-02-17 |
| Welcome screen without API key | Setup banner shown | Pass | Dev | 2026-02-17 |
| Welcome screen with API key | No setup banner | Pass | Dev | 2026-02-17 |
| Quick prompt click | Text injected into input | Pass | Dev | 2026-02-17 |

Known limitations:
- `shell_execute` inherits the Tauri process environment — no sandboxing beyond timeout and output limits
- No automated security regression suite yet (planned for v2.3.0 CSP Phase 2)

---

## 6) Regression Watchlist

- [x] Plugin execution model — unchanged
- [x] Host key verification paths — unchanged
- [x] Credential storage and migration — unchanged
- [x] OAuth token/client secret handling — unchanged
- [x] Terminal destructive command filtering — inherited by shell_execute
- [x] Tauri capabilities scope — unchanged
- [x] CSP/runtime compatibility — unchanged
- [x] AI tool whitelist — updated from 44 to 45 (shell_execute added)

---

## 7) Risk Acceptance (If Any)

| Risk ID | Severity | Reason accepted | Expiry date | Owner |
|---------|----------|-----------------|------------|-------|
| RISK-223-01 | Low | `shell_execute` runs in Tauri process context (no sandbox) — acceptable given danger:high approval gate and 30s timeout | v2.4.0 | Dev Team |

---

## 8) Evidence Index

- Diffs:
  - `ai_tools.rs` — shell_execute implementation + tool whitelist update
  - `AIChat.tsx` — welcome screen redesign
  - `aiChatUtils.ts` — formatProviderError i18n path fix
  - 46 locale files — structural cleanup + 29 new keys
- CI runs:
  - GitHub Actions triggered on tag push (builds Linux/Windows/macOS)
- Test reports:
  - `npm run build` — zero errors
  - `npm run i18n:validate` — 47/47 at 100%

---

## 9) Security Sign-off

- Engineering owner sign-off: AeroFTP Dev Team
- Security reviewer sign-off: Claude Opus 4.6 (automated)
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
- CSP Phase 2 (v2.3.0) — add security regression suite to CI
- shell_execute sandbox hardening — evaluate seccomp/AppArmor for subprocess isolation
