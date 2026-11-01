// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
//
// AeroSync: unified modal that bundles Compare, Plan, and Sync into a
// single tabbed workflow. Replaces the legacy LocalSyncPanel,
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
import { ScanProgressProvider } from './CompareScanState';
import { PlanTabContent } from './PlanTabContent';
import { SyncTabContent } from './SyncTabContent';
import { JournalHistoryDialog } from './JournalHistoryDialog';
import { SyncScheduler } from '../SyncScheduler';
import { SyncTemplateDialog } from '../Sync/SyncTemplateDialog';
import { MultiPathEditor } from '../Sync/MultiPathEditor';
import { RollbackDialog } from '../Sync/RollbackDialog';
import { TabStateStoreContext, createTabStateStore } from './tabStateStore';
import { buildAeroSyncTabStatePatch, parseSyncExcludePatterns, type ImportedSyncSettings } from '../../utils/syncTemplateApply';
import { AEROSYNC_DEFAULT_BACKUP_DIR, narrowsComparison, sameExcludePatterns } from '../../utils/aeroSyncExcludes';
import type { VersionedBackupConfig } from '../../utils/syncPresets';
import { AERO_SYNC_TOOL_TABS, aeroSyncTabsFor, effectiveAeroSyncTab, type AeroSyncDialogProps, type AeroSyncTab, type AeroSyncToolTab } from './types';
import type { TransferOptimizationHints } from '../../types';
import { limitsFromHints, type ProviderFileLimits } from '../../utils/providerFileLimits';

