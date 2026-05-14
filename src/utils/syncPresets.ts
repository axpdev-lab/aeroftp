// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

// Z.3.8 — FreeFileSync-style sync presets on top of the Z.3.7 compare
// classifier. Four canonical presets:
//
//   mirror  → make the destination identical to the source. Will DELETE
//             extras on the destination and OVERWRITE newer copies on the
//             destination side. Destructive by definition.
//   backup  → conservative one-way: copy missing + newer from source, never
//             delete, never overwrite a newer destination copy. Default
//             preset because the user can always re-run a delete-mode pass.
//   update  → like backup but also leaves untouched anything where source
//             is older or where sizes diverge (i.e. conflicts surface as
//             "skip" with a warning instead of being silently overwritten).
//   bisync  → mirror in both directions: copy missing both ways, propagate
//             newer in either direction, surface conflicts (skip and let
//             the user resolve manually in a follow-up pass).
//
// The helper produces a fully-typed plan structure that the UI can render
// as a per-bucket preview AND a destructive-confirm gate. Execution lives
// in App.tsx (today: local-local only; other pair kinds surface counts
// but disable execute, gated by Z.2.4 and Z.3.5.2).

import type {
    CompareBucket,
    CompareResult,
    CompareResultEntry,
} from './compareEndpoints';

export type SyncPreset = 'mirror' | 'backup' | 'update' | 'bisync';

export type PresetDirection = 'left-to-right' | 'right-to-left';

export type BucketAction =
    /** No-op for this bucket under the selected preset. */
    | 'skip'
    /** Copy the source-side entry to the destination. */
    | 'copy-to-right'
    | 'copy-to-left'
    /** Overwrite the destination-side copy (newer or conflicting). */
    | 'overwrite-right'
    | 'overwrite-left'
    /** Delete the destination-side entry (mirror only). */
    | 'delete-right'
    | 'delete-left'
    /** Conflict surfaced but left untouched; user must resolve. */
    | 'conflict-skip';

export interface BucketPlan {
    bucket: CompareBucket;
    action: BucketAction;
    entries: CompareResultEntry[];
    /** True iff the action mutates an existing destination copy or deletes data. */
    destructive: boolean;
    /** Bytes that will move on the wire (excludes skip / conflict-skip). */
    transferBytes: number;
}

export interface PresetPlan {
    preset: SyncPreset;
    /** Mirror / backup / update consume a direction; bisync ignores it. */
    direction: PresetDirection;
    bucketPlans: BucketPlan[];
    hasDestructive: boolean;
    /** True iff the preset asks to remove files on at least one side. */
    hasDeletes: boolean;
    /** True iff the preset asks to overwrite a newer destination copy. */
    hasOverwritesNewer: boolean;
    /** Aggregate of every "intended action" item (skip excluded). */
    totals: {
        actionable: number;
        skipped: number;
        copyToRight: number;
        copyToLeft: number;
        overwriteRight: number;
        overwriteLeft: number;
        deleteRight: number;
        deleteLeft: number;
        conflicts: number;
        transferBytes: number;
    };
}

interface PresetRuleSet {
    /** Mapping of bucket → action for left-to-right runs. */
    leftToRight: Record<CompareBucket, BucketAction>;
    /**
     * Optional explicit right-to-left mapping. When omitted, the helper
     * mirrors the left-to-right rules by swapping the per-side actions
     * (copy-to-right ↔ copy-to-left, delete-right ↔ delete-left, etc.).
     */
    rightToLeft?: Record<CompareBucket, BucketAction>;
    /** Bisync ignores direction; both sides are propagated symmetrically. */
    bisync?: Record<CompareBucket, BucketAction>;
}

const PRESET_RULES: Record<SyncPreset, PresetRuleSet> = {
    mirror: {
        leftToRight: {
            'only-left': 'copy-to-right',
            'newer-left': 'overwrite-right',
            'only-right': 'delete-right',
            'newer-right': 'overwrite-right', // destructive: overwrites a newer dest copy
            same: 'skip',
            conflict: 'overwrite-right',      // destructive: forces left-wins
        },
    },
    backup: {
        leftToRight: {
            'only-left': 'copy-to-right',
            'newer-left': 'overwrite-right',  // newer-side wins, non-destructive
            'only-right': 'skip',             // never delete
            'newer-right': 'skip',            // never overwrite a newer dest
            same: 'skip',
            conflict: 'skip',                 // safe default
        },
    },
    update: {
        leftToRight: {
            'only-left': 'copy-to-right',
            'newer-left': 'overwrite-right',
            'only-right': 'skip',
            'newer-right': 'skip',
            same: 'skip',
            conflict: 'conflict-skip',        // surface for manual resolution
        },
    },
    bisync: {
        // Direction is irrelevant for bisync; we expose a synthetic map.
        leftToRight: {
            'only-left': 'copy-to-right',
            'newer-left': 'overwrite-right',
            'only-right': 'copy-to-left',
            'newer-right': 'overwrite-left',
            same: 'skip',
            conflict: 'conflict-skip',
        },
        bisync: {
            'only-left': 'copy-to-right',
            'newer-left': 'overwrite-right',
            'only-right': 'copy-to-left',
            'newer-right': 'overwrite-left',
            same: 'skip',
            conflict: 'conflict-skip',
        },
    },
};

