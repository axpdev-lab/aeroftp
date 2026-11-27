// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import {
    URL_SCHEME_SUGGESTIONS,
    applyTemplate,
    filterSuggestions,
    hasInlinePassword,
    nextPlaceholder,
    placeholderRanges,
    schemeOfValue,
    suggestProfileName,
    suggestionForValue,
} from './connectUrlSchemes';

describe('filterSuggestions', () => {
    it('shows the first entries when the field is empty', () => {
        const first = filterSuggestions('');
        expect(first.length).toBe(6);
        expect(first[0].scheme).toBe('sftp');
    });

    it('matches a scheme prefix first, at most max entries', () => {
        const found = filterSuggestions('web');
        expect(found[0].scheme).toBe('webdav');
        expect(found[1].scheme).toBe('webdavs');
        expect(filterSuggestions('s', 3).length).toBeLessThanOrEqual(3);
    });

    it('matches typed fragments like sft, s3, pro, gdr, ft', () => {
        expect(filterSuggestions('sft')[0].scheme).toBe('sftp');
        expect(filterSuggestions('s3')[0].scheme).toBe('s3');
        expect(filterSuggestions('pro')[0].scheme).toBe('proton');
        expect(filterSuggestions('gdr')[0].scheme).toBe('googledrive');
        expect(filterSuggestions('ft')[0].scheme).toBe('ftp');
    });

    it('ignores a scheme the user already typed after ://', () => {
        expect(filterSuggestions('sftp://exa')[0].scheme).toBe('sftp');
    });

    it('matches a label substring', () => {
        expect(filterSuggestions('drive').map((s) => s.scheme)).toContain('googledrive');
    });

    it('covers the non-alias schemes of launch_url.rs', () => {
        const schemes = URL_SCHEME_SUGGESTIONS.map((s) => s.scheme);
        for (const expected of ['ftp', 'ftps', 'sftp', 'scp', 'webdav', 'webdavs', 's3', 'mega', 'proton', 'azure', 'filen', 'github', 'gitlab', 'googledrive', 'dropbox', 'onedrive', 'box', 'pcloud', 'twake']) {
            expect(schemes).toContain(expected);
        }
        // Aliases stay unlisted, like in the backend's help text.
        expect(schemes).not.toContain('ssh');
        expect(schemes).not.toContain('b2');
        expect(schemes).not.toContain('drive');
    });
});

describe('placeholderRanges / applyTemplate / nextPlaceholder', () => {
    const template = 'sftp://{user}@{host}/{path}';

    it('finds every placeholder as an offset range', () => {
        expect(placeholderRanges(template)).toEqual([
            { start: 8, end: 12, name: 'user' },
            { start: 15, end: 19, name: 'host' },
            { start: 22, end: 26, name: 'path' },
        ]);
        expect(placeholderRanges('gdrive://{account}')[0]).toEqual({ start: 10, end: 17, name: 'account' });
        expect(placeholderRanges('plain://host')).toEqual([]);
    });

    it('Tab on a fresh field inserts the template and selects the first placeholder', () => {
        const applied = applyTemplate(template);
        expect(applied.value).toBe(template);
        expect(applied.selection).toEqual({ start: 8, end: 12, name: 'user' });
    });

    it('Tab jumps to the next placeholder and then past the last one', () => {
        expect(nextPlaceholder(template, 8)?.name).toBe('host');
        expect(nextPlaceholder(template, 15)?.name).toBe('path');
        expect(nextPlaceholder(template, 22)).toBeNull();
        expect(nextPlaceholder(template, 0)?.name).toBe('user');
    });
});

describe('schemeOfValue', () => {
    it('reads the scheme case-insensitively', () => {
        expect(schemeOfValue('SFTP://host')).toBe('sftp');
        expect(schemeOfValue('  webdavs://host/dav ')).toBe('webdavs');
        expect(schemeOfValue('sftp')) .toBeNull();
        expect(schemeOfValue('')).toBeNull();
    });
});

describe('hasInlinePassword', () => {
    it('detects an inline user:password@ credential', () => {
        expect(hasInlinePassword('sftp://bob:secret@example.com/')).toBe(true);
        expect(hasInlinePassword('ftp://user:p%40ss@host')).toBe(true);
        expect(hasInlinePassword('sftp://bob@example.com/')).toBe(false);
        expect(hasInlinePassword('sftp://bob@host')).toBe(false);
        expect(hasInlinePassword('not a url')).toBe(false);
    });
});

describe('suggestProfileName', () => {
    it('prefers user@host, then host, then account; S3 uses the bucket', () => {
        expect(suggestProfileName('sftp://bob@example.com:2222/reports')).toBe('bob@example.com');
        expect(suggestProfileName('webdavs://files.example.com/dav')).toBe('files.example.com');
        expect(suggestProfileName('s3://AKIAKEY@my-bucket?region=eu-1')).toBe('my-bucket');
        expect(suggestProfileName('proton://work')).toBe('work');
        expect(suggestProfileName('')).toBe('');
    });
});

describe('suggestionForValue', () => {
    it('resolves aliases like gdrive', () => {
        expect(suggestionForValue('gdrive://')?.scheme).toBe('googledrive');
        expect(suggestionForValue('sftp://h')?.signIn).toBeUndefined();
        expect(suggestionForValue('gdrive://')?.signIn).toBe(true);
        expect(suggestionForValue('nope')).toBeNull();
    });
});
