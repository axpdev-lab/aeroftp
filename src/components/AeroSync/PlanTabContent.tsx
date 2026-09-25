// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { invoke } from '@tauri-apps/api/core';
import {
    AlertTriangle,
    ArrowRight,
    CheckCircle2,
    Copy,
    RefreshCw,
    Terminal,
    FlaskConical,
    Gauge,
    Loader2,
    ShieldAlert,
    ShieldCheck,
    Skull,
    Trash2,
} from 'lucide-react';
import type { CompareResult } from '../../utils/compareEndpoints';
import {
    CONFLICT_POLICIES,
    derivePresetPlan,
    describePreset,
    type BucketAction,
    type ConflictPolicy,
    type PresetDirection,
    type PresetPlan,
    type SyncPreset,
    type VersionedBackupConfig,
} from '../../utils/syncPresets';
import { formatBytes } from '../../utils/formatters';
import {
    actionLabel,
    bucketNameLabel,
    conflictPolicyLabel,
    conflictPolicyTagline,
    planDirectionLabel,
    presetNameLabel,
    presetTaglineLabel,
    resolveLabel,
} from '../../utils/syncDirectionLabels';
import { retryPolicyForSpeed } from '../../utils/remoteSyncRunner';
import { isCyberTheme } from '../Sync/syncConstants';
import { useTranslation } from '../../i18n';
import { useStickyState, useSkipSeedOnRestore } from './tabStateStore';
import {
    AEROSYNC_DEFAULT_BACKUP_DIR,
    AEROSYNC_DEFAULT_EXCLUDES,
    compareExcludePatterns,
    sameExcludePatterns,
} from '../../utils/aeroSyncExcludes';
import { buildCliSyncCommand, type CliNoEquivalentReason } from '../../utils/aeroSyncCliCommand';
import { parseSyncExcludePatterns } from '../../utils/syncTemplateApply';
import { copyText } from '../../utils/clipboard';
import type {
    AeroSyncCanarySelection,
    AeroSyncPairKind,
    AeroSyncRuntime,
    AeroSyncSpeedMode,
    AeroSyncVerifyPolicy,
} from './types';

interface PlanTabContentProps {
    result: CompareResult | null;
    /** GAP-5: true while the recursive connected-remote scan is running. */
    loading?: boolean;
    pairKind?: string | null;
    canExecute: boolean;
    onExecute: (plan: PresetPlan, runtime: AeroSyncRuntime) => void;
    /** The user's exclude patterns the current compare applied. */
    compareExcludes?: string[];
    /** The backup folder the current compare left out; undefined if none was. */
    compareBackupDir?: string;
    /** Re-run the compare with the exclude field and the backup folder. */
    onRescan?: (args: { userExcludes: string[]; backupDir: string }) => void;
    /** True when the remote goes through the provider API rather than the FTP session. */
    isProvider?: boolean;
    /** What the command line under the plan needs to name the same run. */
    cli?: {
        /** Saved server name; absent when the connection is not a saved server. */
        profileName?: string | null;
        profileInitialPath?: string | null;
        localPath: string;
        remotePath: string;
    };
}

/** `sync_backup_validate`: the folder as it will be created, or why not. */
type BackupDirCheck =
    | { status: 'valid'; dir: string; ancestors: string[] }
    | { status: 'invalid'; code: string; message: string };

/** `sync_backup_remote_move`: how this remote moves a file into another folder. */
type RemoteMoveSupport = 'native' | 'server_copy' | 'client_copy' | 'unsupported';

const CLI_REASON_KEYS: Record<CliNoEquivalentReason, string> = {
    'local-pair': 'aerosync.cli.reason.localPair',
    'unsaved-server': 'aerosync.cli.reason.unsavedServer',
    'overwrites-newer': 'aerosync.cli.reason.overwritesNewer',
    'two-way': 'aerosync.cli.reason.twoWay',
    canary: 'aerosync.cli.reason.canary',
    'versioned-backup': 'aerosync.cli.reason.versionedBackup',
    'transfer-budget': 'aerosync.cli.reason.transferBudget',
    'remote-path': 'aerosync.cli.reason.remotePath',
    exclusions: 'aerosync.cli.reason.exclusions',
};

const PRESET_ORDER: SyncPreset[] = ['backup', 'update', 'mirror', 'bisync'];

const ACTION_ICON: Record<BucketAction, React.ReactNode> = {
    skip: <span className="inline-block w-3 h-px bg-current opacity-40" />,
    'copy-to-right': <ArrowRight size={12} />,
    'copy-to-left': <ArrowRight size={12} className="-scale-x-100" />,
    'overwrite-right': <ArrowRight size={12} />,
    'overwrite-left': <ArrowRight size={12} className="-scale-x-100" />,
    'delete-right': <Trash2 size={12} />,
    'delete-left': <Trash2 size={12} />,
    'rename-to-right': <ArrowRight size={12} />,
    'rename-to-left': <ArrowRight size={12} className="-scale-x-100" />,
    'conflict-skip': <AlertTriangle size={12} />,
};

const ACTION_COLOR: Record<BucketAction, string> = {
    skip: 'text-gray-400 dark:text-gray-500',
    'copy-to-right': 'text-sky-600 dark:text-sky-300',
    'copy-to-left': 'text-sky-600 dark:text-sky-300',
    'overwrite-right': 'text-amber-600 dark:text-amber-300',
    'overwrite-left': 'text-amber-600 dark:text-amber-300',
    'delete-right': 'text-rose-600 dark:text-rose-300',
    'delete-left': 'text-rose-600 dark:text-rose-300',
    'rename-to-right': 'text-violet-600 dark:text-violet-300',
    'rename-to-left': 'text-violet-600 dark:text-violet-300',
    'conflict-skip': 'text-rose-500 dark:text-rose-300',
};

