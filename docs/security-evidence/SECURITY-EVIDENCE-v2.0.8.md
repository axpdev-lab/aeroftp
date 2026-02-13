# Security Evidence — v2.0.8

> Public release security evidence pack for AeroFTP.
> Tracks security claims, applied fixes, verification status, and acceptance gates.
>
> Status: Draft (ready for completion)
> Date: 2026-02-12
> Owner: AeroFTP Team
> Reviewers: TBD

Template usage:
- Replace every empty field before sign-off.
- Use permanent links (PR, commit SHA, CI run, artifact URLs).
- If a section is not applicable, write `N/A` with reason (never leave blank).
- Keep evidence reproducible: another reviewer must be able to validate each claim.

---

## 1) Release Metadata

- Version: v2.0.8
- Previous version: v2.0.7
- Branch/Tag: TBD
- Commit range: TBD
- Platform scope tested: Linux / Windows / macOS (TBD final matrix)
- Security score claimed in release notes: 8.5/10
- Score label: Estimated

Minimum completion criteria:
- [ ] Commit range and tag are final
- [ ] Platform test matrix is explicit (what was tested, by whom)
- [x] Score label matches real validation state (no overclaim)

---

## 2) Score Gate Validation

### Gate for 9.0

- [ ] CSP profile re-enabled and validated on dev builds (Linux/Windows/macOS)
- [ ] FTP insecure-cert visual badge implemented
- [ ] FTP insecure-cert first-toggle warning implemented
- [ ] TOFU first-use fingerprint dialog implemented
- [ ] TOFU accept path tested
- [ ] TOFU reject path tested

Evidence links:
- [docs/security-evidence/SECURITY-EVIDENCE-v2.0.8.md](./SECURITY-EVIDENCE-v2.0.8.md) (this gate checklist)

Gate status:
- [ ] Pass
- [ ] Fail
- [x] Deferred (owner: AeroFTP Team, due date: 2026-03-31)

### Gate for 9.5

- [ ] Plugin signature verification (SHA-256) enforced by default
- [ ] Security regression suite enabled in CI
- [ ] Plugin execution tests passing
- [ ] Host-key trust tests passing
- [ ] Credential persistence leak tests passing
- [ ] Terminal denylist tests passing
- [ ] No critical/high regressions for 2 consecutive pre-releases

Evidence links:
- [src-tauri/src/plugins.rs](../../src-tauri/src/plugins.rs)
- [src-tauri/src/providers/sftp.rs](../../src-tauri/src/providers/sftp.rs)
- [src-tauri/src/ssh_shell.rs](../../src-tauri/src/ssh_shell.rs)
- [src/components/DevTools/AIChat.tsx](../../src/components/DevTools/AIChat.tsx)

Gate status:
- [ ] Pass
- [ ] Fail
- [x] Deferred (owner: AeroFTP Team, due date: 2026-04-30)

### Gate for 10.0

- [ ] External cryptography review completed (AeroVault v2)
- [ ] No unresolved critical/high findings in external report
- [ ] Settings migration to vault completed
- [ ] Legacy plaintext fallback removed across settings layer
- [ ] Per-window/per-context capabilities split shipped
- [ ] Capability split verified on all supported platforms

Evidence links:
- N/A (external review and capability split not yet executed in v2.0.8 scope)

Gate status:
- [ ] Pass
- [ ] Fail
- [x] Deferred (owner: AeroFTP Team, due date: 2026-06-30)

---

## 3) Findings Ledger (Current Release)

| ID | Severity | Area | Description | Status | Linked Fix |
|----|----------|------|-------------|--------|------------|
| SEC-P1-01 | Critical | Plugin System | Plugin shell injection surface via shell interpreter | Fixed | v2.0.8 hardening |
| SEC-P1-02 | High | SFTP/SSH | TOFU unknown error branch accepted connections | Fixed | v2.0.8 hardening |
| SEC-P1-03 | High | OAuth Credentials | localStorage fallback remained active after migration | Fixed | v2.0.8 hardening |
| SEC-P2-01 | Medium | DevTools Terminal | Missing destructive command filter for terminal_execute | Fixed | v2.0.8 hardening |

Severity reference:
- Critical: immediate exploitation / high-impact compromise
- High: realistic exploitation with significant impact
- Medium: constrained impact or requires strong preconditions
- Low: hard to exploit or limited impact

---

## 4) Applied Fixes Summary

