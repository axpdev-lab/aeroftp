---
description: Batch-translate new i18n keys across all 47 languages using the GLM agent method with merge script
user_invocable: true
---

# AeroFTP i18n Batch Translation Skill

Automates translation of new i18n keys across 47 languages using parallel agents and a merge script.

Always respond in **Italian**.

---

## Prerequisites

The user must provide:
- **New keys** to translate (or point to `en.json` additions)
- Whether Italian (`it.json`) needs manual review

If unclear, read `src/i18n/locales/en.json` and diff against git to find new keys.

---

## Phase 1 — Identify New Keys

1. Run `git diff HEAD src/i18n/locales/en.json` to find newly added keys
2. Extract the key paths and English values
3. Show the user the list and ask for confirmation

---

## Phase 2 — Prepare Batch Files

Create `/tmp/i18n-batch/` directory. For each language, agents write standalone JSON files.

**Critical rules:**
- `en.json` keys are at ROOT level (e.g. `protocol.zohoworkdriveDesc`)
- All other 46 locales wrap keys under `translations.*` (e.g. `{ "translations": { "protocol": { "zohoworkdriveDesc": "..." } } }`)
- Agents write ONLY standalone files — they do NOT read/modify original locale files
- Use **3 batches of agents** (15+15+14 languages) to avoid context overflow
- Armenian (`hy.json`): use Unicode escape sequences in agent prompts

### Language Batches

**Batch 1** (15): bg, cs, da, de, el, es, et, fi, fr, hi, hr, hu, hy, id, it
**Batch 2** (15): ja, ka, kk, ko, lt, lv, mk, ms, nl, no, pl, pt, ro, ru, sk
**Batch 3** (14): sl, sq, sr, sv, th, tr, uk, vi, zh, af, bn, fil, sw, ta

---

## Phase 3 — Launch Translation Agents

For each batch, launch agents that:
1. Receive the English keys + values
2. Translate to their assigned language
3. Write to `/tmp/i18n-batch/{locale}.json`
4. Wrap in `{ "translations": { ... } }` (except en.json)

**Aero Family terms are NEVER translated**: AeroSync, AeroVault, AeroPlayer, AeroAgent, AeroTools, AeroFile, AeroFTP

---

## Phase 4 — Merge

Use the merge script `scripts/merge-i18n-batch.cjs` (or create if missing):

```bash
# Test on 1 file first
node scripts/merge-i18n-batch.cjs bg

# Then all
node scripts/merge-i18n-batch.cjs all
```

The script reads `/tmp/i18n-batch/{locale}.json` and deep-merges into `src/i18n/locales/{locale}.json`.

---

## Phase 5 — Validate

```bash
npm run i18n:validate
```

Spot-check Armenian:
```bash
node -e "const j=require('./src/i18n/locales/hy.json'); console.log(JSON.stringify(j.translations).substring(0,200))"
```

---

## Phase 6 — Report

Show summary:
- Number of keys translated
- Number of languages
- Any validation errors
- Armenian decode check result

Do NOT commit — let the user decide.