const PresetChip: React.FC<{
    preset: SyncPreset;
    active: boolean;
    onSelect: () => void;
}> = ({ preset, active, onSelect }) => {
    const t = useTranslation();
    const info = describePreset(preset);
    const isDefault = preset === 'backup';
    return (
        <button
            type="button"
            onClick={onSelect}
            className={`group flex flex-col items-start gap-1 rounded-lg border px-3 py-2 text-left transition-colors ${
                active
                    ? 'border-blue-500 bg-blue-50 text-blue-900 shadow-sm dark:border-blue-400 dark:bg-blue-900/30 dark:text-blue-100'
                    : 'border-gray-200 bg-white text-gray-700 hover:border-blue-300 hover:bg-blue-50/40 dark:border-gray-700 dark:bg-gray-900/40 dark:text-gray-200 dark:hover:border-blue-500/40 dark:hover:bg-blue-900/20'
            }`}
        >
            <div className="flex w-full items-center justify-between gap-2">
                <div className="flex items-center gap-1.5">
                    {info.safe ? (
                        <ShieldCheck size={14} className="text-emerald-500" />
                    ) : (
                        <ShieldAlert size={14} className="text-amber-500" />
                    )}
                    <span className="text-sm font-semibold">{resolveLabel(t, presetNameLabel(preset))}</span>
                </div>
                {isDefault && (
                    <span className="rounded bg-emerald-100 px-1.5 py-0.5 text-[10px] font-semibold uppercase text-emerald-700 dark:bg-emerald-900/40 dark:text-emerald-300">
                        {t('aerosync.presetDefault') || 'Default'}
                    </span>
                )}
            </div>
            <p className="text-[11px] leading-snug text-gray-500 dark:text-gray-400">{resolveLabel(t, presetTaglineLabel(preset))}</p>
        </button>
    );
};

const BASE_SPEED_MODES: AeroSyncSpeedMode[] = ['normal', 'fast', 'turbo', 'extreme'];
const VERIFY_POLICIES: AeroSyncVerifyPolicy[] = ['none', 'size_only', 'size_and_mtime', 'full_checksum'];
// GAP-9b: stream counts the legacy SyncPanel offered, filtered by the real
// provider capability at render time.
const CANARY_PERCENTS = [5, 10, 25, 50];
const CANARY_SELECTIONS: AeroSyncCanarySelection[] = ['random', 'newest', 'largest'];

