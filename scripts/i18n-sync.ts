#!/usr/bin/env npx tsx
/**
 * i18n Sync Script
 * Synchronizes new keys from English to all other language files
 * Missing keys are added with "[NEEDS TRANSLATION] Original text" placeholder
 *
 * Usage: npm run i18n:sync
 */

import * as fs from 'fs';
import * as path from 'path';
import { fileURLToPath } from 'url';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const LOCALES_DIR = path.join(__dirname, '../src/i18n/locales');
const REFERENCE_LANG = 'en';
const PLACEHOLDER_PREFIX = '[NEEDS TRANSLATION] ';

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
 * Recursively get all keys and values from an object using dot notation
 */
function getAllKeysWithValues(obj: Record<string, unknown>, prefix = ''): Map<string, string> {
    const result = new Map<string, string>();

    for (const [key, value] of Object.entries(obj)) {
        const fullKey = prefix ? `${prefix}.${key}` : key;

        if (typeof value === 'object' && value !== null && !Array.isArray(value)) {
            const nested = getAllKeysWithValues(value as Record<string, unknown>, fullKey);
            nested.forEach((v, k) => result.set(k, v));
        } else if (typeof value === 'string') {
            result.set(fullKey, value);
        }
    }

    return result;
}

/**
 * Set a nested value in an object using dot notation
 */
function setNestedValue(obj: Record<string, unknown>, keyPath: string, value: string): void {
    const keys = keyPath.split('.');
    let current = obj;

    for (let i = 0; i < keys.length - 1; i++) {
        const key = keys[i];
        if (!(key in current) || typeof current[key] !== 'object') {
            current[key] = {};
        }
        current = current[key] as Record<string, unknown>;
    }

    current[keys[keys.length - 1]] = value;
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
 * Main sync function
 */
async function sync(): Promise<void> {
    console.log('🔄 AeroFTP i18n Sync\n');
    console.log('='.repeat(50));

    // Load reference file (English)
    const referenceFile = path.join(LOCALES_DIR, `${REFERENCE_LANG}.json`);
    const reference: TranslationFile = JSON.parse(fs.readFileSync(referenceFile, 'utf-8'));
    const referenceKeysWithValues = getAllKeysWithValues(reference.translations);

    console.log(`📚 Reference language: ${REFERENCE_LANG}`);
    console.log(`🔑 Total keys: ${referenceKeysWithValues.size}\n`);

    // Get all locale files
    const localeFiles = fs.readdirSync(LOCALES_DIR)
        .filter(f => f.endsWith('.json') && f !== `${REFERENCE_LANG}.json`)
        .sort();

    console.log(`📂 Syncing ${localeFiles.length} language files...\n`);

    let totalKeysAdded = 0;
    let filesModified = 0;

    for (const file of localeFiles) {
        const filePath = path.join(LOCALES_DIR, file);
        const langCode = file.replace('.json', '');

        try {
            const translation: TranslationFile = JSON.parse(fs.readFileSync(filePath, 'utf-8'));
            let keysAdded = 0;

            // Add missing keys
            referenceKeysWithValues.forEach((englishValue, key) => {
                if (!hasKey(translation.translations, key)) {
                    // Add key with placeholder
                    setNestedValue(
                        translation.translations,
                        key,
                        `${PLACEHOLDER_PREFIX}${englishValue}`
                    );
                    keysAdded++;
                }
            });

            if (keysAdded > 0) {
                // Save updated file with proper formatting
                fs.writeFileSync(
                    filePath,
                    JSON.stringify(translation, null, 4) + '\n',
                    'utf-8'
                );

                console.log(`✅ ${langCode.padEnd(5)} - Added ${keysAdded} missing keys`);
                totalKeysAdded += keysAdded;
                filesModified++;
            } else {
                console.log(`✓  ${langCode.padEnd(5)} - Already up to date`);
            }

        } catch (error) {
            console.log(`❌ ${langCode.padEnd(5)} - Error: ${error}`);
        }
    }

    // Summary
    console.log('\n' + '='.repeat(50));
    console.log('📊 Summary\n');

    if (totalKeysAdded === 0) {
        console.log('🎉 All files are already synchronized!');
    } else {
        console.log(`✅ Modified ${filesModified} file(s)`);
        console.log(`📝 Added ${totalKeysAdded} placeholder keys`);
        console.log('\n💡 Search for "[NEEDS TRANSLATION]" to find keys that need translation.');
    }

    console.log('\n' + '='.repeat(50));
}

// Run sync
sync().catch(console.error);
