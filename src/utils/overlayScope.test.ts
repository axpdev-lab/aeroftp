import { describe, it, expect } from 'vitest';
import { normalizeRemotePath, isValidOverlayScope } from './overlayScope';

describe('normalizeRemotePath', () => {
    it('normalizes root variants to empty', () => {
        expect(normalizeRemotePath('')).toBe('');
        expect(normalizeRemotePath('/')).toBe('');
        expect(normalizeRemotePath('  /  ')).toBe('');
    });

    it('strips trailing slash and collapses duplicates', () => {
        expect(normalizeRemotePath('/data/')).toBe('/data');
        expect(normalizeRemotePath('/data//vault/')).toBe('/data/vault');
        expect(normalizeRemotePath('data/vault')).toBe('/data/vault');
    });
});

describe('isValidOverlayScope', () => {
    it('accepts a blank field always (means same as Remote Path)', () => {
        expect(isValidOverlayScope('', '/data')).toBe(true);
        expect(isValidOverlayScope('   ', '/data')).toBe(true);
        expect(isValidOverlayScope('', '')).toBe(true);
    });

    it('treats "/" as the remote root, not a blank field', () => {
        // "/" is the root: valid only when the Remote Path is also the root.
        expect(isValidOverlayScope('/', '/')).toBe(true);
        expect(isValidOverlayScope('/', '')).toBe(true);
        // "/" against a subfolder Remote Path is an ancestor: rejected. This is
        // exactly the #386 misconfiguration the field must prevent (anchoring the
        // overlay at the whole remote makes the root listing decrypt-or-drop).
        expect(isValidOverlayScope('/', '/data')).toBe(false);
    });

    it('accepts exact match', () => {
        expect(isValidOverlayScope('/data', '/data')).toBe(true);
        expect(isValidOverlayScope('/vault', '/vault')).toBe(true);
    });

    it('accepts strict descendant', () => {
        expect(isValidOverlayScope('/data/vault', '/data')).toBe(true);
        expect(isValidOverlayScope('/data/vault/sub', '/data')).toBe(true);
    });

    it('accepts any descendant when remote is root', () => {
        expect(isValidOverlayScope('/vault', '/')).toBe(true);
        expect(isValidOverlayScope('/a/b', '')).toBe(true);
    });

    it('rejects ancestor', () => {
        expect(isValidOverlayScope('/', '/data')).toBe(false);
        expect(isValidOverlayScope('/data', '/data/vault')).toBe(false);
    });

    it('rejects sibling', () => {
        expect(isValidOverlayScope('/other', '/data')).toBe(false);
    });

    it('rejects prefix trap (/database vs /data)', () => {
        expect(isValidOverlayScope('/database', '/data')).toBe(false);
        expect(isValidOverlayScope('/data2', '/data')).toBe(false);
    });

    it('rejects unrelated', () => {
        expect(isValidOverlayScope('/foo/bar', '/baz')).toBe(false);
    });
});
