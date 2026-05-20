// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
//
// AeroFile Sync: unified modal that bundles Compare, Plan, and Sync into
// a single 3-tab workflow. Replaces the legacy LocalSyncPanel,
// UnifiedCompareDialog, and SyncPresetDialog dialogs which were
// disconnected entries in the View menu.

import * as React from 'react';
import { FolderSync, X } from 'lucide-react';
import { useTranslation } from '../../i18n';
import { useDraggableModal } from '../../hooks/useDraggableModal';
import { CompareTabContent } from './CompareTabContent';
import { PlanTabContent } from './PlanTabContent';
import { SyncTabContent } from './SyncTabContent';
import type { AeroFileSyncDialogProps, AeroFileSyncTab } from './types';

const TAB_ORDER: AeroFileSyncTab[] = ['compare', 'plan', 'sync'];

export const AeroFileSyncDialog: React.FC<AeroFileSyncDialogProps> = ({
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
    const [activeTab, setActiveTab] = React.useState<AeroFileSyncTab>(initialTab);

    React.useEffect(() => {
        if (isOpen) setActiveTab(initialTab);
    }, [isOpen, initialTab]);

    if (!isOpen) return null;

    const canMirrorAny = context.pairKind === 'local-local'
        || context.pairKind === 'local-remote'
        || context.pairKind === 'remote-local';
    const canExecutePlan = canMirrorAny;

    const tabLabels: Record<AeroFileSyncTab, string> = {
        compare: t('aerofileSync.tabCompare') || 'Compare',
        plan: t('aerofileSync.tabPlan') || 'Plan',
        sync: t('aerofileSync.tabSync') || 'Sync',
    };

    return (
        <div
            className="fixed inset-0 z-[9998] flex items-start justify-center pt-[5vh] bg-black/50 animate-scale-in"
            role="dialog"
            aria-modal="true"
            aria-label={t('aerofileSync.title') || 'AeroFile Sync'}
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
                            {t('aerofileSync.title') || 'AeroFile Sync'}
                        </h2>
                    </div>
                    <button
                        type="button"
                        onClick={onClose}
                        aria-label={t('common.close') || 'Close'}
                        className="p-2 hover:bg-gray-100 dark:hover:bg-gray-700 rounded-lg transition-colors"
                    >
                        <X size={18} />
                    </button>
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
                        />
                    )}
                </div>
            </div>
        </div>
    );
};

export default AeroFileSyncDialog;
