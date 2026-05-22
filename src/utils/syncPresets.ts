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
    | 'conflict-skip'
    /**
     * Z.3.9 — keep BOTH copies: copy the source side to the destination
     * under a timestamped suffix (e.g. `file.txt.20260514T143012.bak`).
     * Non-destructive. Execution wiring lands with Z.3.9.2.
     */
    | 'rename-to-right'
    | 'rename-to-left';

/**
 * Z.3.9 — Conflict policy: how to resolve the `conflict` compare bucket.
 *   - `skip`         → leave both copies untouched (default for Update)
 *   - `rename`       → copy the source-side entry with a timestamp suffix
 *   - `newer-wins`   → the side with the more recent mtime overwrites the other
 *   - `older-wins`   → the older side overwrites the newer (rare; archive use case)
 *   - `larger-wins`  → the side with the larger size overwrites the smaller
 *   - `smaller-wins` → the smaller side overwrites the larger
 *
 * `newer-wins` / `older-wins` / `larger-wins` / `smaller-wins` are
 * per-entry decisions: BucketPlan.entryActions stores the resolved action
 * for each entry, while `action` keeps the dominant action so the UI can
 * render a one-shot summary. `skip` and `rename` are uniform.
 */
export type ConflictPolicy =
    | 'skip'
    | 'rename'
    | 'newer-wins'
    | 'older-wins'
    | 'larger-wins'
    | 'smaller-wins';

/**
 * Z.3.9 — Versioned backup: before an overwrite or delete fires, capture
 * the destination copy under `backupDir/<timestamp>/<relative-path>`.
 *
 * The helper only flags the BucketPlan (and totals) with the predicted
 * cost; actual backup-dir staging lives in the execution layer
 * (Z.3.9.2) since it requires new IPC and per-pair semantics.
 */
export interface VersionedBackupConfig {
    enabled: boolean;
    /** Defaults to `.aeroftp-versions`. */
    backupDir?: string;
}

export interface BucketPlan {
    bucket: CompareBucket;
    /**
     * Dominant action for the bucket. When the conflict policy resolves to
     * different actions per entry (e.g. newer-wins on a mixed-mtime bucket)
     * this carries the majority action; the per-entry detail lives in
     * `entryActions`.
     */
    action: BucketAction;
    entries: CompareResultEntry[];
    /** Parallel array to `entries`: action resolved for each row. */
    entryActions: BucketAction[];
    /** True iff at least one entry mutates an existing destination copy or deletes data. */
    destructive: boolean;
    /** Bytes that will move on the wire (excludes skip / conflict-skip / rename costs). */
    transferBytes: number;
    /** Z.3.9 — bytes that need to be staged under the versioned backup dir for this bucket. */
    versionedBackupBytes: number;
    /** Z.3.9 — true iff at least one entry triggers a versioned backup capture. */
    requiresVersionedBackup: boolean;
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
        /** Z.3.9 — renames produce a copy of the source under a timestamped suffix. */
        renameToRight: number;
        renameToLeft: number;
        conflicts: number;
        transferBytes: number;
        /** Z.3.9 — total bytes that would land in the versioned backup dir. */
        versionedBackupBytes: number;
    };
    /** Echo of the conflict + versioned-backup options actually applied. */
    appliedOptions: {
        conflictPolicy: ConflictPolicy;
        versionedBackup: VersionedBackupConfig;
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
    'rename-to-right': 'rename-to-left',
    'rename-to-left': 'rename-to-right',
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
    /**
     * Z.3.9 — how to resolve the `conflict` bucket. When omitted the helper
     * falls back to the preset's built-in default (Update / Backup → 'skip',
     * Mirror → 'newer-wins' is NOT applied: mirror always force-overwrites
     * with the source side; the conflictPolicy on mirror is therefore
     * ignored). Bisync uses 'skip' unless the caller picks something else.
     */
    conflictPolicy?: ConflictPolicy;
    /** Z.3.9 — versioned backup configuration. Default: disabled. */
    versionedBackup?: VersionedBackupConfig;
}

/**
 * Resolve a single conflict entry into a concrete action based on the
 * active conflict policy + direction. Returns 'conflict-skip' when the
 * policy lacks the data needed to decide (e.g. newer-wins on entries
 * missing mtime metadata).
 */
