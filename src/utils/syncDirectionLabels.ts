// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

/**
 * One vocabulary for sync directions and modes, wherever AeroSync shows them.
 *
 * Two separate questions, answered with the same words everywhere:
 * - Direction: where the data flows (Local → Remote, Remote → Local, Both ways).
 * - Mode: what happens on arrival (Backup, Update, Mirror, Two-way sync).
 *
 * The Plan tab stores a direction as left/right, and a `.aerosync` template
 * stores it as `local_to_remote` / `remote_to_local` / `bidirectional` plus a
 * `delete_orphans` flag. Those serialized values stay as they are, so existing
 * files keep working; this module only decides what the user reads.
 */

import type { SyncDirection } from '../types';
import type { AeroSyncPairKind } from '../components/AeroSync/types';
import { describePreset, type BucketAction, type ConflictPolicy, type PresetDirection, type SyncPreset } from './syncPresets';
import type { CompareBucket, ComparePolicy } from './compareEndpoints';

export interface LabelRef {
    key: string;
    fallback: string;
}

export const LOCAL_TO_REMOTE: LabelRef = { key: 'aerosync.directionLocalToRemote', fallback: 'Local → Remote' };
export const REMOTE_TO_LOCAL: LabelRef = { key: 'aerosync.directionRemoteToLocal', fallback: 'Remote → Local' };
export const BOTH_WAYS: LabelRef = { key: 'aerosync.directionBothWays', fallback: 'Both ways' };
const LEFT_TO_RIGHT: LabelRef = { key: 'aerosync.directionLeftRight', fallback: 'Left to Right' };
const RIGHT_TO_LEFT: LabelRef = { key: 'aerosync.directionRightLeft', fallback: 'Right to Left' };

/**
 * The Plan tab's left/right toggle, named by what is on each side when the
 * pair has a remote. Two local folders keep Left/Right: both are "local".
 */
export function planDirectionLabel(direction: PresetDirection, pairKind: AeroSyncPairKind | null | undefined): LabelRef {
    const toRight = direction === 'left-to-right';
    if (pairKind === 'local-remote') return toRight ? LOCAL_TO_REMOTE : REMOTE_TO_LOCAL;
    if (pairKind === 'remote-local') return toRight ? REMOTE_TO_LOCAL : LOCAL_TO_REMOTE;
    return toRight ? LEFT_TO_RIGHT : RIGHT_TO_LEFT;
}

/** A template's serialized direction, in the words the Plan tab uses. */
export function templateDirectionLabel(direction: SyncDirection | string): LabelRef {
    switch (direction) {
        case 'local_to_remote': return LOCAL_TO_REMOTE;
        case 'remote_to_local': return REMOTE_TO_LOCAL;
        case 'bidirectional': return BOTH_WAYS;
        default: return { key: '', fallback: String(direction) };
    }
}

/** The Plan tab mode a template's direction and `delete_orphans` stand for. */
export function presetForTemplate(direction: SyncDirection | string, deleteOrphans: boolean | undefined): SyncPreset {
    if (direction === 'bidirectional') return 'bisync';
    return deleteOrphans ? 'mirror' : 'backup';
}

export function presetNameLabel(preset: SyncPreset): LabelRef {
    return { key: `aerosync.preset.${preset}.name`, fallback: describePreset(preset).name };
}

export function presetTaglineLabel(preset: SyncPreset): LabelRef {
    return { key: `aerosync.preset.${preset}.tagline`, fallback: describePreset(preset).tagline };
}

/**
 * Resolve a label with the app's `t`, falling back to English. `t` answers a
 * missing key with the key itself, so that answer counts as missing too.
 */
export function resolveLabel(t: (key: string) => string, label: LabelRef): string {
    const value = label.key ? t(label.key) : '';
    return value && value !== label.key ? value : label.fallback;
}

// ── Plan and Compare labels (#347 follow-up: the rest of the tabs in English) ──

const ACTION_FALLBACK: Record<BucketAction, string> = {
    'skip': 'Skip',
    'copy-to-right': 'Copy → right',
    'copy-to-left': 'Copy ← left',
    'overwrite-right': 'Overwrite right',
    'overwrite-left': 'Overwrite left',
    'delete-right': 'Delete on right',
    'delete-left': 'Delete on left',
    'rename-to-right': 'Keep both → right',
    'rename-to-left': 'Keep both ← left',
    'conflict-skip': 'Conflict (skip)',
};

