# AeroFTP Internationalization (i18n) Guide

> Last Updated: 26 February 2026
> Version: v2.7.0
> Languages: 47 | Keys: 2860 | Coverage: 100%

---

## Overview

AeroFTP uses a lightweight, custom i18n system built on React Context:

- **Zero dependencies** - No external i18n libraries
- **Type-safe** - Full TypeScript support with autocompletion
- **Browser detection** - Automatically detects user's preferred language
- **Persistence** - Language preference saved to localStorage
- **Fallback** - Falls back to English for missing translations
- **Parameter interpolation** - Supports `{paramName}` syntax
- **All LTR** - All supported languages use left-to-right text direction

---

## Supported Languages (47)

### Original (5)

| Code | Language | Native Name | Status |
|------|----------|-------------|--------|
| `en` | English | English | Base language |
| `it` | Italian | Italiano | Manually translated |
| `es` | Spanish | Espanol | Translated + reviewed |
| `fr` | French | Francais | Translated + reviewed |
| `zh` | Chinese | Simplified Chinese | Translated + native reviewed |

### Major European (12)

| Code | Language | Native Name |
|------|----------|-------------|
| `de` | German | Deutsch |
| `pt` | Portuguese | Portugues |
| `ru` | Russian | Russkij |
| `nl` | Dutch | Nederlands |
| `pl` | Polish | Polski |
| `uk` | Ukrainian | Ukrainska |
| `ro` | Romanian | Romana |
| `cs` | Czech | Cestina |
| `hu` | Hungarian | Magyar |
| `el` | Greek | Ellinika |
| `bg` | Bulgarian | Bulgarski |
| `sk` | Slovak | Slovencina |

### Nordic (5)

| Code | Language | Native Name |
|------|----------|-------------|
| `sv` | Swedish | Svenska |
| `da` | Danish | Dansk |
| `no` | Norwegian | Norsk |
| `fi` | Finnish | Suomi |
| `is` | Icelandic | Islenska |

### Asian (10)

| Code | Language | Native Name |
|------|----------|-------------|
| `ja` | Japanese | Nihongo |
| `ko` | Korean | Hangugeo |
| `vi` | Vietnamese | Tieng Viet |
| `th` | Thai | Thai |
| `id` | Indonesian | Bahasa Indonesia |
| `ms` | Malay | Bahasa Melayu |
| `tl` | Filipino | Tagalog |
| `km` | Khmer | Phasa Khmer |
| `hi` | Hindi | Hindi |
| `bn` | Bengali | Bangla |

### Balkan & Caucasus (6)

| Code | Language | Native Name |
|------|----------|-------------|
| `hr` | Croatian | Hrvatski |
| `sr` | Serbian | Srpski |
| `sl` | Slovenian | Slovenscina |
| `mk` | Macedonian | Makedonski |
| `ka` | Georgian | Kartuli |
| `hy` | Armenian | Hayeren |

### Baltic (3)

| Code | Language | Native Name |
|------|----------|-------------|
| `lt` | Lithuanian | Lietuviu |
| `lv` | Latvian | Latviesu |
| `et` | Estonian | Eesti |

### Celtic & Iberian (4)

| Code | Language | Native Name |
|------|----------|-------------|
| `cy` | Welsh | Cymraeg |
| `gl` | Galician | Galego |
| `ca` | Catalan | Catala |
| `eu` | Basque | Euskara |

### African (1)

| Code | Language | Native Name |
|------|----------|-------------|
| `sw` | Swahili | Kiswahili |

### Turkish (1)

| Code | Language | Native Name |
|------|----------|-------------|
| `tr` | Turkish | Turkce |

---

## Translation Keys (2320)

Translations are organized by 36+ namespaces:

