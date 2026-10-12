#!/usr/bin/env node
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Detects locale strings contaminated by characters of the wrong writing system.
 *
 * `i18n:validate` cannot see this class either: the value is present, it is not
 * a placeholder, and it reads as the right language at a glance. It is simply
 * built from the wrong letters. Two Georgian letters inside a Khmer string
 * (`discoverMegaS4`, PR #1191) were found by eye, and the scan that followed
 * found 142 contaminated strings in eleven locales: Japanese kanji inside
 * Bulgarian, Hebrew and Telugu inside Armenian, Cyrillic inside Croatian and
 * Basque, Latin look-alikes inside Ukrainian and Macedonian words.
 *
 * Three rules, each the evidence for a different way the damage gets in.
 *
 *  FOREIGN: a character whose script is neither Latin nor one of the locale's
 *    own. Latin is always allowed, because every locale legitimately carries
 *    product names, protocol acronyms, placeholders and paths. The test reads
 *    the Unicode Script_Extensions property, not Script, and the difference is
 *    the whole point: the danda `।` is Script=Common but belongs to the Indic
 *    scripts, so Bengali may use it and Thai may not, and a scan that reads
 *    character names instead reports 1195 Devanagari characters in a Bengali
 *    file that holds none. Characters shared by every script (digits, the
 *    arrows, emoji, combining accents) are never a hit.
 *
 *  MIXED: one word, with no separator, made of Latin letters and native ones.
 *    This is the homoglyph half of the class and the FOREIGN rule is blind to
 *    it, since both scripts are allowed on their own: `Введiть` with a Latin i
 *    in Ukrainian, `сè` with a Latin è in Macedonian, `Պատcենել` in Armenian.
 *    It fires on writing systems that set Latin terms apart with a space, which
 *    the corpus confirms: across the Cyrillic, Greek, Armenian, Georgian, Indic,
 *    Thai and Khmer files a glued Latin run was a defect every time it was read.
 *    Japanese, Chinese and Korean glue Latin to native text as a matter of
 *    course (`SSHキー`, `API를`), so for them the rule is off by declaration.
 *    A one-letter Latin word between two native words is the same defect with
 *    spaces around it (`папок i перевпорядкувати`) and is reported too.
 *
 *  INVISIBLE: a character that renders as nothing or as a box. U+FFFD is a
 *    decoding failure that shipped. Format characters (zero-width space and
 *    joiners, bidirectional controls, byte-order marks) break search, copying
 *    and line breaking where the script does not use them, and a bidi control
 *    can reorder what the user reads. Khmer separates words with U+200B and the
 *    Indic scripts shape conjuncts with ZWJ and ZWNJ, so those are declared per
 *    locale. The soft hyphen is a hyphenation hint and is allowed everywhere.
 *
 * Consequences worth knowing before trusting the number:
 *  - It sees letters, not language. A string that is nonsense in the right
 *    script passes. Several Armenian and Khmer strings this gate first flagged
 *    were word salad well beyond the flagged character, so a hit is a reason
 *    to reread the whole string, not to swap one letter.
 *  - Latin is allowed everywhere, so English left in a non-Latin locale is not
 *    reported here; that is `i18n:untranslated`'s job.
 *
 * Every locale's scripts are declared below. A locale file the table does not
 * declare fails the gate, and so does a declaration with no file: a new
 * language must say what it is written in before it can ship.
 *
 * A hit judged correct is recorded, one reviewed string at a time, in
 * scripts/i18n-contamination-accepted.json, keyed by locale, key AND value, the
 * same convention as the diacritics and untranslated baselines: rewording the
 * string retires the acceptance. An acceptance that no longer matches a hit is
 * stale and fails the gate too, so the list cannot quietly outlive its reason.
 *
 * Usage:
 *   node scripts/i18n-contamination.mjs            # report, always exit 0
 *   node scripts/i18n-contamination.mjs --strict   # exit 1 on any hit (what CI runs)
 *   node scripts/i18n-contamination.mjs --all      # ignore the baseline
 *   node scripts/i18n-contamination.mjs --json     # machine-readable
 *   --locales=<dir> --accepted=<file>              # fixtures, for the self-test
 */

import { existsSync, readFileSync, readdirSync } from 'node:fs';
import { join, dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
// Overridable so .github/scripts/test-i18n-contamination.mjs can run the real
// gate on fixture locales; CI and `npm run` never pass them.
const pathArg = (name) => process.argv.find((a) => a.startsWith(`--${name}=`))?.slice(name.length + 3);
const LOCALES_DIR = resolve(pathArg('locales') ?? join(HERE, '..', 'src', 'i18n', 'locales'));
const ACCEPTED_FILE = resolve(pathArg('accepted') ?? join(HERE, 'i18n-contamination-accepted.json'));

const LATIN_ONLY = { scripts: [] };

/**
 * What each locale is written in, besides Latin. `glued` turns the MIXED rule
 * off for writing systems that attach Latin terms to native text without a
 * space. `format` lists the format characters the script itself uses.
 */
const LOCALES = {
    bg: { scripts: ['Cyrillic'] },
    bn: { scripts: ['Bengali'], format: ['\u200C', '\u200D'] },
    ca: LATIN_ONLY,
    cs: LATIN_ONLY,
    cy: LATIN_ONLY,
    da: LATIN_ONLY,
    de: LATIN_ONLY,
    el: { scripts: ['Greek'] },
    en: LATIN_ONLY,
    es: LATIN_ONLY,
    et: LATIN_ONLY,
    eu: LATIN_ONLY,
    fi: LATIN_ONLY,
    fr: LATIN_ONLY,
    gl: LATIN_ONLY,
    hi: { scripts: ['Devanagari'], format: ['\u200C', '\u200D'] },
    hr: LATIN_ONLY,
    hu: LATIN_ONLY,
    hy: { scripts: ['Armenian'] },
    id: LATIN_ONLY,
    is: LATIN_ONLY,
    it: LATIN_ONLY,
    ja: { scripts: ['Han', 'Hiragana', 'Katakana'], glued: true },
    ka: { scripts: ['Georgian'] },
    km: { scripts: ['Khmer'], format: ['\u200B'] },
    ko: { scripts: ['Hangul'], glued: true },
    lt: LATIN_ONLY,
    lv: LATIN_ONLY,
    mk: { scripts: ['Cyrillic'] },
    ms: LATIN_ONLY,
    nl: LATIN_ONLY,
    no: LATIN_ONLY,
    pl: LATIN_ONLY,
    pt: LATIN_ONLY,
    ro: LATIN_ONLY,
    ru: { scripts: ['Cyrillic'] },
    sk: LATIN_ONLY,
    sl: LATIN_ONLY,
    sr: { scripts: ['Cyrillic'] },
    sv: LATIN_ONLY,
    sw: LATIN_ONLY,
    th: { scripts: ['Thai'] },
    tl: LATIN_ONLY,
    tr: LATIN_ONLY,
    uk: { scripts: ['Cyrillic'] },
    vi: LATIN_ONLY,
    zh: { scripts: ['Han'], glued: true },
};

/** Allowed in every locale: a hyphenation hint, invisible unless the line breaks
 *  there. fi and no use it on purpose in the long Security Tools title. */
const FORMAT_EVERYWHERE = new Set(['\u00AD']);

/** Only used to NAME a script in the report; detection never consults it, so a
 *  script missing here is still caught and reported by code point. */
const SCRIPT_NAMES = [
    'Latin', 'Greek', 'Cyrillic', 'Armenian', 'Hebrew', 'Arabic', 'Syriac', 'Thaana', 'Devanagari',
    'Bengali', 'Gurmukhi', 'Gujarati', 'Oriya', 'Tamil', 'Telugu', 'Kannada', 'Malayalam',
    'Sinhala', 'Thai', 'Lao', 'Tibetan', 'Myanmar', 'Georgian', 'Hangul', 'Ethiopic', 'Cherokee',
    'Khmer', 'Mongolian', 'Hiragana', 'Katakana', 'Bopomofo', 'Han', 'Yi', 'Tifinagh', 'Nko',
];

const scxRe = (name) => new RegExp(`^\\p{Script_Extensions=${name}}$`, 'u');
const SCX = new Map(SCRIPT_NAMES.map((name) => [name, scxRe(name)]));
const scx = (name) => SCX.get(name) ?? scxRe(name);

/** Shared by every script: punctuation, digits, symbols, emoji, combining marks. */
const GENERIC = /^[\p{Script_Extensions=Common}\p{Script_Extensions=Inherited}]$/u;
const LETTER = /^\p{L}$/u;
const WORD = /[\p{L}\p{M}\u200C\u200D]+/gu;

const codePoint = (ch) => `U+${ch.codePointAt(0).toString(16).toUpperCase().padStart(4, '0')}`;

const scriptsOf = (ch) => {
    if (ch === '\uFFFD') return 'replacement character';
    if (/^\p{Cf}$/u.test(ch)) return 'format character';
    if (/^\p{Cc}$/u.test(ch)) return 'control character';
    if (/^\p{Cn}$/u.test(ch)) return 'unassigned';
    if (/^\p{Co}$/u.test(ch)) return 'private use';
    const names = SCRIPT_NAMES.filter((name) => scx(name).test(ch));
    return names.length ? names.join('/') : 'other script';
};

const describe = (ch) => `${JSON.stringify(ch)} ${codePoint(ch)} ${scriptsOf(ch)}`;

/** ZWJ between two pictographs is part of an emoji sequence, not text. */
const inEmojiSequence = (chars, i) =>
    chars[i] === '\u200D'
    && /\p{Extended_Pictographic}/u.test(chars[i - 1] ?? '')
    && /\p{Extended_Pictographic}/u.test(chars[i + 1] ?? '');

/**
 * Every finding in one value, as `{ rule, chars, word }` records. Pure: the
 * baseline logic below and the report read nothing else about a string.
 */
function inspect(value, declaration) {
    const allowed = ['Latin', ...declaration.scripts].map(scx);
    const format = new Set([...FORMAT_EVERYWHERE, ...(declaration.format ?? [])]);
    const findings = [];
    const chars = [...value];

    const foreign = [];
    const invisible = [];
    chars.forEach((ch, i) => {
        if (ch === '\n') return;
        if (ch === '\uFFFD') {
            invisible.push(ch);
            return;
        }
        if (/^[\p{Cf}\p{Cc}]$/u.test(ch)) {
            if (!format.has(ch) && !inEmojiSequence(chars, i)) invisible.push(ch);
            return;
        }
        if (allowed.some((re) => re.test(ch)) || GENERIC.test(ch)) return;
        foreign.push(ch);
    });
    if (foreign.length) findings.push({ rule: 'foreign', chars: foreign });
    if (invisible.length) findings.push({ rule: 'invisible', chars: invisible });

    if (declaration.scripts.length && !declaration.glued) {
        // Script, not Script_Extensions, on both sides: a letter shared by
        // several scripts belongs to neither. The Ukrainian apostrophe `ʼ` in
        // `звʼязці` is a letter whose extensions include Latin and Cyrillic.
        const native = declaration.scripts.map((name) => new RegExp(`^\\p{Script=${name}}$`, 'u'));
        const latin = /^\p{Script=Latin}$/u;
        for (const word of value.match(WORD) ?? []) {
            const letters = [...word].filter((ch) => LETTER.test(ch));
            if (!letters.some((ch) => latin.test(ch))) continue;
            if (!letters.some((ch) => native.some((re) => re.test(ch)))) continue;
            // Report the Latin runs, not every letter: `сè` is one finding, and
            // eight lines for the eight letters of `security` read as eight.
            const runs = word.match(/\p{Script=Latin}+/gu);
            findings.push({ rule: 'mixed', chars: runs, word });
        }

        // The same defect with spaces around it: a one-letter word is a word on
        // its own, so `папок i перевпорядкувати`, with a Latin i for the
        // Ukrainian conjunction і, is not a mixed word. A single Latin letter
        // between two native words is that, and nothing legitimate looked like
        // it in the corpus. Tokens split on whitespace and punctuation only, so
        // `v{version}` stays one token and is not a lone letter.
        const tokens = value.match(/[^\s.,;:!?()[\]«»"“”„/]+/gu) ?? [];
        const nativeWord = (token) => /^\p{L}/u.test(token ?? '')
            && [...token].every((ch) => !LETTER.test(ch) || native.some((re) => re.test(ch)));
        tokens.forEach((token, i) => {
            if (!/^\p{Script=Latin}$/u.test(token)) return;
            if (!nativeWord(tokens[i - 1]) || !nativeWord(tokens[i + 1])) return;
            findings.push({ rule: 'lone', chars: [token], word: `${tokens[i - 1]} ${token} ${tokens[i + 1]}` });
        });
    }
    return findings;
}

function flatten(node, prefix = '', out = {}) {
    for (const [k, v] of Object.entries(node ?? {})) {
        if (typeof v === 'string') out[prefix + k] = v;
        else if (v && typeof v === 'object') flatten(v, `${prefix}${k}.`, out);
    }
    return out;
}

const read = (loc) => flatten(JSON.parse(readFileSync(join(LOCALES_DIR, `${loc}.json`), 'utf8')).translations);

const files = readdirSync(LOCALES_DIR)
    .filter((f) => f.endsWith('.json'))
    .map((f) => f.replace(/\.json$/, ''))
    .sort();

const baseline = existsSync(ACCEPTED_FILE) ? JSON.parse(readFileSync(ACCEPTED_FILE, 'utf8')) : {};
const accepted = baseline.accepted ?? {};
const acceptedKey = (loc, key, value) => `${loc}\u001F${key}\u001F${value}`;

const strict = process.argv.includes('--strict');
const showAll = process.argv.includes('--all');
const asJson = process.argv.includes('--json');

// The table is part of the gate: a locale it does not describe would otherwise
// be checked as Latin-only, or skipped, and either way say nothing about it.
const undeclared = files.filter((loc) => !Object.hasOwn(LOCALES, loc));
const orphaned = Object.keys(LOCALES).filter((loc) => !files.includes(loc));

const found = [];
const matchedAcceptances = new Set();
let suppressed = 0;

for (const loc of files) {
    const declaration = LOCALES[loc];
    if (!declaration) continue;
    for (const [key, value] of Object.entries(read(loc))) {
        const findings = inspect(value, declaration);
        if (!findings.length) continue;
        const id = acceptedKey(loc, key, value);
        if (Object.hasOwn(accepted, id)) {
            matchedAcceptances.add(id);
            if (!showAll) {
                suppressed += 1;
                continue;
            }
        }
        found.push({ locale: loc, key, value, findings });
    }
}

// An acceptance whose string was fixed, reworded or deleted now protects
// nothing, and the next reader would take it as a live judgement.
const stale = Object.keys(accepted).filter((id) => !matchedAcceptances.has(id));

const failing = () => (strict && (found.length || undeclared.length || orphaned.length || stale.length) ? 1 : 0);

if (asJson) {
    console.log(JSON.stringify({ contaminated: found, undeclared, orphaned, stale }, null, 2));
    // `process.exitCode`, never `process.exit()`: a large report runs past the
    // pipe buffer, and exiting outright truncates it for any caller that pipes us.
    process.exitCode = failing();
} else {
    const RULE_LABEL = {
        foreign: 'foreign script',
        mixed: 'mixed-script word',
        lone: 'lone Latin letter between native words',
        invisible: 'invisible or broken character',
    };
    let current = null;
    for (const hit of found) {
        if (hit.locale !== current) {
            current = hit.locale;
            const declared = ['Latin', ...LOCALES[current].scripts].join(', ');
            console.log(`\n  ${current} (written in ${declared}):`);
        }
        console.log(`    ${hit.key}`);
        for (const f of hit.findings) {
            const where = f.word ? ` in ${JSON.stringify(f.word)}` : '';
            const what = f.rule === 'mixed' || f.rule === 'lone'
                ? f.chars.map((run) => `Latin ${JSON.stringify(run)}`).join(', ')
                : f.chars.map(describe).join(', ');
            console.log(`      ${RULE_LABEL[f.rule]}${where}: ${what}`);
        }
        console.log(`      value: ${JSON.stringify(hit.value)}`);
    }

    if (undeclared.length) {
        console.log(`\n  UNDECLARED locale(s), add them to LOCALES in this script: ${undeclared.join(', ')}`);
    }
    if (orphaned.length) {
        console.log(`\n  DECLARED locale(s) with no file, remove them from LOCALES: ${orphaned.join(', ')}`);
    }
    if (stale.length) {
        console.log('\n  STALE acceptance(s) in scripts/i18n-contamination-accepted.json, matching no hit any more:');
        for (const id of stale) console.log(`    ${id.split('\u001F').slice(0, 2).join(' ')}`);
    }

    const locales = new Set(found.map((hit) => hit.locale));
    console.log(`\n${found.length} contaminated string(s) in ${locales.size} locale(s), ${suppressed} already reviewed and accepted.`);
    if (found.length) {
        console.log('Fix by retranslating the whole string in the locale\'s own script, with the i18n-batch-translate skill.');
        console.log('Genuinely correct as written? Record it in scripts/i18n-contamination-accepted.json with the reason.');
    }
    process.exitCode = failing();
}
