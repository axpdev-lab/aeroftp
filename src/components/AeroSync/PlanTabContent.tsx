// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import {
    AlertTriangle,
    ArrowRight,
    CheckCircle2,
    FlaskConical,
    Gauge,
    Loader2,
    ShieldAlert,
    ShieldCheck,
    Trash2,
} from 'lucide-react';
import type { CompareResult } from '../../utils/compareEndpoints';
import {
    CONFLICT_POLICIES,
    derivePresetPlan,
    describeAction,
    describeConflictPolicy,
    describePreset,
    type BucketAction,
    type ConflictPolicy,
    type PresetDirection,
    type PresetPlan,
    type SyncPreset,
    type VersionedBackupConfig,
} from '../../utils/syncPresets';
import { formatBytes } from '../../utils/formatters';
import { retryPolicyForSpeed } from '../../utils/remoteSyncRunner';
import { useTranslation } from '../../i18n';
import type {
    AeroSyncCanarySelection,
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
}

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
                    <span className="text-sm font-semibold">{info.name}</span>
                </div>
                {isDefault && (
                    <span className="rounded bg-emerald-100 px-1.5 py-0.5 text-[10px] font-semibold text-emerald-700 dark:bg-emerald-900/40 dark:text-emerald-300">
                        DEFAULT
                    </span>
                )}
            </div>
            <p className="text-[11px] leading-snug text-gray-500 dark:text-gray-400">{info.tagline}</p>
        </button>
    );
};

const SPEED_MODES: AeroSyncSpeedMode[] = ['normal', 'fast', 'turbo', 'extreme'];
const VERIFY_POLICIES: AeroSyncVerifyPolicy[] = ['none', 'size_only', 'size_and_mtime', 'full_checksum'];
const CANARY_PERCENTS = [5, 10, 25, 50];
const CANARY_SELECTIONS: AeroSyncCanarySelection[] = ['random', 'newest', 'largest'];