| Namespace | Description | Keys (approx.) |
|-----------|-------------|----------------|
| `common` | Buttons, actions (Save, Cancel, Delete...) | General UI |
| `connection` | Connection screen labels | Login form |
| `protocol` | Protocol-specific labels | FTP, SFTP, S3, etc. |
| `browser` | File browser UI | Panels, address bar |
| `contextMenu` | Right-click menu items | File operations |
| `transfer` | Transfer progress/queue | Upload/download |
| `settings` | Settings panel | Preferences |
| `devtools` | DevTools/code editor | Monaco integration |
| `cloud` | AeroCloud sync | Sync features |
| `statusBar` | Status bar labels | Connection info |
| `shortcuts` | Keyboard shortcuts | Shortcut dialog |
| `dialogs` | Dialog titles | Modals |
| `overwrite` | Overwrite confirmation | File conflicts |
| `toast` | Toast notifications | Success/error messages |
| `about` | About dialog | App info |
| `support` | Support dialog | Help links |
| `activity` | Activity log | Transfer log |
| `statusbar` | Extended status bar | Quota, info |
| `migration` | Migration messages | Version upgrades |
| `masterPassword` | Master password vault | Encryption vault |
| `search` | Remote search | Search bar |
| `versions` | File versions | Version history |
| `sharing` | Share permissions | Sharing dialog |
| `locking` | File locking | Lock/unlock |
| `ai` | AI assistant | Chat, tools, cost, templates |
| `archive` | Archive operations | Browse, compress, extract |
| `vault` | AeroVault | Create, open, extract, security |
| `cryptomator` | Cryptomator support | Unlock, browse, errors |
| `compress` | Compression dialog | Format, level, password |
| `sidebar` | Places Sidebar | Sections, actions, locations |
| `breadcrumb` | Breadcrumb bar | Navigation, edit, siblings |
| `cyberTools` | Security Toolkit | Hash, crypto, password (v2.0.6) |
| `savedServers` | Saved servers list | Profiles, badges (v2.0.6) |
| `debug` | Debug panel | Diagnostics, metrics (v2.0.6) |
| `dependencies` | Dependencies panel | Crate list (v2.0.6) |
| `properties` | File properties | Metadata, permissions (v2.0.6) |
| `preview` | File preview | Text, image, PDF (v2.0.6) |
| `syncPanel` | AeroSync panel | Profiles, compression, delta sync, multi-path, templates, rollback (v2.1.2+) |
| `commandPalette` | Command Palette | Commands, categories, search (v2.6.0) |
| `pluginBrowser` | Plugin Browser | Install, update, remove (v2.6.0) |
| `filelu` | FileLu provider | Password, privacy, clone, trash, remote upload (v2.7.0) |

---

## Project Structure

```
src/i18n/
├── index.ts              # Public exports
├── I18nContext.tsx        # Provider, hooks, getNestedValue
├── types.ts              # TypeScript interfaces, AVAILABLE_LANGUAGES (47)
└── locales/
    ├── en.json           # English (base, 2320 keys)
    ├── it.json           # Italian (manually translated)
    ├── ...               # 44 more languages
    └── zh.json           # Chinese
```

---

## CLI Tools

```bash
# Validate all translations (100% coverage check)
npm run i18n:validate

# Sync new keys from en.json to all 47 languages
# Adds [NEEDS TRANSLATION] placeholder for missing keys
npm run i18n:sync

# Show translation statistics
npm run i18n:stats
```

---

## Translation Workflow for Large Batches

When adding many keys (40+) or fixing translations across all 46 non-English languages, follow this workflow to prevent context window overflow and ensure no work is lost.

### Strategy: Divide by Language Group

Split languages into **independent groups** so multiple agents can work in parallel without file write conflicts:

| Group | Languages | Count |
|-------|-----------|-------|
| **West EU** | de, fr, es, pt, ca, nl, gl, eu, cy | 9 |
| **East Asian + SEA** | ja, zh, ko, vi, th, id, ms, tl, km | 9 |
| **Slavic** | ru, uk, pl, cs, sk, hr, sr, sl, bg, mk | 10 |
| **Nordic + Mediterranean** | sv, da, no, fi, is, tr, el, ro, hu | 9 |
| **Other** | hi, bn, hy, ka, et, lv, lt, sw | 8 |

**Key rule**: Each agent works on **different locale files** — never two agents writing to the same `.json` file simultaneously.

### Step-by-Step Process

1. **Audit** — Run a Python script to detect "silent intruders" (values identical to English without `[NEEDS TRANSLATION]` marker):
   ```python
   # Compare each locale to en.json, find identical values
   for key in en_keys:
       if locale_val == en_val and not is_tech_term(en_val):
           intruders.append(key)
   ```

2. **Group by section** — Organize keys into thematic sections (common, connection, migration, settings, etc.) for coherent translation batches.

3. **Create Python translation scripts** — Each script:
   - Reads the locale JSON file
   - Only replaces values that **exactly match the English** (idempotent)
   - Writes back with `ensure_ascii=False, indent=4` + trailing newline
   - Saved to `/tmp/translate_{section}_{group}.py`

4. **Launch parallel agents** — One per language group:
   ```
   Agent 1: West EU (9 langs)     → /tmp/translate_section_westeu.py
   Agent 2: East Asian (9 langs)  → /tmp/translate_section_asian.py
   Agent 3: Slavic (10 langs)     → /tmp/translate_section_slavic.py
   Agent 4: Nordic+Med (9 langs)  → /tmp/translate_section_nordic.py
   Agent 5: Other (8 langs)       → /tmp/translate_section_other.py
   ```

5. **Verify idempotency** — Each script is run twice. Second run must show 0 replacements.

6. **Validate** — `npm run i18n:validate` after each batch, `npm run build` at the end.

### Non-Latin Script Handling

