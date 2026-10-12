// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// Runs the real foreign-script gate (scripts/i18n-contamination.mjs) on fixture
// locales. The corpus only exercises the cases it happens to contain, so the
// allowances it does not (an emoji ZWJ sequence, a bidi control, a stale
// acceptance) are pinned here, next to the ones it does.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..', '..');
const GATE = join(ROOT, 'scripts', 'i18n-contamination.mjs');
const REAL_LOCALES = readdirSync(join(ROOT, 'src', 'i18n', 'locales'))
    .filter((f) => f.endsWith('.json'))
    .map((f) => f.replace(/\.json$/, ''));

/** A full set of declared locales, each holding `ok: "AeroFTP"` plus `extra`. */
function fixture(t, extra = {}, accepted = {}) {
    const dir = mkdtempSync(join(tmpdir(), 'i18n-contamination-'));
    t.after(() => rmSync(dir, { recursive: true, force: true }));
    const locales = join(dir, 'locales');
    mkdirSync(locales);
    const names = new Set([...REAL_LOCALES, ...Object.keys(extra)]);
    for (const loc of names) {
        const translations = { ok: 'AeroFTP', ...(extra[loc] ?? {}) };
        writeFileSync(join(locales, `${loc}.json`), JSON.stringify({ meta: { code: loc }, translations }));
    }
    const acceptedFile = join(dir, 'accepted.json');
    writeFileSync(acceptedFile, JSON.stringify({ accepted }));
    return [`--locales=${locales}`, `--accepted=${acceptedFile}`];
}

const report = (args) => JSON.parse(execFileSync('node', [GATE, ...args, '--json'], { encoding: 'utf8' }));
const strictExit = (args) => spawnSync('node', [GATE, ...args, '--strict'], { encoding: 'utf8' }).status;
const hits = (args) => new Set(report(args).contaminated.map((h) => `${h.locale}.${h.key}`));

test('what each writing system legitimately uses is not a hit', (t) => {
    const args = fixture(t, {
        bn: { danda: 'ফাইল।', zwj: 'র\u200D্যাপ' },
        hi: { version: 'संस्करण v{version} चल रहा है' },
        uk: { apostrophe: 'звʼязці', brand: 'через Google Drive' },
        en: {
            emoji: 'Family 👨\u200D👩\u200D👧 sync',
            selector: 'Couple \u{1F469}\u200D\u2764\uFE0F\u200D\u{1F468}',
            skinTone: 'Coder \u{1F469}\u{1F3FD}\u200D\u{1F4BB}',
        },
        de: { softHyphen: 'Sicherheits\u00ADwerkzeuge' },
        km: { zwsp: 'សួស្តី\u200Bពិភពលោក' },
        ja: { glued: 'SSHキー' },
        ko: { glued: 'API를' },
        zh: { glued: 'SFTP连接' },
        el: { acronym: 'Ελληνικά (FTP)' },
    });
    assert.deepEqual([...hits(args)], []);
    assert.equal(strictExit(args), 0);
});

test('a letter from another script fails, including a mark shared only by other scripts', (t) => {
    const args = fixture(t, {
        km: { georgian: 'សមგანទ' },
        bg: { kanji: 'значки на状態' },
        hr: { cyrillic: 'Konfigurirајte' },
        th: { danda: 'ข้อความ।' },
        fr: { privateUse: 'xy' },
    });
    assert.deepEqual([...hits(args)].sort(), ['bg.kanji', 'fr.privateUse', 'hr.cyrillic', 'km.georgian', 'th.danda']);
});

test('a Latin look-alike inside a native word fails, glued or standing alone', (t) => {
    const args = fixture(t, {
        uk: { mixed: 'Введiть', lone: 'папок i перевпорядкувати' },
        mk: { grave: 'Избери сè' },
        km: { glued: 'គណនីKoofr' },
    });
    const found = report(args).contaminated;
    assert.deepEqual(found.map((h) => `${h.locale}.${h.key}`).sort(), ['km.glued', 'mk.grave', 'uk.lone', 'uk.mixed']);
    assert.equal(found.find((h) => h.key === 'lone').findings[0].rule, 'lone');
});

test('an invisible or broken character fails where the script does not use it', (t) => {
    const args = fixture(t, {
        et: { zwsp: 'tühis\u200Btatud' },
        en: { bidi: 'abc\u202Edef', replacement: 'broken \uFFFD', strayZwj: 'a\uFE0F\u200D\u{1F468}' },
        bn: { zwsp: 'ফা\u200Bইল' },
    });
    assert.deepEqual([...hits(args)].sort(), ['bn.zwsp', 'en.bidi', 'en.replacement', 'en.strayZwj', 'et.zwsp']);
});

test('the gate fails on a hit, an undeclared locale and a stale acceptance, and honours a live one', (t) => {
    // A language named in its own script is the legitimate case the baseline exists for.
    const value = 'Русский';
    assert.equal(strictExit(fixture(t, { ru: { bad: 'Введiть' } })), 1);
    assert.equal(strictExit(fixture(t, { xx: { ok: 'fine' } })), 1);

    const id = (key) => `hy\u001F${key}\u001F${value}`;
    const entry = { locale: 'hy', chars: value, reason: 'the name of the language, in its own script' };
    assert.equal(strictExit(fixture(t, { hy: { quoted: value } }, { [id('quoted')]: entry })), 0);
    assert.equal(strictExit(fixture(t, {}, { [id('gone')]: entry })), 1);
});
