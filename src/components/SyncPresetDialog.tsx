// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import {
    AlertTriangle,
    ArrowLeftRight,
    ArrowRight,
    CheckCircle2,
    FolderSync,
    ShieldAlert,
    ShieldCheck,
    Trash2,
    X,
} from 'lucide-react';
import type { CompareResult } from '../utils/compareEndpoints';
import {
    derivePresetPlan,
    describeAction,
    describePreset,
    type BucketAction,
    type PresetDirection,
    type PresetPlan,
    type SyncPreset,
} from '../utils/syncPresets';
import { formatBytes } from '../utils/formatters';

interface SyncPresetDialogProps {
    result: CompareResult;
    leftLabel: string;
    rightLabel: string;
    pairKind?: string;
    /** True when the parent has a working execute path for this pair kind. */
    canExecute: boolean;
    /**
     * Fires when the user confirms execution. Receives the resolved plan
     * (preset + direction already applied). Parent is responsible for the
     * actual transfer; the dialog will close itself on the user's intent
     * but execution result handling stays in the parent.
     */
    onExecute: (plan: PresetPlan) => void;
    onClose: () => void;
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

export const SyncPresetDialog: React.FC<SyncPresetDialogProps> = ({
    result,
    leftLabel,
    rightLabel,
    pairKind,
    canExecute,
    onExecute,
    onClose,
}) => {
    const [preset, setPreset] = React.useState<SyncPreset>('backup');
    const [direction, setDirection] = React.useState<PresetDirection>('left-to-right');
    const [confirmedDestructive, setConfirmedDestructive] = React.useState(false);

    // Reset confirmation when preset/direction changes so the user re-affirms
    // every destructive plan rather than carrying over a stale checkbox.
    React.useEffect(() => {
        setConfirmedDestructive(false);
    }, [preset, direction]);

    const plan = React.useMemo(
        () => derivePresetPlan(result, { preset, direction }),
        [result, preset, direction],
    );

    const bisyncMode = preset === 'bisync';

    const executable = canExecute && plan.totals.actionable > 0;
    const needsConfirm = plan.hasDestructive;
    const canFireExecute = executable && (!needsConfirm || confirmedDestructive);

    return (
        <div
            className="fixed inset-0 z-[9998] flex items-center justify-center bg-black/55 px-4 backdrop-blur-sm"
            role="dialog"
            aria-modal="true"
            aria-label="Sync presets"
            onClick={(event) => {
                if (event.target === event.currentTarget) onClose();
            }}
        >
            <div className="w-full max-w-3xl overflow-hidden rounded-lg bg-white shadow-2xl dark:bg-gray-800">
                <div className="flex items-center justify-between border-b border-gray-200 px-4 py-3 dark:border-gray-700">
                    <div className="flex items-center gap-2">
                        <FolderSync size={18} className="text-blue-500" />
                        <div>
                            <h2 className="text-sm font-semibold text-gray-900 dark:text-white">Sync presets</h2>
                            <p className="text-xs text-gray-500 dark:text-gray-400">
                                {leftLabel} <ArrowLeftRight size={11} className="inline align-middle" /> {rightLabel}
                                {pairKind ? ` · ${pairKind}` : ''}
                            </p>
                        </div>
                    </div>
                    <button
                        type="button"
                        onClick={onClose}
                        className="rounded p-1 text-gray-400 transition-colors hover:bg-gray-100 hover:text-gray-700 dark:hover:bg-gray-700 dark:hover:text-gray-200"
                        aria-label="Close"
                    >
                        <X size={16} />
                    </button>
                </div>

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
                                    Left → Right
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
                                    Right → Left
                                </button>
                            </div>
                        )}
                    </div>
                </div>

