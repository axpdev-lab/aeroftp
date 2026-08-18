// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import { jsxTagAt, jsxTagContaining, jsxTags, withoutComments } from './jsxTag';

/**
 * The helper several source-scanning pins now depend on, so its own failure mode
 * is a false green in all of them.
 */
describe('withoutComments', () => {
    it('preserves every offset, so line numbers and indices still line up', () => {
        const src = 'const a = 1; // note\nconst b = 2;\n';
        const out = withoutComments(src);
        expect(out.length).toBe(src.length);
        expect(out.split('\n').length).toBe(src.split('\n').length);
        expect(out).toContain('const b = 2;');
        expect(out).not.toContain('note');
    });

    it('leaves comment delimiters that are inside a string literal alone', () => {
        // A regex hunting for `//` finds it here and blanks the rest of the line,
        // deleting the very code an assertion was about.
        const src = `const u = "https://x/y"; const keep = 1;\nconst s = 'a // b'; const also = 2;\nconst t = \`x /* y \`; const third = 3;\n`;
        const out = withoutComments(src);
        expect(out, 'code after a URL').toContain('const keep = 1;');
        expect(out, 'code after a // inside a single-quoted string').toContain('const also = 2;');
        expect(out, 'code after a /* inside a template').toContain('const third = 3;');
        expect(out, 'the literals themselves are not comments').toContain('https://x/y');
    });

    it('ends a literal on an even backslash run and not on an odd one', () => {
        // `"C:\\"` closes: the two backslashes escape each other. Reading only the
        // previous character says it is escaped and swallows the rest of the file.
        const even = 'const p = "C:\\\\"; // gone\nconst after = 1;\n';
        expect(withoutComments(even)).toContain('const after = 1;');
        expect(withoutComments(even)).not.toContain('gone');

        // `"\\"" ` does not close on the escaped quote.
        const odd = 'const q = "a\\"// still in the string"; const tail = 2;\n';
        expect(withoutComments(odd)).toContain('const tail = 2;');
        expect(withoutComments(odd)).toContain('still in the string');
    });

    it('blanks real comments of both kinds', () => {
        const src = 'a; /* block\nspanning */ b; // line\nc;';
        const out = withoutComments(src);
        expect(out).not.toContain('block');
        expect(out).not.toContain('spanning');
        expect(out).not.toContain('line');
        expect(out).toContain('a;');
        expect(out).toContain('b;');
        expect(out).toContain('c;');
    });
});

describe('jsxTagAt / jsxTagContaining', () => {
    const nested = `
        <ProviderThumbnail
          path={file.path}
          signature={signatureOf(file.size, file.modified)}
          fallback={
            <ImageThumbnail path={other} signature={sigB} />
          }
          cacheScope={scope}
        />
    `;

    it('does not let a nested element end the tag that contains it', () => {
        // `<Name[\\s\\S]*?/>` stops at the inner element's `/>`, which returns the
        // outer tag's name carrying the inner tag's props.
        const tag = jsxTagAt(nested, nested.indexOf('<ProviderThumbnail'))!;
        expect(tag).toContain('fallback=');
        expect(tag.endsWith('/>')).toBe(true);
        expect(tag).toContain('<ImageThumbnail');
        expect(tag, "the outer tag's own last prop").toContain('cacheScope={scope}');
        // The naive match ends at the *inner* element's `/>`: it swallows the
        // inner props and never reaches the outer tag's own, so an assertion
        // about either element is answered by the other.
        const naive = nested.match(/<ProviderThumbnail[\s\S]*?\/>/)![0];
        expect(naive.length, 'the naive match stops early').toBeLessThan(tag.length);
        expect(naive, 'and never sees the outer tag reach its end').not.toContain('cacheScope={scope}');
    });

    it('finds both elements separately', () => {
        expect(jsxTags(nested, 'ProviderThumbnail')).toHaveLength(1);
        expect(jsxTags(nested, 'ImageThumbnail')).toHaveLength(1);
    });

    it('does not match an element whose name merely starts the same', () => {
        const src = '<Thumb a={1} /><ThumbList b={2} />';
        expect(jsxTags(src, 'Thumb')).toHaveLength(1);
        expect(jsxTags(src, 'Thumb')[0]).toContain('a={1}');
    });

    it('returns the element that owns the needle, not the one nearest to it', () => {
        const two = `
            <Grid files={local} isRemote={false} />
            <Grid files={remote} isRemote />
        `;
        const tag = jsxTagContaining(two, 'Grid', 'files={remote}')!;
        expect(tag).toContain('isRemote ');
        expect(tag, 'the sibling flag must not leak in').not.toContain('isRemote={false}');
    });

    it('is not fooled by a > inside a prop expression', () => {
        const src = '<Row render={(a) => a > 1} label="x" />';
        const tag = jsxTagAt(src, 0)!;
        expect(tag).toContain('label="x"');
        expect(tag.endsWith('/>')).toBe(true);
    });

    it('returns null for a tag that never closes', () => {
        expect(jsxTagAt('<Broken prop={', 0)).toBeNull();
        expect(jsxTagContaining('<Grid files={x} />', 'Grid', 'nope')).toBeNull();
    });
});
