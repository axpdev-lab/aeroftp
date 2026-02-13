# Security Evidence — vX.Y.Z

> Public release security evidence pack for AeroFTP.
> Tracks security claims, applied fixes, verification status, and acceptance gates.
>
> Status: Draft
> Date: YYYY-MM-DD
> Owner: TBD
> Reviewers: TBD

Template usage:
- Replace every empty field before sign-off.
- Use permanent links (PR, commit SHA, CI run, artifact URLs).
- If a section is not applicable, write `N/A` with reason (never leave blank).
- Keep evidence reproducible: another reviewer must be able to validate each claim.

---

## 1) Release Metadata

- Version: vX.Y.Z
- Previous version: vX.Y.Z-1
- Branch/Tag: TBD
- Commit range: TBD
- Platform scope tested: Linux / Windows / macOS
- Security score claimed in release notes: TBD
- Score label: Estimated / Externally Verified

Minimum completion criteria:
- [ ] Commit range and tag are final
- [ ] Platform test matrix is explicit (what was tested, by whom)
- [ ] Score label matches real validation state (no overclaim)

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
- TBD

Gate status:
- [ ] Pass
- [ ] Fail
- [ ] Deferred (must include owner + due date)

### Gate for 9.5

- [ ] Plugin signature verification (SHA-256) enforced by default
- [ ] Security regression suite enabled in CI
- [ ] Plugin execution tests passing
- [ ] Host-key trust tests passing
- [ ] Credential persistence leak tests passing
- [ ] Terminal denylist tests passing
- [ ] No critical/high regressions for 2 consecutive pre-releases

Evidence links:
- TBD

Gate status:
- [ ] Pass
- [ ] Fail
- [ ] Deferred (must include owner + due date)

### Gate for 10.0

- [ ] External cryptography review completed (AeroVault v2)
- [ ] No unresolved critical/high findings in external report
- [ ] Settings migration to vault completed
- [ ] Legacy plaintext fallback removed across settings layer
- [ ] Per-window/per-context capabilities split shipped
- [ ] Capability split verified on all supported platforms

Evidence links:
- TBD

Gate status:
- [ ] Pass
- [ ] Fail
- [ ] Deferred (must include owner + due date)

---

## 3) Findings Ledger (Current Release)

| ID | Severity | Area | Description | Status | Linked Fix |
|----|----------|------|-------------|--------|------------|
| SEC- | Critical/High/Medium/Low | Backend/Frontend/Tools |  | Open/Fixed/Accepted Risk | PR/Commit |

Severity reference:
- Critical: immediate exploitation / high-impact compromise
- High: realistic exploitation with significant impact
- Medium: constrained impact or requires strong preconditions
- Low: hard to exploit or limited impact

---

## 4) Applied Fixes Summary

| Fix ID | Priority | Description | Files | Verification |
|--------|----------|-------------|-------|--------------|
| SEC-P | P1/P2/P3 |  |  | unit/manual/ci |

Minimum fix evidence per row:
- PR or commit SHA
- Test proof (CI job or manual script/log)
- Rollback note (how to revert safely if needed)

---

## 5) Security Tests and Results

### Automated

| Test Suite | Scope | Result | CI Job | Artifact |
|------------|-------|--------|--------|----------|
| TBD |  | Pass/Fail | TBD | TBD |

### Manual Validation

| Scenario | Expected | Result | Tester | Date |
|----------|----------|--------|--------|------|
| TBD |  | Pass/Fail | TBD | YYYY-MM-DD |

Known limitations:
- TBD

Validation quality gate:
- [ ] At least one automated security regression run linked
- [ ] At least one manual adversarial test per P1/P2 fix
- [ ] Failed tests are documented with decision (fix now / accepted risk)

---

## 6) Regression Watchlist

- [ ] Plugin execution model
- [ ] Host key verification paths
- [ ] Credential storage and migration
- [ ] OAuth token/client secret handling
- [ ] Terminal destructive command filtering
- [ ] Tauri capabilities scope
- [ ] CSP/runtime compatibility (Monaco, xterm, WebGL, workers)

---

## 7) Risk Acceptance (If Any)

| Risk ID | Severity | Reason accepted | Expiry date | Owner |
|---------|----------|-----------------|------------|-------|
| RISK- |  |  | YYYY-MM-DD | TBD |

Notes:
- Any accepted High/Critical risk must include mitigation and expiry.
- Expired accepted risks automatically become release blockers until renewed.

---

## 8) Evidence Index

- Diffs:
  - TBD
- CI runs:
  - TBD
- Test reports:
  - TBD
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
- [ ] Blocked

Blocking reasons (if blocked):
- TBD

Release rule:
- If any Critical finding is Open, decision must be `Blocked`.
- If any High finding is Open, decision can be `Approved with accepted risks` only with explicit expiry and owner.

---

## 10) Post-release Follow-up

- [ ] 24h monitoring completed
- [ ] 7-day regression check completed
- [ ] New findings triaged into roadmap

Follow-up issues:
- TBD

Closure criteria:
- [ ] Follow-up issues created and linked
- [ ] Target release assigned to each follow-up
- [ ] Ownership confirmed for each unresolved item
