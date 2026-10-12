// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// Runs the real former-English gate (scripts/i18n-stale-english.mjs) on fixture
// locales. The assertion that a locale still holding the former English fails
// is what proves the rule: delete the comparison and this test goes red.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { execFileSync, spawnSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..', '..');
const GATE = join(ROOT, 'scripts', 'i18n-stale-english.mjs');

function enHash(map) {
    const body = JSON.stringify(Object.keys(map).sort().map((k) => [k, map[k]]));
    return createHash('sha256').update(body).digest('hex');
}

function fixture(t, extra, former, accepted = {}, hash = null) {
    const dir = mkdtempSync(join(tmpdir(), 'i18n-stale-english-'));
    t.after(() => rmSync(dir, { recursive: true, force: true }));
    const locales = join(dir, 'locales');
    mkdirSync(locales);
    const en = { ok: 'AeroFTP', ...(extra.en ?? {}) };
    writeFileSync(join(locales, 'en.json'), JSON.stringify({ meta: { code: 'en' }, translations: en }));
    for (const [loc, values] of Object.entries(extra)) {
        if (loc === 'en') continue;
        writeFileSync(join(locales, `${loc}.json`), JSON.stringify({ meta: { code: loc }, translations: { ok: 'AeroFTP', ...values } }));
    }
    const formerFile = join(dir, 'former.json');
    writeFileSync(formerFile, JSON.stringify({ enHash: hash ?? enHash(en), former }));
    const acceptedFile = join(dir, 'accepted.json');
    writeFileSync(acceptedFile, JSON.stringify({ accepted }));
    return [`--locales=${locales}`, `--former=${formerFile}`, `--accepted=${acceptedFile}`];
}

const report = (args) => JSON.parse(execFileSync('node', [GATE, ...args, '--json'], { encoding: 'utf8' }));
const strictExit = (args) => spawnSync('node', [GATE, ...args, '--strict'], { encoding: 'utf8' }).status;

test('the current English and a real translation are not hits', (t) => {
    const args = fixture(t, {
        en: { label: 'Password must be at least 8 characters' },
        de: { label: 'Passwort muss mindestens 8 Zeichen haben' },
        fr: { label: 'Password must be at least 8 characters' },
    }, { 'label': ['Minimum 8 characters'] });
    assert.deepEqual(report(args).stale, []);
    assert.equal(strictExit(args), 0);
});

test('a locale still holding a former English value fails', (t) => {
    const args = fixture(t, {
        en: { label: 'Password must be at least 8 characters' },
        it: { label: 'Minimum 8 characters' },
        pl: { label: 'Hasło musi mieć co najmniej 8 znaków' },
    }, { label: ['Minimum 8 characters'] });
    const found = report(args).stale;
    assert.deepEqual(found.map((h) => `${h.locale}.${h.key}`), ['it.label']);
    assert.equal(found[0].rule, 'former');
    assert.equal(strictExit(args), 1);
});

test('a reviewed acceptance is honoured and a stale one fails', (t) => {
    const value = 'es. s3.example.com';
    const id = `it\u001fprotocol.endpointPlaceholder\u001f${value}`;
    const entry = { locale: 'it', key: 'protocol.endpointPlaceholder', value, reason: 'correct Italian for e.g.' };
    const ok = fixture(t, {
        en: { 'protocol.endpointPlaceholder': 'e.g. s3.example.com' },
        it: { 'protocol.endpointPlaceholder': value },
    }, { 'protocol.endpointPlaceholder': [value] }, { [id]: entry });
    assert.deepEqual(report(ok).stale, []);
    assert.equal(strictExit(ok), 0);

    const gone = fixture(t, {
        en: { 'protocol.endpointPlaceholder': 'e.g. s3.example.com' },
        it: { 'protocol.endpointPlaceholder': 'per esempio s3.example.com' },
    }, { 'protocol.endpointPlaceholder': [value] }, { [id]: entry });
    assert.equal(strictExit(gone), 1);
    assert.deepEqual(report(gone).staleAcceptances, [id]);
});

test('a ledger hash that does not match en.json fails', (t) => {
    const args = fixture(t, {
        en: { label: 'Password must be at least 8 characters' },
        de: { label: 'Passwort muss mindestens 8 Zeichen haben' },
    }, {}, {}, 'deadbeef');
    const body = report(args);
    assert.equal(body.hashOk, false);
    assert.equal(strictExit(args), 1);
});
