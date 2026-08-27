#!/usr/bin/env npx tsx
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Emits `docs/PROVIDER-INVENTORY.json`, the single source of truth for every
 * "how many providers / protocols / presets" figure the project publishes.
 *
 * The problem this exists to end. The figures were correct in the code and
 * wrong everywhere they were quoted, because they were quoted as prose. The
 * README caption said "51 providers, 65 connection methods" from OUTSIDE the
 * generated grid block, so regenerating the grid never touched the numbers.
 * `docs/PROTOCOL-FEATURES.md` claimed "45+ presets" over a table listing 33.
 * The docs site claimed "22 storage protocols", a number matching no source at
 * all. Three different figures for the same noun, none of them checkable.
 *
 * Note the shape of that failure: nothing was broken, nothing errored, and
 * every individual statement looked plausible. It is the same class as a
 * command that prints success while changing nothing. The fix is not to correct
 * the numbers, which would drift again by the next release. It is to make them
 * derived and to make the derivation checkable, which is what
 * `docs/COMMAND-INVENTORY.json` already does for the CLI, MCP and agent
 * surfaces. This is that pattern applied to providers.
 *
 * The vocabulary matters as much as the arithmetic, because most of the drift
 * came from one word standing for four different things. The counts below are
 * deliberately named after what they count:
 *
 *   transport_protocols  the wire protocols implemented natively (7)
 *   native_integrations  providers with a dedicated OAuth2 / API-key / SDK code
 *                        path rather than a preset over a transport protocol
 *   presets              a connection form that ARRIVES ALREADY FILLED IN: it
 *                        carries defaults or endpoints, so the user does not
 *                        type the host, the port or the base path. This is the
 *                        project's long-standing split, specific against
 *                        generic, and it is the word to use in prose. Derived
 *                        from the data rather than from an `isGeneric` flag or
 *                        an id prefix, so a new blank form is classified right
 *                        even if nobody remembers to name it `custom-`
 *   connection_forms     every form in the registry: the presets, the blank
 *                        protocol forms, and a third group worth naming. Three
 *                        named services carry a form with nothing pre-filled
 *                        (4shared, FileLu, native Backblaze) because they
 *                        authenticate by OAuth or an API key, so there is no
 *                        host or port to preset. They are not presets and they
 *                        are not generic either, and lumping them into either
 *                        bucket is how a count starts lying
 *   presets_over_transport
 *                        the subset of presets that sit on a transport protocol
 *                        rather than on a provider's own code path. Narrower and
 *                        separately named because it is what the Add Services
 *                        catalog counts as registry-sourced; do not use it when
 *                        the sentence just says "presets"
 *   providers            companies in the logo grid / catalog
 *   connection_methods   provider-protocol pairs, so a company reachable over
 *                        both WebDAV and S3 counts twice
 *   catalog_services     tiles a user actually sees in Add Services, generic
 *                        ones and the AeroShare tile included
 *
 * Quote the one that matches the noun in the sentence. They are all correct and
 * they are all different.
 *
 * Usage:
 *   npx tsx scripts/gen-provider-inventory.ts           # write the file
 *   npx tsx scripts/gen-provider-inventory.ts --check   # exit 1 if stale (CI)
 */