const resolveConflictAction = (
    entry: CompareResultEntry,
    policy: ConflictPolicy,
    direction: PresetDirection,
): BucketAction => {
    const isLeftToRight = direction === 'left-to-right';
    if (policy === 'skip') return 'conflict-skip';
    if (policy === 'rename') return isLeftToRight ? 'rename-to-right' : 'rename-to-left';

    if (policy === 'newer-wins' || policy === 'older-wins') {
        const lm = typeof entry.leftMtimeMs === 'number' ? entry.leftMtimeMs : null;
        const rm = typeof entry.rightMtimeMs === 'number' ? entry.rightMtimeMs : null;
        if (lm === null || rm === null || lm === rm) return 'conflict-skip';
        const leftIsNewer = lm > rm;
        const winnerIsLeft = policy === 'newer-wins' ? leftIsNewer : !leftIsNewer;
        return winnerIsLeft ? 'overwrite-right' : 'overwrite-left';
    }

    if (policy === 'larger-wins' || policy === 'smaller-wins') {
        const ls = typeof entry.leftSize === 'number' ? entry.leftSize : null;
        const rs = typeof entry.rightSize === 'number' ? entry.rightSize : null;
        if (ls === null || rs === null || ls === rs) return 'conflict-skip';
        const leftIsLarger = ls > rs;
        const winnerIsLeft = policy === 'larger-wins' ? leftIsLarger : !leftIsLarger;
        return winnerIsLeft ? 'overwrite-right' : 'overwrite-left';
    }

    return 'conflict-skip';
};

/**
 * Pick the dominant action across an array of per-entry actions. Used to
 * keep BucketPlan.action coherent with `entryActions` so existing UI rows
 * still surface a single label.
 */
const dominantAction = (actions: BucketAction[], fallback: BucketAction): BucketAction => {
    if (actions.length === 0) return fallback;
    const counts = new Map<BucketAction, number>();
    for (const action of actions) {
        counts.set(action, (counts.get(action) ?? 0) + 1);
    }
    let best: BucketAction = actions[0];
    let bestCount = 0;
    counts.forEach((count, action) => {
        if (count > bestCount) {
            best = action;
            bestCount = count;
        }
    });
    return best;
};

const wireBytesForAction = (entry: CompareResultEntry, action: BucketAction): number => {
    switch (action) {
        case 'copy-to-right':
        case 'overwrite-right':
        case 'rename-to-right':
            return entryBytes(entry, 'left');
        case 'copy-to-left':
        case 'overwrite-left':
        case 'rename-to-left':
            return entryBytes(entry, 'right');
        case 'delete-right':
            return entryBytes(entry, 'right');
        case 'delete-left':
            return entryBytes(entry, 'left');
        default:
            return 0;
    }
};

