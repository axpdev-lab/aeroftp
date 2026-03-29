---
description: Launch a multi-agent security audit on recent code changes — parallel reviewers with consolidated findings report
user_invocable: true
---

# AeroFTP Security Audit Skill

Launches parallel security review agents on recent code changes, consolidates findings, and fixes critical/high issues.

Always respond in **Italian**.

---

## Input

The user may specify:
- **Scope**: specific files, a feature area, or "all recent changes"
- **Depth**: quick (1 agent), standard (3 agents), thorough (5 agents)

Default: standard (3 agents) on `git diff main...HEAD` or last commit.

---

## Phase 1 — Identify Scope

1. Run `git diff --name-only HEAD~1` (or `main...HEAD` if on branch)
2. Filter to Rust (`.rs`) and TypeScript (`.ts`, `.tsx`) files
3. Show file list and ask for confirmation

---

## Phase 2 — Launch Audit Agents

Launch agents in parallel, each with a different focus:

### Agent 1 — Input Validation & Injection
- SQL injection, command injection, path traversal
- Unsanitized user input reaching dangerous APIs
- Missing `validate_relative_path()` on file operations

### Agent 2 — Authentication & Secrets
- Credentials in logs or error messages
- Missing `SecretString` wrapping
- OAuth token handling, key exposure
- TOCTOU race conditions

### Agent 3 — Memory Safety & Resource
- Buffer overflows, OOM vectors
- Missing bounds checks, unbounded allocations
- File handle leaks, mutex poisoning
- Timeout enforcement

### Agent 4 (thorough only) — Frontend Security
- XSS via `dangerouslySetInnerHTML` or unsanitized markdown
- CSP violations
- Prototype pollution
- Sensitive data in localStorage

### Agent 5 (thorough only) — Protocol & Network
- TLS validation, certificate pinning
- SSRF vectors
- Insecure defaults
- Error message information leakage

Each agent reads the changed files and reports findings in this format:

```
## Finding ID: AUDIT-XXX
- **Severity**: CRITICAL / HIGH / MEDIUM / LOW / INFO
- **File**: path/to/file.rs:123
- **Description**: What the issue is
- **Impact**: What could go wrong
- **Fix**: Suggested remediation
```

---

## Phase 3 — Consolidate

Merge all agent findings into a single report:
1. Deduplicate (same file + same issue = 1 finding)
2. Sort by severity (CRITICAL first)
3. Count by severity level
4. Assign a letter grade: A (0 critical, 0 high), B (0 critical, <=3 high), C (<=1 critical), D (>1 critical)

---

## Phase 4 — Fix Critical & High

For CRITICAL and HIGH findings:
1. Show the finding and proposed fix
2. Ask user confirmation before applying
3. Apply fix and verify it compiles (`cargo check` / `npm run build`)

MEDIUM and below: report only, do not auto-fix.

---

## Phase 5 — Summary

Present final report:

```
Security Audit Report — AeroFTP vX.Y.Z
========================================
Scope: N files (M Rust, K TypeScript)
Agents: 3
Grade: B+

CRITICAL: 0
HIGH: 2 (2 fixed)
MEDIUM: 5
LOW: 8
INFO: 3

All critical and high findings resolved.
```

---

## Rules

- Never auto-fix without user confirmation
- Run `cargo clippy` after Rust fixes
- Run `npm run build` after TypeScript fixes
- Reference OWASP Top 10 where applicable
- Check against project security patterns in CLAUDE.md