export const PlanTabContent: React.FC<PlanTabContentProps> = ({
    result,
    loading,
    pairKind,
    canExecute,
    onExecute,
}) => {
    const t = useTranslation();
    const [preset, setPreset] = React.useState<SyncPreset>('backup');
    const [direction, setDirection] = React.useState<PresetDirection>('left-to-right');
    const [conflictPolicy, setConflictPolicy] = React.useState<ConflictPolicy>('skip');
    const [versionedBackup, setVersionedBackup] = React.useState<VersionedBackupConfig>({
        enabled: false,
        backupDir: '.aeroftp-versions',
    });
    const [confirmedDestructive, setConfirmedDestructive] = React.useState(false);
    // CO-1: Speed mode + verify policy are now lifted into the
    // `onExecute(plan, runtime)` callback so App.tsx can forward them
    // to the unified runner (LocalSyncRequest.speed_mode /
    // verify_policy) and the Rust backend can honour the verify pass.
    const [speedMode, setSpeedMode] = React.useState<AeroSyncSpeedMode>('normal');
    const [verifyPolicy, setVerifyPolicy] = React.useState<AeroSyncVerifyPolicy>('size_only');
    // GAP-7: Canary trial — only meaningful for a connected remote.
    const [canaryMode, setCanaryMode] = React.useState(false);
    const [canaryPercent, setCanaryPercent] = React.useState(10);
    const [canarySelection, setCanarySelection] = React.useState<AeroSyncCanarySelection>('random');
    // GAP-8: transfer budget in MB (0 = unlimited); retry policy is derived
    // from the speed mode, versioned-backup reuses the existing toggle.
    const [transferBudgetMb, setTransferBudgetMb] = React.useState(0);
    const isConnectedRemote = pairKind === 'local-remote' || pairKind === 'remote-local';

    React.useEffect(() => {
        setConfirmedDestructive(false);
    }, [preset, direction, conflictPolicy, versionedBackup.enabled, versionedBackup.backupDir]);

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

    const executable = canExecute && plan.totals.actionable > 0;
    const needsConfirm = plan.hasDestructive;
    // A canary run is a non-destructive trial, so it does not gate on the
    // destructive-confirm checkbox.
    const canFireExecute = executable && (canaryMode || !needsConfirm || confirmedDestructive);

    return (
        <div className="flex flex-col">
            <div className="px-4 py-3">
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
                        {describePreset(preset).tagline}
                    </div>
                    {!bisyncMode && (
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
                                {t('aerosync.directionLeftRight') || 'Left to Right'}
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
                                {t('aerosync.directionRightLeft') || 'Right to Left'}
                            </button>
                        </div>
                    )}
                </div>
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
                                const info = describeConflictPolicy(policy);
                                return (
                                    <option key={policy} value={policy}>
                                        {info.label}
                                    </option>
                                );
                            })}
                        </select>
                        <p className="mt-1 text-[10px] leading-snug text-gray-500 dark:text-gray-400">
                            {describeConflictPolicy(conflictPolicy).tagline}
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
                            {t('aerosync.versionedBackupHint') || 'Capture the destination copy under this directory before each overwrite or delete.'}
                        </p>
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
                            {SPEED_MODES.map((mode) => (
                                <button
                                    key={mode}
                                    type="button"
                                    onClick={() => setSpeedMode(mode)}
                                    className={`flex-1 px-2 py-1 text-[11px] font-medium capitalize ${
                                        speedMode === mode
                                            ? 'bg-blue-500 text-white'
                                            : 'bg-white text-gray-600 hover:bg-gray-50 dark:bg-gray-900/40 dark:text-gray-300 dark:hover:bg-gray-700'
                                    }`}
                                >
                                    {t(`aerosync.speedMode_${mode}`) || mode}
                                </button>
                            ))}
                        </div>
                        <p className="mt-1 text-[10px] leading-snug text-gray-500 dark:text-gray-400">
                            {t('aerosync.speedModeHint') || 'Parallel streams + compression preset applied to the run.'}
                        </p>
                    </div>
                    <div>
                        <label className="mb-1 block text-[11px] font-semibold uppercase tracking-wide text-gray-500 dark:text-gray-400">
                            <ShieldCheck size={11} className="mr-1 inline align-text-bottom" />
                            {t('aerosync.verifyPolicy') || 'Verify policy'}
                        </label>
                        <select
                            value={verifyPolicy}
                            onChange={(event) => setVerifyPolicy(event.target.value as AeroSyncVerifyPolicy)}
                            className="w-full rounded-md border border-gray-300 bg-white px-2 py-1.5 text-sm text-gray-800 focus:border-blue-400 focus:outline-none dark:border-gray-600 dark:bg-gray-900/60 dark:text-gray-100"
                        >
                            {VERIFY_POLICIES.map((policy) => (
                                <option key={policy} value={policy}>
                                    {t(`aerosync.verifyPolicy_${policy}`) || policy}
                                </option>
                            ))}
                        </select>
                        <p className="mt-1 text-[10px] leading-snug text-gray-500 dark:text-gray-400">
                            {t('aerosync.verifyPolicyHint') || 'Post-transfer integrity check applied to each file.'}
                        </p>
                    </div>
                </div>

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
                                <td className="px-2 py-1 font-medium text-gray-800 dark:text-gray-200">{bp.bucket}</td>
                                <td className="px-2 py-1 text-right text-gray-700 dark:text-gray-200">{bp.entries.length}</td>
                                <td className={`px-2 py-1 ${ACTION_COLOR[bp.action]}`}>
                                    <span className="inline-flex items-center gap-1">
                                        {ACTION_ICON[bp.action]}
                                        {describeAction(bp.action)}
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
                            verifyPolicy,
                            canary: canaryMode
                                ? { percent: canaryPercent, selection: canarySelection }
                                : undefined,
                            retryPolicy: retryPolicyForSpeed(speedMode),
                            transferBudget: transferBudgetMb > 0
                                ? transferBudgetMb * 1024 * 1024
                                : 0,
                            versioningStrategy: versionedBackup.enabled ? 'trash_can' : null,
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
                            : `${t('aerosync.execute') || 'Execute'} ${describePreset(preset).name} (${plan.totals.actionable})`}
                    </button>
                </div>
            </div>
        </div>
    );
};

export default PlanTabContent;
