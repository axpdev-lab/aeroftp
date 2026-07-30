// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
//
// AeroSync: unified modal that bundles Compare, Plan, and Sync into a
// single 3-tab workflow. Replaces the legacy LocalSyncPanel,
// UnifiedCompareDialog, and SyncPresetDialog dialogs which were
// disconnected entries in the View menu.

import * as React from 'react';
import { invoke } from '@tauri-apps/api/core';
import { CalendarClock, FileDown, FolderSync, History, Layers, RotateCcw, Undo2, X } from 'lucide-react';
import { useTranslation } from '../../i18n';
import { useDraggableModal } from '../../hooks/useDraggableModal';
import { useGuardedClose } from '../../hooks/useGuardedClose';
import { GuardedCloseConfirm } from '../GuardedCloseConfirm';
import { CompareTabContent } from './CompareTabContent';
import { PlanTabContent } from './PlanTabContent';
import { SyncTabContent } from './SyncTabContent';
import { JournalHistoryDialog } from './JournalHistoryDialog';
import { SyncSchedulerDialog } from './SyncSchedulerDialog';
import { SyncTemplateDialog } from '../Sync/SyncTemplateDialog';
import { MultiPathEditor } from '../Sync/MultiPathEditor';
import { RollbackDialog } from '../Sync/RollbackDialog';
import { useTransferCapabilities } from '../Sync/useTransferCapabilities';
import type { AeroSyncDialogProps, AeroSyncTab } from './types';

const TAB_ORDER: AeroSyncTab[] = ['compare', 'plan', 'sync'];

