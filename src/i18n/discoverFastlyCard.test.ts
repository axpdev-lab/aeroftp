// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import React, { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, describe, expect, it, vi } from 'vitest';
import en from './locales/en.json';
import { PROVIDER_CATALOG } from '../components/providerCatalog';
import { ServiceCard } from '../components/IntroHub/DiscoverPanel';

vi.mock('../i18n', () => ({
    useTranslation: () => (key: string) => key === 'protocol.discoverFastly'
        ? 'Fastly Object Storage: S3-compatible storage in 12 regions (5 GB free, card required)'
        : key,
}));

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
let root: Root | undefined;
let host: HTMLDivElement | undefined;

afterEach(() => {
    if (root) act(() => root?.unmount());
    host?.remove();
    root = undefined;
    host = undefined;
});

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

    it('keeps the whole requirement visible on a minimum-width card', () => {
        host = document.createElement('div');
        host.style.width = '260px';
        document.body.appendChild(host);
        act(() => {
            root = createRoot(host!);
            root.render(React.createElement(ServiceCard, {
                item: {
                    id: 'fastly', name: 'Fastly Object Storage', description: 'Fastly',
                    protocol: 's3', providerId: 'fastly', source: 'registry',
                },
                onSelect: () => {},
                iconSize: 32,
            }));
        });
        const description = host.querySelector('.break-words');
        expect(description?.textContent).toContain('card required');
        expect(description?.className).not.toContain('truncate');
    });
});
