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

    it('treats a template interpolation as the code it is', () => {
        // `${...}` is not literal text: a comment in there is a real comment,
        // and staying in template mode across it leaves the comment standing,
        // which is the false positive a comment-stripping scan exists to stop.
        const src = 'const a = `x ${ y /* drop me */ } z`; const after = 1;\n';
        const out = withoutComments(src);
        expect(out, 'the comment inside the interpolation').not.toContain('drop me');
        expect(out, 'the literal text around it survives').toContain('x $');
        expect(out).toContain('z`');
        expect(out).toContain('const after = 1;');
        expect(out.length).toBe(src.length);
    });

    it('does not let a brace inside an interpolation close it early', () => {
        // An object literal or a block inside `${}` has braces of its own.
        const src = 'const a = `${ f({ k: 1 }) } // not a comment`; const b = 2;\n';
        const out = withoutComments(src);
        expect(out, 'still inside the template after the object literal').toContain('// not a comment');
        expect(out).toContain('const b = 2;');
    });

    it('handles a template nested inside an interpolation', () => {
        const src = 'const a = `${ `inner ${ q } // deep` } // outer`; const c = 3;\n';
        const out = withoutComments(src);
        expect(out, 'both // are literal text, not comments').toContain('// deep');
        expect(out).toContain('// outer');
        expect(out).toContain('const c = 3;');
    });

    it('does not read the slashes inside a regex literal as a comment', () => {
        // `/https?:\\/\\//` ends in two adjacent slashes. Without a regex state
        // the scan called them a `//` and blanked the rest of the line, so every
        // span that ran past such a regex came back truncated and the pins
        // reading it asserted about text that was no longer there.
        const src = 'const re = /https?:\\/\\//; const after = 1;\n';
        const out = withoutComments(src);
        expect(out, 'code after a regex ending in two slashes').toContain('const after = 1;');
        expect(out.length).toBe(src.length);
    });

    it('does not end a regex on a slash inside a character class', () => {
        const src = 'const re = /[/]x/; const after = 2; // gone\nconst tail = 3;\n';
        const out = withoutComments(src);
        expect(out).toContain('const after = 2;');
        expect(out).not.toContain('gone');
        expect(out).toContain('const tail = 3;');
    });

    it('still reads division as division', () => {
        // The reverse mistake costs more than the one it fixes: calling a
        // division a regex hides real code up to the next slash.
        const src = 'const r = total / count; // note\nconst x = (a) / 2; const y = 1;\n';
        const out = withoutComments(src);
        expect(out).not.toContain('note');
        expect(out).toContain('const y = 1;');
    });

    it('keeps JSX slashes out of it', () => {
        // `/>` closes an element and `</` opens a closing tag; reading either as
        // a regex start swallows everything up to the next slash in the file.
        const src = '<Row label="x" /><Wrap>{value}</Wrap> // note\nconst after = 4;\n';
        const out = withoutComments(src);
        expect(out).toContain('<Row label="x" />');
        expect(out).toContain('</Wrap>');
        expect(out).not.toContain('note');
        expect(out).toContain('const after = 4;');
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

    it('reads a tag whose prop holds a regex', () => {
        const src = '<Row match={/a\\/\\//} label="x" />';
        const tag = jsxTagAt(src, 0)!;
        expect(tag, "the outer tag's own last prop").toContain('label="x"');
        expect(tag.endsWith('/>')).toBe(true);
    });

    it('takes the tag name literally instead of as a pattern', () => {
        // A JSX member expression is a legal name. Interpolated unescaped, the
        // `.` matches any character, so `Menu.Item` also answers for `MenuXItem`
        // and the assertion is served by the wrong element.
        const src = '<MenuXItem a={1} /><Menu.Item b={2} />';
        const tags = jsxTags(src, 'Menu.Item');
        expect(tags).toHaveLength(1);
        expect(tags[0]).toContain('b={2}');
        expect(jsxTagContaining(src, 'Menu.Item', 'b={2}')).toContain('b={2}');
        expect(jsxTagContaining(src, 'Menu.Item', 'a={1}')).toBeNull();
    });

    it('does not throw on a name carrying regex metacharacters', () => {
        expect(() => jsxTags('<Row a={1} />', 'Item[0]')).not.toThrow();
        expect(jsxTags('<Row a={1} />', 'Item[0]')).toEqual([]);
    });

    it('returns null for a tag that never closes', () => {
        expect(jsxTagAt('<Broken prop={', 0)).toBeNull();
        expect(jsxTagContaining('<Grid files={x} />', 'Grid', 'nope')).toBeNull();
    });
});
