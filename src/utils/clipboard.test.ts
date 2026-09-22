import { describe, expect, it } from 'vitest';

// Every source file under src, as text. Tests are left out: they may name the
// web API on purpose.
const sources = import.meta.glob(['../**/*.ts', '../**/*.tsx', '!../**/*.test.ts', '!../**/*.test.tsx'], {
    query: '?raw',
    import: 'default',
    eager: true,
}) as Record<string, string>;

describe('clipboard writes', () => {
    it('reads a real source tree', () => {
        // A glob that matched nothing would make the next test pass on nothing.
        expect(Object.keys(sources).length).toBeGreaterThan(100);
    });

    it('go through copyText, never straight to navigator.clipboard', () => {
        const offenders = Object.entries(sources)
            .filter(([path]) => path !== './clipboard.ts')
            .flatMap(([path, text]) =>
                text
                    .split('\n')
                    .map((line, i) => ({ line, n: i + 1 }))
                    // `?.` included: one site wrote `navigator.clipboard\n?.writeText`.
                    .filter(({ line }) => /\.writeText\s*\(/.test(line) && !/copyText/.test(line))
                    .map(({ n }) => `${path}:${n}`),
            );
        expect(offenders).toEqual([]);
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
