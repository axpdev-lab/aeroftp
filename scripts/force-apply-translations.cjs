const fs = require('fs');
const path = require('path');

const lang = process.argv[2];
const translationsFile = process.argv[3];

if (!lang || !translationsFile) {
    console.error('Usage: node force-apply-translations.cjs <lang> <translations-json-file>');
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
        if (!current[parts[i]]) {
            current[parts[i]] = {};
        }
        current = current[parts[i]];
    }
    current[parts[parts.length - 1]] = value;
}

let applied = 0;

for (const [key, value] of Object.entries(translations)) {
    const fullKey = 'translations.' + key;
    // Force overwrite
    setNestedValue(locale, fullKey, value);
    applied++;
}

fs.writeFileSync(localeFile, JSON.stringify(locale, null, 4));
console.log(`${lang}: Force applied ${applied} translations.`);
