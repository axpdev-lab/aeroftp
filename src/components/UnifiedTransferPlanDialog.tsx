// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { AlertTriangle, ArrowRight, CheckCircle2, Copy, FolderSync, Loader2, X } from 'lucide-react';
import type { UnifiedTransferPlan } from '../utils/unifiedTransferPlanner';
import { getEndpointLabel } from '../utils/panelEndpoints';
import { formatBytes } from '../utils/formatters';
import { useDraggableModal } from '../hooks/useDraggableModal';

interface UnifiedTransferPlanDialogProps {
    plan: UnifiedTransferPlan;
    executing?: boolean;
    onExecute: () => void;
    onClose: () => void;
}

const ENGINE_LABEL: Record<UnifiedTransferPlan['engine'], string> = {
    'local-fs': 'Local filesystem',
    'local-delta': 'Local delta transport',
    'provider-upload': 'Provider upload',
    'provider-download': 'Provider download',
    'aerosync': 'AeroSync / AeroRsync',
    'cross-profile-transfer': 'Cross-Profile Transfer',
    'cross-profile-sync': 'Cross-Profile Sync',
    'aerovault-overlay': 'AeroVault overlay',
    'compare-only': 'Compare only',
    unsupported: 'Unsupported',
};

const MODE_LABEL: Record<UnifiedTransferPlan['mode'], string> = {
    copy: 'Copy',
    move: 'Move',
    sync: 'Sync',
    compare: 'Compare',
    mirror: 'Mirror',
    backup: 'Backup',
    bisync: 'Bisync',
};

