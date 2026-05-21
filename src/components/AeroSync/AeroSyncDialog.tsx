// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
//
// AeroSync: unified modal that bundles Compare, Plan, and Sync into a
// single 3-tab workflow. Replaces the legacy LocalSyncPanel,
// UnifiedCompareDialog, and SyncPresetDialog dialogs which were
// disconnected entries in the View menu.

import * as React from 'react';
import { CalendarClock, FileDown, FolderSync, History, Layers, Undo2, X } from 'lucide-react';
import { useTranslation } from '../../i18n';
import { useDraggableModal } from '../../hooks/useDraggableModal';
import { CompareTabContent } from './CompareTabContent';
import { PlanTabContent } from './PlanTabContent';
import { SyncTabContent } from './SyncTabContent';
import { JournalHistoryDialog } from './JournalHistoryDialog';
import { SyncSchedulerDialog } from './SyncSchedulerDialog';
import { SyncTemplateDialog } from '../Sync/SyncTemplateDialog';
import { MultiPathEditor } from '../Sync/MultiPathEditor';
import { RollbackDialog } from '../Sync/RollbackDialog';
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
}) => {
    const t = useTranslation();
    const modalDrag = useDraggableModal();
    const [activeTab, setActiveTab] = React.useState<AeroSyncTab>(initialTab);
    const [showTemplates, setShowTemplates] = React.useState(false);
    const [showMultiPath, setShowMultiPath] = React.useState(false);
    const [showRollback, setShowRollback] = React.useState(false);
    const [showHistory, setShowHistory] = React.useState(false);
    const [showScheduler, setShowScheduler] = React.useState(false);

    React.useEffect(() => {
        if (isOpen) setActiveTab(initialTab);
    }, [isOpen, initialTab]);

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
    const profileId = context.activeProfileId || '';
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
            onClick={onClose}
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
                                <button
                                    type="button"
                                    onClick={() => setShowTemplates(true)}
                                    className="p-2 text-gray-500 hover:text-blue-600 dark:text-gray-400 dark:hover:text-blue-300 hover:bg-gray-100 dark:hover:bg-gray-700 rounded-lg transition-colors"
                                    title={t('aerosync.launcherTemplates') || 'Templates'}
                                    aria-label={t('aerosync.launcherTemplates') || 'Templates'}
                                >
                                    <FileDown size={16} />
                                </button>
                                <button
                                    type="button"
                                    onClick={() => setShowMultiPath(true)}
                                    className="p-2 text-gray-500 hover:text-blue-600 dark:text-gray-400 dark:hover:text-blue-300 hover:bg-gray-100 dark:hover:bg-gray-700 rounded-lg transition-colors"
                                    title={t('aerosync.launcherMultiPath') || 'Multi-Path'}
                                    aria-label={t('aerosync.launcherMultiPath') || 'Multi-Path'}
                                >
                                    <Layers size={16} />
                                </button>
                                <button
                                    type="button"
                                    onClick={() => setShowRollback(true)}
                                    className="p-2 text-gray-500 hover:text-blue-600 dark:text-gray-400 dark:hover:text-blue-300 hover:bg-gray-100 dark:hover:bg-gray-700 rounded-lg transition-colors"
                                    title={t('aerosync.launcherRollback') || 'Rollback snapshots'}
                                    aria-label={t('aerosync.launcherRollback') || 'Rollback snapshots'}
                                >
                                    <Undo2 size={16} />
                                </button>
                                <button
                                    type="button"
                                    onClick={() => setShowHistory(true)}
                                    className="p-2 text-gray-500 hover:text-blue-600 dark:text-gray-400 dark:hover:text-blue-300 hover:bg-gray-100 dark:hover:bg-gray-700 rounded-lg transition-colors"
                                    title={t('aerosync.launcherHistory') || 'Journal history'}
                                    aria-label={t('aerosync.launcherHistory') || 'Journal history'}
                                >
                                    <History size={16} />
                                </button>
                                <button
                                    type="button"
                                    onClick={() => setShowScheduler(true)}
                                    className="p-2 text-gray-500 hover:text-blue-600 dark:text-gray-400 dark:hover:text-blue-300 hover:bg-gray-100 dark:hover:bg-gray-700 rounded-lg transition-colors"
                                    title={t('aerosync.launcherScheduler') || 'Scheduler'}
                                    aria-label={t('aerosync.launcherScheduler') || 'Scheduler'}
                                >
                                    <CalendarClock size={16} />
                                </button>
                                <div className="mx-1 h-6 w-px bg-gray-300 dark:bg-gray-600" />
                            </>
                        )}
                        <button
                            type="button"
                            onClick={onClose}
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
                        return (
                            <button
                                key={tab}
                                type="button"
                                onClick={() => setActiveTab(tab)}
                                className={`px-4 py-2 text-sm font-medium rounded-t-lg transition-colors border-b-2 ${
                                    active
                                        ? 'border-blue-500 text-blue-600 dark:text-blue-300'
                                        : 'border-transparent text-gray-500 hover:text-gray-700 hover:bg-gray-50 dark:text-gray-400 dark:hover:text-gray-200 dark:hover:bg-gray-700/50'
                                }`}
                                aria-selected={active}
                                role="tab"
                            >
                                {tabLabels[tab]}
                            </button>
                        );
                    })}
                </div>

                <div className="flex-1 overflow-y-auto">
                    {activeTab === 'compare' && (
                        <CompareTabContent
                            result={context.compareResult}
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
                            pairKind={context.pairKind}
                            canExecute={canExecutePlan}
                            onExecute={onExecutePreset}
                        />
                    )}
                    {activeTab === 'sync' && (
                        <SyncTabContent
                            initialSource={context.initialSource}
                            initialDestination={context.initialDestination}
                            pairKind={context.pairKind}
                            activeProfileId={context.activeProfileId}
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
                        profileId={profileId}
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
        </div>
    );
};

export default AeroSyncDialog;
