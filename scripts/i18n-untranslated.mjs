#!/usr/bin/env node
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Detects locale strings that ship as raw English.
 *
 * `i18n:validate` cannot see this class, for the same reason it could not see
 * stripped diacritics: the key is present, the value is a non-empty string and
 * it is not a `[NEEDS TRANSLATION]` placeholder. It is simply still English.
 * The class hides because the naive test, "value equals its English source",
 * fires on roughly 16000 locale/key pairs across 1093 keys, and almost all of
 * them are correct: `FTP`, `AeroCloud`, `RAG`, `Rust + React 18 + TypeScript`,
 * `us-east-1`. A gate that reports 16000 lines is a gate nobody reads.
 *
 * So the value being identical is only the trigger. The evidence is the shape
 * of the ENGLISH source: a string is judged translatable prose when it holds at
 * least MIN_WORDS alphabetic words outside its interpolation tokens AND at
 * least one English function word. Brand names, protocol acronyms, URLs, region
 * codes and placeholder masks never satisfy both, which is what takes the
 * report from 16000 pairs down to about 2000 that a human can actually read.
 *
 * Consequences worth knowing before trusting the number:
 *  - Recall is a floor, not a total. A two-word English label with no function
 *    word ("Save kit", "Retry now") is indistinguishable, by this method, from
 *    a brand, so it is not reported. Widening the rule to catch it also catches
 *    every product name in the file, which is the trade that was refused.
 *  - Precision is high but not perfect, and the residue is linguistic, not
 *    mechanical: "Send via AeroShare" is genuinely Danish, "{size} in {seconds}s"
 *    is genuinely Italian, "FileLu via FTP (port 21)" is genuinely French. Those
 *    are what the reviewed baseline is for.
 *  - It says nothing about translation QUALITY. A wrong translation is present,
 *    is not English, and passes silently here.
 *
 * Unlike the diacritics gate, a hit here IS safely fixable in bulk: the repair
 * is a real translation of a string we know is untranslated, not a guess at a
 * misspelling. Use the `i18n-batch-translate` skill and delegate the batches;
 * the project rule is that 46 locale files are never hand-edited one by one.
 *
 * Second job, cheap and worth having: DELIBERATE keys. Some strings are English
 * in all 46 locales on purpose (the OAuth client-id and client-secret masks, the
 * S3 role ARNs, the region codes). They are listed in the baseline file, and
 * this script checks they are STILL identical everywhere. Without that check the
 * next person to read the list "corrects" them and the placeholders start
 * telling users to paste an ARN in translated Hungarian.
 *
 * Third job, the mirror image of the first: a REFERENCE LEAK, text of the wrong
 * language sitting in `en.json` itself. `protocol.endpointPlaceholder` read
 * "es. s3.example.com" for as long as it had existed, where `es.` is the Italian
 * for "e.g.". This is worse than an ordinary typo, because the reference locale
 * is what every other locale is propagated from and what every translator reads:
 * one leak there is 46 wrong strings. The check is a short deny-list of
 * abbreviations no English string ever contains. It is a floor, not a language
 * detector: it catches the leaks whose shape we have already seen, and it exists
 * so the class cannot grow back quietly.
 *
 * How this is gated, and why it is not `--strict` yet. The backlog on the day
 * the detector landed was 1966 pairs over 69 keys. `--strict` would have shipped
 * a permanently red gate, and the only way to make it green on day one would
 * have been to bulk-accept the whole backlog, which is precisely what the
 * baseline forbids: an acceptance is a sentence a human read, and nobody reads
 * 1966. So CI runs the RATCHET instead, `--max=<n>` with n set to the backlog of
 * the day. A newly untranslated string pushes the count over the line and fails
 * the build, while the existing backlog stays visible and countable instead of
 * being laundered into a baseline. Every batch of translations lowers n, and
 * when n reaches 0 the flag is replaced by `--strict` and the ratchet is gone.
 * Lowering n is part of the commit that does the translating, never a separate
 * courtesy commit, and n is never raised.
 *
 * Usage:
 *   node scripts/i18n-untranslated.mjs            # report, always exit 0
 *   node scripts/i18n-untranslated.mjs --strict   # exit 1 on ANY hit (what CI runs)
 *   node scripts/i18n-untranslated.mjs --max=<n> # ratchet, kept for a future backlog
 *   node scripts/i18n-untranslated.mjs --all      # ignore the baseline
 *   node scripts/i18n-untranslated.mjs --json     # machine-readable, for the
 *                                                 # batch-translate pipeline
 */

