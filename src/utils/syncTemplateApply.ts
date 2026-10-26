// SPDX-License-Identifier: GPL-3.0-or-later

import type {
    AerosyncImportScriptResult,
    SyncDirection,
    SyncScriptMeta,
    SyncTemplate,
    VerifyPolicy,
} from '../types';
import type { AeroSyncPairKind, AeroSyncSpeedMode, AeroSyncVerifyPolicy } from '../components/AeroSync/types';
import type { ConflictPolicy, PresetDirection, SyncPreset, VersionedBackupConfig } from './syncPresets';
import type { TabStateStore } from '../components/AeroSync/tabStateStore';
import { presetForTemplate } from './syncDirectionLabels';
import { AEROSYNC_DEFAULT_BACKUP_DIR, cliExcludePatterns, userExcludePatterns } from './aeroSyncExcludes';

export const parseSyncExcludePatterns = (text: string): string[] =>
    text.split(/[\n,]/).map(pattern => pattern.trim()).filter(Boolean);

/** Read at export time, including an explicitly cleared field and unmounted tabs. */
export const readSyncExcludePatterns = (store: TabStateStore | null, fallback: string[]): string[] =>
    parseSyncExcludePatterns(store?.get('sync.exclude', fallback.join(', ')) ?? fallback.join(', '));

/**
 * What an exported script hands to `aeroftp-cli sync --exclude`
 * ([`cliExcludePatterns`]): the compare's defaults, the user's patterns and
 * the Plan's backup folder. Since the shared matcher the CLI excludes at least
 * what the compare excludes for the same list, so the script does what the
 * Plan showed.
 */
export const readScriptExcludePatterns = (store: TabStateStore | null, fallback: string[]): string[] =>
    cliExcludePatterns(readSyncExcludePatterns(store, fallback), readScriptBackupDir(store));

/**
 * The Plan's backup folder an exported script records on its own, next to
 * the exclude list that also carries it, so an import can give it back to
 * the Plan rather than read it as one of the user's patterns.
 */
export const readScriptBackupDir = (store: TabStateStore | null): string =>
    store?.get<VersionedBackupConfig | undefined>('plan.versionedBackup', undefined)?.backupDir
        ?? AEROSYNC_DEFAULT_BACKUP_DIR;

export interface ImportedSyncSettings {
    localPath: string;
    remotePath: string;
    direction: SyncDirection;
    deleteOrphans: boolean;
    excludePatterns: string[];
    /** True for script formats that append their backup folder to the user's patterns. */
    excludePatternsIncludeBackupDir?: boolean;
    preset?: SyncPreset;
    /** The backup folder the exported script recorded, when it has one. */
    backupDir?: string;
    versionedBackup?: VersionedBackupConfig;
    verifyPolicy?: VerifyPolicy;
    speedMode?: AeroSyncSpeedMode;
    transferBudgetMb?: number;
    errorCorrection?: { enabled: boolean; pct: number };
    dryRun?: boolean;
    conflictMode?: string | null;
    canaryMode?: boolean;
    canaryPercent?: number;
    canarySelection?: string;
}

