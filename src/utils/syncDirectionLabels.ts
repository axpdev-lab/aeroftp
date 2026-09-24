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
import { describePreset, type PresetDirection, type SyncPreset } from './syncPresets';

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
