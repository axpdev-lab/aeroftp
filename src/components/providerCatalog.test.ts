// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import type { CatalogCategoryId } from '../types/catalog';
import committedCliCatalog from '../../src-tauri/src/cli_catalog.json?raw';
import {
    PROVIDER_CATALOG,
    buildCliCatalog,
    companyInCategory,
    companyLaunchProtocol,
} from './providerCatalog';

describe('CLI catalog drift guard', () => {
    it('committed cli_catalog.json matches buildCliCatalog() byte-for-byte', () => {
        const expected = JSON.stringify(buildCliCatalog(), null, 2) + '\n';
        expect(
            committedCliCatalog,
            'src-tauri/src/cli_catalog.json is stale - run `npm run gen:cli-catalog`',
        ).toBe(expected);
    });
});

describe('provider catalog category model (issue #224)', () => {
    const CATEGORIES: CatalogCategoryId[] = [
        'protocols', 'object-storage', 'webdav', 'cloud-storage', 'media-services', 'developer',
    ];

    it('every protocol carries a valid category', () => {
        for (const c of PROVIDER_CATALOG) {
            for (const p of c.protocols) {
                expect(CATEGORIES, `${c.company}/${p.label}`).toContain(p.category);
            }
        }
    });

    it('every company belongs to at least one category', () => {
        const orphans = PROVIDER_CATALOG.filter(c => !CATEGORIES.some(cat => companyInCategory(c, cat)));
        expect(orphans.map(c => c.company)).toEqual([]);
    });

    it('multi-protocol companies surface in every category their protocols touch', () => {
        const filen = PROVIDER_CATALOG.find(c => c.company === 'Filen')!;
        expect(companyInCategory(filen, 'cloud-storage')).toBe(true);
        expect(companyInCategory(filen, 'object-storage')).toBe(true);
        expect(companyInCategory(filen, 'webdav')).toBe(true);

        const mega = PROVIDER_CATALOG.find(c => c.company === 'MEGA')!;
        expect(companyInCategory(mega, 'cloud-storage')).toBe(true);
        expect(companyInCategory(mega, 'webdav')).toBe(true);
        // The S3 product was split out, so the MEGA row no longer hits S3.
        expect(companyInCategory(mega, 'object-storage')).toBe(false);
    });

    it('row launch target is context-aware per category', () => {
        const filen = PROVIDER_CATALOG.find(c => c.company === 'Filen')!;
        expect(companyLaunchProtocol(filen, 'object-storage').providerId).toBe('filen-desktop-s3');
        expect(companyLaunchProtocol(filen, 'webdav').providerId).toBe('filen-desktop-webdav');
        // "All" (no category match) falls back to the company default.
        expect(companyLaunchProtocol(filen, 'all').label).toBe(filen.protocols[0].label);
    });

    it('MEGA S4 and Yandex Object Storage are split into their own paid object-storage rows', () => {
        const megaS4 = PROVIDER_CATALOG.find(c => c.company === 'MEGA S4 Object Storage');
        expect(megaS4).toBeDefined();
        expect(megaS4!.logoId).toBe('mega-s4');
        expect(megaS4!.freeStorageGb).toBeNull();
        expect(megaS4!.protocols.every(p => p.paid)).toBe(true);
        expect(companyInCategory(megaS4!, 'object-storage')).toBe(true);

        const yandexStorage = PROVIDER_CATALOG.find(c => c.company === 'Yandex Object Storage');
        expect(yandexStorage).toBeDefined();
        expect(yandexStorage!.logoId).toBe('yandex-storage');
        expect(yandexStorage!.protocols.every(p => p.paid)).toBe(true);

        // The free parent rows must NOT carry the moved S3 method anymore.
        const yandexDisk = PROVIDER_CATALOG.find(c => c.company === 'Yandex Disk')!;
        expect(yandexDisk.protocols.some(p => p.providerId === 'yandex-storage')).toBe(false);
    });
});
