import { describe, expect, it } from 'vitest';
import { sharesFoldersByLink, supportsNativeShareLink } from './types';

describe('supportsNativeShareLink', () => {
    it('does not offer native Filen links while creation fails closed', () => {
        expect(supportsNativeShareLink('filen')).toBe(false);
    });

    it('keeps native links for providers that still create them', () => {
        expect(supportsNativeShareLink('pcloud')).toBe(true);
        expect(supportsNativeShareLink('dropbox')).toBe(true);
    });
});

describe('sharesFoldersByLink', () => {
    it('offers no folder link on an object store, where it would sign a same-named object', () => {
        for (const store of ['s3', 'backblaze', 'azure'] as const) {
            expect(sharesFoldersByLink(store)).toBe(false);
        }
    });

    it('keeps folder links where the provider shares a folder', () => {
        expect(sharesFoldersByLink('dropbox')).toBe(true);
        expect(sharesFoldersByLink('googledrive')).toBe(true);
    });
});
