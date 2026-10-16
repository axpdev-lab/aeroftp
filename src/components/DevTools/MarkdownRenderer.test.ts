import { describe, expect, it } from 'vitest';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { MarkdownRenderer } from './MarkdownRenderer';

const render = (content: string) => renderToStaticMarkup(createElement(MarkdownRenderer, { content }));

/** The `style` attribute of every `<tag>` in document order, '' when it has none. */
const cellStyles = (html: string, tag: 'th' | 'td') =>
    [...html.matchAll(new RegExp(`<${tag}\\b([^>]*)>`, 'g'))].map(m => /style="([^"]*)"/.exec(m[1])?.[1] ?? '');

describe('MarkdownRenderer tables', () => {
    const table = [
        '| Default | Left | Center | Right |',
        '|---------|:-----|:------:|------:|',
        '| a | first | middle | 1 |',
        '| bb | second | mid | 22 |',
    ].join('\n');

    it('keeps the alignment each column declares, on the header and on every cell', () => {
        const html = render(table);
        expect(cellStyles(html, 'th')).toEqual(['', 'text-align:left', 'text-align:center', 'text-align:right']);
        expect(cellStyles(html, 'td')).toEqual([
            '', 'text-align:left', 'text-align:center', 'text-align:right',
            '', 'text-align:left', 'text-align:center', 'text-align:right',
        ]);
    });

    it('leaves a table that declares nothing on the default alignment', () => {
        const html = render('| A | B |\n|---|---|\n| 1 | 2 |');
        expect(cellStyles(html, 'th')).toEqual(['', '']);
        expect(cellStyles(html, 'td')).toEqual(['', '']);
    });
});
