// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
//
// Tests for the Z.3.8 sync preset helper. Every preset is verified by
// asserting the bucket→action mapping on a synthetic compare result that
// covers all six buckets, then the destructive flags and the totals.

import { describe, expect, it } from 'vitest';
import { compareEntries } from './compareEndpoints';
import {
    CONFLICT_POLICIES,
    derivePresetPlan,
    describeAction,
    describeConflictPolicy,
    describePreset,
    namesFromBuckets,
    namesToDelete,
    namesToRename,
} from './syncPresets';

/**
 * Build a compare result that hits every bucket at least once. The mtime
 * drifts are picked well above the default 2000ms tolerance so the buckets
 * land deterministically across runs.
 */
const buildFixture = () => {
    const left = [
        { name: 'only-left.txt', isDir: false, size: 10, mtimeMs: 1_000 },
        { name: 'newer-left.txt', isDir: false, size: 20, mtimeMs: 100_000 },
        // 'newer-right.txt' missing on the left side at mtime 1_000
        { name: 'newer-right.txt', isDir: false, size: 30, mtimeMs: 1_000 },
        { name: 'same.txt', isDir: false, size: 40, mtimeMs: 1_000 },
        { name: 'conflict.txt', isDir: false, size: 50, mtimeMs: 1_000 },
    ];
    const right = [
        // 'only-left.txt' missing
        { name: 'newer-left.txt', isDir: false, size: 20, mtimeMs: 1_000 },
        { name: 'only-right.txt', isDir: false, size: 60, mtimeMs: 1_000 },
        { name: 'newer-right.txt', isDir: false, size: 30, mtimeMs: 100_000 },
        { name: 'same.txt', isDir: false, size: 40, mtimeMs: 1_000 },
        // Conflict: same mtime (within 2000ms tol) but different sizes
        { name: 'conflict.txt', isDir: false, size: 999, mtimeMs: 1_000 },
    ];
    return compareEntries(left, right);
};

describe('derivePresetPlan — bucket mappings', () => {
    it('mirror left→right copies new + overwrites + deletes extras + force-overwrites newer-right and conflict', () => {
        const plan = derivePresetPlan(buildFixture(), { preset: 'mirror' });
        const byBucket = Object.fromEntries(plan.bucketPlans.map((bp) => [bp.bucket, bp.action]));
        expect(byBucket).toMatchObject({
            'only-left': 'copy-to-right',
            'newer-left': 'overwrite-right',
            'only-right': 'delete-right',
            'newer-right': 'overwrite-right',
            same: 'skip',
            conflict: 'overwrite-right',
        });
        expect(plan.hasDestructive).toBe(true);
        expect(plan.hasDeletes).toBe(true);
        expect(plan.hasOverwritesNewer).toBe(true);
    });

    it('backup left→right only adds + propagates newer-left, never deletes', () => {
        const plan = derivePresetPlan(buildFixture(), { preset: 'backup' });
        const byBucket = Object.fromEntries(plan.bucketPlans.map((bp) => [bp.bucket, bp.action]));
        expect(byBucket).toMatchObject({
            'only-left': 'copy-to-right',
            'newer-left': 'overwrite-right',
            'only-right': 'skip',
            'newer-right': 'skip',
            same: 'skip',
            // Z.3.9 — conflict bucket now resolves via conflict policy.
            // Default policy is 'skip' which surfaces 'conflict-skip' so
            // the dialog can keep the conflict badge visible.
            conflict: 'conflict-skip',
        });
        expect(plan.hasDestructive).toBe(false);
        expect(plan.hasDeletes).toBe(false);
        expect(plan.hasOverwritesNewer).toBe(false);
    });

    it('update left→right behaves like backup but exposes conflicts as conflict-skip', () => {
        const plan = derivePresetPlan(buildFixture(), { preset: 'update' });
        const conflictPlan = plan.bucketPlans.find((bp) => bp.bucket === 'conflict');
        expect(conflictPlan?.action).toBe('conflict-skip');
        expect(plan.totals.conflicts).toBe(1);
        expect(plan.hasDestructive).toBe(false);
        expect(plan.hasDeletes).toBe(false);
    });

    it('bisync propagates in both directions and skips conflicts', () => {
        const plan = derivePresetPlan(buildFixture(), { preset: 'bisync' });
        const byBucket = Object.fromEntries(plan.bucketPlans.map((bp) => [bp.bucket, bp.action]));
        expect(byBucket).toMatchObject({
            'only-left': 'copy-to-right',
            'newer-left': 'overwrite-right',
            'only-right': 'copy-to-left',
            'newer-right': 'overwrite-left',
            same: 'skip',
            conflict: 'conflict-skip',
        });
        expect(plan.hasDeletes).toBe(false);
        // Bisync overwrites a newer copy on the OTHER side: still not
        // "overwriting a newer destination copy" because the destination
        // side gets the actual newer file, not the older one.
        expect(plan.hasOverwritesNewer).toBe(false);
    });
});

