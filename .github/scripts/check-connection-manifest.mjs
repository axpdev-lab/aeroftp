// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
import fs from 'node:fs';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';

const files = {
    rust: 'src-tauri/src/providers/types.rs',
    ts: 'src/types.ts',
    registry: 'src/providers/registry.ts',
    selector: 'src/components/ProtocolSelector.tsx',
    catalog: 'src/components/providerCatalog.ts',
    cli: 'src-tauri/src/bin/aeroftp_cli.rs',
};
const refIndex = process.argv.indexOf('--source-ref');
const sourceRef = refIndex < 0 ? null : process.argv[refIndex + 1];
const sources = Object.fromEntries(Object.entries(files).map(([key, path]) => [key,
    sourceRef ? execFileSync('git', ['show', `${sourceRef}:${path}`], { encoding: 'utf8', maxBuffer: 32 * 1024 * 1024 }) : fs.readFileSync(path, 'utf8'),
]));
const manifest = JSON.parse(fs.readFileSync('src/shared/connection-manifest.json', 'utf8'));
const normalize = value => value.toLowerCase().replace(/[ _-]/g, '');
const quoted = source => [...source.matchAll(/['"]([a-z][a-z0-9_-]*)['"]/g)].map(m => m[1]);
const stripComments = source => source.replace(/\/\*[\s\S]*?\*\/|\/\/[^\n]*/g, '');

function validate(data, src) {
    const errors = [];
    const expect = (ok, message) => { if (!ok) errors.push(message); };
    const tokens = new Set(data.protocols.map(row => row.token));
    const backends = new Set(data.protocols.map(row => row.backend));
    expect(data.version === 1, 'unsupported manifest version');
    expect(tokens.size === data.protocols.length, 'duplicate canonical protocol');
    expect(backends.size === data.protocols.length, 'duplicate backend identity');
    const rustBody = stripComments(src.rust).match(/pub enum ProviderType\s*\{([\s\S]*?)\n\}/)?.[1] ?? '';
    const variants = new Set([...rustBody.matchAll(/^\s*(\w+),/gm)].map(m => m[1].toLowerCase()));
    const frontend = new Set(quoted(stripComments(src.ts).match(/export type ProviderType\s*=([\s\S]*?);/)?.[1] ?? ''));
    for (const token of variants) expect(backends.has(token), `Rust-only backend: ${token}`);
    for (const row of data.protocols) {
        expect(variants.has(row.backend), `manifest has no Rust backend: ${row.token}`);
        expect(frontend.has(row.token) === row.frontend, `frontend/manifest mismatch: ${row.token}`);
        expect(['profile', 'managed', 'runtime'].includes(row.creation), `creation policy missing: ${row.token}`);
        expect(row.creation !== 'profile' || row.defaultPort > 0, `network port missing: ${row.token}`);
        expect(Array.isArray(row.requiredOptions) && typeof row.credentialKind === 'string', `auth/options contract missing: ${row.token}`);
    }
    for (const token of frontend) expect(tokens.has(token), `frontend-only protocol: ${token}`);
    const registryIds = [...src.registry.matchAll(/^\s{8}id: '([^']+)'/gm)].map(m => m[1]);
    const presetIds = new Set(data.presets.map(p => p.id));
    expect(registryIds.length === new Set(registryIds).size, 'duplicate presentation preset');
    expect(presetIds.size === data.presets.length, 'duplicate manifest preset');
    for (const id of registryIds) expect(presetIds.has(id), `GUI-only preset: ${id}`);
    for (const preset of data.presets) {
        expect(registryIds.includes(preset.id), `manifest-only preset: ${preset.id}`);
        expect(tokens.has(preset.protocol), `unknown preset transport: ${preset.id}`);
    }
    for (const [name, alias] of Object.entries(data.aliases)) {
        expect(tokens.has(alias.protocol), `alias ${name} has no transport`);
        if (alias.providerId) expect((data.presets.some(p => p.id === alias.providerId && p.protocol === alias.protocol) || data.identities?.[alias.providerId] === alias.protocol), `alias ${name} has an incompatible preset`);
    }
    const lookup = value => data.protocols.find(p => [p.token, ...p.aliases].some(a => normalize(a) === normalize(value)));
    for (const token of [...src.catalog.matchAll(/protocol: '([^']+)'/g), ...src.selector.matchAll(/type: '([^']+)'/g)].map(m => m[1])) {
        expect(!!lookup(token), `launch target has no connection contract: ${token}`);
    }
    const selectorTargets = new Set([...src.selector.matchAll(/type: '([^']+)'/g)].map(m => m[1]));
    for (const match of src.catalog.matchAll(/protocol: '([^']+)'/g)) {
        expect(selectorTargets.has(match[1]), `catalog launch has no selector metadata: ${match[1]}`);
    }
    expect(!/const KNOWN_PROTOCOLS\b/.test(src.cli), 'CLI maintains an independent protocol allowlist');
    expect(src.cli.includes('connection_manifest::resolve_profile('), 'CLI creation bypasses shared profile resolution');
    expect(src.cli.includes('ProviderType::from_lowercase(protocol)'), 'saved CLI profiles bypass the canonical parser');
    expect(!/\bdefaults:\s*\{/.test(src.registry), 'GUI still owns duplicate connection defaults');
    return errors;
}

const errors = validate(manifest, sources);
if (errors.length) {
    process.stderr.write(errors.join('\n') + '\n');
    process.exit(1);
}
if (process.argv.includes('--self-test')) {
    const mutations = [
        ['frontend', { ...sources, ts: sources.ts.replace(/(export type ProviderType\s*=[\s\S]*?);/, '$1 | "parityprobe";') }, manifest],
        ['CLI backend', { ...sources, rust: sources.rust.replace(/(pub enum ProviderType\s*\{)/, '$1\n    ParityProbe,') }, manifest],
        ['GUI preset', { ...sources, registry: sources.registry + "\n        id: 'parityprobe',\n" }, manifest],
        ['CLI allowlist', { ...sources, cli: sources.cli + '\nconst KNOWN_PROTOCOLS = ["parityprobe"];' }, manifest],
        ['manifest', sources, { ...manifest, protocols: [...manifest.protocols, { ...manifest.protocols[0], token: 'parityprobe', backend: 'parityprobe' }] }],
    ];
    for (const [surface, src, data] of mutations) assert.ok(validate(data, src).length > 0, `${surface}-only addition escaped the parity gate`);
    process.stdout.write(`Rejected all ${mutations.length} one-surface mutations.\n`);
}
process.stdout.write(`Connection manifest matches ${manifest.protocols.length} transports and ${manifest.presets.length} presets.\n`);