export const AeroSyncDialog: React.FC<AeroSyncDialogProps> = ({
    isOpen,
    onClose,
    onBusyChange,
    initialTab = 'compare',
    context,
    onApplyMirrorLeftToRight,
    onApplyMirrorRightToLeft,
    onExecutePreset,
    onResumeJournal,
    onDismissJournal,
    onInvalidateComparison,
    onRescan,
    onNarrowComparison,
    onStartCompare,
    onStopCompare,
}) => {
    const t = useTranslation();
    const modalDrag = useDraggableModal();
    const isConnectedRemote = (context.pairKind === 'local-remote' || context.pairKind === 'remote-local')
        && !!context.activeProfileId;
    const visibleTabs = aeroSyncTabsFor(context.pairKind, isConnectedRemote);
    const [selectedTab, setActiveTab] = React.useState<AeroSyncTab>(initialTab);
    // Keep visited tool panes mounted: switching tabs must not discard drafts
    // or detach an import, export or snapshot operation already in flight.
    const [visitedTools, setVisitedTools] = React.useState<readonly AeroSyncToolTab[]>([]);
    const tabId = React.useId();
    // Lifted from the Sync tab so the close path can guard against losing an
    // in-progress local sync (issue #332).
    const [syncRunning, setSyncRunning] = React.useState(false);
    const [restoreRunning, setRestoreRunning] = React.useState(false);
    const [journalRefresh, setJournalRefresh] = React.useState(0);
    const activeTab = effectiveAeroSyncTab(selectedTab, context.pairKind, syncRunning, isConnectedRemote);

    React.useEffect(() => {
        if (isOpen && AERO_SYNC_TOOL_TABS.includes(activeTab as AeroSyncToolTab)) {
            setVisitedTools(previous => previous.includes(activeTab as AeroSyncToolTab) ? previous : [...previous, activeTab as AeroSyncToolTab]);
        }
    }, [isOpen, activeTab]);

    React.useEffect(() => { onBusyChange?.(syncRunning || restoreRunning); }, [onBusyChange, syncRunning, restoreRunning]);
    React.useEffect(() => () => onBusyChange?.(false), [onBusyChange]);

    const cancelSync = React.useCallback(() => {
        invoke('local_sync_cancel').catch(() => { /* no run in flight */ });
    }, []);

    const guarded = useGuardedClose({
        guard: syncRunning ? 'busy' : null,
        // Restore has no cancellation command: keep the workflow mounted until
        // its native write operation actually settles.
        onClose: () => { if (!restoreRunning) onClose(); },
        onAbort: cancelSync,
    });

    // The settings the Plan and Sync tabs hold, kept alive across a tab switch
    // (which unmounts the tab) and cleared for the next open.
    //
    // The clearing happens when the dialog CLOSES rather than when it opens: the
    // store has to be empty before a tab mounts and reads it, and an effect runs
    // after that mount. Resetting during render would be early enough, but a ref
    // written mid-render is not safe against React discarding or replaying the
    // work. Closing is the same moment from the store's point of view, and it is
    // a side effect in a place where side effects belong. A first-ever open sees
    // a fresh store anyway.
    const tabState = React.useRef(createTabStateStore()).current;
    const applyImportedSettings = React.useCallback((settings: ImportedSyncSettings) => {
        const patch = buildAeroSyncTabStatePatch(
            settings,
            context.pairKind,
            tabState.get<VersionedBackupConfig | undefined>('plan.versionedBackup', undefined),
        );
        Object.entries(patch).forEach(([key, value]) => tabState.set(key, value));
        // Requested 116: settings that only narrow the comparison filter it, so
        // choosing a template does not cost a scan of the same two paths.
        const userExcludes = parseSyncExcludePatterns(patch['sync.exclude'] as string);
        const backupDir = tabState.get<VersionedBackupConfig | undefined>('plan.versionedBackup', undefined)?.backupDir
            ?? AEROSYNC_DEFAULT_BACKUP_DIR;
        const compareExcludes = context.compareExcludes ?? [];
        const unchanged = sameExcludePatterns(userExcludes, compareExcludes) && backupDir === context.compareBackupDir;
        if (context.compareResult && !context.compareLoading && !unchanged
            && narrowsComparison(compareExcludes, context.compareBackupDir, userExcludes, backupDir)) {
            void onNarrowComparison?.({ userExcludes, backupDir });
        }
        // Plan summarizes the imported direction/preset. Sync is populated in
        // the same store and shows the imported paths when the user opens it.
        setActiveTab('plan');
        document.getElementById(`${tabId}-tab-plan`)?.focus();
    }, [context.pairKind, context.compareResult, context.compareLoading, context.compareExcludes, context.compareBackupDir, onNarrowComparison, tabState, tabId]);
    React.useEffect(() => {
        if (!isOpen) {
            tabState.reset();
            setVisitedTools([]);
            setActiveTab(initialTab);
        }
    }, [isOpen, initialTab, tabState]);

    React.useEffect(() => {
        if (isOpen) setActiveTab(initialTab);
    }, [isOpen, initialTab]);


    // #347: the remote's documented file limits, for the Compare warning.
    const [remoteLimits, setRemoteLimits] = React.useState<ProviderFileLimits | null>(null);
    const hasRemote = context.pairKind === 'local-remote' || context.pairKind === 'remote-local';
    React.useEffect(() => {
        if (!isOpen || !hasRemote || !context.protocol) {
            setRemoteLimits(null);
            return;
        }
        let cancelled = false;
        invoke<TransferOptimizationHints>('get_transfer_optimization_hints', { providerType: context.protocol })
            .then((hints) => { if (!cancelled) setRemoteLimits(limitsFromHints(hints)); })
            .catch(() => { if (!cancelled) setRemoteLimits(null); });
        return () => { cancelled = true; };
    }, [isOpen, hasRemote, context.protocol]);

    if (!isOpen) return null;

    const canMirrorAny = context.pairKind === 'local-local'
        || context.pairKind === 'local-remote'
        || context.pairKind === 'remote-local';
    const canExecutePlan = canMirrorAny && !restoreRunning && !!context.compareResult;

    const localPath = context.initialSource || '';
    const remotePath = context.initialDestination || '';
    const serverProfileName = context.activeProfileName || '';
    const excludePatterns = context.excludePatterns || [];
    const isProvider = !!context.isProvider;

    const tabLabels: Record<AeroSyncTab, string> = {
        compare: t('aerosync.tabCompare') || 'Compare',
        plan: t('aerosync.tabPlan') || 'Plan',
        sync: t('aerosync.tabSync') || 'Local mirror',
        templates: t('aerosync.launcherTemplates'),
        multiPath: t('aerosync.launcherMultiPath'),
        rollback: t('aerosync.launcherRollback'),
        history: t('aerosync.launcherHistory'),
        scheduler: t('aerosync.launcherScheduler'),
    };

    const toolIcons = { templates: FileDown, multiPath: Layers, rollback: Undo2, history: History, scheduler: CalendarClock };
    const selectTab = (tab: AeroSyncTab) => {
        if (syncRunning && tab !== 'sync') return;
        setActiveTab(tab);
        document.getElementById(`${tabId}-tab-${tab}`)?.focus();
    };

    return (
        <div
            className="fixed inset-0 z-[9998] flex items-start justify-center pt-[5vh] bg-black/50 animate-scale-in"
            role="dialog"
            aria-modal="true"
            data-gui-owned="aerosync"
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
                        <button
                            type="button"
                            disabled={restoreRunning}
                            onClick={guarded.requestClose}
                            aria-label={t('common.close') || 'Close'}
                            className="p-2 hover:bg-gray-100 dark:hover:bg-gray-700 rounded-lg transition-colors"
                        >
                            <X size={18} />
                        </button>
                    </div>
                </div>

                <div
                    role="tablist"
                    aria-label={t('aerosync.title')}
                    className="flex shrink-0 gap-1 px-4 pt-3 overflow-x-auto border-b border-gray-200 dark:border-gray-700"
                    onKeyDown={(event) => {
                        if (!['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) return;
                        const tabs = Array.from(event.currentTarget.querySelectorAll<HTMLButtonElement>('[role="tab"]:not(:disabled)'));
                        const index = tabs.indexOf(document.activeElement as HTMLButtonElement);
                        if (index < 0 || tabs.length === 0) return;
                        event.preventDefault();
                        const next = event.key === 'Home' ? 0 : event.key === 'End' ? tabs.length - 1
                            : (index + (event.key === 'ArrowRight' ? 1 : -1) + tabs.length) % tabs.length;
                        tabs[next].focus();
                        tabs[next].click();
                    }}
                >
                    {(visibleTabs.includes(activeTab) ? visibleTabs : [...visibleTabs, activeTab]).map((tab) => {
                        const active = activeTab === tab;
                        const Icon = toolIcons[tab as AeroSyncToolTab];
                        // While a local sync runs, lock the user onto the Sync
                        // tab: leaving it would unmount SyncTabContent and orphan
                        // the in-flight run (issue #332).
                        const locked = syncRunning && tab !== 'sync';
                        return (
                            <React.Fragment key={tab}>
                            {tab === 'templates' && <span role="separator" aria-orientation="vertical" className="mx-1 mb-2 w-px shrink-0 self-stretch bg-gray-300 dark:bg-gray-600" />}
                            <button
                                id={`${tabId}-tab-${tab}`}
                                type="button"
                                onClick={() => selectTab(tab)}
                                disabled={locked}
                                className={`inline-flex shrink-0 items-center gap-1.5 whitespace-nowrap px-3 py-2 text-sm font-medium rounded-t-lg transition-colors border-b-2 ${
                                    active
                                        ? 'border-blue-500 text-blue-600 dark:text-blue-300'
                                        : 'border-transparent text-gray-500 hover:text-gray-700 hover:bg-gray-50 dark:text-gray-400 dark:hover:text-gray-200 dark:hover:bg-gray-700/50'
                                } ${locked ? 'opacity-40 cursor-not-allowed hover:bg-transparent dark:hover:bg-transparent' : ''}`}
                                aria-selected={active}
                                aria-controls={`${tabId}-panel`}
                                tabIndex={active ? 0 : -1}
                                role="tab"
                            >
                                {Icon && <Icon size={14} aria-hidden="true" />}
                                {tabLabels[tab]}
                            </button>
                            </React.Fragment>
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
                                disabled={restoreRunning}
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
                                onClick={() => {
                                    // Wait for deletion before reloading the active journal pane.
                                    void Promise.resolve(onDismissJournal()).then(
                                        () => setJournalRefresh(revision => revision + 1),
                                        () => { /* Leave the current list available if dismissal fails. */ },
                                    );
                                }}
                                className="rounded bg-gray-200 px-3 py-1 text-xs font-medium hover:bg-gray-300 dark:bg-gray-700 dark:hover:bg-gray-600"
                            >
                                {t('syncPanel.dismissJournal') || 'Dismiss'}
                            </button>
                        </div>
                    </div>
                )}

                <TabStateStoreContext.Provider value={tabState}>
                <ScanProgressProvider active={!!context.compareLoading} progressId={context.scanProgressId}>
                <div className="flex-1 min-h-0 overflow-y-auto">
                    <div role="tabpanel" id={`${tabId}-panel`} aria-labelledby={`${tabId}-tab-${activeTab}`} tabIndex={0}>
                    {activeTab === 'compare' && (
                        <CompareTabContent
                            result={context.compareResult}
                            narrowed={context.compareNarrowed}
                            loading={context.compareLoading}
                            scanProgressId={context.scanProgressId}
                            stopped={context.compareStopped}
                            onStartCompare={context.compareIdle && !restoreRunning ? onStartCompare : undefined}
                            onStopCompare={onStopCompare}
                            leftLabel={context.leftLabel}
                            rightLabel={context.rightLabel}
                            pairKind={context.pairKind}
                            canMirrorLeftToRight={canMirrorAny && !restoreRunning}
                            canMirrorRightToLeft={canMirrorAny && !restoreRunning}
                            onApplyMirrorLeftToRight={onApplyMirrorLeftToRight}
                            onApplyMirrorRightToLeft={onApplyMirrorRightToLeft}
                            remoteLimits={remoteLimits}
                            remoteBasePath={context.initialDestination || ''}
                        />
                    )}
                    {activeTab === 'plan' && (
                        <PlanTabContent
                            result={context.compareResult}
                            loading={context.compareLoading}
                            scanProgressId={context.scanProgressId}
                            compareStopped={context.compareStopped}
                            leftLabel={context.leftLabel}
                            rightLabel={context.rightLabel}
                            onStartCompare={context.compareIdle && !restoreRunning ? onStartCompare : undefined}
                            onStopCompare={onStopCompare}
                            pairKind={context.pairKind}
                            canExecute={canExecutePlan}
                            onExecute={onExecutePreset}
                            compareExcludes={context.compareExcludes ?? []}
                            compareBackupDir={context.compareBackupDir}
                            onRescan={restoreRunning ? undefined : onRescan}
                            onNarrowComparison={restoreRunning ? undefined : onNarrowComparison}
                            narrowRefusal={context.compareNarrowRefusal}
                            isProvider={context.isProvider}
                            cli={{
                                profileName: context.activeProfileSaved ? context.activeProfileName : null,
                                profileInitialPath: context.activeProfileInitialPath,
                                localPath: context.initialSource ?? '',
                                remotePath: context.initialDestination ?? '',
                            }}
                        />
                    )}
                    {activeTab === 'sync' && (
                        <SyncTabContent
                            initialSource={context.initialSource}
                            initialDestination={context.initialDestination}
                            pairKind={context.pairKind}
                            activeProfileId={context.activeProfileId}
                            initialExcludePatterns={excludePatterns}
                            onRunningChange={setSyncRunning}
                        />
                    )}
                    {isConnectedRemote && AERO_SYNC_TOOL_TABS.map(tool => (
                        (visitedTools.includes(tool) || activeTab === tool) && (
                            <div key={`${context.activeProfileId}:${localPath}:${remotePath}:${tool}`} hidden={activeTab !== tool}>
                                {tool === 'templates' && <SyncTemplateDialog embedded isOpen onClose={() => selectTab('plan')}
                                    localPath={localPath} remotePath={remotePath} serverProfileName={serverProfileName}
                                    excludePatterns={excludePatterns} pairKind={context.pairKind} compareResult={context.compareResult}
                                    compareLoading={context.compareLoading} compareExcludes={context.compareExcludes} compareBackupDir={context.compareBackupDir}
                                    onApplyImport={applyImportedSettings} />}
                                {tool === 'multiPath' && <MultiPathEditor embedded isOpen onClose={() => selectTab('plan')}
                                    localPath={localPath} remotePath={remotePath} />}
                                {tool === 'rollback' && <RollbackDialog embedded isOpen onClose={() => selectTab('plan')}
                                    onRestoringChange={running => {
                                        // Invalidate both before writes begin and before Plan is unlocked,
                                        // including rejected restores that may have partially changed files.
                                        onInvalidateComparison();
                                        setRestoreRunning(running);
                                    }}
                                    localPath={localPath} remotePath={remotePath} isProvider={isProvider} />}
                                {tool === 'history' && <JournalHistoryDialog embedded isOpen={activeTab === 'history'} onClose={() => selectTab('plan')}
                                    refreshRevision={journalRefresh}
                                    localPath={localPath} remotePath={remotePath} />}
                                {tool === 'scheduler' && <div className="p-4"><SyncScheduler /></div>}
                            </div>
                        )
                    ))}
                    </div>
                </div>
                </ScanProgressProvider>
                </TabStateStoreContext.Provider>
            </div>

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
