// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';

/**
 * Every trash manager acts on the connection it was opened for: App reads
 * `provider_connection_generation` when it opens one and passes it down, and
 * every trash command the manager sends carries it, so the backend refuses a
 * restore, delete or empty that would land on another connection.
 */
const MANAGERS = [
    'AzureTrashManager', 'BoxTrashManager', 'DropboxTrashManager', 'FileLuTrashManager',
    'GoogleDriveTrashManager', 'InternxtTrashManager', 'JottacloudTrashManager', 'KDriveTrashManager',
    'KoofrTrashManager', 'MegaTrashManager', 'OneDriveTrashManager', 'OpenDriveTrashManager',
    'PCloudTrashManager', 'S3TrashManager', 'YandexTrashManager', 'ZohoTrashManager', 'B2HiddenManager',
];
const read = (path: string) => readFileSync(resolve(__dirname, path), 'utf8');

/** Each `invoke(...)` call in a source, from its name to its closing paren. */
function invokeCalls(source: string): string[] {
    const calls: string[] = [];
    const start = /\binvoke(?:<[^(]*>)?\(/g;
    let match: RegExpExecArray | null;
    while ((match = start.exec(source)) !== null) {
        let depth = 1;
        let i = match.index + match[0].length;
        for (; i < source.length && depth > 0; i++) {
            if (source[i] === '(') depth++;
            else if (source[i] === ')') depth--;
        }
        calls.push(source.slice(match.index, i));
    }
    return calls;
}

describe.each(MANAGERS)('the %s', (name) => {
    const source = read(`./${name}.tsx`);

    it('takes the generation as a prop', () => {
        expect(source).toMatch(/\n {2}generation: number;\n/);
        expect(source).toMatch(new RegExp(`export function ${name}\\(\\{ generation,`));
    });

    it('sends it with every command', () => {
        const calls = invokeCalls(source);
        expect(calls.length).toBeGreaterThan(0);
        for (const call of calls) {
            expect(call, call).toMatch(/\{\s*generation[,\s}]/);
        }
    });

    it('is opened by App at a generation it read, and rendered with it', () => {
        const app = read('../App.tsx');
        const render = new RegExp(`trashGeneration !== null && \\(\\n\\s*<${name}\\n\\s*key=\\{trashGeneration\\}\\n\\s*generation=\\{trashGeneration\\}`);
        expect(app).toMatch(render);
    });
});

describe('App opening a trash manager', () => {
    const app = read('../App.tsx');

    it('reads the generation first, and only the latest open shows its manager', () => {
        const open = app.slice(app.indexOf('const openTrashManager'), app.indexOf('}, [notify, t]);'));
        expect(open).toContain("await invoke<number>('provider_connection_generation')");
        expect(open).toContain('if (seq !== trashOpenSeq.current) return;');
        expect(open.indexOf('setTrashGeneration(generation)')).toBeLessThan(open.indexOf('show();'));
    });

    it('opens every manager that has a button through it', () => {
        const map = app.slice(app.indexOf('const trashMap: Record<string, () => void> = {'), app.indexOf('return handler ? ('));
        const opens = map.match(/setShow\w+\(true\)/g) ?? [];
        // Nextcloud binds its generation in its own change.
        const direct = opens.filter((o) => !map.includes(`openTrashManager(() => ${o})`) && !o.startsWith('setShowNextcloud'));
        expect(direct).toEqual([]);
        expect(opens.length).toBeGreaterThanOrEqual(15);
    });
});