import { readFileSync, writeFileSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

import { PROVIDER_CATALOG } from '../src/components/providerCatalog';
import { getAllProviders } from '../src/providers';
import { buildDiscoverCategories, getTotalServiceCount } from '../src/components/IntroHub/discoverData';

const HERE = dirname(fileURLToPath(import.meta.url));
const OUT = join(HERE, '..', 'docs', 'PROVIDER-INVENTORY.json');

/**
 * The wire protocols. `ftps` is not in this list by accident: the catalog models
 * FTPS as a mode of `ftp` rather than a separate entry, so it never appears as a
 * protocol id. It is a real protocol to a user and is counted in
 * `transport_protocols` below, which is why that number is stated here rather
 * than derived from the catalog: deriving it would silently report 6.
 */
const TRANSPORT_PROTOCOL_IDS = ['ftp', 'ftps', 'sftp', 'webdav', 's3', 'azure', 'swift'];

const catalogProtocolIds = (): string[] => [
    ...new Set(PROVIDER_CATALOG.flatMap((c) => (c.protocols ?? []).map((p) => p.protocol))),
].sort();

function build() {
    const companies = PROVIDER_CATALOG;
    const connectionMethods = companies.reduce((n, c) => n + (c.protocols ?? []).length, 0);

    // A preset is a form that arrives already filled in. Tested on the data,
    // not on a naming convention: the two generic forms carry an empty
    // `defaults` AND an empty `endpoints`, every real preset carries at least
    // one of them. Three named services also come out blank (4shared, FileLu,
    // native Backblaze) because they authenticate by OAuth or an API key and
    // have no host to preset; they are reported separately rather than pushed
    // into whichever bucket happens to be convenient.
    const TRANSPORT_CATEGORIES = ['s3', 'webdav', 'ftp', 'swift', 'azure'];
    const registry = getAllProviders();
    const isPrefilled = (p: { defaults?: object; endpoints?: object }) =>
        Object.keys(p.defaults ?? {}).length > 0 || Object.keys(p.endpoints ?? {}).length > 0;
    const presets = registry.filter(isPrefilled);
    const blank = registry.filter((p) => !isPrefilled(p));
    const generic = blank.filter((p) => p.isGeneric || /^custom-/.test(p.id));
    const blankNamed = blank.filter((p) => !generic.includes(p));
    const presetsOverTransport = presets.filter((p) => TRANSPORT_CATEGORIES.includes(p.category));

    const nativeIntegrations = catalogProtocolIds().filter((p) => !TRANSPORT_PROTOCOL_IDS.includes(p));

    const categories = buildDiscoverCategories();

    return {
        schema_version: 1,
        generated_by: 'scripts/gen-provider-inventory.ts',
        counts: {
            transport_protocols: TRANSPORT_PROTOCOL_IDS.length,
            native_integrations: nativeIntegrations.length,
            presets: presets.length,
            presets_over_transport: presetsOverTransport.length,
            connection_forms: registry.length,
            connection_forms_generic: generic.length,
            connection_forms_named_without_defaults: blankNamed.length,
            providers: companies.length,
            connection_methods: connectionMethods,
            catalog_services: getTotalServiceCount(),
        },
        transport_protocols: TRANSPORT_PROTOCOL_IDS,
        native_integrations: nativeIntegrations,
        presets_by_category: Object.fromEntries(
            [...new Set(presets.map((p) => p.category))].sort().map((cat) => [
                cat,
                presets.filter((p) => p.category === cat).map((p) => p.id).sort(),
            ]),
        ),
        generic_connection_forms: generic.map((p) => p.id).sort(),
        connection_forms_named_without_defaults: blankNamed.map((p) => p.id).sort(),
        catalog_categories: Object.fromEntries(categories.map((c) => [c.id, c.count])),
        providers: companies.map((c) => ({
            id: c.logoId,
            name: c.name,
            protocols: (c.protocols ?? []).map((p) => p.protocol),
        })).sort((a, b) => a.id.localeCompare(b.id)),
    };
}

const inventory = build();
const serialised = `${JSON.stringify(inventory, null, 2)}\n`;

if (process.argv.includes('--check')) {
    let committed: string;
    try {
        committed = readFileSync(OUT, 'utf8');
    } catch {
        console.error(`Missing ${OUT}. Run: npm run gen:provider-inventory`);
        process.exitCode = 1;
        throw new Error('inventory missing');
    }
    if (committed !== serialised) {
        console.error('docs/PROVIDER-INVENTORY.json is stale.');
        const now = inventory.counts;
        const was = (JSON.parse(committed) as typeof inventory).counts;
        for (const key of Object.keys(now) as (keyof typeof now)[]) {
            if (now[key] !== was[key]) console.error(`  ${key}: committed ${was[key]}, actual ${now[key]}`);
        }
        console.error('Run: npm run gen:provider-inventory, and update any prose that quotes these figures.');
        process.exitCode = 1;
    } else {
        console.log('docs/PROVIDER-INVENTORY.json is current.');
    }
} else {
    writeFileSync(OUT, serialised, 'utf8');
    console.log(`Wrote ${OUT}`);
    for (const [k, v] of Object.entries(inventory.counts)) console.log(`  ${k.padEnd(20)} ${v}`);
}
