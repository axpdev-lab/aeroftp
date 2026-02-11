const en = require('../src/i18n/locales/en.json');
const es = require('../src/i18n/locales/es.json');
const fs = require('fs');

function findNeedsTranslation(obj, prefix) {
  prefix = prefix || '';
  const keys = [];
  for (const k of Object.keys(obj)) {
    const v = obj[k];
    const path = prefix ? prefix + '.' + k : k;
    if (typeof v === 'object' && v !== null) {
      keys.push(...findNeedsTranslation(v, path));
    } else if (typeof v === 'string' && v.includes('[NEEDS TRANSLATION]')) {
      keys.push(path);
    }
  }
  return keys;
}

function getNestedValue(obj, path) {
  const parts = path.split('.');
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

const missing = findNeedsTranslation(es);
console.log('Total missing keys:', missing.length);

// Get all unique English values
const values = {};
missing.forEach(function(k) {
  const enVal = getNestedValue(en, k);
  if (enVal) {
    const relKey = k.replace('translations.', '');
    values[relKey] = enVal;
  }
});

fs.writeFileSync('/tmp/missing-translations.json', JSON.stringify(values, null, 2));
console.log('Exported', Object.keys(values).length, 'keys to /tmp/missing-translations.json');

// Show subcategories
const subcats = {};
Object.keys(values).forEach(function(k) {
  const parts = k.split('.');
  const sub = parts[0];
  subcats[sub] = (subcats[sub] || 0) + 1;
});
console.log('\nBy subcategory:');
Object.entries(subcats).sort(function(a,b) { return b[1]-a[1]; }).forEach(function(e) {
  console.log('  ' + e[0] + ': ' + e[1]);
});
