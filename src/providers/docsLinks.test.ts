import { describe, expect, it } from 'vitest';
import { PROVIDER_CATALOG, PROVIDER_GRID } from '../components/providerCatalog';
import { getProviderDocsUrl } from './docsLinks';

describe('Quick Connect docs links', () => {
    // Methods with a docs page of their own, apart from the company's page.
    const METHOD_PAGES: Record<string, string> = {
        'mega-s4': 'providers/mega-s4',
        'filen-desktop-s3': 'providers/filen-desktop',
        'filen-desktop-webdav': 'providers/filen-desktop',
    };

    // Pre-release review of 4.2.1 (L18): IBM COS resolved to the AWS S3 page
    // (the plain `s3` protocol fallback), Twake and Mail.ru to no page at
    // all; Backblaze and Quotaless over S3, and the WebDAV presets of 4shared
    // and Quotaless, had the same gap. Every company the provider grid links
    // to a docs page opens that page from each of its Quick Connect methods,
    // or the method's own page.
    it('every method of a grid provider opens the page its grid tile links to', () => {
        const mismatches: string[] = [];
        for (const tile of PROVIDER_GRID) {
            if (!tile.docsPath.startsWith('providers/')) continue;
            const company = PROVIDER_CATALOG.find(c => c.logoId === tile.logoId);
            expect(company, `${tile.logoId} has no catalog entry`).toBeDefined();
            for (const method of company!.protocols) {
                const url = getProviderDocsUrl(method.providerId ?? method.protocol, method.protocol);
                const page = (method.providerId && METHOD_PAGES[method.providerId]) || tile.docsPath;
                const expected = `https://docs.aeroftp.app/${page}`;
                if (url !== expected) {
                    mismatches.push(`${tile.logoId} via ${method.providerId ?? method.protocol}: ${url} (expected ${expected})`);
                }
            }
        }
        expect(mismatches).toEqual([]);
    });
});