export const PlanTabContent: React.FC<PlanTabContentProps> = ({
    result,
    loading,
    pairKind,
    canExecute,
    onExecute,
    compareExcludes = [],
    compareBackupDir,
    onRescan,
    isProvider,
    cli,
}) => {
    const t = useTranslation();
    // One exclude list per pair, shared with the Local mirror tab and the
    // template import and export (the same store key).
    const [excludeText, setExcludeText] = useStickyState('sync.exclude', '');
    // Which command text the last copy was for, so a changed plan never
    // shows "Copied" for a command that was not.
    const [copyResult, setCopyResult] = React.useState<{ text: string; ok: boolean } | null>(null);
    const [preset, setPreset] = useStickyState<SyncPreset>('plan.preset', 'backup');
    const [direction, setDirection] = useStickyState<PresetDirection>('plan.direction', 'left-to-right');
    const [conflictPolicy, setConflictPolicy] = useStickyState<ConflictPolicy>('plan.conflictPolicy', 'skip');
    const [versionedBackup, setVersionedBackup] = useStickyState<VersionedBackupConfig>('plan.versionedBackup', {
        enabled: false,
        backupDir: AEROSYNC_DEFAULT_BACKUP_DIR,
    });
    // What the backend says about the backup folder, and about moving files on
    // this remote, asked before the run so the Plan says it up front.
    const [backupCheck, setBackupCheck] = React.useState<BackupDirCheck | null>(null);
    const [remoteMove, setRemoteMove] = React.useState<RemoteMoveSupport | 'error' | null>(null);
    const [confirmedDestructive, setConfirmedDestructive] = React.useState(false);
    // CO-1: Speed mode + verify policy are now lifted into the
    // `onExecute(plan, runtime)` callback so App.tsx can forward them
    // to the unified runner (LocalSyncRequest.speed_mode /
    // verify_policy) and the Rust backend can honour the verify pass.
    const [speedMode, setSpeedMode] = useStickyState<AeroSyncSpeedMode>('plan.speedMode', 'normal');
    const [verifyPolicy, setVerifyPolicy] = useStickyState<AeroSyncVerifyPolicy>('plan.verifyPolicy', 'size_only');
    // GAP-7: Canary trial — only meaningful for a connected remote.
    const [canaryMode, setCanaryMode] = useStickyState('plan.canaryMode', false);
    const [canaryPercent, setCanaryPercent] = useStickyState('plan.canaryPercent', 10);
    const [canarySelection, setCanarySelection] = useStickyState<AeroSyncCanarySelection>('plan.canarySelection', 'random');
    // GAP-8: transfer budget in MB (0 = unlimited); retry policy is derived
    // from the speed mode, versioned-backup reuses the existing toggle.
    const [transferBudgetMb, setTransferBudgetMb] = useStickyState('plan.transferBudgetMb', 0);
    // GAP-9a: Maniac is the Cyber-theme-gated 5th speed mode. Selecting it
    // arms a warning card; Execute stays blocked until the user confirms.
    const [maniacConfirmed, setManiacConfirmed] = React.useState(false);
    // P3: Error Correction (AeroSync EC slice). Reuses the exact preset+slider+input
    // trio + clamp from VaultCreate.tsx:183-227 (vault.recoveryLevel* i18n).
    // Default: only 'backup' preset (Backup-class) gets enabled=true + pct=15 (Medium);
    // Mirror/Two-way/Pull/update default to OFF. Always rendered (per handoff minima).
    const [ecEnabled, setEcEnabled] = useStickyState('plan.ecEnabled', true);
    const [ecPct, setEcPct] = useStickyState('plan.ecPct', 15);
    const isConnectedRemote = pairKind === 'local-remote' || pairKind === 'remote-local';
    React.useEffect(() => {
        let cancelled = false;
        invoke<BackupDirCheck>('sync_backup_validate', { dir: versionedBackup.backupDir ?? '' })
            .then((check) => { if (!cancelled) setBackupCheck(check); })
            .catch((e) => { if (!cancelled) setBackupCheck({ status: 'invalid', code: 'error', message: String(e) }); });
        return () => { cancelled = true; };
    }, [versionedBackup.backupDir]);
    React.useEffect(() => {
        if (!versionedBackup.enabled || !isConnectedRemote) {
            setRemoteMove(null);
            return;
        }
        let cancelled = false;
        invoke<RemoteMoveSupport>('sync_backup_remote_move', { useProvider: isProvider === true })
            .then((support) => { if (!cancelled) setRemoteMove(support); })
            .catch(() => { if (!cancelled) setRemoteMove('error'); });
        return () => { cancelled = true; };
    }, [versionedBackup.enabled, isConnectedRemote, isProvider]);
    const showManiac = isCyberTheme();
    const speedModes: AeroSyncSpeedMode[] = showManiac
        ? [...BASE_SPEED_MODES, 'maniac']
        : BASE_SPEED_MODES;
    const maniacArmed = speedMode === 'maniac';
    const maniacBlocked = maniacArmed && !maniacConfirmed;

    React.useEffect(() => {
        setConfirmedDestructive(false);
    }, [preset, direction, conflictPolicy, versionedBackup.enabled, versionedBackup.backupDir]);

    // GAP-9a: any move away from Maniac re-arms the confirmation gate, so a
    // later return to Maniac always re-prompts.
    React.useEffect(() => {
        if (speedMode !== 'maniac') setManiacConfirmed(false);
    }, [speedMode]);


    // P3 EC profile defaults (handoff §2c): Backup preset → ON/Medium(15%).
    // Other presets (mirror, update, bisync) → OFF. Effect resets on preset
    // change so the recommended default for the chosen "profile" wins; manual
    // toggle on a non-backup preset is a per-run override until next preset pick.
    // Same guard: a per-run EC override must survive Plan -> Sync -> Plan.
    const skipEcSeed = useSkipSeedOnRestore('plan.ecEnabled', 'plan.ecPct', 'plan.preset');
    React.useEffect(() => {
        if (skipEcSeed.current) {
            skipEcSeed.current = false;
            return;
        }
        if (preset === 'backup') {
            setEcEnabled(true);
            setEcPct(15);
        } else {
            setEcEnabled(false);
        }
    }, [preset]);

    if (!result) {
        if (loading) {
            return (
                <div className="flex flex-col items-center gap-2 p-8 text-center text-sm text-gray-500 dark:text-gray-400">
                    <Loader2 size={24} className="animate-spin text-blue-500" />
                    {t('syncPanel.scanning') || 'Scanning directories'}
                </div>
            );
        }
        return (
            <div className="p-6 text-center text-sm text-gray-500 dark:text-gray-400">
                {t('aerosync.planUnavailable') || 'A compare result is required before picking a preset. Open the Compare tab first.'}
            </div>
        );
    }

    const plan = derivePresetPlan(result, { preset, direction, conflictPolicy, versionedBackup });
    const bisyncMode = preset === 'bisync';
    const userExcludes = parseSyncExcludePatterns(excludeText);
    // The compare has to have applied the patterns the field shows before the
    // plan built from it may run.
    const excludesStale = !sameExcludePatterns(userExcludes, compareExcludes);
    // Versioned backup, settled before the run: the folder must be valid, not
    // inside a folder this plan writes, left out of the compare that built
    // the plan, and movable into on every side the plan writes.
    const validBackupDir = backupCheck?.status === 'valid' ? backupCheck.dir : null;
    const backupStale = validBackupDir !== null && validBackupDir !== compareBackupDir;
    const compareStale = excludesStale || backupStale;
    const localIsLeft = pairKind !== 'remote-local';
    const writesRemote = isConnectedRemote
        && (bisyncMode || (direction === 'left-to-right') === localIsLeft);
    const writtenAncestor = backupCheck?.status === 'valid'
        ? backupCheck.ancestors.find((folder) => plan.bucketPlans.some((bp) =>
            bp.action !== 'skip' && bp.action !== 'conflict-skip'
            && bp.entries.some((entry) => {
                const path = entry.relativePath ?? entry.name;
                return path === folder || path.startsWith(`${folder}/`);
            })))
        : undefined;
    let backupBlock: string | null = null;
    if (versionedBackup.enabled) {
        if (backupCheck?.status === 'invalid') {
            const code = ({ empty: 'empty', absolute: 'absolute', parent: 'parent' } as Record<string, string>)[backupCheck.code] ?? 'name';
            backupBlock = t(`aerosync.backup.invalid.${code}`) || backupCheck.message;
        } else if (writtenAncestor) {
            backupBlock = t('aerosync.backup.nested', { folder: writtenAncestor })
                || `The backup folder is inside ${writtenAncestor}, which this sync writes to.`;
        } else if (writesRemote && remoteMove === 'unsupported') {
            backupBlock = t('aerosync.backup.unsupported')
                || 'This server cannot move files into another folder, so it cannot keep old copies.';
        } else if (writesRemote && remoteMove === 'error') {
            backupBlock = t('aerosync.backup.checkFailed')
                || 'Could not ask the server how it moves files.';
        } else if (backupCheck === null || (writesRemote && remoteMove === null)) {
            backupBlock = t('aerosync.backup.checking') || 'Checking how this server moves files...';
        }
    }
    const cliLine = buildCliSyncCommand({
        pairKind: (pairKind ?? null) as AeroSyncPairKind | null,
        preset,
        direction,
        profileName: cli?.profileName,
        profileInitialPath: cli?.profileInitialPath,
        localPath: cli?.localPath ?? '',
        remotePath: cli?.remotePath ?? '',
        excludes: compareExcludePatterns(userExcludes),
        canary: canaryMode,
        transferBudgetBytes: transferBudgetMb > 0 ? transferBudgetMb * 1024 * 1024 : 0,
        errorCorrectionPct: ecEnabled ? ecPct : null,
        versionedBackup: versionedBackup.enabled,
        shell: typeof navigator !== 'undefined' && navigator.platform.startsWith('Win') ? 'powershell' : 'posix',
    });
    const copyCli = (text: string) => {
        copyText(text).then(
            () => setCopyResult({ text, ok: true }),
            () => setCopyResult({ text, ok: false }),
        );
    };

    const executable = canExecute && plan.totals.actionable > 0;
    const needsConfirm = plan.hasDestructive;
    // A canary run is a non-destructive trial, so it does not gate on the
    // destructive-confirm checkbox.
    // GAP-9a: Maniac additionally blocks Execute until its own warning card
    // is acknowledged.
    const canFireExecute =
        executable
        && (canaryMode || !needsConfirm || confirmedDestructive)
        && !maniacBlocked
        && !compareStale
        && !backupBlock;

    return (
        <div className="flex flex-col">
            <div className="px-4 py-3">
                <div className="mb-1 text-[11px] font-semibold uppercase tracking-wide text-gray-500 dark:text-gray-400">
                    {t('aerosync.modeHeader') || 'Mode'}
                </div>
                <div className="grid grid-cols-2 gap-2 sm:grid-cols-4">
                    {PRESET_ORDER.map((option) => (
                        <PresetChip
                            key={option}
                            preset={option}
                            active={preset === option}
                            onSelect={() => setPreset(option)}
                        />
                    ))}
                </div>

                <div className="mt-3 flex flex-wrap items-center justify-between gap-2">
                    <div className="text-[11px] text-gray-500 dark:text-gray-400">
                        {resolveLabel(t, presetTaglineLabel(preset))}
                    </div>
                    {!bisyncMode && (
                        <div className="flex items-center gap-2">
                        <span className="text-[11px] font-semibold uppercase tracking-wide text-gray-500 dark:text-gray-400">
                            {t('syncPanel.direction') || 'Direction'}
                            {/* The CLI flag names the same choice only against a
                                remote: its local-to-local mode ignores it. */}
                            {isConnectedRemote && (
                                <span className="ml-1 font-mono normal-case tracking-normal">(--direction)</span>
                            )}
                        </span>
                        <div className="inline-flex overflow-hidden rounded-md border border-gray-200 dark:border-gray-700">
                            <button
                                type="button"
                                onClick={() => setDirection('left-to-right')}
                                className={`px-2 py-1 text-[11px] font-medium ${
                                    direction === 'left-to-right'
                                        ? 'bg-blue-500 text-white'
                                        : 'bg-white text-gray-600 hover:bg-gray-50 dark:bg-gray-900/40 dark:text-gray-300 dark:hover:bg-gray-700'
                                }`}
                            >
                                {resolveLabel(t, planDirectionLabel('left-to-right', pairKind as AeroSyncPairKind | null))}
                            </button>
                            <button
                                type="button"
                                onClick={() => setDirection('right-to-left')}
                                className={`px-2 py-1 text-[11px] font-medium ${
                                    direction === 'right-to-left'
                                        ? 'bg-blue-500 text-white'
                                        : 'bg-white text-gray-600 hover:bg-gray-50 dark:bg-gray-900/40 dark:text-gray-300 dark:hover:bg-gray-700'
                                }`}
                            >
                                {resolveLabel(t, planDirectionLabel('right-to-left', pairKind as AeroSyncPairKind | null))}
                            </button>
                        </div>
                        </div>
                    )}
                </div>
            </div>

            <div className="border-t border-gray-200 px-4 py-3 dark:border-gray-700">
                <label className="mb-1 block text-[11px] font-semibold uppercase tracking-wide text-gray-500 dark:text-gray-400">
                    {t('aerosync.exclude.label') || 'Exclude patterns'}
                </label>
                <textarea
                    value={excludeText}
                    onChange={(event) => setExcludeText(event.target.value)}
                    rows={2}
                    spellCheck={false}
                    placeholder="cache, *.tmp, build/output"
                    className="w-full rounded-md border border-gray-300 bg-white px-2 py-1.5 font-mono text-[12px] text-gray-800 focus:border-blue-400 focus:outline-none dark:border-gray-600 dark:bg-gray-900/60 dark:text-gray-100"
                />
                <p className="mt-1 text-[10px] leading-snug text-gray-500 dark:text-gray-400">
                    {t('aerosync.exclude.hint', { list: AEROSYNC_DEFAULT_EXCLUDES.join(', ') })
                        || `Comma or newline separated, for example cache, *.tmp or build/output. Always excluded: ${AEROSYNC_DEFAULT_EXCLUDES.join(', ')}.`}
                </p>
            </div>

            <div className="border-t border-gray-200 px-4 py-3 dark:border-gray-700">
                <div className="grid gap-3 sm:grid-cols-2">
                    <div>
                        <div className="mb-1 flex items-center justify-between gap-2">
                            <label className="text-[11px] font-semibold uppercase tracking-wide text-gray-500 dark:text-gray-400">
                                {t('aerosync.conflictPolicy') || 'Conflict policy'}
                            </label>
                            {preset === 'mirror' && (
                                <span className="rounded bg-amber-100 px-1.5 py-0.5 text-[10px] font-semibold text-amber-700 dark:bg-amber-900/40 dark:text-amber-300">
                                    {t('aerosync.ignoredForMirror') || 'IGNORED FOR MIRROR'}
                                </span>
                            )}
                        </div>
                        <select
                            value={conflictPolicy}
                            onChange={(event) => setConflictPolicy(event.target.value as ConflictPolicy)}
                            disabled={preset === 'mirror'}
                            className="w-full rounded-md border border-gray-300 bg-white px-2 py-1.5 text-sm text-gray-800 focus:border-blue-400 focus:outline-none disabled:opacity-50 dark:border-gray-600 dark:bg-gray-900/60 dark:text-gray-100"
                        >
                            {CONFLICT_POLICIES.map((policy) => {
                                return (
                                    <option key={policy} value={policy}>
                                        {resolveLabel(t, conflictPolicyLabel(policy))}
                                    </option>
                                );
                            })}
                        </select>
                        <p className="mt-1 text-[10px] leading-snug text-gray-500 dark:text-gray-400">
                            {resolveLabel(t, conflictPolicyTagline(conflictPolicy))}
                        </p>
                    </div>
                    <div>
                        <div className="mb-1 flex items-center justify-between gap-2">
                            <label className="text-[11px] font-semibold uppercase tracking-wide text-gray-500 dark:text-gray-400">
                                {t('aerosync.versionedBackup') || 'Versioned backup'}
                            </label>
                            <label className="inline-flex cursor-pointer items-center gap-1 text-[10px] font-medium text-gray-500 dark:text-gray-400">
                                <input
                                    type="checkbox"
                                    checked={versionedBackup.enabled}
                                    onChange={(event) => setVersionedBackup((prev) => ({ ...prev, enabled: event.target.checked }))}
                                    className="h-3 w-3 rounded border-gray-300 text-blue-600 focus:ring-blue-500"
                                />
                                {t('aerosync.enable') || 'Enable'}
                            </label>
                        </div>
                        <input
                            type="text"
                            value={versionedBackup.backupDir ?? ''}
                            onChange={(event) => setVersionedBackup((prev) => ({ ...prev, backupDir: event.target.value || '.aeroftp-versions' }))}
                            disabled={!versionedBackup.enabled}
                            placeholder=".aeroftp-versions"
                            className="w-full rounded-md border border-gray-300 bg-white px-2 py-1.5 font-mono text-xs text-gray-800 focus:border-blue-400 focus:outline-none disabled:opacity-50 dark:border-gray-600 dark:bg-gray-900/60 dark:text-gray-100"
                        />
                        <p className="mt-1 text-[10px] leading-snug text-gray-500 dark:text-gray-400">
                            {t('aerosync.versionedBackupHint', { dir: validBackupDir ?? versionedBackup.backupDir ?? AEROSYNC_DEFAULT_BACKUP_DIR })
                                || `Before a file is overwritten or deleted, its destination copy is moved to ${validBackupDir ?? versionedBackup.backupDir}/<run time>/ in the destination folder.`}
                        </p>
                        {versionedBackup.enabled && writesRemote && (remoteMove === 'server_copy' || remoteMove === 'client_copy') && (
                            <p className="mt-1 text-[10px] leading-snug text-amber-700 dark:text-amber-300">
                                {remoteMove === 'server_copy'
                                    ? (t('aerosync.backup.serverCopy') || 'This server has no move: each kept copy is copied on the server and then deleted.')
                                    : (t('aerosync.backup.clientCopy') || 'This server has no move: each kept copy is downloaded, uploaded to the backup folder and then deleted.')}
                            </p>
                        )}
                        {backupBlock && (
                            <p className="mt-1 text-[10px] leading-snug text-rose-600 dark:text-rose-300">
                                {backupBlock}
                            </p>
                        )}
                    </div>
                </div>

                {/* SLICE 3: Speed mode + Verify policy. Visible knobs that
                    map onto the SyncPanel speed presets and verify
                    pipeline; wired into the execution path in SLICE 4. */}
                <div className="mt-3 grid gap-3 sm:grid-cols-2">
                    <div>
                        <label className="mb-1 block text-[11px] font-semibold uppercase tracking-wide text-gray-500 dark:text-gray-400">
                            <Gauge size={11} className="mr-1 inline align-text-bottom" />
                            {t('aerosync.speedMode') || 'Speed mode'}
                        </label>
                        <div className="inline-flex w-full overflow-hidden rounded-md border border-gray-200 dark:border-gray-700">
                            {speedModes.map((mode) => {
                                const active = speedMode === mode;
                                const isManiac = mode === 'maniac';
                                return (
                                    <button
                                        key={mode}
                                        type="button"
                                        onClick={() => setSpeedMode(mode)}
                                        title={isManiac
                                            ? (t('syncPanel.speedManiacTooltip') || '')
                                            : undefined}
                                        className={`flex flex-1 items-center justify-center gap-1 px-2 py-1 text-[11px] font-medium capitalize ${
                                            active
                                                ? isManiac
                                                    ? 'bg-rose-600 text-white'
                                                    : 'bg-blue-500 text-white'
                                                : 'bg-white text-gray-600 hover:bg-gray-50 dark:bg-gray-900/40 dark:text-gray-300 dark:hover:bg-gray-700'
                                        }`}
                                    >
                                        {isManiac && <Skull size={11} />}
                                        {isManiac
                                            ? (t('syncPanel.speedManiac') || 'Maniac')
                                            : (t(`aerosync.speedMode_${mode}`) || mode)}
                                    </button>
                                );
                            })}
                        </div>
                        <p className="mt-1 text-[10px] leading-snug text-gray-500 dark:text-gray-400">
                            {t('aerosync.speedModeHint') || 'Sets the retry policy. From Fast up, changed files are sent as deltas where the server supports it (SFTP with rsync).'}
                        </p>
                    </div>
                    <div>
                        <label className="mb-1 block text-[11px] font-semibold uppercase tracking-wide text-gray-500 dark:text-gray-400">
                            <ShieldCheck size={11} className="mr-1 inline align-text-bottom" />
                            {t('aerosync.verifyPolicy') || 'Verify policy'}
                        </label>
                        <select
                            value={maniacArmed ? 'none' : verifyPolicy}
                            onChange={(event) => setVerifyPolicy(event.target.value as AeroSyncVerifyPolicy)}
                            disabled={maniacArmed}
                            className="w-full rounded-md border border-gray-300 bg-white px-2 py-1.5 text-sm text-gray-800 focus:border-blue-400 focus:outline-none disabled:opacity-50 dark:border-gray-600 dark:bg-gray-900/60 dark:text-gray-100"
                        >
                            {VERIFY_POLICIES.map((policy) => (
                                <option key={policy} value={policy}>
                                    {t(`aerosync.verifyPolicy_${policy}`) || policy}
                                </option>
                            ))}
                        </select>
                        <p className="mt-1 text-[10px] leading-snug text-gray-500 dark:text-gray-400">
                            {maniacArmed
                                ? (t('syncPanel.maniacWarningBody') || 'No journal and no verification during the run, bandwidth limits are ignored, and a failed file gets only 2 quick retries. After the run, downloaded files are verified; uploaded files are not.')
                                : (t('aerosync.verifyPolicyHint') || 'Post-transfer integrity check applied to each file.')}
                        </p>
                    </div>
                </div>

                {/* P3: Error Correction in Plan tab. Toggle + the reusable
                    preset+slider+input (copied from VaultCreate:183-227, same
                    4 levels, same Math.min/max(5,50) clamp, same vault.* i18n
                    labels for levels). Visible for all; defaults driven by
                    preset (Backup ON 15%, others OFF). Does not touch codec. */}
                <div className="mt-3">
                    <div className="mb-1 flex items-center gap-2">
                        <label className="text-[11px] font-semibold uppercase tracking-wide text-gray-500 dark:text-gray-400">
                            <ShieldCheck size={11} className="mr-1 inline align-text-bottom" />
                            {t('aerosync.errorCorrection') || 'Error Correction'}
                        </label>
                        <label className="inline-flex cursor-pointer items-center gap-1 text-[10px] font-medium text-gray-500 dark:text-gray-400">
                            <input
                                type="checkbox"
                                checked={ecEnabled}
                                onChange={(event) => setEcEnabled(event.target.checked)}
                                className="h-3 w-3 rounded border-gray-300 text-blue-600 focus:ring-blue-500"
                            />
                            {t('aerosync.enable') || 'Enable'}
                        </label>
                    </div>
                    {ecEnabled && (
                        <div className="pl-1 flex flex-col gap-2">
                            <div className="grid grid-cols-4 gap-1.5">
                                {([
                                    { id: 7, label: t('vault.recoveryLevelLow') },
                                    { id: 15, label: t('vault.recoveryLevelMedium') },
                                    { id: 25, label: t('vault.recoveryLevelQuartile') },
                                    { id: 30, label: t('vault.recoveryLevelHigh') },
                                ] as const).map(lvl => {
                                    const selected = ecPct === lvl.id;
                                    return (
                                        <button
                                            key={lvl.id}
                                            onClick={() => setEcPct(lvl.id)}
                                            className={`rounded border px-1.5 py-1 text-center ${selected
                                                ? 'border-amber-500 bg-amber-500/10 text-amber-300'
                                                : 'border-gray-300 dark:border-gray-600 bg-gray-50 dark:bg-gray-800'}`}
                                        >
                                            <div className="text-[11px] font-medium">{lvl.label}</div>
                                            <div className="text-[10px] text-gray-500 dark:text-gray-400">~{lvl.id}%</div>
                                        </button>
                                    );
                                })}
                            </div>
                            <div className="flex items-center gap-2">
                                <input
                                    type="range"
                                    min={5}
                                    max={50}
                                    step={1}
                                    value={ecPct}
                                    onChange={e => setEcPct(Number(e.target.value))}
                                    className="flex-1 accent-amber-600"
                                    aria-label={t('vault.recoveryLevel') || 'Recovery level'}
                                />
                                <div className="flex items-center gap-1">
                                    <input
                                        type="number"
                                        min={5}
                                        max={50}
                                        value={ecPct}
                                        onChange={e => setEcPct(Math.min(50, Math.max(5, Math.round(Number(e.target.value) || 5))))}
                                        className="w-14 bg-gray-50 dark:bg-gray-800 border border-gray-300 dark:border-gray-600 rounded px-1.5 py-0.5 text-[12px] text-right"
                                    />
                                    <span className="text-[11px] text-gray-500 dark:text-gray-400">%</span>
                                </div>
                            </div>
                            <div className="text-[10px] text-gray-500 dark:text-gray-400">
                                {t('aerosync.errorCorrectionHint') || 'Stores a small .aerocorrect parity file next to each backed-up file so a bit-rotted remote copy can be repaired on the next pull, without the original.'}
                            </div>
                        </div>
                    )}
                </div>

                {/* GAP-9a: Maniac confirmation gate. Selecting the Cyber-only
                    5th speed mode arms this card; Execute is blocked until the
                    user acknowledges it. Returning to any other mode re-arms
                    the gate. */}
                {maniacArmed && (
                    <div className={`mt-3 rounded-md border p-2.5 ${
                        maniacConfirmed
                            ? 'border-emerald-300 bg-emerald-50 dark:border-emerald-700 dark:bg-emerald-900/20'
                            : 'border-rose-300 bg-rose-50 dark:border-rose-700 dark:bg-rose-900/20'
                    }`}>
                        <div className="flex items-center gap-2">
                            <Skull size={14} className={maniacConfirmed
                                ? 'text-emerald-500'
                                : 'text-rose-500'} />
                            <span className={`text-[12px] font-bold ${
                                maniacConfirmed
                                    ? 'text-emerald-700 dark:text-emerald-300'
                                    : 'text-rose-700 dark:text-rose-300'
                            }`}>
                                {t('syncPanel.maniacWarningTitle') || 'MANIAC MODE'}
                                {maniacConfirmed && (
                                    <span className="ml-1 font-medium">
                                        &middot; {t('syncPanel.maniacActive') || 'Active'}
                                    </span>
                                )}
                            </span>
                        </div>
                        {!maniacConfirmed && (
                            <>
                                <p className="mt-1.5 text-[11px] leading-relaxed text-rose-800 dark:text-rose-200/90">
                                    {t('syncPanel.maniacWarningBody')
                                        || 'No journal. No verification. No retry. Maximum raw speed.'}
                                </p>
                                <button
                                    type="button"
                                    onClick={() => setManiacConfirmed(true)}
                                    className="mt-2 rounded-lg border border-rose-400 bg-rose-500/20 px-3 py-1.5 text-[11px] font-medium text-rose-700 transition-colors hover:bg-rose-500/30 dark:text-rose-300"
                                >
                                    {t('syncPanel.maniacConfirm') || 'I understand, proceed'}
                                </button>
                            </>
                        )}
                    </div>
                )}


                {/* GAP-8: transfer budget — connected-remote only. Retry
                    policy derives from the speed mode; versioned backup
                    reuses the toggle above. */}
                {isConnectedRemote && (
                    <div className="mt-3">
                        <label className="mb-1 block text-[11px] font-semibold uppercase tracking-wide text-gray-500 dark:text-gray-400">
                            {t('syncPanel.transferBudget') || 'Transfer budget'} (MB)
                        </label>
                        <input
                            type="number"
                            min={0}
                            value={transferBudgetMb}
                            onChange={(event) => setTransferBudgetMb(
                                Math.max(0, Math.floor(Number(event.target.value) || 0)),
                            )}
                            placeholder="0"
                            className="w-32 rounded-md border border-gray-300 bg-white px-2 py-1.5 text-sm text-gray-800 focus:border-blue-400 focus:outline-none dark:border-gray-600 dark:bg-gray-900/60 dark:text-gray-100"
                        />
                    </div>
                )}

                {/* GAP-7: Canary trial — connected-remote only. */}
                {isConnectedRemote && (
                    <div className="mt-3 rounded-md border border-gray-200 p-2.5 dark:border-gray-700">
                        <label className="flex cursor-pointer items-center gap-2 text-[11px] font-semibold uppercase tracking-wide text-gray-500 dark:text-gray-400">
                            <input
                                type="checkbox"
                                checked={canaryMode}
                                onChange={(event) => setCanaryMode(event.target.checked)}
                                className="h-3.5 w-3.5 rounded border-gray-300 text-blue-600 focus:ring-blue-500"
                            />
                            <FlaskConical size={12} className="text-blue-500" />
                            {t('syncPanel.canaryMode') || 'Canary Mode'}
                        </label>
                        <p className="mt-1 text-[10px] leading-snug text-gray-500 dark:text-gray-400">
                            {t('syncPanel.canaryDesc') || 'Run a trial sync on a subset of files before committing to the full operation.'}
                        </p>
                        <p className="mt-1 text-[10px] leading-snug text-gray-500 dark:text-gray-400">
                            {t('syncPanel.canaryDryRun') || 'Nothing is transferred: this previews what a full sync would do, computed from the sample (similar to rclone --dry-run).'}
                        </p>
                        {canaryMode && (
                            <div className="mt-2 flex flex-wrap gap-3">
                                <label className="flex items-center gap-1 text-[11px] text-gray-600 dark:text-gray-300">
                                    {t('syncPanel.canarySample') || 'Sample'}
                                    <select
                                        value={canaryPercent}
                                        onChange={(event) => setCanaryPercent(Number(event.target.value))}
                                        className="rounded border border-gray-300 bg-white px-1.5 py-0.5 text-[11px] dark:border-gray-600 dark:bg-gray-900/60 dark:text-gray-100"
                                    >
                                        {CANARY_PERCENTS.map((p) => (
                                            <option key={p} value={p}>{p}%</option>
                                        ))}
                                    </select>
                                </label>
                                <label className="flex items-center gap-1 text-[11px] text-gray-600 dark:text-gray-300">
                                    {t('syncPanel.canaryStrategy') || 'Strategy'}
                                    <select
                                        value={canarySelection}
                                        onChange={(event) => setCanarySelection(event.target.value as AeroSyncCanarySelection)}
                                        className="rounded border border-gray-300 bg-white px-1.5 py-0.5 text-[11px] dark:border-gray-600 dark:bg-gray-900/60 dark:text-gray-100"
                                    >
                                        {CANARY_SELECTIONS.map((sel) => (
                                            <option key={sel} value={sel}>
                                                {t(`syncPanel.canary${sel.charAt(0).toUpperCase()}${sel.slice(1)}`) || sel}
                                            </option>
                                        ))}
                                    </select>
                                </label>
                            </div>
                        )}
                    </div>
                )}
            </div>

            <div className="border-t border-gray-200 px-4 py-3 dark:border-gray-700">
                <div className="grid gap-3 sm:grid-cols-4">
                    <div className="rounded-md bg-gray-50 p-2 dark:bg-gray-900/40">
                        <div className="text-[10px] uppercase tracking-wide text-gray-500 dark:text-gray-400">
                            {t('aerosync.actionable') || 'Actionable'}
                        </div>
                        <div className="text-base font-semibold text-gray-900 dark:text-white">{plan.totals.actionable}</div>
                    </div>
                    <div className="rounded-md bg-gray-50 p-2 dark:bg-gray-900/40">
                        <div className="text-[10px] uppercase tracking-wide text-gray-500 dark:text-gray-400">
                            {t('aerosync.skipped') || 'Skipped'}
                        </div>
                        <div className="text-base font-semibold text-gray-900 dark:text-white">{plan.totals.skipped}</div>
                    </div>
                    <div className="rounded-md bg-gray-50 p-2 dark:bg-gray-900/40">
                        <div className="text-[10px] uppercase tracking-wide text-gray-500 dark:text-gray-400">
                            {t('aerosync.transfer') || 'Transfer'}
                        </div>
                        <div className="text-base font-semibold text-gray-900 dark:text-white">{formatBytes(plan.totals.transferBytes)}</div>
                    </div>
                    <div className={`rounded-md p-2 ${plan.hasDestructive ? 'bg-amber-50 dark:bg-amber-900/30' : 'bg-gray-50 dark:bg-gray-900/40'}`}>
                        <div className="text-[10px] uppercase tracking-wide text-gray-500 dark:text-gray-400">
                            {versionedBackup.enabled
                                ? (t('aerosync.versionedBackup') || 'Versioned backup')
                                : (t('aerosync.destructive') || 'Destructive')}
                        </div>
                        <div className={`text-base font-semibold ${plan.hasDestructive ? 'text-amber-700 dark:text-amber-200' : 'text-gray-900 dark:text-white'}`}>
                            {versionedBackup.enabled
                                ? formatBytes(plan.totals.versionedBackupBytes)
                                : (plan.totals.deleteRight + plan.totals.deleteLeft + (plan.hasOverwritesNewer ? plan.totals.overwriteLeft + plan.totals.overwriteRight : 0))}
                        </div>
                    </div>
                </div>
            </div>

            <div className="max-h-[35vh] space-y-1 overflow-y-auto border-t border-gray-200 px-4 py-3 dark:border-gray-700">
                <table className="w-full text-[11px]">
                    <thead className="text-left text-gray-500 dark:text-gray-400">
                        <tr>
                            <th className="px-2 py-1 font-medium">{t('aerosync.bucket') || 'Bucket'}</th>
                            <th className="px-2 py-1 font-medium text-right">{t('aerosync.count') || 'Count'}</th>
                            <th className="px-2 py-1 font-medium">{t('aerosync.action') || 'Action'}</th>
                            <th className="px-2 py-1 font-medium text-right">{t('aerosync.transfer') || 'Transfer'}</th>
                        </tr>
                    </thead>
                    <tbody>
                        {plan.bucketPlans.map((bp) => (
                            <tr
                                key={bp.bucket}
                                className={`border-t border-gray-100 dark:border-gray-700/60 ${
                                    bp.destructive
                                        ? 'bg-amber-50/60 dark:bg-amber-900/20'
                                        : ''
                                }`}
                            >
                                <td className="px-2 py-1 font-medium text-gray-800 dark:text-gray-200">{resolveLabel(t, bucketNameLabel(bp.bucket))}</td>
                                <td className="px-2 py-1 text-right text-gray-700 dark:text-gray-200">{bp.entries.length}</td>
                                <td className={`px-2 py-1 ${ACTION_COLOR[bp.action]}`}>
                                    <span className="inline-flex items-center gap-1">
                                        {ACTION_ICON[bp.action]}
                                        {resolveLabel(t, actionLabel(bp.action))}
                                        {bp.destructive && <ShieldAlert size={11} className="text-amber-500" />}
                                    </span>
                                </td>
                                <td className="px-2 py-1 text-right text-gray-600 dark:text-gray-300">{formatBytes(bp.transferBytes)}</td>
                            </tr>
                        ))}
                    </tbody>
                </table>
            </div>

            {needsConfirm && (
                <div className="border-t border-amber-200 bg-amber-50 px-4 py-3 dark:border-amber-800 dark:bg-amber-900/30">
                    <div className="flex items-start gap-2">
                        <AlertTriangle size={16} className="mt-0.5 shrink-0 text-amber-600 dark:text-amber-300" />
                        <div className="flex-1 text-[12px] text-amber-900 dark:text-amber-100">
                            <p className="font-semibold">
                                {t('aerosync.destructiveWarning') || 'This preset will delete or overwrite newer files.'}
                            </p>
                            <p className="text-[11px] text-amber-800 dark:text-amber-200/90">
                                {plan.totals.deleteRight + plan.totals.deleteLeft} {t('aerosync.delete') || 'delete'}
                                <span className="mx-1">&middot;</span>
                                {plan.totals.overwriteRight + plan.totals.overwriteLeft} {t('aerosync.overwrite') || 'overwrite'}
                                <span className="mx-1">&middot;</span>
                                {plan.totals.conflicts} {t('aerosync.conflicts') || 'conflict'}
                            </p>
                            <label className="mt-2 inline-flex cursor-pointer items-center gap-2 text-[11px] font-medium">
                                <input
                                    type="checkbox"
                                    checked={confirmedDestructive}
                                    onChange={(event) => setConfirmedDestructive(event.target.checked)}
                                    className="h-3.5 w-3.5 rounded border-gray-300 text-amber-600 focus:ring-amber-500"
                                />
                                {t('aerosync.confirmDestructive') || 'I have reviewed the destructive actions and want to proceed.'}
                            </label>
                        </div>
                    </div>
                </div>
            )}

            {compareStale && (
                <div className="flex flex-wrap items-center gap-2 border-t border-amber-200 bg-amber-50 px-4 py-2 text-[11px] text-amber-800 dark:border-amber-800 dark:bg-amber-900/30 dark:text-amber-200">
                    <AlertTriangle size={12} className="shrink-0" />
                    <span className="flex-1">
                        {t('aerosync.rescanNeeded') || 'The comparison was made with other exclusions or another backup folder. Rescan before running.'}
                    </span>
                    <button
                        type="button"
                        onClick={() => onRescan?.({
                            userExcludes,
                            backupDir: validBackupDir ?? compareBackupDir ?? AEROSYNC_DEFAULT_BACKUP_DIR,
                        })}
                        disabled={!onRescan}
                        className="inline-flex items-center gap-1 rounded border border-amber-400 px-2 py-0.5 font-medium hover:bg-amber-100 disabled:opacity-40 dark:hover:bg-amber-900/50"
                    >
                        <RefreshCw size={11} />
                        {t('aerosync.rescan') || 'Rescan'}
                    </button>
                </div>
            )}

            <div className="border-t border-gray-200 px-4 py-2 dark:border-gray-700">
                <div className="mb-1 flex items-center gap-1 text-[10px] font-semibold uppercase tracking-wide text-gray-500 dark:text-gray-400">
                    <Terminal size={11} />
                    {t('aerosync.cli.title') || 'Same run from the command line'}
                </div>
                {cliLine.kind === 'command' ? (
                    <div className="flex items-start gap-2">
                        <code className="flex-1 break-all rounded bg-gray-100 px-2 py-1 font-mono text-[11px] text-gray-800 dark:bg-gray-900/60 dark:text-gray-100">
                            {cliLine.text}
                        </code>
                        <button
                            type="button"
                            onClick={() => copyCli(cliLine.text)}
                            className="inline-flex shrink-0 items-center gap-1 rounded border border-gray-300 px-2 py-1 text-[11px] text-gray-600 hover:bg-gray-50 dark:border-gray-600 dark:text-gray-300 dark:hover:bg-gray-700"
                        >
                            {copyResult?.text === cliLine.text && copyResult.ok
                                ? <CheckCircle2 size={11} className="text-emerald-500" />
                                : <Copy size={11} />}
                            {copyResult?.text !== cliLine.text
                                ? (t('aerosync.cli.copy') || 'Copy')
                                : copyResult.ok
                                    ? (t('aerosync.cli.copied') || 'Copied')
                                    : (t('aerosync.cli.copyFailed') || 'Copy failed')}
                        </button>
                    </div>
                ) : (
                    <p className="text-[11px] leading-snug text-gray-500 dark:text-gray-400">
                        {t('aerosync.cli.none', { reason: t(CLI_REASON_KEYS[cliLine.reason]) || cliLine.reason })
                            || `No exact CLI equivalent: ${cliLine.reason}`}
                    </p>
                )}
            </div>

            <div className="flex flex-col gap-2 border-t border-gray-200 px-4 py-3 dark:border-gray-700 sm:flex-row sm:items-center sm:justify-between">
                <p className="text-[11px] text-gray-500 dark:text-gray-400">
                    {canExecute
                        ? (t('aerosync.executeHint') || 'Execute stages the selection and dispatches via the unified transfer planner.')
                        : (t('aerosync.executeUnavailable') || 'Execution is not available for this pair kind.')}
                </p>
                <div className="flex flex-wrap justify-end gap-2">
                    <button
                        type="button"
                        onClick={() => onExecute(plan, {
                            speedMode,
                            // GAP-9a: Maniac runs with verification off during
                            // the transfer; a mandatory post-sync sweep runs
                            // afterwards (handled by the runner).
                            verifyPolicy: maniacArmed ? 'none' : verifyPolicy,
                            canary: canaryMode
                                ? { percent: canaryPercent, selection: canarySelection }
                                : undefined,
                            retryPolicy: retryPolicyForSpeed(speedMode),
                            transferBudget: transferBudgetMb > 0
                                ? transferBudgetMb * 1024 * 1024
                                : 0,
                            versionedBackup: versionedBackup.enabled && validBackupDir ? { dir: validBackupDir } : null,
                            // P3: pass EC control (typed in AeroSyncRuntime, reaches
                            // RemoteSyncConfig.errorCorrection; runner already handles).
                            errorCorrection: ecEnabled ? { enabled: true, pct: ecPct } : undefined,
                        })}
                        disabled={!canFireExecute}
                        className={`inline-flex items-center gap-2 rounded-lg px-3 py-2 text-sm text-white transition-colors disabled:opacity-40 ${
                            canaryMode
                                ? 'bg-violet-600 hover:bg-violet-700'
                                : plan.hasDestructive ? 'bg-amber-600 hover:bg-amber-700' : 'bg-blue-600 hover:bg-blue-700'
                        }`}
                        title={
                            !executable
                                ? (t('aerosync.executeNothing') || 'Nothing to do or execution path not available')
                                : backupBlock
                                    ? backupBlock
                                : compareStale
                                    ? (t('aerosync.rescanNeeded') || 'The comparison was made with other exclusions or another backup folder. Rescan before running.')
                                : !canaryMode && needsConfirm && !confirmedDestructive
                                    ? (t('aerosync.executeConfirm') || 'Confirm destructive actions to enable Execute')
                                    : (t('aerosync.executePreset') || 'Execute preset')
                        }
                    >
                        {canaryMode
                            ? <FlaskConical size={14} />
                            : plan.hasDestructive ? <AlertTriangle size={14} /> : <CheckCircle2 size={14} />}
                        {canaryMode
                            ? (t('syncPanel.canaryRun') || 'Canary Sync')
                            : `${t('aerosync.execute') || 'Execute'} ${resolveLabel(t, presetNameLabel(preset))} (${plan.totals.actionable})`}
                    </button>
                </div>
            </div>
        </div>
    );
};

export default PlanTabContent;