| Fix ID | Priority | Description | Files | PR/Commit | Verification |
|--------|----------|-------------|-------|-----------|--------------|
| SEC-P1-01 | P1 | Removed shell interpreter in plugin execution; direct argv execution; metacharacter blocking; traversal blocking | src-tauri/src/plugins.rs | TBD | Code review + targeted manual tests |
| SEC-P1-02 | P1 | Changed unknown known_hosts verification errors to fail-closed (reject) | src-tauri/src/providers/sftp.rs, src-tauri/src/ssh_shell.rs | TBD | Code review + connection-path tests |
| SEC-P1-03 | P1 | Hard-cut OAuth plaintext fallback paths; vault-only credential resolution in active flows | src/App.tsx, src/components/SettingsPanel.tsx, src/components/OAuthConnect.tsx, src/components/SavedServers.tsx | TBD | Code review + migration flow checks |
| SEC-P2-01 | P2 | Added terminal_execute denylist for destructive commands | src/components/DevTools/AIChat.tsx | TBD | Code review + command-pattern tests |

Notes:
- Current release target score progression: 7.5 → 8.5 (estimated).

Minimum fix evidence per row:
- PR or commit SHA
- Test proof (CI job or manual script/log)
- Rollback note (how to revert safely if needed)

---

## 5) Security Tests and Results

### Automated

| Test Suite | Scope | Result | CI Job | Artifact |
|------------|-------|--------|--------|----------|
| TBD | Plugin execution hardening | Pending | TBD | TBD |
| TBD | Host key trust paths (SFTP/SSH) | Pending | TBD | TBD |
| TBD | OAuth credential storage regression | Pending | TBD | TBD |
| TBD | Terminal denylist regression | Pending | TBD | TBD |

### Manual Validation

| Scenario | Expected | Result | Tester | Date |
|----------|----------|--------|--------|------|
| Plugin with shell metacharacters | Rejected before execution | Pass (TBD formal log) | Team | 2026-02-12 |
| SFTP/SSH known_hosts unknown error | Connection rejected | Pass (TBD formal log) | Team | 2026-02-12 |
| OAuth read path post-migration | Reads from vault path | Pass (TBD formal log) | Team | 2026-02-12 |
| terminal_execute destructive command | Blocked with error | Pass (TBD formal log) | Team | 2026-02-12 |

Known limitations:
- CI evidence links not attached yet.
- External verification not yet completed.

Validation quality gate:
- [ ] At least one automated security regression run linked
- [x] At least one manual adversarial test per P1/P2 fix
- [x] Failed tests are documented with decision (fix now / accepted risk)

---

## 6) Regression Watchlist

- [x] Plugin execution model hardened
- [x] Host key verification error paths hardened
- [x] OAuth localStorage fallback removed from active paths
- [x] Terminal destructive command filtering added
- [ ] Tauri capabilities scope reduction
- [ ] CSP/runtime compatibility hardening completion
- [ ] Full security regression suite in CI

---

## 7) Risk Acceptance (If Any)

| Risk ID | Severity | Reason accepted | Expiry date | Owner |
|---------|----------|-----------------|------------|-------|
| RISK-CSP-001 | Medium | Temporary CSP trade-off pending v2.1.0 compatibility hardening | 2026-06-30 | AeroFTP Team |
| RISK-FS-001 | Medium | Broad filesystem scope required by file-manager architecture; further granularity planned | 2026-09-30 | AeroFTP Team |

Notes:
- Any reopened High/Critical risk must trigger score downgrade until fixed.
- Any accepted High/Critical risk must include mitigation and expiry.
- Expired accepted risks automatically become release blockers until renewed.

---

## 8) Evidence Index

- Diffs:
  - src-tauri/src/plugins.rs
  - src-tauri/src/providers/sftp.rs
  - src-tauri/src/ssh_shell.rs
  - src/App.tsx
  - src/components/SettingsPanel.tsx
  - src/components/OAuthConnect.tsx
  - src/components/SavedServers.tsx
  - src/components/DevTools/AIChat.tsx
- CI runs:
  - TBD
- Test reports:
  - Manual validation log: TBD (to be attached before sign-off)
- External audits:
  - TBD
- Screenshots/recordings:
  - TBD

---

## 9) Security Sign-off

- Engineering owner sign-off: TBD
- Security reviewer sign-off: TBD
- Release manager sign-off: TBD

Decision:
- [ ] Approved for release
- [ ] Approved with accepted risks
- [x] Blocked

Blocking reasons (if blocked):
- Gate 9.0 not passed (CSP + FTP cert UX + TOFU first-use dialog pending).
- Security regression CI evidence not yet attached.
- Sign-off fields are incomplete.

Release rule:
- If any Critical finding is Open, decision must be `Blocked`.
- If any High finding is Open, decision can be `Approved with accepted risks` only with explicit expiry and owner.

---

## 10) Post-release Follow-up

- [ ] 24h monitoring completed
- [ ] 7-day regression check completed
- [ ] New findings triaged into roadmap

Follow-up issues:
- SEC-P1-04 (CSP hardening)
- SEC-P1-05 (FTP cert UX warning/badge)
- SEC-P1-06 (TOFU first-use fingerprint dialog)

Closure criteria:
- [ ] Follow-up issues created and linked
- [ ] Target release assigned to each follow-up
- [ ] Ownership confirmed for each unresolved item