const PER_SIDE_FLIPS: Record<BucketAction, BucketAction> = {
    'copy-to-right': 'copy-to-left',
    'copy-to-left': 'copy-to-right',
    'overwrite-right': 'overwrite-left',
    'overwrite-left': 'overwrite-right',
    'delete-right': 'delete-left',
    'delete-left': 'delete-right',
    skip: 'skip',
    'conflict-skip': 'conflict-skip',
};

const BUCKETS: CompareBucket[] = [
    'only-left',
    'newer-left',
    'only-right',
    'newer-right',
    'conflict',
    'same',
];

/**
 * "Destructive" in the FreeFileSync sense: an action that risks losing
 * user data. Overwriting an older destination copy with a newer source
 * copy is NOT destructive — the destination was already stale. The
 * destructive set is therefore:
 *   - any delete action
 *   - overwriting a destination side that is NEWER than the source (i.e.
 *     `newer-right` + `overwrite-right` under L→R mirror, or
 *     `newer-left` + `overwrite-left` under R→L mirror)
 *   - forcing a `conflict` to overwrite either side (ambiguous → user
 *     might lose the diverging copy)
 */
const isDestructive = (bucket: CompareBucket, action: BucketAction): boolean => {
    if (action === 'delete-right' || action === 'delete-left') return true;
    if (bucket === 'newer-right' && action === 'overwrite-right') return true;
    if (bucket === 'newer-left' && action === 'overwrite-left') return true;
    if (bucket === 'conflict' && (action === 'overwrite-right' || action === 'overwrite-left')) return true;
    return false;
};

const entryBytes = (entry: CompareResultEntry, side: 'left' | 'right'): number => {
    const raw = side === 'left' ? entry.leftSize : entry.rightSize;
    if (typeof raw !== 'number' || !Number.isFinite(raw)) return 0;
    return Math.max(0, raw);
};

const bytesForAction = (entries: CompareResultEntry[], action: BucketAction): number => {
    if (action === 'skip' || action === 'conflict-skip') return 0;
    const side: 'left' | 'right' = (action === 'copy-to-right' || action === 'overwrite-right')
        ? 'left'
        : (action === 'copy-to-left' || action === 'overwrite-left')
            ? 'right'
            // For delete actions there's no transfer cost — the
            // bucketBytes helper still returns the size on the deleted
            // side so the user sees what's at stake. Pick the deleted
            // side explicitly.
            : (action === 'delete-right' ? 'right' : 'left');
    return entries.reduce((sum, entry) => sum + entryBytes(entry, side), 0);
};

/**
 * Resolve the bucket→action mapping for a (preset, direction) pair.
 * Bisync produces a symmetric map and ignores the direction argument.
 */
const resolveMapping = (
    preset: SyncPreset,
    direction: PresetDirection,
): Record<CompareBucket, BucketAction> => {
    const rules = PRESET_RULES[preset];
    if (preset === 'bisync') return rules.bisync ?? rules.leftToRight;
    if (direction === 'right-to-left') {
        if (rules.rightToLeft) return rules.rightToLeft;
        // Mirror the left-to-right map by flipping the side-bound actions.
        const flipped: Record<CompareBucket, BucketAction> = { ...rules.leftToRight };
        for (const bucket of BUCKETS) {
            const action = rules.leftToRight[bucket];
            flipped[bucket] = PER_SIDE_FLIPS[action];
        }
        // The "only-left" bucket label is fixed at compare-time, so
        // when we run right-to-left we are really treating the right
        // side as the source. Swap the bucket roles too: "only-right"
        // becomes the source-side ("only-source"). The action map already
        // covers both bucket names because we flipped per-action.
        return flipped;
    }
    return rules.leftToRight;
};

export interface DerivePresetPlanOptions {
    preset: SyncPreset;
    /** Ignored when preset === 'bisync'. Defaults to 'left-to-right'. */
    direction?: PresetDirection;
}

/**
 * Build the structured preset plan from a compare result. The returned
 * `bucketPlans` array preserves the canonical bucket order so the UI can
 * render predictably.
 */
