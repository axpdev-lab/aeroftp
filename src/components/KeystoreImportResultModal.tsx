// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import React, { useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { CheckCircle2, AlertTriangle, RefreshCw, KeyRound, Archive } from 'lucide-react';
import { useTranslation } from '../i18n';
import { useDraggableModal } from '../hooks/useDraggableModal';

/**
 * Post-import summary modal (F-012 W1 + W2). Replaces the old fire-and-forget
 * toast for any import that touched on-disk databases or carried cross-machine
 * user partitions, so the outcome can never be silently missed:
 *
 *  - W1: when the import rewrote SQLite databases the running app holds open
 *    (`requiresRestart`), a mandatory restart is offered with a single primary
 *    "Restart now" button (the app re-reads the imported state on relaunch).
 *  - W2: when passphrase-less partitions arrived from another machine with no
 *    portable key (`userPartitionsUnreadable > 0`), a prominent amber warning
 *    explains exactly what to do, plus the three-password disambiguation that
 *    caused the original confusion (vault master password vs account password
 *    vs backup file password).
 *  - W3: the path of the pre-import snapshot of user_partitions.db is surfaced
 *    so a destructive import is visibly reversible.
 */
export interface KeystoreImportResult {
    imported: number;
    skipped: number;
    requiresRestart?: boolean;
    userPartitionsRekeyed?: number;
    userPartitionsUnreadable?: number;
    userPartitionsBackupPath?: string;
}

interface KeystoreImportResultModalProps {
    result: KeystoreImportResult;
    onClose: () => void;
}

export const KeystoreImportResultModal: React.FC<KeystoreImportResultModalProps> = ({
    result,
    onClose,
}) => {
    const t = useTranslation();
    const modalDrag = useDraggableModal();
    const [restarting, setRestarting] = useState(false);

    const rekeyed = result.userPartitionsRekeyed ?? 0;
    const unreadable = result.userPartitionsUnreadable ?? 0;
    const requiresRestart = !!result.requiresRestart;
    const hasWarning = unreadable > 0;

    const handleRestart = async () => {
        setRestarting(true);
        try {
            await invoke('restart_app');
        } catch {
            // restart_app diverges on success; reaching here means it could not
            // relaunch. Let the user close and restart manually.
            setRestarting(false);
        }
    };

    return (
        <div
            className="fixed inset-0 z-[9999] flex items-start justify-center pt-[8vh] bg-black/50 backdrop-blur-sm"
            onClick={requiresRestart ? undefined : onClose}
            role="dialog"
            aria-modal="true"
            aria-label={t('settings.keystoreImportComplete')}
        >
            <div
                {...modalDrag.panelProps}
                className="bg-white dark:bg-gray-800 rounded-lg shadow-2xl border border-gray-200 dark:border-gray-700 max-w-md w-full mx-4 overflow-hidden animate-scale-in"
                onClick={e => e.stopPropagation()}
            >
                <div className="p-6">
                    <div {...modalDrag.dragHandleProps} className="flex items-center gap-3 mb-4 cursor-grab active:cursor-grabbing">
                        <div className={`w-10 h-10 rounded-full flex items-center justify-center flex-shrink-0 ${
                            hasWarning ? 'bg-amber-100 dark:bg-amber-900/30' : 'bg-emerald-100 dark:bg-emerald-900/30'
                        }`}>
                            {hasWarning
                                ? <AlertTriangle size={20} className="text-amber-500" />
                                : <CheckCircle2 size={20} className="text-emerald-500" />}
                        </div>
                        <h3 className="text-lg font-semibold text-gray-900 dark:text-gray-100">
                            {t('settings.keystoreImportComplete')}
                        </h3>
                    </div>

                    <p className="text-sm text-gray-700 dark:text-gray-300 mb-3">
                        {t('settings.keystoreImported', { imported: result.imported, skipped: result.skipped })}
                    </p>

                    {rekeyed > 0 && (
                        <div className="flex items-start gap-2 mb-3 text-sm text-emerald-700 dark:text-emerald-300">
                            <KeyRound size={16} className="mt-0.5 flex-shrink-0" />
                            <span>{t('settings.keystoreRekeyedPartitions', { count: rekeyed })}</span>
                        </div>
                    )}

                    {unreadable > 0 && (
                        <div className="rounded-lg border border-amber-300 dark:border-amber-700 bg-amber-50 dark:bg-amber-900/20 p-3 mb-3">
                            <div className="flex items-start gap-2 text-sm text-amber-800 dark:text-amber-200">
                                <AlertTriangle size={16} className="mt-0.5 flex-shrink-0" />
                                <span>{t('settings.keystoreUnreadablePartitions', { count: unreadable })}</span>
                            </div>
                            <p className="text-xs text-amber-700 dark:text-amber-300/90 mt-2 pl-6">
                                {t('settings.keystorePasswordDisambiguation')}
                            </p>
                        </div>
                    )}

                    {result.userPartitionsBackupPath && (
                        <div className="flex items-start gap-2 mb-3 text-xs text-gray-500 dark:text-gray-400">
                            <Archive size={14} className="mt-0.5 flex-shrink-0" />
                            <span className="break-all">
                                {t('settings.keystoreBackupSnapshotPath', { path: result.userPartitionsBackupPath })}
                            </span>
                        </div>
                    )}

                    {requiresRestart && (
                        <div className="flex items-start gap-2 mb-1 text-sm text-gray-700 dark:text-gray-300">
                            <RefreshCw size={16} className="mt-0.5 flex-shrink-0 text-blue-500" />
                            <span>{t('settings.keystoreRestartRequired')}</span>
                        </div>
                    )}
                </div>

                <div className="flex items-center justify-end gap-2 px-6 py-4 bg-gray-50 dark:bg-gray-900/40 border-t border-gray-200 dark:border-gray-700">
                    {requiresRestart ? (
                        <>
                            <button
                                onClick={onClose}
                                disabled={restarting}
                                className="px-4 py-2 rounded-lg text-sm font-medium text-gray-600 dark:text-gray-300 hover:bg-gray-200 dark:hover:bg-gray-700 transition-colors disabled:opacity-50"
                            >
                                {t('settings.keystoreRestartLater')}
                            </button>
                            <button
                                onClick={handleRestart}
                                disabled={restarting}
                                className="inline-flex items-center gap-2 px-4 py-2 rounded-lg text-sm font-medium bg-blue-500 hover:bg-blue-600 text-white transition-colors disabled:opacity-60"
                            >
                                <RefreshCw size={15} className={restarting ? 'animate-spin' : ''} />
                                {t('settings.keystoreRestartNow')}
                            </button>
                        </>
                    ) : (
                        <button
                            onClick={onClose}
                            className="px-4 py-2 rounded-lg text-sm font-medium bg-blue-500 hover:bg-blue-600 text-white transition-colors"
                        >
                            {t('common.close')}
                        </button>
                    )}
                </div>
            </div>
        </div>
    );
};
