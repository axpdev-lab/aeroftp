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
    canaryMode?: boolean;
    canaryPercent?: number;
    canarySelection?: string;
}

/** Plan-tab knobs the .aerosync export must keep (#514). */
export interface LivePlanExport {
    compressionMode?: CompressionMode;
    verifyPolicy?: VerifyPolicy | AeroSyncVerifyPolicy;
    canary?: { percent: number; selection: string } | null;
}

export type AeroSyncTabStatePatch = Record<string, unknown>;

/**
 * Either the settings a template applies, or a refusal carrying the number of
 * usable path pairs it declares, which is what the operator has to act on.
 */
export type TemplateImportResult =
    | { ok: true; settings: ImportedSyncSettings }
    | { ok: false; usablePairs: number };

/**
 * Read the one local/remote pair a template applies, or refuse.
 *
 * `path_patterns` is a list, and an AeroSync tab holds exactly one pair. This
 * used to read `path_patterns[0]` behind an optional chain, which had two
 * silent outcomes: a template with two pairs applied the first and dropped the
 * second with no indication, and a template with none applied two empty paths,
 * which then read as a sync from the filesystem root. Both are worse than not
 * importing, because the operator believes the template was applied.
 *
 * A pair with a blank side counts as unusable rather than as a pair: it leads
 * to the same empty path by a different route.
 */
export function settingsFromTemplate(template: SyncTemplate): TemplateImportResult {
    const usable = (template.path_patterns ?? []).filter(
        (pair) => pair && pair.local?.trim() && pair.remote?.trim(),
    );
    if (usable.length !== 1) return { ok: false, usablePairs: usable.length };
    const paths = usable[0];
    const settings: ImportedSyncSettings = {
        localPath: paths.local,
        remotePath: paths.remote,
        direction: template.profile.direction,
        deleteOrphans: template.profile.delete_orphans,
        excludePatterns: template.exclude_patterns,
        parallelStreams: template.profile.parallel_streams,
        compressionMode: template.profile.compression_mode,
        verifyPolicy: template.profile.verify_policy
            ?? (template.profile.compare_checksum
                ? 'full'
                : template.profile.compare_timestamp
                    ? 'size_and_mtime'
                    : 'size_only'),
        canaryMode: !!template.profile.canary,
        canaryPercent: template.profile.canary?.percent,
        canarySelection: template.profile.canary?.selection,
    };
    return { ok: true, settings };
}

function toTemplateVerify(value: VerifyPolicy | AeroSyncVerifyPolicy | undefined): VerifyPolicy | undefined {
    if (!value) return undefined;
    return value === 'full_checksum' ? 'full' : value;
}

/**
 * Stamp the live Plan-tab values onto an exported .aerosync document.
 * The backend serialises the named preset (Mirror → compression off, no
 * canary, no verify_policy). Without this overlay those knobs round-trip
 * as defaults (#514).
 */
export function overlayLivePlanOnTemplate(template: SyncTemplate, live: LivePlanExport): SyncTemplate {
    const verify = toTemplateVerify(live.verifyPolicy);
    return {
        ...template,
        profile: {
            ...template.profile,
            ...(live.compressionMode ? { compression_mode: live.compressionMode } : {}),
            ...(verify ? { verify_policy: verify } : {}),
            ...(live.canary
                ? { canary: { percent: live.canary.percent, selection: live.canary.selection } }
                : { canary: undefined }),
        },
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
    if (settings.canaryMode != null) patch['plan.canaryMode'] = settings.canaryMode;
    if (settings.canaryPercent != null) patch['plan.canaryPercent'] = settings.canaryPercent;
    if (settings.canarySelection != null) patch['plan.canarySelection'] = settings.canarySelection;
    return patch;
}