const versionedBackupBytesForAction = (entry: CompareResultEntry, action: BucketAction): number => {
    switch (action) {
        case 'overwrite-right':
        case 'delete-right':
            return entryBytes(entry, 'right');
        case 'overwrite-left':
        case 'delete-left':
            return entryBytes(entry, 'left');
        default:
            return 0;
    }
};

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
    const conflictPolicy: ConflictPolicy = options.conflictPolicy
        // Mirror always force-overwrites with the source side; the
        // user-facing conflict policy is ignored for it.
        ?? (options.preset === 'mirror' ? 'skip' : 'skip');
    const versionedBackup: VersionedBackupConfig = options.versionedBackup ?? { enabled: false };

    const totals = {
        actionable: 0,
        skipped: 0,
        copyToRight: 0,
        copyToLeft: 0,
        overwriteRight: 0,
        overwriteLeft: 0,
        deleteRight: 0,
        deleteLeft: 0,
        renameToRight: 0,
        renameToLeft: 0,
        conflicts: 0,
        transferBytes: 0,
        versionedBackupBytes: 0,
    };

    const incrementTotalsFor = (action: BucketAction, entry: CompareResultEntry) => {
        switch (action) {
            case 'skip':
                totals.skipped += 1;
                break;
            case 'conflict-skip':
                totals.skipped += 1;
                totals.conflicts += 1;
                break;
            case 'copy-to-right':
                totals.copyToRight += 1;
                totals.actionable += 1;
                break;
            case 'copy-to-left':
                totals.copyToLeft += 1;
                totals.actionable += 1;
                break;
            case 'overwrite-right':
                totals.overwriteRight += 1;
                totals.actionable += 1;
                break;
            case 'overwrite-left':
                totals.overwriteLeft += 1;
                totals.actionable += 1;
                break;
            case 'delete-right':
                totals.deleteRight += 1;
                totals.actionable += 1;
                break;
            case 'delete-left':
                totals.deleteLeft += 1;
                totals.actionable += 1;
                break;
            case 'rename-to-right':
                totals.renameToRight += 1;
                totals.actionable += 1;
                break;
            case 'rename-to-left':
                totals.renameToLeft += 1;
                totals.actionable += 1;
                break;
            default:
                break;
        }
        totals.transferBytes += wireBytesForAction(entry, action);
        if (versionedBackup.enabled) {
            totals.versionedBackupBytes += versionedBackupBytesForAction(entry, action);
        }
    };

    const bucketPlans: BucketPlan[] = BUCKETS.map((bucket) => {
        const entries = result.buckets[bucket];
        const fallback = mapping[bucket];
        // Conflict bucket honours the conflict policy when the preset
        // exposes it as conflict-skip in the mapping (Update / Backup /
        // Bisync). Mirror keeps its force-overwrite semantics.
        const honourConflictPolicy =
            bucket === 'conflict'
            && options.preset !== 'mirror'
            && (fallback === 'conflict-skip' || fallback === 'skip');

        const entryActions: BucketAction[] = entries.map((entry) => {
            if (honourConflictPolicy) {
                return resolveConflictAction(entry, conflictPolicy, direction);
            }
            return fallback;
        });

        let transferBytes = 0;
        let versionedBackupBytes = 0;
        let requiresVersionedBackup = false;
        let destructive = false;

        for (let i = 0; i < entries.length; i += 1) {
            const action = entryActions[i];
            const entry = entries[i];
            incrementTotalsFor(action, entry);
            transferBytes += wireBytesForAction(entry, action);
            if (versionedBackup.enabled) {
                const vBytes = versionedBackupBytesForAction(entry, action);
                versionedBackupBytes += vBytes;
                if (vBytes > 0) requiresVersionedBackup = true;
            }
            if (isDestructive(bucket, action)) destructive = true;
        }

        const action = honourConflictPolicy
            ? dominantAction(entryActions, fallback)
            : fallback;

        return {
            bucket,
            action,
            entries,
            entryActions,
            destructive,
            transferBytes,
            versionedBackupBytes,
            requiresVersionedBackup,
        };
    });

    const hasDestructive = bucketPlans.some((plan) => plan.destructive);
    const hasDeletes = totals.deleteRight + totals.deleteLeft > 0;
    const hasOverwritesNewer = bucketPlans.some(
        (plan) =>
            plan.entries.length > 0
            && (
                (plan.bucket === 'newer-right' && plan.entryActions.includes('overwrite-right'))
                || (plan.bucket === 'newer-left' && plan.entryActions.includes('overwrite-left'))
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
        appliedOptions: {
            conflictPolicy,
            versionedBackup,
        },
    };
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
        case 'rename-to-right':
            return 'Keep both → right';
        case 'rename-to-left':
            return 'Keep both ← left';
        case 'conflict-skip':
            return 'Conflict (skip)';
        default:
            return 'Unknown';
    }
};

export const describeConflictPolicy = (policy: ConflictPolicy): { label: string; tagline: string } => {
    switch (policy) {
        case 'skip':
            return { label: 'Skip', tagline: 'Leave conflicts untouched on both sides.' };
        case 'rename':
            return { label: 'Keep both', tagline: 'Copy the source-side copy under a timestamped suffix.' };
        case 'newer-wins':
            return { label: 'Newer wins', tagline: 'The side with the more recent mtime overwrites the other.' };
        case 'older-wins':
            return { label: 'Older wins', tagline: 'The older side overwrites the newer (archive use case).' };
        case 'larger-wins':
            return { label: 'Larger wins', tagline: 'The side with the larger size overwrites the smaller.' };
        case 'smaller-wins':
            return { label: 'Smaller wins', tagline: 'The smaller side overwrites the larger.' };
        default:
            return { label: 'Unknown', tagline: 'Unrecognised policy.' };
    }
};

export const CONFLICT_POLICIES: ConflictPolicy[] = [
    'skip',
    'rename',
    'newer-wins',
    'older-wins',
    'larger-wins',
    'smaller-wins',
];

export const __TEST_ONLY__ = {
    BUCKETS,
    PRESET_RULES,
    PER_SIDE_FLIPS,
};
