// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import en from './locales/en.json';
import { PROVIDER_CATALOG } from '../components/providerCatalog';

// Fastly's plan has 5 GB free but signup asks for a card (owner, 2026-11-06).
// The Discover card is the first place a user reads the offer, so it says so,
// and it says it only while the catalog row carries the same fact.
describe('Fastly Discover description', () => {
    it('names the card requirement, in step with the catalog row', () => {
        const text = (en as { translations: { protocol: Record<string, string> } })
            .translations.protocol.discoverFastly;
        expect(text).toMatch(/5 GB free/);
        expect(text).toMatch(/card required/i);
        const fastly = PROVIDER_CATALOG.find(company => company.logoId === 'fastly');
        expect(fastly?.freeRequiresCard).toBe(true);
    });
});
