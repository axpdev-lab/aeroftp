// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import type { CatalogCategoryId } from '../types/catalog';
import committedCliCatalog from '../../src-tauri/src/cli_catalog.json?raw';
import readmeRaw from '../../README.md?raw';
import providersDocRaw from '../../docs/PROVIDERS.md?raw';
import {
    PROVIDER_CATALOG,
    buildCliCatalog,
    buildProvidersMarkdown,
    companyInCategory,
    companyLaunchProtocol,
    companyTier,
    companyTierInCategory,
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

describe('providers table drift guard (issue #270 17104681)', () => {
    const BEGIN = '<!-- BEGIN PROVIDERS-TABLE -->';
    const END = '<!-- END PROVIDERS-TABLE -->';
    const between = (src: string): string => {
        const b = src.indexOf(BEGIN);
        const e = src.indexOf(END);
        expect(b, 'BEGIN PROVIDERS-TABLE anchor present').toBeGreaterThanOrEqual(0);
        expect(e, 'END PROVIDERS-TABLE anchor after BEGIN').toBeGreaterThan(b);
        return src.slice(b + BEGIN.length, e).trim();
    };

    it('README provider matrix matches buildProvidersMarkdown()', () => {
        expect(
            between(readmeRaw),
            'README.md provider matrix is stale - run `npm run gen:providers-table`',
        ).toBe(buildProvidersMarkdown().trim());
    });

    it('docs/PROVIDERS.md matches buildProvidersMarkdown()', () => {
        expect(
            between(providersDocRaw),
            'docs/PROVIDERS.md is stale - run `npm run gen:providers-table`',
        ).toBe(buildProvidersMarkdown().trim());
    });
});

describe('provider catalog category model (issue #224)', () => {
    const CATEGORIES: CatalogCategoryId[] = [
        'protocols', 'object-storage', 'webdav', 'cloud-storage', 'media-services', 'developer', 'devices',
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
        // EF-14/15: MEGA S4 (S3) was merged back into the single MEGA row, so the
        // MEGA row now also hits object-storage.
        expect(companyInCategory(mega, 'object-storage')).toBe(true);
    });

    it('row launch target is context-aware per category', () => {
        const filen = PROVIDER_CATALOG.find(c => c.company === 'Filen')!;
        expect(companyLaunchProtocol(filen, 'object-storage').providerId).toBe('filen-desktop-s3');
        expect(companyLaunchProtocol(filen, 'webdav').providerId).toBe('filen-desktop-webdav');
        // "All" (no category match) falls back to the company default.
        expect(companyLaunchProtocol(filen, 'all').label).toBe(filen.protocols[0].label);
    });

    it('MEGA S4 is merged into the single MEGA row as a paid S3 method (EF-14/15); Yandex Object Storage stays its own row', () => {
        // One row per company: MEGA S4 is MEGA's paid, S3-compatible object storage,
        // so it is a method on the MEGA row (labelled "S4 (S3)"), NOT a duplicate
        // "MEGA S4 Object Storage" company.
        expect(PROVIDER_CATALOG.find(c => c.company === 'MEGA S4 Object Storage')).toBeUndefined();
        const mega = PROVIDER_CATALOG.find(c => c.company === 'MEGA')!;
        const s4 = mega.protocols.find(p => p.providerId === 'mega-s4')!;
        expect(s4).toBeDefined();
        expect(s4.paid).toBe(true);
        expect(s4.category).toBe('object-storage');
        expect(s4.labelOverride).toBe('S4 (S3)');
        // In the S3 category the MEGA row launches the S4 preset.
        expect(companyLaunchProtocol(mega, 'object-storage').providerId).toBe('mega-s4');

        // FileLu's S3 object storage is branded "S5" (same EF-14/15 labelling).
        const fileluS3 = PROVIDER_CATALOG.find(c => c.company === 'FileLu')!.protocols.find(p => p.providerId === 'filelu-s3')!;
        expect(fileluS3.labelOverride).toBe('S5 (S3)');

        const yandexStorage = PROVIDER_CATALOG.find(c => c.company === 'Yandex Object Storage');
        expect(yandexStorage).toBeDefined();
        expect(yandexStorage!.logoId).toBe('yandex-storage');

        // The free parent rows must NOT carry the moved S3 method anymore.
        const yandexDisk = PROVIDER_CATALOG.find(c => c.company === 'Yandex Disk')!;
        expect(yandexDisk.protocols.some(p => p.providerId === 'yandex-storage')).toBe(false);
    });

    it('Yandex Disk WebDAV is paid (Yandex 360 subscription) while its OAuth stays free, mirroring pCloud Drive', () => {
        const yandexDisk = PROVIDER_CATALOG.find(c => c.company === 'Yandex Disk')!;
        const webdav = yandexDisk.protocols.find(p => p.providerId === 'yandexdisk-webdav')!;
        const oauth = yandexDisk.protocols.find(p => p.protocol === 'yandexdisk')!;
        expect(webdav.paid).toBe(true);
        expect(oauth.paid).toBeFalsy();
        // Same free-OAuth + paid-WebDAV shape as pCloud Drive.
        const pcloud = PROVIDER_CATALOG.find(c => c.company === 'pCloud Drive')!;
        expect(pcloud.protocols.find(p => p.providerId === 'pcloud-webdav')!.paid).toBe(true);
    });
});