export const derivePresetPlan = (
    result: CompareResult,
    options: DerivePresetPlanOptions,
): PresetPlan => {
    const direction = options.direction ?? 'left-to-right';
    const mapping = resolveMapping(options.preset, direction);

    const totals = {
        actionable: 0,
        skipped: 0,
        copyToRight: 0,
        copyToLeft: 0,
        overwriteRight: 0,
        overwriteLeft: 0,
        deleteRight: 0,
        deleteLeft: 0,
        conflicts: 0,
        transferBytes: 0,
    };

    const bucketPlans: BucketPlan[] = BUCKETS.map((bucket) => {
        const action = mapping[bucket];
        const entries = result.buckets[bucket];
        const transferBytes = bytesForAction(entries, action);
        totals.transferBytes += transferBytes;

        if (action === 'skip') {
            totals.skipped += entries.length;
        } else if (action === 'conflict-skip') {
            totals.skipped += entries.length;
            totals.conflicts += entries.length;
        } else {
            totals.actionable += entries.length;
            switch (action) {
                case 'copy-to-right':
                    totals.copyToRight += entries.length;
                    break;
                case 'copy-to-left':
                    totals.copyToLeft += entries.length;
                    break;
                case 'overwrite-right':
                    totals.overwriteRight += entries.length;
                    break;
                case 'overwrite-left':
                    totals.overwriteLeft += entries.length;
                    break;
                case 'delete-right':
                    totals.deleteRight += entries.length;
                    break;
                case 'delete-left':
                    totals.deleteLeft += entries.length;
                    break;
                default:
                    break;
            }
        }

        return {
            bucket,
            action,
            entries,
            destructive: entries.length > 0 && isDestructive(bucket, action),
            transferBytes,
        };
    });

    const hasDestructive = bucketPlans.some((plan) => plan.destructive);
    const hasDeletes = totals.deleteRight + totals.deleteLeft > 0;
    const hasOverwritesNewer = bucketPlans.some(
        (plan) =>
            plan.entries.length > 0
            && (
                (plan.bucket === 'newer-right' && plan.action === 'overwrite-right')
                || (plan.bucket === 'newer-left' && plan.action === 'overwrite-left')
            ),
    );

    return {
        preset: options.preset,
        direction,
        bucketPlans,
        hasDestructive,
        hasDeletes,
        hasOverwritesNewer,
        totals,
    };
};

/**
 * Convenience: pick the names that should be transferred from source to
 * destination under a given preset run. Useful for staging the existing
 * F5/F6 selection-driven planner without re-walking the result.
 */
export const namesFromBuckets = (
    plan: PresetPlan,
    sourceSide: 'left' | 'right',
): string[] => {
    const wanted: BucketAction[] = sourceSide === 'left'
        ? ['copy-to-right', 'overwrite-right']
        : ['copy-to-left', 'overwrite-left'];
    return plan.bucketPlans
        .filter((bp) => wanted.includes(bp.action))
        .flatMap((bp) => bp.entries.map((entry) => entry.name));
};

export const namesToDelete = (
    plan: PresetPlan,
    side: 'left' | 'right',
): string[] => {
    const target: BucketAction = side === 'right' ? 'delete-right' : 'delete-left';
    return plan.bucketPlans
        .filter((bp) => bp.action === target)
        .flatMap((bp) => bp.entries.map((entry) => entry.name));
};

export const describePreset = (preset: SyncPreset): { name: string; tagline: string; safe: boolean } => {
    switch (preset) {
        case 'mirror':
            return {
                name: 'Mirror',
                tagline: 'Make the destination identical to the source. Deletes extras and overwrites newer copies.',
                safe: false,
            };
        case 'backup':
            return {
                name: 'Backup',
                tagline: 'Copy missing and newer files. Never delete, never overwrite a newer destination copy.',
                safe: true,
            };
        case 'update':
            return {
                name: 'Update',
                tagline: 'Copy missing and newer files. Skip conflicts so you can resolve them manually.',
                safe: true,
            };
        case 'bisync':
            return {
                name: 'Two-way sync',
                tagline: 'Propagate missing and newer files in both directions. Skip conflicts.',
                safe: false, // can still move data the user did not expect
            };
        default:
            return { name: 'Unknown', tagline: 'Unrecognised preset.', safe: false };
    }
};

export const describeAction = (action: BucketAction): string => {
    switch (action) {
        case 'skip':
            return 'Skip';
        case 'copy-to-right':
            return 'Copy → right';
        case 'copy-to-left':
            return 'Copy ← left';
        case 'overwrite-right':
            return 'Overwrite right';
        case 'overwrite-left':
            return 'Overwrite left';
        case 'delete-right':
            return 'Delete on right';
        case 'delete-left':
            return 'Delete on left';
        case 'conflict-skip':
            return 'Conflict (skip)';
        default:
            return 'Unknown';
    }
};

export const __TEST_ONLY__ = {
    BUCKETS,
    PRESET_RULES,
    PER_SIDE_FLIPS,
};