/** Live Plan controls that a .aerosync export preserves. */
export interface LivePlanExport {
    preset?: SyncPreset;
    direction?: SyncDirection;
    conflictPolicy?: ConflictPolicy;
    versionedBackup?: VersionedBackupConfig;
    speedMode?: AeroSyncSpeedMode;
    verifyPolicy?: VerifyPolicy | AeroSyncVerifyPolicy;
    canary?: { percent: number; selection: string } | null;
    transferBudgetMb?: number;
    errorCorrection?: { enabled: boolean; pct: number };
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
    const backup = template.profile.versioned_backup;
    const settings: ImportedSyncSettings = {
        localPath: paths.local,
        remotePath: paths.remote,
        direction: template.profile.direction,
        deleteOrphans: template.profile.delete_orphans,
        excludePatterns: template.exclude_patterns,
        excludePatternsIncludeBackupDir: false,
        preset: template.profile.mode,
        backupDir: backup?.backup_dir,
        versionedBackup: backup
            ? { enabled: backup.enabled, backupDir: backup.backup_dir }
            : undefined,
        verifyPolicy: template.profile.verify_policy
            ?? (template.profile.compare_checksum
                ? 'full'
                : template.profile.compare_timestamp
                    ? 'size_and_mtime'
                    : 'size_only'),
        speedMode: template.profile.speed_mode,
        transferBudgetMb: template.profile.transfer_budget_mb,
        errorCorrection: template.profile.error_correction,
        conflictMode: template.profile.conflict_mode,
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
 * The `compare_*` triple a verify policy implies. The backend serialises the
 * named preset, so an exported template carried the preset's comparison
 * fields (true / true / false for every stock preset) whatever the Plan tab
 * said, and a reader of the file, human or the CLI's `sync`, saw "size and
 * mtime" on a template exported with full checksum (#514). The verify
 * policy is the only comparison knob the Plan tab exposes, so it is the
 * source of truth for all four fields; `none` verifies nothing after the
 * transfer and says nothing about how to compare, so the preset's own
 * fields stand.
 */
function compareFieldsForVerify(verify: VerifyPolicy): Partial<SyncTemplate['profile']> {
    switch (verify) {
        case 'full':
            return { compare_size: true, compare_timestamp: true, compare_checksum: true };
        case 'size_and_mtime':
            return { compare_size: true, compare_timestamp: true, compare_checksum: false };
        case 'size_only':
            return { compare_size: true, compare_timestamp: false, compare_checksum: false };
        case 'none':
            return {};
    }
}

function toTemplateConflict(value: ConflictPolicy | undefined): SyncTemplate['profile']['conflict_mode'] {
    switch (value) {
        case 'newer-wins': return 'newer';
        case 'older-wins': return 'older';
        case 'larger-wins': return 'larger';
        case 'smaller-wins': return 'smaller';
        case 'rename': return 'rename';
        case 'skip': return 'skip';
        default: return undefined;
    }
}

/**
 * Stamp the live Plan-tab values onto an exported .aerosync document.
 * The backend supplies the portable document and named-preset fields; this
 * overlay makes the controls visible in the current Plan authoritative.
 *
 * Streams and compression are not the Plan's to set: the backend writes them
 * neutral (`export_sync_template`), and an import ignores them.
 */
export function overlayLivePlanOnTemplate(template: SyncTemplate, live: LivePlanExport): SyncTemplate {
    const verify = toTemplateVerify(live.verifyPolicy);
    const conflict = toTemplateConflict(live.conflictPolicy);
    return {
        ...template,
        profile: {
            ...template.profile,
            ...(live.preset
                ? { mode: live.preset, delete_orphans: live.preset === 'mirror' }
                : {}),
            ...(live.direction ? { direction: live.direction } : {}),
            ...(conflict ? { conflict_mode: conflict } : {}),
            ...(live.versionedBackup
                ? {
                    versioned_backup: {
                        enabled: live.versionedBackup.enabled,
                        backup_dir: live.versionedBackup.backupDir ?? AEROSYNC_DEFAULT_BACKUP_DIR,
                    },
                }
                : {}),
            ...(live.speedMode ? { speed_mode: live.speedMode } : {}),
            ...(live.transferBudgetMb != null ? { transfer_budget_mb: live.transferBudgetMb } : {}),
            ...(live.errorCorrection ? { error_correction: live.errorCorrection } : {}),
            ...(verify ? { verify_policy: verify, ...compareFieldsForVerify(verify) } : {}),
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
        backupDir: script.backup_dir ?? undefined,
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
        backupDir: imported.backup_dir ?? undefined,
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
    return settings.preset ?? presetForTemplate(settings.direction, settings.deleteOrphans);
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

/**
 * Convert every import format into the controls owned by AeroSync's tabs.
 * `backupDir` is the Plan's versioned-backup folder at the time of the
 * import: an exported script carries it as its last exclude pattern
 * ([`cliExcludePatterns`]), and it must not land in the user's field, or a
 * later change of the backup folder would leave the old one excluded from
 * every compare and export (review of #979).
 */
export function buildAeroSyncTabStatePatch(
    settings: ImportedSyncSettings,
    pairKind: AeroSyncPairKind | null,
    currentBackup?: VersionedBackupConfig,
): AeroSyncTabStatePatch {
    // A script that recorded its backup folder gives it back to the Plan; an
    // older one is read against the Plan's current folder.
    const backupDir = settings.backupDir
        ?? currentBackup?.backupDir
        ?? AEROSYNC_DEFAULT_BACKUP_DIR;
    const excludesIncludeBackupDir = settings.excludePatternsIncludeBackupDir ?? true;
    const patch: AeroSyncTabStatePatch = {
        'sync.source': settings.localPath,
        'sync.destination': settings.remotePath,
        // Scripts append the compare's backup folder; .aerosync templates
        // already contain only the user's own patterns and stay literal.
        'sync.exclude': (excludesIncludeBackupDir
            ? userExcludePatterns(settings.excludePatterns, backupDir)
            : settings.excludePatterns
        ).join(', '),
        'plan.preset': planPreset(settings),
        'plan.direction': planDirection(settings.direction, pairKind),
    };
    if (settings.versionedBackup) {
        patch['plan.versionedBackup'] = {
            enabled: settings.versionedBackup.enabled,
            backupDir: settings.versionedBackup.backupDir ?? backupDir,
        } satisfies VersionedBackupConfig;
    } else if (settings.backupDir) {
        patch['plan.versionedBackup'] = {
            enabled: currentBackup?.enabled ?? false,
            backupDir: settings.backupDir,
        } satisfies VersionedBackupConfig;
    }
    const verify = planVerify(settings.verifyPolicy);
    if (verify) patch['plan.verifyPolicy'] = verify;
    if (settings.speedMode) patch['plan.speedMode'] = settings.speedMode;
    if (settings.transferBudgetMb != null) patch['plan.transferBudgetMb'] = settings.transferBudgetMb;
    if (settings.errorCorrection) {
        patch['plan.ecEnabled'] = settings.errorCorrection.enabled;
        patch['plan.ecPct'] = settings.errorCorrection.pct;
    }
    if (settings.dryRun != null) patch['sync.dryRun'] = settings.dryRun;
    const conflict = planConflict(settings.conflictMode);
    if (conflict) patch['plan.conflictPolicy'] = conflict;
    if (settings.canaryMode != null) patch['plan.canaryMode'] = settings.canaryMode;
    if (settings.canaryPercent != null) patch['plan.canaryPercent'] = settings.canaryPercent;
    if (settings.canarySelection != null) patch['plan.canarySelection'] = settings.canarySelection;
    return patch;
}
