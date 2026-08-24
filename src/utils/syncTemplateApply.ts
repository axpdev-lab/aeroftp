// SPDX-License-Identifier: GPL-3.0-or-later

import type {
    AerosyncImportScriptResult,
    CompressionMode,
    SyncDirection,
    SyncScriptMeta,
    SyncTemplate,
    VerifyPolicy,
} from '../types';
import type { AeroSyncPairKind, AeroSyncVerifyPolicy } from '../components/AeroSync/types';
import type { ConflictPolicy, PresetDirection, SyncPreset } from './syncPresets';

export interface ImportedSyncSettings {
    localPath: string;
    remotePath: string;
    direction: SyncDirection;
    deleteOrphans: boolean;
    excludePatterns: string[];
    parallelStreams?: number;
    compressionMode?: CompressionMode;
    verifyPolicy?: VerifyPolicy;
    dryRun?: boolean;
    conflictMode?: string | null;
}

export type AeroSyncTabStatePatch = Record<string, unknown>;

export function settingsFromTemplate(template: SyncTemplate): ImportedSyncSettings {
    const paths = template.path_patterns[0];
    return {
        localPath: paths?.local || '',
        remotePath: paths?.remote || '',
        direction: template.profile.direction,
        deleteOrphans: template.profile.delete_orphans,
        excludePatterns: template.exclude_patterns,
        parallelStreams: template.profile.parallel_streams,
        compressionMode: template.profile.compression_mode,
        verifyPolicy: template.profile.compare_checksum
            ? 'full'
            : template.profile.compare_timestamp
                ? 'size_and_mtime'
                : 'size_only',
    };
}

export function settingsFromLegacyScript(script: SyncScriptMeta): ImportedSyncSettings {
    return {
        localPath: script.local_path,
        remotePath: script.remote_path,
        direction: script.direction,
        deleteOrphans: script.delete_orphans,
        excludePatterns: script.exclude_patterns,
    };
}

export function settingsFromAerosyncScript(script: AerosyncImportScriptResult): ImportedSyncSettings {
    const imported = script.profile;
    return {
        localPath: imported.local_path,
        remotePath: imported.remote_path,
        direction: imported.profile.direction,
        deleteOrphans: imported.profile.delete_orphans,
        excludePatterns: imported.profile.exclude_patterns,
        parallelStreams: imported.profile.parallel_streams,
        compressionMode: imported.profile.compression_mode,
        verifyPolicy: imported.profile.verify_policy,
        dryRun: imported.dry_run,
        conflictMode: imported.conflict_mode,
    };
}

function planDirection(direction: SyncDirection, pairKind: AeroSyncPairKind | null): PresetDirection {
    const localIsLeft = pairKind !== 'remote-local';
    if (direction === 'remote_to_local') return localIsLeft ? 'right-to-left' : 'left-to-right';
    return localIsLeft ? 'left-to-right' : 'right-to-left';
}

function planPreset(settings: ImportedSyncSettings): SyncPreset {
    if (settings.direction === 'bidirectional') return 'bisync';
    return settings.deleteOrphans ? 'mirror' : 'backup';
}

function planVerify(value: VerifyPolicy | undefined): AeroSyncVerifyPolicy | undefined {
    return value === 'full' ? 'full_checksum' : value;
}

function planConflict(value: string | null | undefined): ConflictPolicy | undefined {
    switch (value) {
        case 'newer': return 'newer-wins';
        case 'older': return 'older-wins';
        case 'larger': return 'larger-wins';
        case 'smaller': return 'smaller-wins';
        case 'rename':
        case 'keep_both': return 'rename';
        case 'skip': return 'skip';
        default: return undefined;
    }
}

/** Convert every import format into the controls owned by AeroSync's tabs. */
export function buildAeroSyncTabStatePatch(
    settings: ImportedSyncSettings,
    pairKind: AeroSyncPairKind | null,
): AeroSyncTabStatePatch {
    const patch: AeroSyncTabStatePatch = {
        'sync.source': settings.localPath,
        'sync.destination': settings.remotePath,
        'sync.exclude': settings.excludePatterns.join(', '),
        'plan.preset': planPreset(settings),
        'plan.direction': planDirection(settings.direction, pairKind),
    };
    if (settings.parallelStreams != null) patch['plan.parallelStreams'] = settings.parallelStreams;
    if (settings.compressionMode != null) patch['plan.compressionMode'] = settings.compressionMode;
    const verify = planVerify(settings.verifyPolicy);
    if (verify) patch['plan.verifyPolicy'] = verify;
    if (settings.dryRun != null) patch['sync.dryRun'] = settings.dryRun;
    const conflict = planConflict(settings.conflictMode);
    if (conflict) patch['plan.conflictPolicy'] = conflict;
    return patch;
}