/** What the Plan table's Action column says a bucket will do. */
export function actionLabel(action: BucketAction): LabelRef {
    return { key: `aerosync.actionLabel.${action}`, fallback: ACTION_FALLBACK[action] ?? String(action) };
}

const CONFLICT_POLICY_FALLBACK: Record<ConflictPolicy, { label: string; tagline: string }> = {
    'skip': { label: 'Skip', tagline: 'Leave conflicts untouched on both sides.' },
    'rename': { label: 'Keep both', tagline: 'Copy the source-side copy under a timestamped suffix.' },
    'newer-wins': { label: 'Newer wins', tagline: 'The side with the more recent mtime overwrites the other.' },
    'older-wins': { label: 'Older wins', tagline: 'The older side overwrites the newer (archive use case).' },
    'larger-wins': { label: 'Larger wins', tagline: 'The side with the larger size overwrites the smaller.' },
    'smaller-wins': { label: 'Smaller wins', tagline: 'The smaller side overwrites the larger.' },
};

export function conflictPolicyLabel(policy: ConflictPolicy): LabelRef {
    return {
        key: `aerosync.conflictPolicyOption.${policy}.label`,
        fallback: CONFLICT_POLICY_FALLBACK[policy]?.label ?? String(policy),
    };
}

export function conflictPolicyTagline(policy: ConflictPolicy): LabelRef {
    return {
        key: `aerosync.conflictPolicyOption.${policy}.tagline`,
        fallback: CONFLICT_POLICY_FALLBACK[policy]?.tagline ?? '',
    };
}

const BUCKET_FALLBACK: Record<CompareBucket, { name: string; description: string }> = {
    'only-left': { name: 'Only on left', description: 'Present on the source panel, missing from the destination.' },
    'newer-left': { name: 'Newer on left', description: 'Present on both sides, the source copy has the more recent timestamp.' },
    'only-right': { name: 'Only on right', description: 'Present on the destination, missing from the source panel.' },
    'newer-right': { name: 'Newer on right', description: 'Present on both sides, the destination copy has the more recent timestamp.' },
    'conflict': { name: 'Conflict', description: 'Both sides have the entry but they disagree on size with comparable timestamps.' },
    'same': { name: 'Same', description: 'Entries match according to the active comparison policy.' },
};

/** One name per Compare bucket, shared by the Compare list and the Plan table. */
export function bucketNameLabel(bucket: CompareBucket): LabelRef {
    return { key: `aerosync.bucketName.${bucket}`, fallback: BUCKET_FALLBACK[bucket]?.name ?? String(bucket) };
}

export function bucketDescriptionLabel(bucket: CompareBucket): LabelRef {
    return { key: `aerosync.bucketDescription.${bucket}`, fallback: BUCKET_FALLBACK[bucket]?.description ?? '' };
}

const COMPARE_POLICY_FALLBACK: Record<ComparePolicy, string> = {
    'size-only': 'Size only',
    'mtime-only': 'Timestamp only',
    'size-and-mtime': 'Size + timestamp',
};

const COMPARE_POLICY_KEY: Record<ComparePolicy, string> = {
    'size-only': 'sizeOnly',
    'mtime-only': 'mtimeOnly',
    'size-and-mtime': 'sizeAndMtime',
};

export function comparePolicyLabel(policy: ComparePolicy | undefined): LabelRef {
    const p: ComparePolicy = policy ?? 'size-and-mtime';
    return { key: `aerosync.comparePolicy.${COMPARE_POLICY_KEY[p] ?? 'sizeAndMtime'}`, fallback: COMPARE_POLICY_FALLBACK[p] ?? 'Size + timestamp' };
}

/** Every key the labels above can produce, for the locale pin. */
export const PLAN_COMPARE_LABEL_KEYS: string[] = [
    ...(Object.keys(ACTION_FALLBACK) as BucketAction[]).map(a => actionLabel(a).key),
    ...(Object.keys(CONFLICT_POLICY_FALLBACK) as ConflictPolicy[]).flatMap(p => [conflictPolicyLabel(p).key, conflictPolicyTagline(p).key]),
    ...(Object.keys(BUCKET_FALLBACK) as CompareBucket[]).flatMap(b => [bucketNameLabel(b).key, bucketDescriptionLabel(b).key]),
    ...(Object.keys(COMPARE_POLICY_FALLBACK) as ComparePolicy[]).map(p => comparePolicyLabel(p).key),
];