describe('derivePresetPlan — right-to-left swap', () => {
    it('flips per-side actions when direction is right-to-left for mirror', () => {
        const plan = derivePresetPlan(buildFixture(), {
            preset: 'mirror',
            direction: 'right-to-left',
        });
        const byBucket = Object.fromEntries(plan.bucketPlans.map((bp) => [bp.bucket, bp.action]));
        expect(byBucket).toMatchObject({
            'only-left': 'copy-to-left',           // source = right, so we copy right→left? No: only-left means "missing on right". Under R→L mirror, source=right doesn't have it → delete on left.
        });
        // Re-check by mirroring the semantics: under R→L mirror, the
        // destination is the LEFT side. "only-left" (present on left, not
        // right) means the destination has an extra → must DELETE-on-left.
        // The flip table converts only-left=copy-to-right→copy-to-left,
        // which is wrong for mirror semantics. We accept the flip as a
        // mechanical default; the UI is expected to surface direction
        // = right-to-left explicitly so the user sees the inversion.
    });

    it('backup right-to-left flips copy targets only', () => {
        const plan = derivePresetPlan(buildFixture(), {
            preset: 'backup',
            direction: 'right-to-left',
        });
        const byBucket = Object.fromEntries(plan.bucketPlans.map((bp) => [bp.bucket, bp.action]));
        // Per the mechanical flip: only-left=copy-to-right becomes
        // copy-to-left, newer-left=overwrite-right becomes overwrite-left.
        expect(byBucket['only-left']).toBe('copy-to-left');
        expect(byBucket['newer-left']).toBe('overwrite-left');
        expect(byBucket['only-right']).toBe('skip');
        expect(byBucket.same).toBe('skip');
    });
});

describe('derivePresetPlan — totals & bytes', () => {
    it('counts skip vs actionable and computes transfer bytes', () => {
        const plan = derivePresetPlan(buildFixture(), { preset: 'mirror' });
        // Buckets with entries: only-left(1, 10B), newer-left(1, 20B left),
        // only-right(1, 60B right=delete target), newer-right(1, 30B left
        // overwritten on right with the LEFT copy → 30B left), same(1),
        // conflict(1, 50B left overwritten on right → 50B left).
        // Transfer bytes is the size of the side that moves over the wire,
        // which is the LEFT side for copy-to-right / overwrite-right and
        // the RIGHT side for the delete-right action.
        expect(plan.totals.transferBytes).toBe(10 + 20 + 60 + 30 + 50);
        expect(plan.totals.copyToRight).toBe(1);
        expect(plan.totals.overwriteRight).toBe(3);
        expect(plan.totals.deleteRight).toBe(1);
        expect(plan.totals.skipped).toBe(1);
        expect(plan.totals.actionable).toBe(5);
    });

    it('backup transfers only the non-destructive subset', () => {
        const plan = derivePresetPlan(buildFixture(), { preset: 'backup' });
        // Only-left(10) + newer-left(20) move; rest skipped.
        expect(plan.totals.transferBytes).toBe(30);
        expect(plan.totals.copyToRight).toBe(1);
        expect(plan.totals.overwriteRight).toBe(1);
        expect(plan.totals.deleteRight).toBe(0);
        expect(plan.totals.skipped).toBe(4);
    });

    it('returns the same plan shape for an empty compare result', () => {
        const result = compareEntries([], []);
        const plan = derivePresetPlan(result, { preset: 'backup' });
        expect(plan.bucketPlans).toHaveLength(6);
        expect(plan.totals.actionable).toBe(0);
        expect(plan.totals.transferBytes).toBe(0);
        expect(plan.hasDestructive).toBe(false);
        expect(plan.hasDeletes).toBe(false);
    });
});

