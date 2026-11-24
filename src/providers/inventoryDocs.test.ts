// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import inventoryRaw from '../../docs/PROVIDER-INVENTORY.json?raw';
import readmeRaw from '../../README.md?raw';
import protocolFeaturesRaw from '../../docs/PROTOCOL-FEATURES.md?raw';
import providersDocRaw from '../../docs/PROVIDERS.md?raw';
import threatModelRaw from '../../docs/THREAT-MODEL.md?raw';
import metainfoRaw from '../../app.aeroftp.AeroFTP.metainfo.xml?raw';

// Every "how many providers / presets / connection methods" figure the project
// publishes in prose must equal the generated `docs/PROVIDER-INVENTORY.json`
// (scripts/gen-provider-inventory.ts). The prose used to drift from the
// inventory silently; this test fails when it does.
describe('published figures match docs/PROVIDER-INVENTORY.json', () => {
    const inventory = JSON.parse(inventoryRaw) as {
        counts: { presets: number; providers: number; connection_methods: number };
    };
    const presets = inventory.counts.presets;
    const providers = inventory.counts.providers;
    const methods = inventory.counts.connection_methods;

    it('README quotes the inventory counts', () => {
        expect(readmeRaw).toContain(`**Pre-configured presets (${presets}):**`);
        expect(readmeRaw).toContain(`<sub>${providers} providers, ${methods} connection methods.`);
    });

    it('docs/PROTOCOL-FEATURES.md quotes the preset count', () => {
        expect(protocolFeaturesRaw).toContain(`**${presets} pre-configured presets**`);
    });

    it('docs/PROVIDERS.md quotes the providers caption', () => {
        expect(providersDocRaw).toContain(`<sub>${providers} providers, ${methods} connection methods.`);
    });

    it('docs/THREAT-MODEL.md quotes the provider count', () => {
        expect(threatModelRaw).toContain(`${providers} providers in \`docs/PROVIDER-INVENTORY.json\``);
    });

    it('the AppStream metainfo quotes the preset count', () => {
        expect(metainfoRaw).toContain(`${presets} presets, one app.`);
    });
});
