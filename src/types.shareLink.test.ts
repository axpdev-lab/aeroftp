import { describe, expect, it } from 'vitest';
import { supportsNativeShareLink } from './types';

describe('supportsNativeShareLink', () => {
    it('does not offer native Filen links while creation fails closed', () => {
        expect(supportsNativeShareLink('filen')).toBe(false);
    });

    it('keeps native links for providers that still create them', () => {
        expect(supportsNativeShareLink('pcloud')).toBe(true);
        expect(supportsNativeShareLink('dropbox')).toBe(true);
    });
});
