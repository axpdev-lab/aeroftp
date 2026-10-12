#!/usr/bin/env node
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Detects a locale value that is still a previous English string of the same key.
 *
 * `i18n:untranslated` compares each locale with the current English only, so it
 * cannot see this class. `masterPassword.tooShort` stayed "Minimum 8 characters"
 * in 39 locales after the English became "Password must be at least 8 characters".
 * The key is present, the value is not a placeholder, and it is not the current
 * English, so every other gate passes it.
 *
 * The rule is exact, per key: a locale value equal to a former English value of
 * that key, and different from the current English, fails. A real translation
 * that happens to use the same words as some older English is rare. The one
 * known case (Italian `protocol.endpointPlaceholder`, which is the correct
 * "es. s3.example.com" and was also the old English, itself an Italian leak)
 * is recorded in scripts/i18n-stale-english-accepted.json, keyed by locale, key
 * AND value. Rewording the string retires the acceptance. An acceptance that
 * no longer matches a hit is stale and fails the gate, so the list cannot
 * outlive its reason. Never bulk-accept.
 *
 * Why a committed ledger, and not `git log` inside the gate. The job that runs
 * this check (checks.yml) checks out with the default fetch-depth of 1. On that
 * checkout `git log` of en.json sees only the tip commit, so a value copied
 * from an older English string would pass. Cloning the full history on every
 * pull request (`fetch-depth: 0`) would see those strings, and it would also
 * make every checks run depend on a walk that a shallow local clone cannot
 * repeat. The ledger is the former English strings, rebuilt by `--write` on a
 * full clone and committed next to the gate. CI only reads the file. It also
 * stores the hash of the current flattened en.json. Changing English without
 * rebuilding the ledger changes the hash and fails the gate, so a new former
 * string cannot be forgotten while the check stays green.
 *
 * What this does not see: a stale translation that was rewritten into the
 * language. "Minimum 8 characters" left verbatim fails. The Welsh sentence that
 * still says AeroFTP collects data does not, because it is not English. That
 * class is a reader, not this gate.
 *
 * Usage:
 *   node scripts/i18n-stale-english.mjs            # report, always exit 0
 *   node scripts/i18n-stale-english.mjs --strict   # exit 1 on any hit (what CI runs)
 *   node scripts/i18n-stale-english.mjs --all      # ignore the acceptance file
 *   node scripts/i18n-stale-english.mjs --json     # machine-readable
 *   node scripts/i18n-stale-english.mjs --write    # rebuild the ledger (full clone)
 *   --locales=<dir> --former=<file> --accepted=<file>
 *                                                  # fixtures, for the self-test
 */