describe('commercial tier model: free / free-card / paid', () => {
    const tierOf = (name: string) => {
        const c = PROVIDER_CATALOG.find(x => x.company === name);
        expect(c, `${name} present`).toBeDefined();
        return companyTier(c!);
    };

    it('free-card companies have a free allowance but require a card, and stay OUT of the paid bucket', () => {
        // Alibaba OSS has a permanent 5 GB/mo fixed quota (overseas regions) but a card on file.
        for (const name of ['Amazon Web Services (AWS)', 'Microsoft Azure Blob', 'Yandex Object Storage', 'Oracle Cloud', 'Google Cloud Storage', 'Cloudflare R2', 'Alibaba OSS']) {
            const c = PROVIDER_CATALOG.find(x => x.company === name)!;
            expect(c.freeRequiresCard, `${name} freeRequiresCard`).toBe(true);
            expect(c.freeStorageGb, `${name} has a free GB figure`).not.toBeNull();
            expect(companyTier(c), `${name} tier`).toBe('free-card');
        }
    });

    it('paid-only companies have no free tier (trial or paid plan only)', () => {
        // Storj and IDrive e2 discontinued their permanent free tiers (now trial-only).
        // MEGA S4 is no longer here: it merged into the MEGA row (EF-14/15), and MEGA
        // has a free API tier, so the company classifies as 'free' (its S4 method
        // stays individually paid).
        for (const name of ['Wasabi', 'DigitalOcean Spaces', 'Hetzner Storage Box', 'Tencent COS', 'Storj', 'IDrive e2']) {
            expect(tierOf(name), `${name} tier`).toBe('paid');
        }
    });

    it('no-card free tiers classify as plain free', () => {
        expect(tierOf('TAB.DIGITAL')).toBe('free');
        expect(tierOf('MEGA')).toBe('free');
    });

    it('category-aware tier: a hybrid provider is paid in the category where its only protocol is paid-only (F1)', () => {
        // MEGA is a free cloud drive (cloud-storage/webdav), but its S4/S3 object
        // storage is paid-only. The list-view tier filter must classify per the
        // ACTIVE category, else MEGA S4 is unreachable from Paid and MEGA wrongly
        // shows under S3 + Free tier.
        const mega = PROVIDER_CATALOG.find(x => x.company === 'MEGA');
        expect(mega, 'MEGA present').toBeDefined();
        expect(companyTierInCategory(mega!, 'object-storage'), 'MEGA under S3 tab').toBe('paid');
        expect(companyTierInCategory(mega!, 'cloud-storage'), 'MEGA under Cloud tab').toBe('free');
        expect(companyTierInCategory(mega!, 'webdav'), 'MEGA under WebDAV tab').toBe('free');
        // 'all' falls back to the whole-company tier (unchanged behavior).
        expect(companyTierInCategory(mega!, 'all'), 'MEGA under All == whole-company').toBe(companyTier(mega!));
        expect(companyTierInCategory(mega!, 'all'), 'MEGA under All == free').toBe('free');
        // A paid-only company stays paid inside its category too.
        const wasabi = PROVIDER_CATALOG.find(x => x.company === 'Wasabi');
        expect(wasabi, 'Wasabi present').toBeDefined();
        expect(companyTierInCategory(wasabi!, 'object-storage'), 'Wasabi under S3 tab').toBe('paid');
        // A card-gated free tier stays free-card per-category, NOT paid: its free
        // allowance IS the S3 product (paid-flagged method), so it must land under
        // Free + card, never leak into Paid nor vanish from Free + card.
        const aws = PROVIDER_CATALOG.find(x => x.company === 'Amazon Web Services (AWS)');
        expect(aws, 'AWS present').toBeDefined();
        expect(companyTierInCategory(aws!, 'object-storage'), 'AWS under S3 tab').toBe('free-card');
    });

    it('freeRequiresCard companies are never reported as paid-only by the CLI projection', () => {
        const cli = buildCliCatalog();
        for (const name of ['Amazon Web Services (AWS)', 'Microsoft Azure Blob', 'Yandex Object Storage', 'Oracle Cloud', 'Google Cloud Storage', 'Cloudflare R2', 'Alibaba OSS']) {
            const row = cli.find(c => c.company === name)!;
            expect(row.freeRequiresCard, `${name} CLI freeRequiresCard`).toBe(true);
        }
    });
});
