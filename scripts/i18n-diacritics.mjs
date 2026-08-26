#!/usr/bin/env node
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Detects locale strings whose diacritics were stripped.
 *
 * `i18n:validate` cannot see this class: the value is present, it is not a
 * placeholder, and it is the right length. It is simply spelled wrong. The
 * class grew for eight months across eight separate commits because every
 * batch of new keys leaks a share of ASCII-folded strings and nothing looked.
 *
 * The method needs no dictionary. Inside one locale file, fold every word to
 * ASCII and collect the real spellings that fold to it. A pure-ASCII word is
 * reported only when an accented twin exists in that same file and is at least
 * MIN_RATIO times more frequent, which is the evidence that the locale itself
 * considers the accented form correct.
 *
 * Consequences worth knowing before trusting the number:
 *  - Recall is bounded by the file. A word whose correct spelling appears
 *    nowhere else cannot be flagged, so the count is a floor, not a total.
 *  - Where stripping is systemic the ASCII form can outvote the correct one
 *    and go silent. Turkish and Polish are the known cases.
 *  - It cannot see the inverse defect, a word wrongly given an accent.
 *
 * So this is a WARNING gate. It exists to stop the class growing, not to
 * certify a locale as clean, and it never fails a build unless --strict is
 * passed. Do not turn its output into an automated rewrite: the previous
 * attempt at that (issue #512) changed meanings and was reverted the same day.
 * A hit is a question for a human, not a patch.
 */

import { existsSync, readFileSync, readdirSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const LOCALES_DIR = join(HERE, '..', 'src', 'i18n', 'locales');
const ACCEPTED_FILE = join(HERE, 'i18n-diacritics-accepted.json');
const MIN_RATIO = 3;
const MIN_WORD_LEN = 4;

/** Latin-script locales that actually mark diacritics. A pure-ASCII value in a
 *  non-Latin locale is untranslated English, which is a different defect, and
 *  these six Latin ones write ordinary text without diacritics. */
const SKIP = new Set(['en', 'tl', 'eu', 'id', 'ms', 'sw', 'nl',
    'bg', 'bn', 'el', 'hi', 'hy', 'ja', 'ka', 'km', 'ko', 'mk', 'ru', 'sr', 'th', 'uk', 'zh']);

/** NFD strips the combining accents. These letters do not decompose, and they
 *  are where the damage actually concentrates: Turkish dotless i, Polish
 *  l-stroke, Icelandic thorn and eth, Scandinavian o-slash and ash. */
const EXTRA = { 'ł': 'l', 'ø': 'o', 'đ': 'd', 'ß': 'ss', 'æ': 'ae', 'þ': 'th', 'ð': 'd', 'ı': 'i', 'œ': 'oe' };

const fold = (word) => [...word.toLowerCase().normalize('NFD')]
    .filter((c) => !/\p{M}/u.test(c))
    .map((c) => EXTRA[c] ?? c)
    .join('');

// `{version}` is an interpolation token, not prose: the word inside it is an
// identifier shared with the code and must never be respelled. Strip the tokens
// before looking at words, or every locale reports its own placeholders.
const words = (s) => s.replace(/\{[^}]*\}/g, ' ').match(/\p{L}+/gu) ?? [];
const isAscii = (s) => !/[^\x00-\x7F]/.test(s);

function flatten(node, prefix = '', out = {}) {
    for (const [k, v] of Object.entries(node ?? {})) {
        if (typeof v === 'string') out[prefix + k] = v;
        else if (v && typeof v === 'object') flatten(v, `${prefix}${k}.`, out);
    }
    return out;
}

const read = (loc) => flatten(JSON.parse(readFileSync(join(LOCALES_DIR, `${loc}.json`), 'utf8')).translations);
const english = read('en');

/**
 * Hits already read by a human and judged correct as they stand. Without this
 * the gate prints the same wall of known-benign lines on every run, and a wall
 * nobody reads is worse than no gate: the new hit hides inside it.
 *
 * Keyed by locale, key AND the exact value. Rewording a string retires its
 * acceptance and the hit comes back, which is right: the judgement was about
 * that sentence, not about that key forever.
 */
const accepted = existsSync(ACCEPTED_FILE)
    ? JSON.parse(readFileSync(ACCEPTED_FILE, 'utf8')).accepted ?? {}
    : {};
const acceptedKey = (loc, key, value) => `${loc}\u001F${key}\u001F${value}`;

let total = 0;
let suppressed = 0;
const strict = process.argv.includes('--strict');
const showAll = process.argv.includes('--all');
const asJson = process.argv.includes('--json');
const found = [];

for (const file of readdirSync(LOCALES_DIR).filter((f) => f.endsWith('.json')).sort()) {
    const loc = file.replace(/\.json$/, '');
    if (SKIP.has(loc)) continue;
    const flat = read(loc);

    // How often each real spelling occurs, grouped by its ASCII fold.
    const byFold = new Map();
    for (const value of Object.values(flat)) {
        for (const w of words(value)) {
            if (w.length < MIN_WORD_LEN) continue;
            const key = fold(w);
            const seen = byFold.get(key) ?? new Map();
            seen.set(w.toLowerCase(), (seen.get(w.toLowerCase()) ?? 0) + 1);
            byFold.set(key, seen);
        }
    }

    const hits = [];
    for (const [key, value] of Object.entries(flat)) {
        // A value identical to its English source is untranslated, not stripped.
        if (!isAscii(value) || english[key] === value) continue;
        // One report per string, on its first suspicious word. A sentence is
        // read and judged whole, so a second hit inside it adds noise and, when
        // the string is already accepted, would count the same acceptance twice.
        const suspicious = words(value)
            .filter((w) => w.length >= MIN_WORD_LEN)
            // An all-caps word is not evidence of stripping. Turkish is the
            // reason: the uppercase of `kapalı` really is `KAPALI`, because
            // dotless i uppercases to plain I, so a shouted label legitimately
            // folds to ASCII while the lowercase form does not.
            .filter((w) => !(w === w.toUpperCase() && w !== w.toLowerCase()))
            .map((w) => {
                const lower = w.toLowerCase();
                const seen = byFold.get(fold(w));
                if (!seen) return null;
                const plain = seen.get(lower) ?? 0;
                for (const [spelling, count] of seen) {
                    if (spelling !== lower && !isAscii(spelling) && count >= Math.max(1, plain) * MIN_RATIO) {
                        return { lower, spelling, count, plain };
                    }
                }
                return null;
            })
            .find(Boolean);

        if (!suspicious) continue;
        if (!showAll && accepted[acceptedKey(loc, key, value)]) {
            suppressed += 1;
            continue;
        }
        const { lower, spelling, count, plain } = suspicious;
        hits.push(`${key}: "${value}"\n      ${lower} -> ${spelling} (${count} vs ${plain})`);
        found.push({ locale: loc, key, value, from: lower, to: spelling });
    }

    if (hits.length) {
        total += hits.length;
        if (!asJson) {
            console.log(`\n  ${loc}: ${hits.length}`);
            for (const h of hits) console.log(`    ${h}`);
        }
    }
}

if (asJson) {
    console.log(JSON.stringify(found, null, 2));
    process.exit(strict && total > 0 ? 1 : 0);
}
console.log(`\n${total} new suspected stripped-diacritic string(s), ${suppressed} already reviewed and accepted.`);
if (total > 0) {
    console.log('A hit is a question for a human, never an automated rewrite (see issue #512).');
    console.log(`Judged correct as written? Add it to ${'scripts/i18n-diacritics-accepted.json'} with the reason.`);
}
process.exit(strict && total > 0 ? 1 : 0);