                <div className="border-t border-gray-200 px-4 py-3 dark:border-gray-700">
                    <div className="grid gap-3 sm:grid-cols-4">
                        <div className="rounded-md bg-gray-50 p-2 dark:bg-gray-900/40">
                            <div className="text-[10px] uppercase tracking-wide text-gray-500 dark:text-gray-400">Actionable</div>
                            <div className="text-base font-semibold text-gray-900 dark:text-white">{plan.totals.actionable}</div>
                        </div>
                        <div className="rounded-md bg-gray-50 p-2 dark:bg-gray-900/40">
                            <div className="text-[10px] uppercase tracking-wide text-gray-500 dark:text-gray-400">Skipped</div>
                            <div className="text-base font-semibold text-gray-900 dark:text-white">{plan.totals.skipped}</div>
                        </div>
                        <div className="rounded-md bg-gray-50 p-2 dark:bg-gray-900/40">
                            <div className="text-[10px] uppercase tracking-wide text-gray-500 dark:text-gray-400">Transfer</div>
                            <div className="text-base font-semibold text-gray-900 dark:text-white">{formatBytes(plan.totals.transferBytes)}</div>
                        </div>
                        <div className={`rounded-md p-2 ${plan.hasDestructive ? 'bg-amber-50 dark:bg-amber-900/30' : 'bg-gray-50 dark:bg-gray-900/40'}`}>
                            <div className="text-[10px] uppercase tracking-wide text-gray-500 dark:text-gray-400">Destructive</div>
                            <div className={`text-base font-semibold ${plan.hasDestructive ? 'text-amber-700 dark:text-amber-200' : 'text-gray-900 dark:text-white'}`}>
                                {plan.totals.deleteRight + plan.totals.deleteLeft + (plan.hasOverwritesNewer ? plan.totals.overwriteLeft + plan.totals.overwriteRight : 0)}
                            </div>
                        </div>
                    </div>
                </div>

                <div className="max-h-[40vh] space-y-1 overflow-y-auto border-t border-gray-200 px-4 py-3 dark:border-gray-700">
                    <table className="w-full text-[11px]">
                        <thead className="text-left text-gray-500 dark:text-gray-400">
                            <tr>
                                <th className="px-2 py-1 font-medium">Bucket</th>
                                <th className="px-2 py-1 font-medium text-right">Count</th>
                                <th className="px-2 py-1 font-medium">Action</th>
                                <th className="px-2 py-1 font-medium text-right">Transfer</th>
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
                                <p className="font-semibold">This preset will delete or overwrite newer files.</p>
                                <p className="text-[11px] text-amber-800 dark:text-amber-200/90">
                                    {plan.totals.deleteRight + plan.totals.deleteLeft} delete · {plan.totals.overwriteRight + plan.totals.overwriteLeft} overwrite · {plan.totals.conflicts} conflict
                                </p>
                                <label className="mt-2 inline-flex cursor-pointer items-center gap-2 text-[11px] font-medium">
                                    <input
                                        type="checkbox"
                                        checked={confirmedDestructive}
                                        onChange={(event) => setConfirmedDestructive(event.target.checked)}
                                        className="h-3.5 w-3.5 rounded border-gray-300 text-amber-600 focus:ring-amber-500"
                                    />
                                    I have reviewed the destructive actions and want to proceed.
                                </label>
                            </div>
                        </div>
                    </div>
                )}

                <div className="flex flex-col gap-2 border-t border-gray-200 bg-gray-50 px-4 py-3 dark:border-gray-700 dark:bg-gray-800/70 sm:flex-row sm:items-center sm:justify-between">
                    <p className="text-[11px] text-gray-500 dark:text-gray-400">
                        {canExecute
                            ? 'Execute will stage the matching selection and dispatch via the unified transfer planner.'
                            : 'Execution is currently limited to local-local pairs; other pair kinds land with Z.3.8.2.'}
                    </p>
                    <div className="flex flex-wrap justify-end gap-2">
                        <button
                            type="button"
                            onClick={onClose}
                            className="rounded-lg px-3 py-2 text-sm text-gray-600 transition-colors hover:bg-gray-100 dark:text-gray-300 dark:hover:bg-gray-700"
                        >
                            Close
                        </button>
                        <button
                            type="button"
                            onClick={() => onExecute(plan)}
                            disabled={!canFireExecute}
                            className={`inline-flex items-center gap-2 rounded-lg px-3 py-2 text-sm text-white transition-colors disabled:opacity-40 ${
                                plan.hasDestructive ? 'bg-amber-600 hover:bg-amber-700' : 'bg-blue-600 hover:bg-blue-700'
                            }`}
                            title={
                                !executable
                                    ? 'Nothing to do or execution path not available for this pair'
                                    : needsConfirm && !confirmedDestructive
                                        ? 'Confirm destructive actions to enable Execute'
                                        : 'Execute preset'
                            }
                        >
                            {plan.hasDestructive ? <AlertTriangle size={14} /> : <CheckCircle2 size={14} />}
                            Execute {describePreset(preset).name} ({plan.totals.actionable})
                        </button>
                    </div>
                </div>
            </div>
        </div>
    );
};

export default SyncPresetDialog;
