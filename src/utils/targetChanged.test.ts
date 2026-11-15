// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import { readdirSync, readFileSync, statSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';
import TABLE from '../shared/target-changed-cases.json';
import { BULK_TARGET_CHANGED } from './remoteSyncRunner';
import { isTargetChangedRefusal, TARGET_CHANGED_MARKER } from './targetChanged';

const sourcesUnder = (dir: string): string[] => readdirSync(dir).flatMap((name) => {
    const path = join(dir, name);
    return statSync(path).isDirectory() ? sourcesUnder(path) : [path];
});

describe('a changed-target refusal', () => {
    it('is read as the shared table says, the same as the backend reads it', () => {
        expect(TABLE.cases.length).toBeGreaterThan(20);
        for (const row of TABLE.cases as { text: string; path?: string; refusal: boolean }[]) {
            expect(isTargetChangedRefusal(row.text, row.path), `${row.path ?? '(no path)'}: ${row.text}`).toBe(row.refusal);
        }
    });

    it('is found in a thrown Error and in a rejected string alike', () => {
        const refusal = `${TARGET_CHANGED_MARKER} the connection changed since the permissions were read; nothing was changed`;
        expect(isTargetChangedRefusal(refusal)).toBe(true);
        expect(isTargetChangedRefusal(new Error(refusal))).toBe(true);
        expect(isTargetChangedRefusal(new Error(`chmod failed: ${refusal}`))).toBe(true);
        expect(isTargetChangedRefusal(undefined)).toBe(false);
        expect(isTargetChangedRefusal(new Error(`Permission denied on /srv/${TARGET_CHANGED_MARKER} notes`))).toBe(false);
    });

    it('is never written without its colon by the window', () => {
        // The word with no colon is no refusal (the shared table's row), so a
        // window source that wrote it would be read as an ordinary failure.
        expect(TARGET_CHANGED_MARKER.endsWith(':')).toBe(true);
        const files = sourcesUnder('src').filter((path) => /\.(ts|tsx)$/.test(path) && !/\.test\.|testkit/.test(path));
        expect(files.length).toBeGreaterThan(100);
        const bare = files.flatMap((path) => readFileSync(path, 'utf8').split('\n').flatMap((line, n) =>
            /(['"`])CONNECTION_CHANGED(?!:)/.test(line) ? [`${path}:${n + 1}: ${line.trim()}`] : []));
        expect(bare).toEqual([]);
        // The bulk path's text is a row both readers run.
        expect(TABLE.cases).toContainEqual({ text: BULK_TARGET_CHANGED, refusal: true });
    });
});
