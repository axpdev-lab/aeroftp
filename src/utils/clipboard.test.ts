import { describe, expect, it } from 'vitest';
import { withoutComments } from './jsxTag';

// Every source file under src, as text. Tests are left out: they may name the
// web API on purpose.
const sources = import.meta.glob(['../**/*.ts', '../**/*.tsx', '!../**/*.test.ts', '!../**/*.test.tsx'], {
    query: '?raw',
    import: 'default',
    eager: true,
}) as Record<string, string>;

// Any use of the web clipboard, read or write, in any spelling: a bound
// `navigator.clipboard`, a bracket access, a destructured `{ clipboard }`, a
// call split over lines, the Tauri clipboard plugin. Matching `.writeText(`
// on one line missed all of them, and any line that also said `copyText`
// (pre-release review of 4.2.1, L20).
const WEB_CLIPBOARD = [
    /\bnavigator\s*(?:\?\.|\.)\s*clipboard\b/g,
    /\bnavigator\s*(?:\?\.)?\s*\[\s*['"`]clipboard['"`]\s*\]/g,
    /\{[^{}]*\bclipboard\b[^{}]*\}\s*=\s*(?:(?:window|globalThis)\s*\.\s*)?navigator\b/g,
    /['"`]@tauri-apps\/plugin-clipboard-manager['"`]/g,
];

/** The lines of `text` that use the web clipboard. Comments are blanked by the
 *  shared, string-aware `withoutComments`: a comment may name the API, and a
 *  regex that hunts for `//` finds it inside a literal. Line numbers are kept. */
function webClipboardLines(text: string): number[] {
    const code = withoutComments(text);
    return WEB_CLIPBOARD.flatMap(pattern =>
        [...code.matchAll(pattern)].map(m => code.slice(0, m.index).split('\n').length),
    );
}

describe('clipboard writes', () => {
    it('reads a real source tree', () => {
        // A glob that matched nothing would make the next test pass on nothing.
        expect(Object.keys(sources).length).toBeGreaterThan(100);
    });

    it('go through copyText, never straight to navigator.clipboard', () => {
        const offenders = Object.entries(sources)
            .filter(([path]) => path !== './clipboard.ts')
            .flatMap(([path, text]) => webClipboardLines(text).map(n => `${path}:${n}`));
        expect(offenders).toEqual([]);
    });

    it('the web clipboard check sees every spelling it is meant to see', () => {
        // The check above passes on a clean tree, which is also what a blind
        // check does: held against each spelling it must flag, it flags it,
        // and it leaves a comment that names the API alone.
        for (const code of [
            'const c = navigator.clipboard;',
            'await navigator?.clipboard?.readText();',
            "const f = navigator['clipboard'];",
            'await navigator\n    .clipboard.writeText(t);',
            'const { clipboard } = navigator;',
            'const { clipboard: c } = window.navigator;',
            "import { writeText } from '@tauri-apps/plugin-clipboard-manager';",
            'navigator.clipboard.writeText(t); // copyText',
        ]) {
            expect(webClipboardLines(code), code).not.toEqual([]);
        }
        for (const code of [
            '// WebKitGTK rejects navigator.clipboard here',
            '/**\n * `navigator.clipboard` is only the fallback\n */',
            "const url = 'https://example.org/'; // navigator.clipboard",
        ]) {
            expect(webClipboardLines(code), code).toEqual([]);
        }
    });

    it('never call the native command directly, so a failure falls back and is reported once', () => {
        // A direct call skipped the web fallback, and a site that retried
        // through copyText after it ran the native command twice.
        const offenders = Object.entries(sources)
            .filter(([path]) => path !== './clipboard.ts')
            .flatMap(([path, text]) =>
                text
                    .split('\n')
                    .map((line, i) => ({ line, n: i + 1 }))
                    .filter(({ line }) => /\binvoke\s*(<[^>]*>)?\s*\(\s*['"]copy_to_clipboard['"]/.test(line))
                    .map(({ n }) => `${path}:${n}`),
            );
        expect(offenders).toEqual([]);
    });
});