import { existsSync, readFileSync, readdirSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const LOCALES_DIR = join(HERE, '..', 'src', 'i18n', 'locales');
const ACCEPTED_FILE = join(HERE, 'i18n-untranslated-accepted.json');

/** Below this many words a string carries no reliable signal: two-word labels
 *  are where brands and real labels are indistinguishable without a dictionary. */
const MIN_WORDS = 2;

/**
 * English function words. The list is deliberately closed-class only, no nouns
 * and no verbs: a shared noun ("Server", "Import", "Status") is exactly what a
 * European locale legitimately keeps, so admitting nouns would trade the whole
 * precision advantage away.
 */
const FUNCTION_WORDS = new Set([
    'the', 'a', 'an', 'to', 'of', 'in', 'on', 'for', 'and', 'or', 'is', 'are', 'was', 'were',
    'be', 'been', 'this', 'that', 'with', 'from', 'by', 'at', 'as', 'not', 'no', 'you', 'your',
    'it', 'its', 'can', 'will', 'would', 'has', 'have', 'had', 'all', 'any', 'when', 'while',
    'if', 'so', 'but', 'into', 'up', 'out', 'more', 'than', 'then', 'there', 'these', 'those',
    'use', 'using', 'only', 'now', 'new', 'via', 'per', 'about', 'after', 'before', 'during',
    'each', 'every', 'must', 'should', 'may', 'do', 'does', 'did', 'here', 'what', 'which',
    'who', 'how', 'why', 'too', 'very', 'just', 'also', 'still', 'already', 'yet', 'again',
    'once', 'both', 'either', 'neither', 'other', 'another', 'same', 'such', 'own', 'few',
    'most', 'less', 'least', 'much', 'many', 'because', 'since', 'until', 'unless', 'though',
    'although', 'however', 'instead', 'rather', 'without', 'within', 'between', 'among',
    'across', 'against', 'along', 'around', 'behind', 'below', 'beneath', 'beside', 'beyond',
    'inside', 'outside', 'over', 'under', 'through', 'toward', 'towards', 'upon',
]);

/**
 * Abbreviations that no English UI string contains, with the language that owns
 * them. Matched as whole tokens and case-insensitively against `en.json` only.
 *
 * Keep this list short and unambiguous. `ex.` is not here on purpose: it is
 * French for "e.g." but also an ordinary English abbreviation for "example",
 * and a marker that fires on correct English is a marker that gets deleted.
 */
const REFERENCE_LEAK_MARKERS = [
    ['es.', 'Italian, for "e.g."'],
    ['ad es.', 'Italian, for "e.g."'],
    ['ecc.', 'Italian, for "etc."'],
    ['cfr.', 'Italian, for "cf."'],
    ['oppure', 'Italian, for "or"'],
    ['p.es.', 'Spanish, for "e.g."'],
    ['p. ej.', 'Spanish, for "e.g."'],
    ['z.b.', 'German, for "e.g."'],
    ['bzw.', 'German, for "or rather"'],
    ['usw.', 'German, for "etc."'],
    ['par ex.', 'French, for "e.g."'],
];

// `{filename}` is an identifier shared with the code, never prose: strip the
// tokens before counting words, or `"{size} in {seconds}s"` reads as four words
// when it carries exactly one.
const words = (s) => s.replace(/\{[^}]*\}/g, ' ').match(/[A-Za-z][A-Za-z'-]*/g) ?? [];

const isProse = (s) => {
    if (typeof s !== 'string' || !s.trim()) return false;
    if (/^https?:\/\//.test(s.trim())) return false;
    const w = words(s);
    if (w.length < MIN_WORDS) return false;
    return w.some((word) => FUNCTION_WORDS.has(word.toLowerCase()));
};

function flatten(node, prefix = '', out = {}) {
    for (const [k, v] of Object.entries(node ?? {})) {
        if (typeof v === 'string') out[prefix + k] = v;
        else if (v && typeof v === 'object') flatten(v, `${prefix}${k}.`, out);
    }
    return out;
}

const read = (loc) => flatten(JSON.parse(readFileSync(join(LOCALES_DIR, `${loc}.json`), 'utf8')).translations);

const locales = readdirSync(LOCALES_DIR)
    .filter((f) => f.endsWith('.json'))
    .map((f) => f.replace(/\.json$/, ''))
    .filter((loc) => loc !== 'en')
    .sort();

const english = read('en');
const translations = new Map(locales.map((loc) => [loc, read(loc)]));

/**
 * Hits a human read and judged correct as they stand, plus the keys that are
 * English everywhere on purpose.
 *
 * `accepted` is keyed by locale, key AND the exact value, the same convention as
 * the diacritics baseline: rewording a string retires its acceptance and the hit
 * comes back, which is right, the judgement was about that sentence.
 */
const baseline = existsSync(ACCEPTED_FILE) ? JSON.parse(readFileSync(ACCEPTED_FILE, 'utf8')) : {};
const accepted = baseline.accepted ?? {};
const deliberate = baseline.deliberate ?? {};
/** Reference leaks already known and scheduled, keyed by key AND value so that
 *  fixing or rewording the string retires the entry and a NEW leak still fails. */
const knownLeaks = baseline.knownLeaks ?? {};
const acceptedKey = (loc, key, value) => `${loc}${key}${value}`;

const strict = process.argv.includes('--strict');
const showAll = process.argv.includes('--all');
const asJson = process.argv.includes('--json');
// The ratchet. Absent means report-only; `--max=0` is the same rule as --strict.
const maxArg = process.argv.find((a) => a.startsWith('--max='));
const max = maxArg ? Number.parseInt(maxArg.slice('--max='.length), 10) : null;
if (maxArg && !Number.isInteger(max)) {
    // The one `process.exit()` in this file, and it is safe: nothing has been
    // written to stdout yet, so there is no queued output to abandon. Exit 2
    // rather than 1 to separate "you called me wrong" from "the gate failed".
    console.error(`Not a number: ${maxArg}`);
    process.exit(2);
}

const found = [];
let suppressed = 0;

for (const loc of locales) {
    const flat = translations.get(loc);
    for (const [key, value] of Object.entries(english)) {
        if (!isProse(value)) continue;
        if (flat[key] !== value) continue;
        // A key declared deliberate is English on purpose in every locale, so it
        // is never a hit. Its own invariant is checked separately below.
        if (Object.hasOwn(deliberate, key)) continue;
        if (!showAll && accepted[acceptedKey(loc, key, value)]) {
            suppressed += 1;
            continue;
        }
        found.push({ locale: loc, key, value });
    }
}

// The deliberate invariant, in the other direction: these must stay English.
const drifted = [];
for (const key of Object.keys(deliberate)) {
    if (!Object.hasOwn(english, key)) {
        drifted.push({ key, reason: 'no longer exists in en.json' });
        continue;
    }
    const translated = locales.filter((loc) => translations.get(loc)[key] !== english[key]);
    if (translated.length) drifted.push({ key, reason: `translated in ${translated.join(', ')}` });
}

// Wrong-language text in the reference locale, which propagates to all 46.
const leaks = [];
for (const [key, value] of Object.entries(english)) {
    if (typeof value !== 'string') continue;
    const lower = value.toLowerCase();
    for (const [marker, language] of REFERENCE_LEAK_MARKERS) {
        // A leading boundary only: these markers all end in a period or are
        // whole words, and requiring a trailing boundary would miss "es." at
        // the end of a string.
        const at = lower.indexOf(marker);
        if (at < 0) continue;
        if (at > 0 && /[a-z0-9]/.test(lower[at - 1])) continue;
        const known = !showAll && knownLeaks[`${key}\u001f${value}`];
        leaks.push({ key, value, marker, language, known: Boolean(known) });
        break;
    }
}

/**
 * A drifted deliberate key and a reference leak are absolutes: neither is a
 * backlog and neither is subject to the ratchet, so they fail the moment the
 * script is asked to gate anything at all. Only the untranslated COUNT ratchets.
 */
const gating = strict || max !== null;
const overRatchet = strict ? found.length > 0 : max !== null && found.length > max;
const newLeaks = leaks.filter((l) => !l.known);
const failing = () => (gating && (overRatchet || drifted.length || newLeaks.length) ? 1 : 0);

if (asJson) {
    console.log(JSON.stringify({ untranslated: found, drifted, leaks }, null, 2));
    // `process.exitCode`, never `process.exit()`: the JSON report runs well past
    // the 64KB pipe buffer, and exiting outright truncates it mid-string for any
    // caller that pipes us. Setting the code lets node drain stdout first.
    process.exitCode = failing();
} else {
    const byKey = new Map();
    for (const hit of found) {
        const seen = byKey.get(hit.key) ?? [];
        seen.push(hit.locale);
        byKey.set(hit.key, seen);
    }

    for (const [key, locs] of [...byKey.entries()].sort((a, b) => b[1].length - a[1].length)) {
        console.log(`\n  ${key}: ${locs.length} locale(s)`);
        console.log(`    en: ${JSON.stringify(english[key])}`);
        console.log(`    ${locs.join(' ')}`);
    }

    if (drifted.length) {
        console.log('\n  DELIBERATE keys that moved (these are meant to stay English):');
        for (const d of drifted) console.log(`    ${d.key}: ${d.reason}`);
    }

    if (leaks.length) {
        console.log('\n  REFERENCE LEAK: non-English text in en.json, which propagates to all 46 locales:');
        for (const l of leaks) console.log(`    ${l.key}: ${JSON.stringify(l.value)}\n      "${l.marker}" is ${l.language}${l.known ? ' (known, scheduled)' : ''}`);
    }

    console.log(`\n${found.length} untranslated locale/key pair(s) over ${byKey.size} key(s), ${suppressed} already reviewed and accepted.`);
    if (max !== null) {
        console.log(overRatchet
            ? `RATCHET BROKEN: ${found.length} is above the agreed ${max}. Translate the new strings, do not raise the number.`
            : `Ratchet: ${found.length} of at most ${max}.${found.length < max ? ` Lower --max to ${found.length} in this same commit.` : ''}`);
    }
    if (found.length) {
        console.log('Fix by translating, in batches, with the i18n-batch-translate skill: never hand-edit 46 files.');
        console.log('Genuinely correct as English in that language? Record it in scripts/i18n-untranslated-accepted.json with the reason.');
    }
    process.exitCode = failing();
}
