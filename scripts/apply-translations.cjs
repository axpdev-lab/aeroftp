/**
 * Helper script to apply translations to a locale file.
 * Usage: node apply-translations.cjs <lang> <translations-json-file>
 *
 * The translations JSON file should be a flat object: { "key.path": "translated value", ... }
 * Keys use dot notation matching the structure under "translations" in the locale file.
 */
const fs = require('fs');
const path = require('path');

const lang = process.argv[2];
const translationsFile = process.argv[3];

if (!lang || !translationsFile) {
  console.error('Usage: node apply-translations.cjs <lang> <translations-json-file>');
  process.exit(1);
}

const localeFile = path.join(__dirname, '..', 'src', 'i18n', 'locales', `${lang}.json`);

if (!fs.existsSync(localeFile)) {
  console.error(`Locale file not found: ${localeFile}`);
  process.exit(1);
}

const locale = JSON.parse(fs.readFileSync(localeFile, 'utf8'));
const translations = JSON.parse(fs.readFileSync(translationsFile, 'utf8'));

function setNestedValue(obj, keyPath, value) {
  const parts = keyPath.split('.');
  let current = obj;
  for (let i = 0; i < parts.length - 1; i++) {
    if (!current[parts[i]] || typeof current[parts[i]] !== 'object') {
      current[parts[i]] = {};
    }
    current = current[parts[i]];
  }
  current[parts[parts.length - 1]] = value;
}

function getNestedValue(obj, keyPath) {
  const parts = keyPath.split('.');
  let current = obj;
  for (const p of parts) {
    if (current && typeof current === 'object') {
      current = current[p];
    } else {
      return undefined;
    }
  }
  return current;
}

let applied = 0;
let skipped = 0;

for (const [key, value] of Object.entries(translations)) {
  const fullKey = 'translations.' + key;
  const currentValue = getNestedValue(locale, fullKey);

  if (currentValue && typeof currentValue === 'string' && currentValue.includes('[NEEDS TRANSLATION]')) {
    setNestedValue(locale, fullKey, value);
    applied++;
  } else if (currentValue === undefined) {
    // Key doesn't exist yet, add it
    setNestedValue(locale, fullKey, value);
    applied++;
  } else {
    skipped++;
  }
}

fs.writeFileSync(localeFile, JSON.stringify(locale, null, 4) + '\n');
console.log(`${lang}: Applied ${applied} translations, skipped ${skipped} (already translated)`);