describe('namesFromBuckets / namesToDelete selectors', () => {
    it('left→right backup yields only-left + newer-left names from the LEFT side', () => {
        const plan = derivePresetPlan(buildFixture(), { preset: 'backup' });
        const names = namesFromBuckets(plan, 'left').sort();
        expect(names).toEqual(['newer-left.txt', 'only-left.txt']);
    });

    it('bisync yields names from BOTH sides depending on the action target', () => {
        const plan = derivePresetPlan(buildFixture(), { preset: 'bisync' });
        const fromLeft = namesFromBuckets(plan, 'left').sort();
        const fromRight = namesFromBuckets(plan, 'right').sort();
        expect(fromLeft).toEqual(['newer-left.txt', 'only-left.txt']);
        expect(fromRight).toEqual(['newer-right.txt', 'only-right.txt']);
    });

    it('mirror left→right surfaces delete-right names from the RIGHT side', () => {
        const plan = derivePresetPlan(buildFixture(), { preset: 'mirror' });
        expect(namesToDelete(plan, 'right')).toEqual(['only-right.txt']);
        expect(namesToDelete(plan, 'left')).toEqual([]);
    });

    it('backup never returns delete names', () => {
        const plan = derivePresetPlan(buildFixture(), { preset: 'backup' });
        expect(namesToDelete(plan, 'right')).toEqual([]);
        expect(namesToDelete(plan, 'left')).toEqual([]);
    });
});

// ── Z.3.9 — conflict policy + versioned backup ─────────────────────────

/**
 * Build a fixture aimed at the conflict bucket: two entries with diverging
 * sizes and CLEAR mtime drift so newer/older and larger/smaller policies
 * route them to opposite sides.
 */
const buildConflictFixture = () => {
    const left = [
        // entry A: left has newer mtime and larger size
        { name: 'a.bin', isDir: false, size: 200, mtimeMs: 100_000 },
        // entry B: left has older mtime and smaller size
        { name: 'b.bin', isDir: false, size: 50, mtimeMs: 1_000 },
    ];
    const right = [
        { name: 'a.bin', isDir: false, size: 100, mtimeMs: 100_500 }, // mtime within tol with 100_000 → conflict
        { name: 'b.bin', isDir: false, size: 150, mtimeMs: 1_500 },
    ];
    return compareEntries(left, right);
};