export const AeroSyncDialog: React.FC<AeroSyncDialogProps> = ({
    isOpen,
    onClose,
    initialTab = 'compare',
    context,
    onApplyMirrorLeftToRight,
    onApplyMirrorRightToLeft,
    onExecutePreset,
    onResumeJournal,
    onDismissJournal,
}) => {
    const t = useTranslation();
    const modalDrag = useDraggableModal();
    const [activeTab, setActiveTab] = React.useState<AeroSyncTab>(initialTab);
    const [showTemplates, setShowTemplates] = React.useState(false);
    const [showMultiPath, setShowMultiPath] = React.useState(false);
    const [showRollback, setShowRollback] = React.useState(false);
    const [showHistory, setShowHistory] = React.useState(false);
    const [showScheduler, setShowScheduler] = React.useState(false);
    // Lifted from the Sync tab so the close path can guard against losing an
    // in-progress local sync (issue #332).
    const [syncRunning, setSyncRunning] = React.useState(false);

    const cancelSync = React.useCallback(() => {
        invoke('local_sync_cancel').catch(() => { /* no run in flight */ });
    }, []);

    const guarded = useGuardedClose({
        guard: syncRunning ? 'busy' : null,
        onClose,
        onAbort: cancelSync,
    });

    React.useEffect(() => {
        if (isOpen) setActiveTab(initialTab);
    }, [isOpen, initialTab]);

    // GAP-9b: resolve the provider's real transfer capability so the Plan
    // tab clamps the parallel-streams selector honestly. The hook is mounted
    // for the modal's whole lifetime (the Plan tab itself unmounts on tab
    // switch), so the lookup runs once per open instead of per tab visit.
    const { caps: transferCaps } = useTransferCapabilities(context.protocol, isOpen);
    const streamCap = React.useMemo(() => {
        if (!transferCaps) return 8;
        return Math.max(
            1,
            transferCaps.max_file_slots ?? 1,
            transferCaps.max_chunk_slots ?? 1,
        );
    }, [transferCaps]);

    if (!isOpen) return null;

    const canMirrorAny = context.pairKind === 'local-local'
        || context.pairKind === 'local-remote'
        || context.pairKind === 'remote-local';
    const canExecutePlan = canMirrorAny;

    // Header launchers (Templates / Multi-Path / Rollback) require a
    // connected remote session; they piggy-back on the existing Sync/*
    // dialogs which were originally wired to SyncPanel.
    const isConnectedRemote = (context.pairKind === 'local-remote' || context.pairKind === 'remote-local')
        && !!context.activeProfileId;
    const localPath = context.initialSource || '';
    const remotePath = context.initialDestination || '';
    const serverProfileName = context.activeProfileName || '';
    const excludePatterns = context.excludePatterns || [];
    const isProvider = !!context.isProvider;

    const tabLabels: Record<AeroSyncTab, string> = {
        compare: t('aerosync.tabCompare') || 'Compare',
        plan: t('aerosync.tabPlan') || 'Plan',
        sync: t('aerosync.tabSync') || 'Sync',
    };

    return (
        <div
            className="fixed inset-0 z-[9998] flex items-start justify-center pt-[5vh] bg-black/50 animate-scale-in"
            role="dialog"
            aria-modal="true"
            aria-label={t('aerosync.title') || 'AeroSync'}
            onClick={(e) => {
                if (e.target === e.currentTarget) guarded.requestBackdropClose();
            }}
        >
            <div
                {...modalDrag.panelProps}
                className="relative bg-white dark:bg-gray-800 rounded-lg shadow-2xl w-full max-w-4xl max-h-[90vh] overflow-hidden flex flex-col"
                onClick={(e) => e.stopPropagation()}
            >
                <div
                    {...modalDrag.dragHandleProps}
                    className="flex items-center justify-between p-4 border-b border-gray-200 dark:border-gray-700 cursor-grab active:cursor-grabbing"
                >
                    <div className="flex items-center gap-2 pointer-events-none">
                        <FolderSync size={20} className="text-blue-500" />
                        <h2 className="text-lg font-semibold">
                            {t('aerosync.title') || 'AeroSync'}
                        </h2>
                    </div>
                    <div className="flex items-center gap-1">
                        {isConnectedRemote && (
                            <>
                                {([
                                    { Icon: FileDown, labelKey: 'aerosync.launcherTemplates', fallback: 'Templates', onClick: () => setShowTemplates(true) },
                                    { Icon: Layers, labelKey: 'aerosync.launcherMultiPath', fallback: 'Multi-Path', onClick: () => setShowMultiPath(true) },
                                    { Icon: Undo2, labelKey: 'aerosync.launcherRollback', fallback: 'Rollback snapshots', onClick: () => setShowRollback(true) },
                                    { Icon: History, labelKey: 'aerosync.launcherHistory', fallback: 'Journal history', onClick: () => setShowHistory(true) },
                                    { Icon: CalendarClock, labelKey: 'aerosync.launcherScheduler', fallback: 'Scheduler', onClick: () => setShowScheduler(true) },
                                ] as const).map(({ Icon, labelKey, fallback, onClick }) => {
                                    const label = t(labelKey) || fallback;
                                    return (
                                        <button
                                            key={labelKey}
                                            type="button"
                                            onClick={onClick}
                                            className="p-2 text-gray-500 hover:text-blue-600 dark:text-gray-400 dark:hover:text-blue-300 hover:bg-gray-100 dark:hover:bg-gray-700 rounded-lg transition-colors"
                                            title={label}
                                            aria-label={label}
                                        >
                                            <Icon size={16} />
                                        </button>
                                    );
                                })}
                                <div className="mx-1 h-6 w-px bg-gray-300 dark:bg-gray-600" />
                            </>
                        )}
                        <button
                            type="button"
                            onClick={guarded.requestClose}
                            aria-label={t('common.close') || 'Close'}
                            className="p-2 hover:bg-gray-100 dark:hover:bg-gray-700 rounded-lg transition-colors"
                        >
                            <X size={18} />
                        </button>
                    </div>
                </div>

                <div className="flex gap-1 px-4 pt-3 border-b border-gray-200 dark:border-gray-700">
                    {TAB_ORDER.map((tab) => {
                        const active = activeTab === tab;
                        // While a local sync runs, lock the user onto the Sync
                        // tab: leaving it would unmount SyncTabContent and orphan
                        // the in-flight run (issue #332).
                        const locked = syncRunning && tab !== 'sync';
                        return (
                            <button
                                key={tab}
                                type="button"
                                onClick={() => { if (!locked) setActiveTab(tab); }}
                                disabled={locked}
                                className={`px-4 py-2 text-sm font-medium rounded-t-lg transition-colors border-b-2 ${
                                    active
                                        ? 'border-blue-500 text-blue-600 dark:text-blue-300'
                                        : 'border-transparent text-gray-500 hover:text-gray-700 hover:bg-gray-50 dark:text-gray-400 dark:hover:text-gray-200 dark:hover:bg-gray-700/50'
                                } ${locked ? 'opacity-40 cursor-not-allowed hover:bg-transparent dark:hover:bg-transparent' : ''}`}
                                aria-selected={active}
                                role="tab"
                            >
                                {tabLabels[tab]}
                            </button>
                        );
                    })}
                </div>

                {/* GAP-6: interrupted-sync resume banner. Shown when an
                    incomplete journal exists for the connected-remote pair. */}
                {context.pendingJournal && (
                    <div className="mx-4 my-2 rounded-lg border border-amber-300 bg-amber-50 p-3 dark:border-amber-700 dark:bg-amber-900/20">
                        <div className="mb-1 flex items-center gap-2">
                            <RotateCcw size={16} className="text-amber-500" />
                            <span className="text-sm font-semibold text-gray-800 dark:text-gray-100">
                                {t('syncPanel.journalFound') || 'Interrupted sync detected'}
                            </span>
                        </div>
                        <p className="mb-2 text-xs text-gray-600 dark:text-gray-400">
                            {t('syncPanel.journalDescription', {
                                completed: String(
                                    context.pendingJournal.entries.filter((e) => e.status === 'completed').length,
                                ),
                                total: String(context.pendingJournal.entries.length),
                                date: new Date(context.pendingJournal.updated_at).toLocaleString(),
                            })}
                        </p>
                        <div className="flex gap-2">
                            <button
                                type="button"
                                onClick={() => {
                                    if (context.pendingJournal) onResumeJournal(context.pendingJournal);
                                }}
                                className="inline-flex items-center gap-1 rounded bg-amber-500 px-3 py-1 text-xs font-medium text-white hover:bg-amber-600"
                            >
                                <RotateCcw size={12} />
                                {t('syncPanel.resumeSync') || 'Resume'}
                            </button>
                            <button
                                type="button"
                                onClick={onDismissJournal}
                                className="rounded bg-gray-200 px-3 py-1 text-xs font-medium hover:bg-gray-300 dark:bg-gray-700 dark:hover:bg-gray-600"
                            >
                                {t('syncPanel.dismissJournal') || 'Dismiss'}
                            </button>
                        </div>
                    </div>
                )}

                <div className="flex-1 overflow-y-auto">
                    {activeTab === 'compare' && (
                        <CompareTabContent
                            result={context.compareResult}
                            loading={context.compareLoading}
                            leftLabel={context.leftLabel}
                            rightLabel={context.rightLabel}
                            pairKind={context.pairKind}
                            canMirrorLeftToRight={canMirrorAny}
                            canMirrorRightToLeft={canMirrorAny}
                            onApplyMirrorLeftToRight={onApplyMirrorLeftToRight}
                            onApplyMirrorRightToLeft={onApplyMirrorRightToLeft}
                        />
                    )}
                    {activeTab === 'plan' && (
                        <PlanTabContent
                            result={context.compareResult}
                            loading={context.compareLoading}
                            pairKind={context.pairKind}
                            canExecute={canExecutePlan}
                            streamCap={streamCap}
                            onExecute={onExecutePreset}
                        />
                    )}
                    {activeTab === 'sync' && (
                        <SyncTabContent
                            initialSource={context.initialSource}
                            initialDestination={context.initialDestination}
                            pairKind={context.pairKind}
                            activeProfileId={context.activeProfileId}
                            onRunningChange={setSyncRunning}
                        />
                    )}
                </div>
            </div>

            {/* Re-mounted Sync/* dialogs, surfaced from the AeroSync
                header. They share the same path context and stay
                self-contained: state lives inside each dialog. */}
            {isConnectedRemote && (
                <>
                    <SyncTemplateDialog
                        isOpen={showTemplates}
                        onClose={() => setShowTemplates(false)}
                        localPath={localPath}
                        remotePath={remotePath}
                        serverProfileName={serverProfileName}
                        excludePatterns={excludePatterns}
                    />
                    <MultiPathEditor
                        isOpen={showMultiPath}
                        onClose={() => setShowMultiPath(false)}
                        localPath={localPath}
                        remotePath={remotePath}
                    />
                    <RollbackDialog
                        isOpen={showRollback}
                        onClose={() => setShowRollback(false)}
                        localPath={localPath}
                        remotePath={remotePath}
                        isProvider={isProvider}
                    />
                    <JournalHistoryDialog
                        isOpen={showHistory}
                        onClose={() => setShowHistory(false)}
                        localPath={localPath}
                        remotePath={remotePath}
                    />
                    <SyncSchedulerDialog
                        isOpen={showScheduler}
                        onClose={() => setShowScheduler(false)}
                    />
                </>
            )}

            {guarded.confirmOpen && guarded.confirmKind && (
                <GuardedCloseConfirm
                    kind={guarded.confirmKind}
                    onKeep={guarded.cancelConfirm}
                    onConfirm={guarded.confirmAndClose}
                />
            )}
        </div>
    );
};

export default AeroSyncDialog;