import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { existsSync, readFileSync, readdirSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = join(HERE, '..');
const pathArg = (name) => process.argv.find((a) => a.startsWith(`--${name}=`))?.slice(name.length + 3);
const LOCALES_DIR = resolve(pathArg('locales') ?? join(ROOT, 'src', 'i18n', 'locales'));
const FORMER_FILE = resolve(pathArg('former') ?? join(HERE, 'i18n-former-english.json'));
const ACCEPTED_FILE = resolve(pathArg('accepted') ?? join(HERE, 'i18n-stale-english-accepted.json'));
const STRICT = process.argv.includes('--strict');
const JSON_OUT = process.argv.includes('--json');
const WRITE = process.argv.includes('--write');
const ALL = process.argv.includes('--all');

const SEP = '\u001f';

function flatten(node, prefix = '', out = {}) {
    for (const [k, v] of Object.entries(node ?? {})) {
        if (typeof v === 'string') out[prefix + k] = v;
        else if (v && typeof v === 'object') flatten(v, `${prefix}${k}.`, out);
    }
    return out;
}

function readLocale(file) {
    return flatten(JSON.parse(readFileSync(file, 'utf8')).translations);
}

function enHash(map) {
    const body = JSON.stringify(Object.keys(map).sort().map((k) => [k, map[k]]));
    return createHash('sha256').update(body).digest('hex');
}

function loadLocales() {
    const names = readdirSync(LOCALES_DIR).filter((f) => f.endsWith('.json')).map((f) => f.slice(0, -5)).sort();
    const maps = {};
    for (const name of names) maps[name] = readLocale(join(LOCALES_DIR, `${name}.json`));
    return maps;
}

function git(args) {
    const result = spawnSync('git', ['-C', ROOT, ...args], { encoding: 'utf8', maxBuffer: 1 << 30 });
    if (result.status !== 0) {
        const err = (result.stderr || result.stdout || '').trim();
        throw new Error(`git ${args[0]} failed: ${err}`);
    }
    return result.stdout;
}

function historyMaps() {
    const shallow = git(['rev-parse', '--is-shallow-repository']).trim();
    if (shallow === 'true') {
        throw new Error('--write needs a full clone. CI checks out with fetch-depth 1 and only reads the committed ledger.');
    }
    const revs = git(['rev-list', 'HEAD', '--', 'src/i18n/locales/en.json']).trim().split('\n').filter(Boolean);
    const requests = revs.map((rev) => `${rev}:src/i18n/locales/en.json\n`).join('');
    // Byte length, not UTF-16 length: en.json contains arrows and the header
    // size is bytes. Slicing a string here drops the parser out of sync.
    const child = spawnSync('git', ['-C', ROOT, 'cat-file', '--batch'], { input: requests, maxBuffer: 1 << 30 });
    if (child.status !== 0) {
        throw new Error(`git cat-file failed: ${child.stderr.toString('utf8').trim()}`);
    }
    const maps = [];
    const buf = child.stdout;
    let i = 0;
    while (i < buf.length) {
        const nl = buf.indexOf(0x0a, i);
        const header = buf.slice(i, nl).toString('utf8');
        const parts = header.split(' ');
        if (parts[1] === 'missing' || parts.length < 3) {
            i = nl + 1;
            continue;
        }
        const size = Number(parts[2]);
        const start = nl + 1;
        const body = buf.slice(start, start + size).toString('utf8');
        i = start + size + 1;
        try { maps.push(flatten(JSON.parse(body).translations)); } catch { /* a non-json blob is not English */ }
    }
    return maps;
}

function rebuildFormer(current) {
    const history = historyMaps();
    const former = {};
    for (const map of history) {
        for (const [k, v] of Object.entries(map)) {
            if (typeof v !== 'string' || v === current[k]) continue;
            if (!Object.prototype.hasOwnProperty.call(current, k)) continue;
            (former[k] ??= new Set()).add(v);
        }
    }
    const out = {};
    for (const k of Object.keys(former).sort()) {
        const values = [...former[k]].filter((v) => v !== current[k]).sort();
        if (values.length) out[k] = values;
    }
    return out;
}

function writeLedger(current, former) {
    const doc = {
        _comment: [
            'Former English strings of each key, rebuilt by `node scripts/i18n-stale-english.mjs --write` on a full clone.',
            'CI reads this file. It does not walk git history: checks.yml checks out with fetch-depth 1.',
            'enHash is the SHA-256 of the flattened current en.json. Change English and run --write in the same commit.',
            'A value listed here fails the gate when a locale still equals it and the current English does not.',
        ],
        enHash: enHash(current),
        former,
    };
    writeFileSync(FORMER_FILE, `${JSON.stringify(doc, null, 4)}\n`);
}

function loadLedger() {
    if (!existsSync(FORMER_FILE)) {
        throw new Error(`former-English ledger not found: ${FORMER_FILE}`);
    }
    const doc = JSON.parse(readFileSync(FORMER_FILE, 'utf8'));
    if (typeof doc.enHash !== 'string' || !doc.former || typeof doc.former !== 'object') {
        throw new Error(`former-English ledger is missing enHash or former: ${FORMER_FILE}`);
    }
    return doc;
}

function loadAccepted() {
    if (ALL || !existsSync(ACCEPTED_FILE)) return {};
    const doc = JSON.parse(readFileSync(ACCEPTED_FILE, 'utf8'));
    return doc.accepted ?? {};
}

function scan(maps, former) {
    const en = maps.en;
    if (!en) throw new Error(`en.json not found in ${LOCALES_DIR}`);
    const hits = [];
    for (const loc of Object.keys(maps).sort()) {
        if (loc === 'en') continue;
        for (const [k, val] of Object.entries(maps[loc])) {
            if (typeof val !== 'string') continue;
            const cur = en[k];
            if (typeof cur !== 'string' || val === cur) continue;
            const past = former[k];
            if (!Array.isArray(past) || !past.includes(val)) continue;
            hits.push({ locale: loc, key: k, value: val, rule: 'former' });
        }
    }
    return hits;
}

function main() {
    const maps = loadLocales();
    const current = maps.en;
    if (!current) throw new Error(`en.json not found in ${LOCALES_DIR}`);

    if (WRITE) {
        const former = rebuildFormer(current);
        writeLedger(current, former);
        if (!JSON_OUT) {
            const n = Object.values(former).reduce((s, a) => s + a.length, 0);
            console.log(`wrote ${FORMER_FILE}: ${Object.keys(former).length} keys, ${n} former strings, enHash ${enHash(current).slice(0, 12)}`);
        }
    }

    const ledger = loadLedger();
    const hashOk = ledger.enHash === enHash(current);
    const hits = scan(maps, ledger.former);
    const accepted = loadAccepted();
    const live = [];
    const suppressed = [];
    for (const hit of hits) {
        const id = `${hit.locale}${SEP}${hit.key}${SEP}${hit.value}`;
        if (Object.prototype.hasOwnProperty.call(accepted, id)) suppressed.push(hit);
        else live.push(hit);
    }
    const hitIds = new Set(hits.map((h) => `${h.locale}${SEP}${h.key}${SEP}${h.value}`));
    const staleAcceptances = Object.keys(accepted).filter((id) => !hitIds.has(id)).sort();

    const failed = !hashOk || live.length > 0 || staleAcceptances.length > 0;
    if (JSON_OUT) {
        console.log(JSON.stringify({
            enHash: enHash(current),
            ledgerHash: ledger.enHash,
            hashOk,
            stale: live,
            suppressed,
            staleAcceptances,
        }, null, 2));
    } else if (!WRITE || failed) {
        if (!hashOk) {
            console.log('ledger out of date: en.json changed and scripts/i18n-former-english.json was not rebuilt.');
            console.log('run: node scripts/i18n-stale-english.mjs --write');
        }
        for (const hit of live) console.log(`${hit.locale}\t${hit.key}\t${hit.value}`);
        for (const id of staleAcceptances) console.log(`stale acceptance\t${id.replaceAll(SEP, '\t')}`);
        console.log(`${live.length} former-English ${live.length === 1 ? 'string' : 'strings'}, ${staleAcceptances.length} stale ${staleAcceptances.length === 1 ? 'acceptance' : 'acceptances'}`);
    }
    if (STRICT && failed) process.exit(1);
}

try {
    main();
} catch (err) {
    console.error(err.message);
    process.exit(1);
}
