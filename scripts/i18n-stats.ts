#!/usr/bin/env npx tsx
/**
 * i18n Statistics Script
 * Shows translation completion statistics for all languages
 *
 * Usage: npm run i18n:stats
 */

import * as fs from 'fs';
import * as path from 'path';
import { fileURLToPath } from 'url';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const LOCALES_DIR = path.join(__dirname, '../src/i18n/locales');
const REFERENCE_LANG = 'en';
const PLACEHOLDER_PREFIX = '[NEEDS TRANSLATION]';

interface TranslationFile {
    meta: {
        code: string;
        name: string;
        nativeName: string;
        direction: string;
    };
    translations: Record<string, unknown>;
}

interface LanguageStats {
    code: string;
    name: string;
    nativeName: string;
    direction: string;
    totalKeys: number;
    translatedKeys: number;
    placeholderKeys: number;
    missingKeys: number;
    percentage: number;
}

/**
 * Recursively count all string keys in an object
 */
function countKeys(obj: Record<string, unknown>, prefix = ''): { total: number; placeholders: number } {
    let total = 0;
    let placeholders = 0;

    for (const [key, value] of Object.entries(obj)) {
        if (typeof value === 'object' && value !== null && !Array.isArray(value)) {
            const nested = countKeys(value as Record<string, unknown>, `${prefix}${key}.`);
            total += nested.total;
            placeholders += nested.placeholders;
        } else if (typeof value === 'string') {
            total++;
            if (value.startsWith(PLACEHOLDER_PREFIX)) {
                placeholders++;
            }
        }
    }

    return { total, placeholders };
}

/**
 * Create a progress bar string
 */
function progressBar(percentage: number, width = 20): string {
    const filled = Math.round(percentage / 100 * width);
    const empty = width - filled;
    const bar = '█'.repeat(filled) + '░'.repeat(empty);
    return bar;
}

/**
 * Main stats function
 */
async function stats(): Promise<void> {
    console.log('📊 AeroFTP i18n Statistics\n');
    console.log('='.repeat(70));

    // Load reference file (English)
    const referenceFile = path.join(LOCALES_DIR, `${REFERENCE_LANG}.json`);
    const reference: TranslationFile = JSON.parse(fs.readFileSync(referenceFile, 'utf-8'));
    const { total: referenceKeyCount } = countKeys(reference.translations);

    console.log(`📚 Reference language: English (${REFERENCE_LANG})`);
    console.log(`🔑 Total translation keys: ${referenceKeyCount}`);
    console.log(`📂 Locales directory: ${LOCALES_DIR}\n`);

    // Get all locale files
    const localeFiles = fs.readdirSync(LOCALES_DIR)
        .filter(f => f.endsWith('.json'))
        .sort();

    console.log('='.repeat(70));
    console.log('Language Statistics (47 languages)');
    console.log('='.repeat(70) + '\n');

    const allStats: LanguageStats[] = [];

    // Collect stats for all languages
    for (const file of localeFiles) {
        const filePath = path.join(LOCALES_DIR, file);
        const langCode = file.replace('.json', '');

        try {
            const translation: TranslationFile = JSON.parse(fs.readFileSync(filePath, 'utf-8'));
            const { total, placeholders } = countKeys(translation.translations);

            const missing = referenceKeyCount - total;
            const translated = total - placeholders;
            const percentage = (translated / referenceKeyCount) * 100;

            allStats.push({
                code: langCode,
                name: translation.meta.name,
                nativeName: translation.meta.nativeName,
                direction: translation.meta.direction,
                totalKeys: total,
                translatedKeys: translated,
                placeholderKeys: placeholders,
                missingKeys: missing < 0 ? 0 : missing,
                percentage: percentage,
            });

        } catch (error) {
            console.log(`❌ ${langCode}: Error reading file`);
        }
    }

    // Sort by completion percentage (descending)
    allStats.sort((a, b) => b.percentage - a.percentage);

    // Display table header
    console.log(`${'Code'.padEnd(6)}${'Language'.padEnd(16)}${'Native'.padEnd(14)}${'Dir'.padEnd(5)}${'Progress'.padEnd(24)}${'%'.padStart(7)}`);
    console.log('-'.repeat(70));

    // Display each language
    for (const stat of allStats) {
        const bar = progressBar(stat.percentage);
        const pct = stat.percentage.toFixed(1).padStart(6) + '%';
        const dir = stat.direction === 'rtl' ? 'RTL' : 'LTR';

        // Status emoji
        let status = '';
        if (stat.percentage === 100) status = '✅';
        else if (stat.percentage >= 90) status = '🟢';
        else if (stat.percentage >= 70) status = '🟡';
        else if (stat.percentage >= 50) status = '🟠';
        else status = '🔴';

        console.log(
            `${stat.code.padEnd(6)}` +
            `${stat.name.slice(0, 14).padEnd(16)}` +
            `${stat.nativeName.slice(0, 12).padEnd(14)}` +
            `${dir.padEnd(5)}` +
            `${bar} ` +
            `${pct} ${status}`
        );
    }

    // Summary statistics
    console.log('\n' + '='.repeat(70));
    console.log('Summary');
    console.log('='.repeat(70) + '\n');

    const complete = allStats.filter(s => s.percentage === 100).length;
    const partial = allStats.filter(s => s.percentage >= 50 && s.percentage < 100).length;
    const incomplete = allStats.filter(s => s.percentage < 50).length;
    const avgCompletion = allStats.reduce((sum, s) => sum + s.percentage, 0) / allStats.length;

    console.log(`📈 Total languages: ${allStats.length}`);
    console.log(`✅ Complete (100%): ${complete}`);
    console.log(`🟡 Partial (50-99%): ${partial}`);
    console.log(`🔴 Incomplete (<50%): ${incomplete}`);
    console.log(`📊 Average completion: ${avgCompletion.toFixed(1)}%`);

    // All languages are LTR (RTL languages removed)

    // Competitor comparison
    console.log('\n' + '='.repeat(70));
    console.log('Competitor Comparison');
    console.log('='.repeat(70) + '\n');
    console.log(`🏆 AeroFTP:   ${allStats.length} languages`);
    console.log(`📁 FileZilla: 47 languages`);
    console.log(`🦆 Cyberduck: 31 languages`);

    if (allStats.length > 47) {
        console.log(`\n🎉 AeroFTP leads with ${allStats.length - 47} more languages than FileZilla!`);
    }

    console.log('\n' + '='.repeat(70));
}

// Run stats
stats().catch(console.error);