export const UnifiedTransferPlanDialog: React.FC<UnifiedTransferPlanDialogProps> = ({
    plan,
    executing = false,
    onExecute,
    onClose,
}) => {
    const modalDrag = useDraggableModal();
    const sourceLabel = getEndpointLabel(plan.source);
    const destinationLabel = getEndpointLabel(plan.destination);
    const destructive = plan.destructive;

    return (
        <div
            className="fixed inset-0 z-[9998] flex items-center justify-center bg-black/55 px-4 backdrop-blur-sm"
            role="dialog"
            aria-modal="true"
            aria-label="Transfer plan"
            onClick={(event) => {
                if (event.target === event.currentTarget && !executing) onClose();
            }}
        >
            <div {...modalDrag.panelProps} className="w-full max-w-lg overflow-hidden rounded-lg bg-white shadow-2xl dark:bg-gray-800">
                <div {...modalDrag.dragHandleProps} className="flex items-center justify-between border-b border-gray-200 px-4 py-3 dark:border-gray-700 cursor-grab active:cursor-grabbing">
                    <div className="flex items-center gap-2">
                        {plan.mode === 'sync' || plan.mode === 'backup' || plan.mode === 'mirror' || plan.mode === 'bisync' ? (
                            <FolderSync size={18} className="text-blue-500" />
                        ) : (
                            <Copy size={18} className="text-blue-500" />
                        )}
                        <div>
                            <h2 className="text-sm font-semibold text-gray-900 dark:text-white">
                                {MODE_LABEL[plan.mode]} plan
                            </h2>
                            <p className="text-xs text-gray-500 dark:text-gray-400">
                                {plan.pairKind} via {ENGINE_LABEL[plan.engine]}
                            </p>
                        </div>
                    </div>
                    <button
                        type="button"
                        onClick={onClose}
                        disabled={executing}
                        className="rounded p-1 text-gray-400 transition-colors hover:bg-gray-100 hover:text-gray-700 disabled:opacity-50 dark:hover:bg-gray-700 dark:hover:text-gray-200"
                    >
                        <X size={16} />
                    </button>
                </div>

                <div className="space-y-4 p-4">
                    <div className="grid grid-cols-[1fr_auto_1fr] items-center gap-3">
                        <div className="min-w-0 rounded-md border border-gray-200 bg-gray-50 p-3 dark:border-gray-700 dark:bg-gray-900/40">
                            <div className="text-[11px] font-semibold uppercase tracking-wide text-gray-500 dark:text-gray-400">Source</div>
                            <div className="mt-1 truncate text-sm font-medium text-gray-900 dark:text-white" title={sourceLabel}>{sourceLabel}</div>
                            <div className="truncate text-xs text-gray-500 dark:text-gray-400" title={plan.source.path}>{plan.source.path}</div>
                        </div>
                        <ArrowRight size={16} className="text-gray-400" />
                        <div className="min-w-0 rounded-md border border-gray-200 bg-gray-50 p-3 dark:border-gray-700 dark:bg-gray-900/40">
                            <div className="text-[11px] font-semibold uppercase tracking-wide text-gray-500 dark:text-gray-400">Destination</div>
                            <div className="mt-1 truncate text-sm font-medium text-gray-900 dark:text-white" title={destinationLabel}>{destinationLabel}</div>
                            <div className="truncate text-xs text-gray-500 dark:text-gray-400" title={plan.destination.path}>{plan.destination.path}</div>
                        </div>
                    </div>

                    <div className="grid grid-cols-3 gap-2 text-sm">
                        <div className="rounded-md bg-gray-50 p-3 dark:bg-gray-900/40">
                            <div className="text-xs text-gray-500 dark:text-gray-400">Items</div>
                            <div className="font-semibold text-gray-900 dark:text-white">{plan.entryCount}</div>
                        </div>
                        <div className="rounded-md bg-gray-50 p-3 dark:bg-gray-900/40">
                            <div className="text-xs text-gray-500 dark:text-gray-400">Size</div>
                            <div className="font-semibold text-gray-900 dark:text-white">{formatBytes(plan.totalBytes)}</div>
                        </div>
                        <div className="rounded-md bg-gray-50 p-3 dark:bg-gray-900/40">
                            <div className="text-xs text-gray-500 dark:text-gray-400">Parallel</div>
                            <div className="font-semibold text-gray-900 dark:text-white">{plan.maxParallel}</div>
                        </div>
                    </div>

                    {plan.warnings.length > 0 && (
                        <div className={`rounded-md border p-3 text-sm ${
                            destructive
                                ? 'border-amber-200 bg-amber-50 text-amber-800 dark:border-amber-800 dark:bg-amber-900/30 dark:text-amber-200'
                                : 'border-blue-200 bg-blue-50 text-blue-800 dark:border-blue-800 dark:bg-blue-900/30 dark:text-blue-200'
                        }`}>
                            <div className="mb-1 flex items-center gap-2 font-medium">
                                <AlertTriangle size={15} />
                                Review before execution
                            </div>
                            <ul className="list-disc space-y-1 pl-5 text-xs">
                                {plan.warnings.map(warning => <li key={warning}>{warning}</li>)}
                            </ul>
                        </div>
                    )}

                    {!plan.canExecute && (
                        <div className="rounded-md border border-red-200 bg-red-50 p-3 text-sm text-red-700 dark:border-red-800 dark:bg-red-900/30 dark:text-red-200">
                            This endpoint pair is not executable yet.
                        </div>
                    )}
                </div>

                <div className="flex justify-end gap-2 border-t border-gray-200 bg-gray-50 px-4 py-3 dark:border-gray-700 dark:bg-gray-800/70">
                    <button
                        type="button"
                        onClick={onClose}
                        disabled={executing}
                        className="rounded-lg px-4 py-2 text-sm text-gray-600 transition-colors hover:bg-gray-100 disabled:opacity-50 dark:text-gray-300 dark:hover:bg-gray-700"
                    >
                        Cancel
                    </button>
                    <button
                        type="button"
                        onClick={onExecute}
                        disabled={!plan.canExecute || executing}
                        className={`inline-flex items-center gap-2 rounded-lg px-4 py-2 text-sm text-white transition-colors disabled:opacity-50 ${
                            destructive ? 'bg-amber-600 hover:bg-amber-700' : 'bg-blue-600 hover:bg-blue-700'
                        }`}
                    >
                        {executing ? <Loader2 size={15} className="animate-spin" /> : <CheckCircle2 size={15} />}
                        Execute
                    </button>
                </div>
            </div>
        </div>
    );
};

export default UnifiedTransferPlanDialog;