For languages with non-Latin scripts (Cyrillic, CJK, Thai, Hindi, Bengali, Khmer, Georgian, Armenian, Greek):

- Write **actual Unicode characters** in Python scripts, not `\uXXXX` escape sequences
- Use `ensure_ascii=False` in `json.dump()` to preserve Unicode in output
- Split non-Latin languages into **smaller batches** (3-5 langs per agent) to avoid hitting the 32K output token limit
- For Armenian: use a reverse-transliteration approach or manual Unicode construction
- Always verify with `has_armenian()` / `has_cyrillic()` checks after conversion

### Delegation and Parallel Workflows

When delegating translation work to external contributors or parallel workflows:

1. Create a delegation document with:
   - Exact keys to translate
   - English reference values
   - Translation rules (tech terms, placeholders, etc.)
   - Delivery format (batch JSON files)
2. **Do NOT duplicate the delegated work locally** — wait for delivery
3. Apply the delivery with a merge script
4. Fill gaps only if the delivery is incomplete

### Translation Audit Summary (v2.0.7)

| Category | Scope | Translations |
|----------|-------|-------------|
| New keys batch delivery | 40 new keys + CJK fixes across 45 langs | ~1,945 |
| masterPassword section | 6 keys across 44 langs | ~264 |
| migration section | 16 keys across 44 langs | ~700 |
| overwrite section | 13 keys across 36 langs | ~468 |
| Silent intruder fix | 50+ keys across 46 langs | ~2,300 |
| Armenian script restoration | 63 romanizations + 53 translations | 116 |
| Chinese native corrections | 16 corrections from native review | 16 |
| Placeholder format fix | `{{param}}` to `{param}` in all locales | 173 |
| Connection keys | 5 keys across 45 langs | ~203 |
| Orphaned key cleanup | 11 removed from ja/ko/zh | -33 |

**Total translations applied**: ~6,350+

---

## How to Add a New Language

### Step 1: Create the Translation File

1. Copy `src/i18n/locales/en.json` to `src/i18n/locales/{code}.json`
   - Use ISO 639-1 language codes (e.g., `de`, `fr`, `es`)

2. Update the `meta` section:
   ```json
   {
     "meta": {
       "code": "de",
       "name": "German",
       "nativeName": "Deutsch",
       "direction": "ltr"
     },
     "translations": { ... }
   }
   ```

### Step 2: Register the Language

In `src/i18n/types.ts`:

1. Add the code to the `Language` type
2. Add to `AVAILABLE_LANGUAGES` array with flag emoji

### Step 3: Import the Translation

In `src/i18n/I18nContext.tsx`:

1. Add `import deTranslations from './locales/de.json';`
2. Add to `TRANSLATIONS` map

### Step 4: Verify

```bash
npm run i18n:validate   # Should show 100%
npm run build           # No TypeScript errors
```

---

## How to Add New Translation Keys

1. Add the key to `src/i18n/locales/en.json` in the appropriate namespace
2. Add the Italian translation manually to `it.json`
3. Run `npm run i18n:sync` to propagate to all 47 languages (adds `[NEEDS TRANSLATION]` placeholders)
4. Translate all languages using the batch workflow above (or delegate to AI agents)
5. Run `npm run i18n:validate` to confirm 100%

---

## Using Translations in Components

```typescript
// Import
import { useTranslation } from '../i18n';

// Use
const t = useTranslation();
return <h1>{t('common.settings')}</h1>;

// With parameters
t('toast.connectionSuccess', { server: 'ftp.example.com' });
// Output: "Connected to ftp.example.com"
```

For full context access (language switching):
```typescript
import { useI18n } from '../i18n';
const { t, language, setLanguage } = useI18n();
```

---

## Best Practices

1. **English is the base** - Always add keys to `en.json` first
2. **Use namespaces** - Group related keys together
3. **Run sync after adding keys** - `npm run i18n:sync`
4. **All languages are LTR** - No RTL languages currently supported
5. **Technical terms stay English** - FTP, SFTP, OAuth, S3, WebDAV, API, URL, token(s), model names
6. **UTF-8 encoding** - All files must be UTF-8 with `ensure_ascii=False`
7. **Preserve placeholders** - Keep `{param}` and `{{param}}` patterns intact
8. **Keep labels concise** - Short labels (buttons, badges) should stay under ~15 chars
9. **"EXP" badge stays "EXP"** - Never translate the EXP badge
10. **URL/path examples stay English** - `https://api.example.com/v1`, `ftp.example.com:21`

---

## Competitor Comparison

| Client | Languages |
|--------|-----------|
| **AeroFTP** | **47** |
| FileZilla | 47 |
| WinSCP | ~15 |
| Cyberduck | ~10 |
| Transmit | ~5 |

---

**Maintainer**: axpdev
**Last Updated**: 22 February 2026
