// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import type { CatalogCategoryId } from '../types/catalog';
import committedCliCatalog from '../../src-tauri/src/cli_catalog.json?raw';
import readmeRaw from '../../README.md?raw';
import providersDocRaw from '../../docs/PROVIDERS.md?raw';
import {
    PROVIDER_CATALOG,
    PROVIDER_GRID,
    buildCliCatalog,
    buildProviderGridHtml,
    buildProvidersMarkdown,
    isDevOnlyProvider,
    catalogParentKey,
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

describe('README logo grid drift guard (#347 17790873)', () => {
    const BEGIN = '<!-- BEGIN PROVIDERS-GRID -->';
    const END = '<!-- END PROVIDERS-GRID -->';
    const publicCatalog = PROVIDER_CATALOG.filter(c => !isDevOnlyProvider(c.logoId));

    it('README grid matches buildProviderGridHtml()', () => {
        const b = readmeRaw.indexOf(BEGIN);
        const e = readmeRaw.indexOf(END);
        expect(b, 'BEGIN PROVIDERS-GRID anchor present').toBeGreaterThanOrEqual(0);
        expect(e, 'END PROVIDERS-GRID anchor after BEGIN').toBeGreaterThan(b);
        expect(
            readmeRaw.slice(b + BEGIN.length, e).trim(),
            'README.md logo grid is stale - run `npm run gen:providers-table`',
        ).toBe(buildProviderGridHtml().trim());
    });

    // The reporter's three findings were all instances of one failure: the grid
    // and the catalog were maintained separately. These pin the relationship
    // rather than the symptoms, so the next provider cannot reintroduce it.
    it('every public catalog company has exactly one tile', () => {
        const tiled = PROVIDER_GRID.map(t => t.logoId);
        const missing = publicCatalog.map(c => c.logoId).filter(id => !tiled.includes(id));
        expect(missing, 'catalog companies with no grid tile').toEqual([]);
    });

    it('no company is tiled twice, and no tile is a method rather than a company', () => {
        const tiled = PROVIDER_GRID.map(t => t.logoId);
        expect(tiled.length, 'duplicate logoId in PROVIDER_GRID').toBe(new Set(tiled).size);

        const known = new Set(publicCatalog.map(c => c.logoId));
        const strays = tiled.filter(id => !known.has(id));
        // 'mega-s4' used to sit here as its own tile: MEGA's paid S4 object
        // storage is a connection METHOD of the MEGA row, not a second company.
        expect(strays, 'grid tiles naming something that is not a public company').toEqual([]);
    });

    it('a tile is never captioned with the parent company instead of the product', () => {
        const byLogoId = new Map(PROVIDER_CATALOG.map(c => [c.logoId, c]));
        for (const tile of PROVIDER_GRID) {
            const company = byLogoId.get(tile.logoId)!;
            const caption = tile.label ?? company.company;
            expect(caption.length, `${company.company} has an empty caption`).toBeGreaterThan(0);
            // The reported bug in one sentence: a tile showed the company that
            // OWNS the drive rather than the drive. "Zoho" for Zoho WorkDrive
            // and "pCloud" for pCloud Drive are both that mistake. Shortening
            // a product name to fit the tile ("AWS S3", "Azure Blob") is not.
            if (company.parentCompany && company.parentCompany !== company.company) {
                expect(
                    caption.toLowerCase(),
                    `${company.company} is captioned with its parent company "${caption}" rather than the product`,
                ).not.toBe(company.parentCompany.toLowerCase());
            }
        }
    });

    it('the products the reporter named are under their full names', () => {
        const captionOf = (logoId: string) => {
            const tile = PROVIDER_GRID.find(t => t.logoId === logoId)!;
            return tile.label ?? PROVIDER_CATALOG.find(c => c.logoId === logoId)!.company;
        };
        expect(captionOf('pcloud')).toBe('pCloud Drive');
        expect(captionOf('zohoworkdrive')).toBe('Zoho WorkDrive');
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

    it('parentCompany groups multi-product parents (Ehud #274 optional Company column)', () => {
        // Microsoft family: OneDrive, Azure Blob, GitHub all sort under "Microsoft".
        for (const name of ['Microsoft OneDrive', 'Microsoft Azure Blob', 'GitHub']) {
            const c = PROVIDER_CATALOG.find(x => x.company === name)!;
            expect(c.parentCompany, name).toBe('Microsoft');
            expect(catalogParentKey(c)).toBe('Microsoft');
        }
        // Google Drive + Google Cloud Storage.
        expect(PROVIDER_CATALOG.find(c => c.company === 'Google Drive')!.parentCompany).toBe('Google');
        expect(PROVIDER_CATALOG.find(c => c.company === 'Google Cloud Storage')!.parentCompany).toBe('Google');
        // Yandex pair, kDrive under Infomaniak (product name differs from company).
        expect(PROVIDER_CATALOG.find(c => c.company === 'Yandex Disk')!.parentCompany).toBe('Yandex');
        expect(PROVIDER_CATALOG.find(c => c.company === 'Yandex Object Storage')!.parentCompany).toBe('Yandex');
        expect(PROVIDER_CATALOG.find(c => c.company === 'kDrive')!.parentCompany).toBe('Infomaniak');
        // Brand-as-product rows leave parent unset so the optional column stays sparse.
        expect(PROVIDER_CATALOG.find(c => c.company === 'Dropbox')!.parentCompany).toBeUndefined();
        expect(catalogParentKey(PROVIDER_CATALOG.find(c => c.company === 'Dropbox')!)).toBe('Dropbox');
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
        // AWS is deliberately NOT here: its S3 5 GB is the 12-month new-account tier,
        // not an always-free allowance, so it belongs in the paid-only list below.
        for (const name of ['Microsoft Azure Blob', 'Yandex Object Storage', 'Oracle Cloud', 'Google Cloud Storage', 'Cloudflare R2', 'Alibaba OSS']) {
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
        // AWS: S3's 5 GB sits in the '12 Months Free' bucket, not the '30+ always free'
        // one, so the allowance expires with the account's first year. New accounts
        // now get up-to-$200 credits instead. Either way there is no ongoing free tier.
        for (const name of ['Wasabi', 'DigitalOcean Spaces', 'Hetzner Storage Box', 'Tencent COS', 'Storj', 'IDrive e2', 'Amazon Web Services (AWS)']) {
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
        // Free + card, never leak into Paid nor vanish from Free + card. Google
        // Cloud Storage carries this case now: its 5 GB-month always-free quota is
        // real, unlike AWS's S3 allowance, which is the 12-month new-account tier.
        const gcs = PROVIDER_CATALOG.find(x => x.company === 'Google Cloud Storage');
        expect(gcs, 'Google Cloud Storage present').toBeDefined();
        expect(companyTierInCategory(gcs!, 'object-storage'), 'GCS under S3 tab').toBe('free-card');
        // AWS is the counter-example: no ongoing free tier, so it is plain paid.
        const aws = PROVIDER_CATALOG.find(x => x.company === 'Amazon Web Services (AWS)');
        expect(aws, 'AWS present').toBeDefined();
        expect(companyTierInCategory(aws!, 'object-storage'), 'AWS under S3 tab').toBe('paid');
    });

    it('freeRequiresCard companies are never reported as paid-only by the CLI projection', () => {
        const cli = buildCliCatalog();
        // AWS dropped out of this list: its S3 allowance is the 12-month tier, not
        // an always-free one, so it no longer has a card-gated free tier to protect.
        for (const name of ['Microsoft Azure Blob', 'Yandex Object Storage', 'Oracle Cloud', 'Google Cloud Storage', 'Cloudflare R2', 'Alibaba OSS']) {
            const row = cli.find(c => c.company === name)!;
            expect(row.freeRequiresCard, `${name} CLI freeRequiresCard`).toBe(true);
        }
    });
});
