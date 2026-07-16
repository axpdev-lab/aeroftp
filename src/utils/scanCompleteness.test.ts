// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
//
// CLAUDE-AV-B3-13: the frontend half of the incomplete-scan guard.

import { describe, expect, it } from 'vitest';
import {
    SCAN_INCOMPLETE_MARKER,
    describeScanIncompleteError,
    isScanIncompleteError,
} from './scanCompleteness';
import { compareEntries, type CompareInputEntry } from './compareEndpoints';
import { derivePresetPlan } from './syncPresets';

describe('isScanIncompleteError', () => {
    it('matches the marker through the wrapper the compare commands add', () => {
        // The commands wrap the walker error as `Failed to scan local directory: ...`,
        // and Tauri hands the frontend a string, not an Error.
        const wire = `Failed to scan local directory: ${SCAN_INCOMPLETE_MARKER}: the local scan of /mnt/backup did not see the whole tree`;
        expect(isScanIncompleteError(wire)).toBe(true);
        expect(isScanIncompleteError(new Error(wire))).toBe(true);
    });

    it('does not swallow unrelated failures, which keep their flat fallback', () => {
        expect(isScanIncompleteError('Connection reset by peer')).toBe(false);
        expect(isScanIncompleteError(null)).toBe(false);
        expect(isScanIncompleteError(undefined)).toBe(false);
    });
});

describe('describeScanIncompleteError', () => {
    it('strips the wire marker but keeps the explanation', () => {
        const wire = `Failed to scan local directory: ${SCAN_INCOMPLETE_MARKER}: the local scan of /mnt/backup did not see the whole tree (1 directory listing(s) failed).`;
        const shown = describeScanIncompleteError(wire);
        expect(shown).not.toContain(SCAN_INCOMPLETE_MARKER);
        expect(shown).toContain('/mnt/backup');
        expect(shown).toContain('did not see the whole tree');
    });

    it('passes an unmarked message through untouched', () => {
        expect(describeScanIncompleteError('boom')).toBe('boom');
    });
});

// The point of the whole guard: what the fail-closed arm hands the Compare tab
// must not be executable. These two assert the DoD directly, against the real
// preset planner rather than a stand-in.
describe('the fail-closed compare result cannot delete', () => {
    const remoteOnly: CompareInputEntry[] = [
        { name: 'photos', isDir: true },
        { name: 'taxes.pdf', isDir: false, size: 120, mtimeMs: 1_700_000_000_000 },
        { name: 'notes.md', isDir: false, size: 40, mtimeMs: 1_700_000_000_000 },
    ];

    it('an incomplete local scan yields a plan with zero deletes', () => {
        // What App.tsx builds on the marker: compareEntries([], []).
        const resolved = compareEntries([], []);
        const plan = derivePresetPlan(resolved, { preset: 'mirror' });

        expect(plan.totals.deleteRight).toBe(0);
        expect(plan.totals.deleteLeft).toBe(0);
        expect(plan.totals.actionable).toBe(0);
        expect(plan.hasDeletes).toBe(false);
    });

    it('proves the same Mirror preset WOULD delete off the flat fallback', () => {
        // The hazard this guard closes, pinned so it cannot silently come back:
        // with the local root gone its panel listing is empty, so the old flat
        // fallback classified every remote entry as `only-right`, which Mirror
        // maps to `delete-right`. Same preset, same planner, real deletes.
        const resolved = compareEntries([], remoteOnly);
        const plan = derivePresetPlan(resolved, { preset: 'mirror' });

        expect(plan.totals.deleteRight).toBeGreaterThan(0);
        expect(plan.hasDeletes).toBe(true);
    });

    it('a complete scan still produces a real, actionable plan', () => {
        // The other half of the guard: it must not break normal compares.
        const local: CompareInputEntry[] = [
            { name: 'taxes.pdf', isDir: false, size: 120, mtimeMs: 1_700_000_000_000 },
        ];
        const resolved = compareEntries(local, remoteOnly);
        const plan = derivePresetPlan(resolved, { preset: 'mirror' });

        expect(plan.totals.actionable).toBeGreaterThan(0);
    });
});
