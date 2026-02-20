#!/usr/bin/env npx tsx
/**
 * i18n Validation Script — Comprehensive Audit
 *
 * Checks:
 * 1. JSON validity
 * 2. Root structure: only `meta` + `translations` allowed at root
 * 3. Meta section: code, name, nativeName, direction
 * 4. Missing keys (in reference but not in locale)
 * 5. Extra/orphan keys (in locale but not in reference)
 * 6. [NEEDS TRANSLATION] placeholders
 * 7. Type consistency (string vs object mismatch between en and locale)
 * 8. Empty string values
 *
 * Usage: npm run i18n:validate
 */

import * as fs from 'fs';
import * as path from 'path';
import { fileURLToPath } from 'url';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const LOCALES_DIR = path.join(__dirname, '../src/i18n/locales');
const REFERENCE_LANG = 'en';

interface TranslationFile {
    meta?: {
        code?: string;
        name?: string;
        nativeName?: string;
        direction?: string;
    };
    translations?: Record<string, unknown>;
    [key: string]: unknown;
}

interface Issue {
    severity: 'error' | 'warning' | 'info';
    message: string;
}

/**
 * Recursively get all leaf keys from an object using dot notation
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
 * Get the type of a nested value (for type consistency checks)
 */
function getNestedType(obj: Record<string, unknown>, keyPath: string): string | undefined {
    const keys = keyPath.split('.');
    let current: unknown = obj;
    for (const key of keys) {
        if (current === null || current === undefined || typeof current !== 'object') {
            return undefined;
        }
        current = (current as Record<string, unknown>)[key];
    }
    if (current === undefined) return undefined;
    if (typeof current === 'object' && current !== null) return 'object';
    return typeof current;
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
 * Get a nested value from an object
 */
function getNestedValue(obj: Record<string, unknown>, keyPath: string): unknown {
    const keys = keyPath.split('.');
    let current: unknown = obj;
    for (const key of keys) {
        if (current === null || current === undefined || typeof current !== 'object') {
            return undefined;
        }
        current = (current as Record<string, unknown>)[key];
    }
    return current;
}

/**
 * Main validation function
 */
async function validate(): Promise<void> {
    console.log('AeroFTP i18n Validation — Comprehensive Audit\n');
    console.log('='.repeat(60));

    // Load reference file (English)
    const referenceFile = path.join(LOCALES_DIR, `${REFERENCE_LANG}.json`);
    let reference: TranslationFile;
    try {
        reference = JSON.parse(fs.readFileSync(referenceFile, 'utf-8'));
    } catch (e) {
        console.error(`FATAL: Cannot parse ${REFERENCE_LANG}.json: ${e}`);
        process.exit(1);
    }

    const referenceKeys = getAllKeys(reference.translations || {});
    console.log(`Reference: ${REFERENCE_LANG}.json — ${referenceKeys.length} keys\n`);

    // Validate reference structure
    const enIssues: Issue[] = [];
    const enRootKeys = Object.keys(reference).filter(k => k !== 'meta' && k !== 'translations');
    if (enRootKeys.length > 0) {
        enIssues.push({ severity: 'error', message: `Unexpected root keys: ${enRootKeys.join(', ')}` });
    }
    if (!reference.meta?.code || !reference.meta?.name) {
        enIssues.push({ severity: 'error', message: 'Missing meta.code or meta.name' });
    }
    if (enIssues.length > 0) {
        console.log(`[!] ${REFERENCE_LANG}.json issues:`);
        enIssues.forEach(i => console.log(`  ${i.severity === 'error' ? 'ERR' : 'WARN'}: ${i.message}`));
        console.log('');
    }

    // Get all locale files
    const localeFiles = fs.readdirSync(LOCALES_DIR)
        .filter(f => f.endsWith('.json') && f !== `${REFERENCE_LANG}.json`)
        .sort();

    console.log(`Checking ${localeFiles.length} locale files...\n`);

    let totalErrors = 0;
    let totalWarnings = 0;
    let totalMissing = 0;
    let totalExtra = 0;
    let totalPlaceholders = 0;
    let languagesClean = 0;
    const detailedIssues: { lang: string; issues: Issue[]; missing: number; extra: number; placeholders: number }[] = [];

    for (const file of localeFiles) {
        const filePath = path.join(LOCALES_DIR, file);
        const langCode = file.replace('.json', '');
        const issues: Issue[] = [];
        let missingCount = 0;
        let extraCount = 0;
        let placeholderCount = 0;

        // 1. JSON validity
        let data: TranslationFile;
        try {
            data = JSON.parse(fs.readFileSync(filePath, 'utf-8'));
        } catch (e) {
            console.log(`ERR ${langCode.padEnd(5)} — Invalid JSON: ${e}`);
            totalErrors++;
            continue;
        }

        // 2. Root structure: only meta + translations allowed
        const rootKeys = Object.keys(data);
        const allowedRootKeys = ['meta', 'translations'];
        const unexpectedRoot = rootKeys.filter(k => !allowedRootKeys.includes(k));
        if (unexpectedRoot.length > 0) {
            issues.push({
                severity: 'error',
                message: `Leaked root-level keys (should be inside translations): ${unexpectedRoot.join(', ')}`
            });
        }

        // 3. Meta section validation
        if (!data.meta) {
            issues.push({ severity: 'error', message: 'Missing meta section' });
        } else {
            if (!data.meta.code) issues.push({ severity: 'error', message: 'Missing meta.code' });
            if (!data.meta.name) issues.push({ severity: 'error', message: 'Missing meta.name' });
            if (!data.meta.nativeName) issues.push({ severity: 'warning', message: 'Missing meta.nativeName' });
            if (!data.meta.direction) issues.push({ severity: 'warning', message: 'Missing meta.direction' });
            if (data.meta.code && data.meta.code !== langCode) {
                issues.push({ severity: 'error', message: `meta.code "${data.meta.code}" does not match filename "${langCode}"` });
            }
        }

        if (!data.translations) {
            issues.push({ severity: 'error', message: 'Missing translations wrapper' });
            const errCount = issues.filter(i => i.severity === 'error').length;
            totalErrors += errCount;
            console.log(`ERR ${langCode.padEnd(5)} — ${errCount} errors`);
            continue;
        }

        const translationKeys = getAllKeys(data.translations);

        // 4. Missing keys
        const missingKeys = referenceKeys.filter(key => !hasKey(data.translations!, key));
        missingCount = missingKeys.length;
        if (missingKeys.length > 0) {
            issues.push({
                severity: 'error',
                message: `${missingKeys.length} missing keys: ${missingKeys.slice(0, 5).join(', ')}${missingKeys.length > 5 ? ` (+${missingKeys.length - 5} more)` : ''}`
            });
        }

        // 5. Extra/orphan keys
        const extraKeys = translationKeys.filter(key => !referenceKeys.includes(key));
        extraCount = extraKeys.length;
        if (extraKeys.length > 0) {
            issues.push({
                severity: 'warning',
                message: `${extraKeys.length} extra keys: ${extraKeys.slice(0, 5).join(', ')}${extraKeys.length > 5 ? ` (+${extraKeys.length - 5} more)` : ''}`
            });
        }

        // 6. [NEEDS TRANSLATION] placeholders
        const placeholders: string[] = [];
        for (const key of translationKeys) {
            const val = getNestedValue(data.translations, key);
            if (typeof val === 'string' && val.includes('[NEEDS TRANSLATION]')) {
                placeholders.push(key);
            }
        }
        placeholderCount = placeholders.length;
        if (placeholders.length > 0) {
            issues.push({
                severity: 'warning',
                message: `${placeholders.length} [NEEDS TRANSLATION]: ${placeholders.slice(0, 5).join(', ')}${placeholders.length > 5 ? ` (+${placeholders.length - 5} more)` : ''}`
            });
        }

        // 7. Type consistency (string vs object mismatch)
        const typeMismatches: string[] = [];
        for (const key of translationKeys) {
            const enType = getNestedType(reference.translations!, key);
            const localeType = getNestedType(data.translations!, key);
            if (enType && localeType && enType !== localeType) {
                typeMismatches.push(`${key} (en: ${enType}, ${langCode}: ${localeType})`);
            }
        }
        if (typeMismatches.length > 0) {
            issues.push({
                severity: 'error',
                message: `${typeMismatches.length} type mismatches: ${typeMismatches.slice(0, 3).join(', ')}`
            });
        }

        // 8. Empty string values
        const emptyStrings: string[] = [];
        for (const key of translationKeys) {
            const val = getNestedValue(data.translations, key);
            if (typeof val === 'string' && val.trim() === '') {
                emptyStrings.push(key);
            }
        }
        if (emptyStrings.length > 0) {
            issues.push({
                severity: 'warning',
                message: `${emptyStrings.length} empty strings: ${emptyStrings.slice(0, 5).join(', ')}`
            });
        }

        // Summary per locale
        const errors = issues.filter(i => i.severity === 'error').length;
        const warnings = issues.filter(i => i.severity === 'warning').length;
        totalErrors += errors;
        totalWarnings += warnings;
        totalMissing += missingCount;
        totalExtra += extraCount;
        totalPlaceholders += placeholderCount;

        if (errors === 0 && warnings === 0) {
            languagesClean++;
            const pct = ((translationKeys.length / referenceKeys.length) * 100).toFixed(1);
            console.log(` OK ${langCode.padEnd(5)} — ${pct}% (${translationKeys.length}/${referenceKeys.length} keys)`);
        } else {
            const pct = (((translationKeys.length - extraCount) / referenceKeys.length) * 100).toFixed(1);
            const statusIcon = errors > 0 ? 'ERR' : 'WRN';
            console.log(`${statusIcon} ${langCode.padEnd(5)} — ${pct}% | ${errors} errors, ${warnings} warnings`);
            detailedIssues.push({ lang: langCode, issues, missing: missingCount, extra: extraCount, placeholders: placeholderCount });
        }
    }

    // Global Summary
    console.log('\n' + '='.repeat(60));
    console.log('SUMMARY\n');
    console.log(`  Languages checked:  ${localeFiles.length}`);
    console.log(`  Languages clean:    ${languagesClean}/${localeFiles.length}`);
    console.log(`  Reference keys:     ${referenceKeys.length}`);
    console.log(`  Total errors:       ${totalErrors}`);
    console.log(`  Total warnings:     ${totalWarnings}`);
    console.log(`  Missing keys:       ${totalMissing}`);
    console.log(`  Extra/orphan keys:  ${totalExtra}`);
    console.log(`  [NEEDS TRANSLATION]: ${totalPlaceholders}`);

    // Detailed issues
    if (detailedIssues.length > 0) {
        console.log('\n' + '='.repeat(60));
        console.log('DETAILED ISSUES\n');
        for (const { lang, issues } of detailedIssues) {
            console.log(`--- ${lang} ---`);
            for (const issue of issues) {
                const prefix = issue.severity === 'error' ? '  [E]' : issue.severity === 'warning' ? '  [W]' : '  [I]';
                console.log(`${prefix} ${issue.message}`);
            }
            console.log('');
        }
    }

    console.log('='.repeat(60));

    // Exit code
    if (totalErrors > 0) {
        console.log(`\nFAILED — ${totalErrors} error(s) found. Fix before release.`);
        process.exit(1);
    } else if (totalWarnings > 0) {
        console.log(`\nPASSED with ${totalWarnings} warning(s).`);
    } else {
        console.log('\nPASSED — All translations are complete and structurally valid!');
    }
}

validate().catch(console.error);
