---
description: Run the full pre-push validation suite — cargo clippy, npm build, i18n validate — before pushing to remote
user_invocable: true
---

# AeroFTP Pre-Push Check Skill

Runs all CI-equivalent checks locally before pushing to avoid failed workflows.

Always respond in **Italian**.

---

## Checks (sequential)

### 1. Cargo Clippy (Rust)

```bash
cd src-tauri && cargo clippy --all-targets -- -D warnings
```

This is exactly what CI runs. Must pass with zero warnings.

If it fails:
- Show the error
- Propose a fix
- Apply after user confirmation
- Re-run clippy

### 2. Frontend Build (TypeScript/React)

```bash
npm run build
```

Verifies TypeScript compiles and Vite produces output.

If it fails:
- Show the TypeScript error
- Propose a fix
- Apply after user confirmation
- Re-run build

### 3. i18n Validation

```bash
npm run i18n:validate
```

Ensures all 47 languages have 100% key coverage.

If it fails:
- Show missing keys
- Run `npm run i18n:sync` to propagate
- Re-validate

---

## Summary

After all checks pass, show:

```
Pre-push validation passed
==========================
  Clippy:    OK (0 warnings)
  Build:     OK
  i18n:      OK (47 languages, 100%)

Safe to push.
```

If any check failed and was fixed, mention what was changed so the user can review before pushing.

---

## Rules

- NEVER push automatically — only validate
- NEVER skip clippy — it catches what CI catches
- If Rust files changed, clippy is mandatory
- Report timing for each check
