import { describe, it, expect } from 'vitest';
import { normalizeRemotePath, isValidOverlayScope, resolveOverlayScope } from './overlayScope';

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

    it('resolves dot segments without escaping above root', () => {
        expect(normalizeRemotePath('/data/vault/../sibling')).toBe('/data/sibling');
        expect(normalizeRemotePath('/data/./vault')).toBe('/data/vault');
        expect(normalizeRemotePath('../../vault')).toBe('/vault');
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

    it('rejects dot-segment traversal outside the Remote Path', () => {
        expect(isValidOverlayScope('/data/vault/../sibling', '/data/vault')).toBe(false);
    });

    it('rejects prefix trap (/database vs /data)', () => {
        expect(isValidOverlayScope('/database', '/data')).toBe(false);
        expect(isValidOverlayScope('/data2', '/data')).toBe(false);
    });

    it('rejects unrelated', () => {
        expect(isValidOverlayScope('/foo/bar', '/baz')).toBe(false);
    });
});

describe('resolveOverlayScope (#369 relative UX)', () => {
    it('blank or "/" resolves to the Remote Path itself', () => {
        expect(resolveOverlayScope('', '/data')).toBe('/data');
        expect(resolveOverlayScope('   ', '/data')).toBe('/data');
        expect(resolveOverlayScope('/', '/data')).toBe('/data');
        expect(resolveOverlayScope('/', '/data/')).toBe('/data');
    });

    it('nests a bare subfolder name under the Remote Path (no prefix re-typing)', () => {
        expect(resolveOverlayScope('folder-try', '/data')).toBe('/data/folder-try');
        expect(resolveOverlayScope('sub/deep', '/data')).toBe('/data/sub/deep');
        expect(resolveOverlayScope('vault', '/data/')).toBe('/data/vault');
    });

    it('nests instead of erroring when the input looks like a sibling absolute path', () => {
        // The old field rejected "/folder-try"; now it is treated as a subfolder.
        expect(resolveOverlayScope('/folder-try', '/data')).toBe('/data/folder-try');
        expect(resolveOverlayScope('/other', '/data')).toBe('/data/other');
    });

    it('neutralizes dot-segment traversal by treating it as relative input', () => {
        expect(resolveOverlayScope('../sibling', '/data/vault')).toBe('/data/vault/sibling');
        expect(resolveOverlayScope('/data/vault/../sibling', '/data/vault')).toBe('/data/vault/data/sibling');
    });

    it('keeps an already-absolute in-scope path verbatim', () => {
        expect(resolveOverlayScope('/data', '/data')).toBe('/data');
        expect(resolveOverlayScope('/data/vault', '/data')).toBe('/data/vault');
        expect(resolveOverlayScope('/data/vault/sub', '/data')).toBe('/data/vault/sub');
    });

    it('treats input as absolute when the Remote Path is the root', () => {
        expect(resolveOverlayScope('vault', '/')).toBe('/vault');
        expect(resolveOverlayScope('/a/b', '')).toBe('/a/b');
        expect(resolveOverlayScope('', '/')).toBe('');
    });

    it('always yields an in-scope result (never fails validation)', () => {
        const remotes = ['/data', '/data/vault', '/', ''];
        const inputs = ['', '/', 'sub', 'a/b/c', '/other', '/data', '/data/x', '  spaced  '];
        for (const r of remotes) {
            for (const i of inputs) {
                expect(isValidOverlayScope(resolveOverlayScope(i, r), r)).toBe(true);
            }
        }
    });
});

// #369: with a bound overlay the anchor is pinned and the Remote Path becomes
// editable. The same predicate answers the mirrored question: does this Remote
// Path still reach the pinned anchor? The form blocks the save when it does not.
describe('editing the Remote Path under a pinned anchor', () => {
    const reaches = (anchor: string, remotePath: string) => isValidOverlayScope(anchor, remotePath);

    it('accepts the anchor itself and any of its parents', () => {
        expect(reaches('/Private/vault', '/Private/vault')).toBe(true);
        expect(reaches('/Private/vault', '/Private')).toBe(true);
        expect(reaches('/Private/vault', '/')).toBe(true);
        expect(reaches('/Private/vault', '')).toBe(true);
    });

    it('rejects a sibling or an unrelated path that leaves the anchor outside', () => {
        expect(reaches('/Private/vault', '/Common documents')).toBe(false);
        expect(reaches('/Private/vault', '/Private/other')).toBe(false);
        expect(reaches('/Private/vault', '/Privateer')).toBe(false);
    });

    it('normalizes both sides before deciding', () => {
        expect(reaches('/Private/vault', '/Private/')).toBe(true);
        expect(reaches('/Private/vault', '//Private//')).toBe(true);
        expect(reaches('/Private/vault', '/Private/sub/..')).toBe(true);
    });
});