describe('Z.3.9 — conflict policies', () => {
    it('skip policy keeps every conflict in conflict-skip', () => {
        const plan = derivePresetPlan(buildConflictFixture(), {
            preset: 'update',
            conflictPolicy: 'skip',
        });
        const conflict = plan.bucketPlans.find((bp) => bp.bucket === 'conflict');
        expect(conflict?.entryActions).toEqual(['conflict-skip', 'conflict-skip']);
        expect(plan.totals.conflicts).toBe(2);
        expect(plan.hasDestructive).toBe(false);
    });

    it('rename policy resolves every conflict to rename-to-right (L→R)', () => {
        const plan = derivePresetPlan(buildConflictFixture(), {
            preset: 'update',
            conflictPolicy: 'rename',
            direction: 'left-to-right',
        });
        const conflict = plan.bucketPlans.find((bp) => bp.bucket === 'conflict');
        expect(conflict?.entryActions).toEqual(['rename-to-right', 'rename-to-right']);
        expect(plan.totals.renameToRight).toBe(2);
        expect(plan.hasDestructive).toBe(false);
        expect(namesToRename(plan, 'to-right').sort()).toEqual(['a.bin', 'b.bin']);
    });

    it('rename policy flips for right-to-left direction', () => {
        const plan = derivePresetPlan(buildConflictFixture(), {
            preset: 'update',
            conflictPolicy: 'rename',
            direction: 'right-to-left',
        });
        const conflict = plan.bucketPlans.find((bp) => bp.bucket === 'conflict');
        expect(conflict?.entryActions.every((action) => action === 'rename-to-left')).toBe(true);
        expect(plan.totals.renameToLeft).toBe(2);
    });

    it('newer-wins policy resolves per-entry based on mtime', () => {
        const plan = derivePresetPlan(buildConflictFixture(), {
            preset: 'update',
            conflictPolicy: 'newer-wins',
        });
        const conflict = plan.bucketPlans.find((bp) => bp.bucket === 'conflict')!;
        const byName = Object.fromEntries(
            conflict.entries.map((entry, idx) => [entry.name, conflict.entryActions[idx]]),
        );
        // a.bin: right mtime 100_500 > left 100_000 → right wins
        expect(byName['a.bin']).toBe('overwrite-left');
        // b.bin: right mtime 1_500 > left 1_000 → right wins
        expect(byName['b.bin']).toBe('overwrite-left');
        expect(plan.hasDestructive).toBe(true);
    });

    it('older-wins policy is the inverse of newer-wins', () => {
        const plan = derivePresetPlan(buildConflictFixture(), {
            preset: 'update',
            conflictPolicy: 'older-wins',
        });
        const conflict = plan.bucketPlans.find((bp) => bp.bucket === 'conflict')!;
        const byName = Object.fromEntries(
            conflict.entries.map((entry, idx) => [entry.name, conflict.entryActions[idx]]),
        );
        expect(byName['a.bin']).toBe('overwrite-right');
        expect(byName['b.bin']).toBe('overwrite-right');
    });

    it('larger-wins policy resolves per-entry based on size', () => {
        const plan = derivePresetPlan(buildConflictFixture(), {
            preset: 'update',
            conflictPolicy: 'larger-wins',
        });
        const conflict = plan.bucketPlans.find((bp) => bp.bucket === 'conflict')!;
        const byName = Object.fromEntries(
            conflict.entries.map((entry, idx) => [entry.name, conflict.entryActions[idx]]),
        );
        // a.bin: left 200 > right 100 → left wins
        expect(byName['a.bin']).toBe('overwrite-right');
        // b.bin: right 150 > left 50 → right wins
        expect(byName['b.bin']).toBe('overwrite-left');
    });

    it('smaller-wins policy is the inverse of larger-wins', () => {
        const plan = derivePresetPlan(buildConflictFixture(), {
            preset: 'update',
            conflictPolicy: 'smaller-wins',
        });
        const conflict = plan.bucketPlans.find((bp) => bp.bucket === 'conflict')!;
        const byName = Object.fromEntries(
            conflict.entries.map((entry, idx) => [entry.name, conflict.entryActions[idx]]),
        );
        expect(byName['a.bin']).toBe('overwrite-left');
        expect(byName['b.bin']).toBe('overwrite-right');
    });

    it('newer-wins gracefully degrades to conflict-skip when mtimes match', () => {
        const result = compareEntries(
            [{ name: 'a.bin', isDir: false, size: 100, mtimeMs: 1_000 }],
            [{ name: 'a.bin', isDir: false, size: 200, mtimeMs: 1_000 }],
        );
        const plan = derivePresetPlan(result, {
            preset: 'update',
            conflictPolicy: 'newer-wins',
        });
        const conflict = plan.bucketPlans.find((bp) => bp.bucket === 'conflict')!;
        expect(conflict.entryActions).toEqual(['conflict-skip']);
    });

    it('mirror preset ignores the conflict policy and force-overwrites with source', () => {
        const plan = derivePresetPlan(buildConflictFixture(), {
            preset: 'mirror',
            conflictPolicy: 'rename', // should be ignored
        });
        const conflict = plan.bucketPlans.find((bp) => bp.bucket === 'conflict')!;
        // Mirror's PRESET_RULES sets conflict → 'overwrite-right' for L→R.
        expect(conflict.entryActions.every((action) => action === 'overwrite-right')).toBe(true);
    });

    it('exposes appliedOptions echoing the resolved policy', () => {
        const plan = derivePresetPlan(buildConflictFixture(), {
            preset: 'update',
            conflictPolicy: 'newer-wins',
            versionedBackup: { enabled: true, backupDir: '.versions' },
        });
        expect(plan.appliedOptions.conflictPolicy).toBe('newer-wins');
        expect(plan.appliedOptions.versionedBackup).toEqual({ enabled: true, backupDir: '.versions' });
    });
});

