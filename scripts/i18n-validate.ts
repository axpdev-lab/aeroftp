#!/usr/bin/env npx tsx
/**
 * i18n Validation Script
 * Checks for missing translation keys across all language files
 *
 * Usage: npm run i18n:validate
 */

import * as fs from 'fs';
import * as path from 'path';
import { fileURLToPath } from 'url';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const LOCALES_DIR = path.join(__dirname, '../src/i18n/locales');
const REFERENCE_LANG = 'en'; // English is the reference/master file

interface TranslationFile {
    meta: {
        code: string;
        name: string;
        nativeName: string;
        direction: string;
    };
    translations: Record<string, unknown>;
}

/**
 * Recursively get all keys from an object using dot notation
 */
function getAllKeys(obj: Record<string, unknown>, prefix = ''): string[] {
    const keys: string[] = [];

    for (const [key, value] of Object.entries(obj)) {
        const fullKey = prefix ? `${prefix}.${key}` : key;

        if (typeof value === 'object' && value !== null && !Array.isArray(value)) {
            keys.push(...getAllKeys(value as Record<string, unknown>, fullKey));
        } else {
            keys.push(fullKey);
        }
    }

    return keys;
}

/**
 * Check if a key exists in a nested object
 */
function hasKey(obj: Record<string, unknown>, keyPath: string): boolean {
    const keys = keyPath.split('.');
    let current: unknown = obj;

    for (const key of keys) {
        if (current === null || current === undefined || typeof current !== 'object') {
            return false;
        }
        current = (current as Record<string, unknown>)[key];
    }

    return current !== undefined;
}

/**
 * Main validation function
 */
async function validate(): Promise<void> {
    console.log('🔍 AeroFTP i18n Validation\n');
    console.log('='.repeat(50));

    // Load reference file (English)
    const referenceFile = path.join(LOCALES_DIR, `${REFERENCE_LANG}.json`);
    const reference: TranslationFile = JSON.parse(fs.readFileSync(referenceFile, 'utf-8'));
    const referenceKeys = getAllKeys(reference.translations);

    console.log(`📚 Reference language: ${REFERENCE_LANG}`);
    console.log(`🔑 Total keys: ${referenceKeys.length}\n`);

    // Get all locale files
    const localeFiles = fs.readdirSync(LOCALES_DIR)
        .filter(f => f.endsWith('.json') && f !== `${REFERENCE_LANG}.json`)
        .sort();

    console.log(`📂 Checking ${localeFiles.length} language files...\n`);

    let totalMissing = 0;
    let languagesWithIssues = 0;
    const results: { lang: string; missing: string[]; extra: string[] }[] = [];

    for (const file of localeFiles) {
        const filePath = path.join(LOCALES_DIR, file);
        const langCode = file.replace('.json', '');

        try {
            const translation: TranslationFile = JSON.parse(fs.readFileSync(filePath, 'utf-8'));
            const translationKeys = getAllKeys(translation.translations);

            // Find missing keys (in reference but not in translation)
            const missingKeys = referenceKeys.filter(key => !hasKey(translation.translations, key));

            // Find extra keys (in translation but not in reference)
            const extraKeys = translationKeys.filter(key => !referenceKeys.includes(key));

            if (missingKeys.length > 0 || extraKeys.length > 0) {
                languagesWithIssues++;
                totalMissing += missingKeys.length;
                results.push({ lang: langCode, missing: missingKeys, extra: extraKeys });
            }

            // Progress indicator
            const percentage = ((translationKeys.length - extraKeys.length) / referenceKeys.length * 100).toFixed(1);
            const status = missingKeys.length === 0 ? '✅' : '⚠️';
            console.log(`${status} ${langCode.padEnd(5)} - ${percentage}% complete (${missingKeys.length} missing)`);

        } catch (error) {
            console.log(`❌ ${langCode.padEnd(5)} - Error reading file: ${error}`);
        }
    }

    // Summary
    console.log('\n' + '='.repeat(50));
    console.log('📊 Summary\n');

    if (languagesWithIssues === 0) {
        console.log('🎉 All translations are complete!');
    } else {
        console.log(`⚠️  ${languagesWithIssues} language(s) have issues`);
        console.log(`📝 Total missing keys: ${totalMissing}\n`);

        // Detailed report for languages with issues
        for (const result of results) {
            if (result.missing.length > 0) {
                console.log(`\n🔴 ${result.lang} - Missing ${result.missing.length} keys:`);
                result.missing.slice(0, 10).forEach(key => console.log(`   - ${key}`));
                if (result.missing.length > 10) {
                    console.log(`   ... and ${result.missing.length - 10} more`);
                }
            }

            if (result.extra.length > 0) {
                console.log(`\n🟡 ${result.lang} - Extra ${result.extra.length} keys (not in reference):`);
                result.extra.slice(0, 5).forEach(key => console.log(`   - ${key}`));
                if (result.extra.length > 5) {
                    console.log(`   ... and ${result.extra.length - 5} more`);
                }
            }
        }
    }

    console.log('\n' + '='.repeat(50));
    console.log('Done! Run `npm run i18n:sync` to fix missing keys.');
}

// Run validation
validate().catch(console.error);
