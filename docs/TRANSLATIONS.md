# AeroFTP Internationalization (i18n) Guide

> Last Updated: 10 February 2026
> Version: v2.0.4
> Languages: 51 | Keys: 1278 | Coverage: 100%

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
| `es` | Spanish | Espanol | Sync + placeholder |
| `fr` | French | Francais | Sync + placeholder |
| `zh` | Chinese | Simplified Chinese | Sync + placeholder |

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

## Translation Keys (1278)

Translations are organized by 30+ namespaces:

| Namespace | Description | Keys |
|-----------|-------------|------|
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
| `search` | Remote search | Search bar (v1.4.0) |
| `versions` | File versions | Version history (v1.4.0) |
| `sharing` | Share permissions | Sharing dialog (v1.4.0) |
| `locking` | File locking | Lock/unlock (v1.4.0) |
| `ai` | AI assistant | Chat, tools, cost, templates (v1.6.0+) |
| `archive` | Archive operations | Browse, compress, extract (v1.7.0) |
| `vault` | AeroVault | Create, open, extract, security (v1.7.0) |
| `cryptomator` | Cryptomator support | Unlock, browse, errors (v1.7.0) |
| `compress` | Compression dialog | Format, level, password (v1.7.0) |
| `sidebar` | Places Sidebar | Sections, actions, locations (v2.0.1) |
| `breadcrumb` | Breadcrumb bar | Navigation, edit, siblings (v2.0.1) |

---

## Project Structure

```
src/i18n/
├── index.ts              # Public exports
├── I18nContext.tsx        # Provider, hooks, getNestedValue
├── types.ts              # TypeScript interfaces, AVAILABLE_LANGUAGES (47)
└── locales/
    ├── en.json           # English (base, 1278 keys)
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
2. Run `npm run i18n:sync` to propagate to all 47 languages
3. Manually translate Italian (`it.json`) - other languages get `[NEEDS TRANSLATION]` placeholders
4. Run `npm run i18n:validate` to confirm 100%

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
5. **Technical terms stay English** - FTP, SFTP, OAuth, S3, WebDAV
6. **UTF-8 encoding** - All files must be UTF-8

---

## Competitor Comparison

| Client | Languages |
|--------|-----------|
| **AeroFTP** | **51** |
| FileZilla | 47 |
| WinSCP | ~15 |
| Cyberduck | ~10 |
| Transmit | ~5 |

---

**Maintainer**: axpdev
**Last Updated**: 8 February 2026