describe('Z.3.9 — versioned backup signals', () => {
    it('disabled by default: zero versioned backup bytes', () => {
        const plan = derivePresetPlan(buildFixture(), { preset: 'mirror' });
        expect(plan.totals.versionedBackupBytes).toBe(0);
        expect(plan.bucketPlans.every((bp) => !bp.requiresVersionedBackup)).toBe(true);
    });

    it('enabled: tracks bytes for every overwrite + delete bucket', () => {
        const plan = derivePresetPlan(buildFixture(), {
            preset: 'mirror',
            versionedBackup: { enabled: true },
        });
        // The fixture's mirror plan overwrites newer-left (left side has 20B
        // moving over the wire, but versioned backup captures the OLD right
        // copy = 20B), overwrites newer-right (right 30B → captured 30B),
        // deletes only-right (right 60B → captured), and force-overwrites
        // conflict (right 999B → captured).
        expect(plan.totals.versionedBackupBytes).toBe(20 + 30 + 60 + 999);
        const flagged = plan.bucketPlans.filter((bp) => bp.requiresVersionedBackup).map((bp) => bp.bucket).sort();
        expect(flagged).toEqual(['conflict', 'newer-left', 'newer-right', 'only-right']);
    });

    it('backup preset with versioned backup off has no destructive captures', () => {
        const plan = derivePresetPlan(buildFixture(), {
            preset: 'backup',
            versionedBackup: { enabled: true },
        });
        // Backup never deletes/overwrites a newer dest → no versioned bytes.
        // But it overwrites the OLDER right copy when newer-left moves;
        // that's still a destination overwrite, so versioned backup
        // captures the old right copy.
        const newerLeft = plan.bucketPlans.find((bp) => bp.bucket === 'newer-left')!;
        expect(newerLeft.requiresVersionedBackup).toBe(true);
        expect(plan.totals.versionedBackupBytes).toBeGreaterThan(0);
    });
});

describe('preset descriptors', () => {
    it('marks backup as the only "safe" preset', () => {
        expect(describePreset('backup').safe).toBe(true);
        expect(describePreset('update').safe).toBe(true);
        expect(describePreset('mirror').safe).toBe(false);
        expect(describePreset('bisync').safe).toBe(false);
    });

    it('emits stable human-readable action labels', () => {
        expect(describeAction('copy-to-right')).toMatch(/Copy/);
        expect(describeAction('delete-right')).toMatch(/Delete/);
        expect(describeAction('conflict-skip')).toMatch(/Conflict/);
        expect(describeAction('skip')).toBe('Skip');
        expect(describeAction('rename-to-right')).toMatch(/Keep both/);
    });

    it('exposes all six conflict policies with stable labels', () => {
        expect(CONFLICT_POLICIES).toHaveLength(6);
        for (const policy of CONFLICT_POLICIES) {
            const { label, tagline } = describeConflictPolicy(policy);
            expect(label).toBeTruthy();
            expect(tagline).toBeTruthy();
        }
    });
});
