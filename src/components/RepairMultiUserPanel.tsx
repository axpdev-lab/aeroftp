// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import React, { useCallback, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { ShieldCheck, ShieldAlert, RefreshCw, Wrench, Upload, Archive, Loader2 } from 'lucide-react';
import { useTranslation } from '../i18n';
import { mapUserPartitionError } from '../utils/userPartitionErrors';

/**
 * F-012 W4: "Repair multi-user data" panel.
 *
 * Surfaces proactively when the active user's data key cannot be unwrapped on
 * this machine (`activeUserReadable === false`) -- the headline cross-machine
 * import symptom -- and always offers the manual repair tools:
 *
 *  - Rebuild from this device: snapshot + remove the broken user_partitions.db
 *    and re-migrate from the legacy credential vault under THIS machine's local
 *    key (the manual local-reset the owner ran by hand, productized with an
 *    automatic pre-step backup).
 *  - Re-key from a backup: re-run the keystore import (a fix-branch backup
 *    carries transport DEKs; a passphrase backup unlocks on this machine).
 *  - Restart AeroFTP: relaunch so freshly rebuilt/imported state is loaded.
 */
interface PartitionHealth {
    activeUserReadable: boolean;
    activeUserName?: string | null;
    profileCount: number;
    errorCode?: string | null;
    canRebuildFromDevice: boolean;
}

interface PartitionRebuildReport {
    recoveredProfiles: number;
    backupPath?: string | null;
    createdDefaultUser: boolean;
}

interface RepairMultiUserPanelProps {
    /** Open the keystore import flow ("Re-key from a backup"). */
    onImportClick: () => void;
    /** Refresh the parent server list after a rebuild repopulates the partition. */
    onRepaired?: () => void;
}

export const RepairMultiUserPanel: React.FC<RepairMultiUserPanelProps> = ({ onImportClick, onRepaired }) => {
    const t = useTranslation();
    const [health, setHealth] = useState<PartitionHealth | null>(null);
    const [loading, setLoading] = useState(true);
    const [confirming, setConfirming] = useState(false);
    const [rebuilding, setRebuilding] = useState(false);
    const [rebuilt, setRebuilt] = useState<PartitionRebuildReport | null>(null);
    const [error, setError] = useState<string | null>(null);
    const [restarting, setRestarting] = useState(false);

    const refreshHealth = useCallback(async () => {
        setLoading(true);
        try {
            const h = await invoke<PartitionHealth>('user_partitions_health');
            setHealth(h);
        } catch (e) {
            setError(mapUserPartitionError(e, t));
        } finally {
            setLoading(false);
        }
    }, [t]);

    useEffect(() => {
        void refreshHealth();
    }, [refreshHealth]);

    const handleRebuild = async () => {
        setRebuilding(true);
        setError(null);
        try {
            const report = await invoke<PartitionRebuildReport>('user_partitions_repair_rebuild');
            setRebuilt(report);
            setConfirming(false);
            onRepaired?.();
            await refreshHealth();
        } catch (e) {
            setError(mapUserPartitionError(e, t));
        } finally {
            setRebuilding(false);
        }
    };

    const handleRestart = async () => {
        setRestarting(true);
        try {
            await invoke('restart_app');
        } catch {
            setRestarting(false);
        }
    };

    const healthy = health?.activeUserReadable ?? true;

    return (
        <div className="border border-gray-200 dark:border-gray-700 rounded-lg overflow-hidden">
            <div className="bg-gray-50 dark:bg-gray-700/50 px-4 py-2 border-b border-gray-200 dark:border-gray-700">
                <h4 className="font-medium flex items-center gap-2 text-sm text-gray-800 dark:text-gray-100">
                    <Wrench size={14} className="text-blue-500" />
                    {t('settings.repairTitle')}
                </h4>
            </div>

            <div className="p-4 space-y-3">
                {/* Health status */}
                {loading ? (
                    <div className="flex items-center gap-2 text-sm text-gray-500 dark:text-gray-400">
                        <Loader2 size={15} className="animate-spin" />
                        {t('settings.repairChecking')}
                    </div>
                ) : healthy ? (
                    <div className="flex items-start gap-2 text-sm text-emerald-700 dark:text-emerald-300">
                        <ShieldCheck size={16} className="mt-0.5 flex-shrink-0" />
                        <span>{t('settings.repairHealthy', { count: health?.profileCount ?? 0, user: health?.activeUserName || '' })}</span>
                    </div>
                ) : (
                    <div className="rounded-lg border border-amber-300 dark:border-amber-700 bg-amber-50 dark:bg-amber-900/20 p-3">
                        <div className="flex items-start gap-2 text-sm text-amber-800 dark:text-amber-200">
                            <ShieldAlert size={16} className="mt-0.5 flex-shrink-0" />
                            <span>{t('settings.repairUnreadable', { user: health?.activeUserName || '' })}</span>
                        </div>
                    </div>
                )}

                {error && (
                    <div className="text-sm text-red-600 dark:text-red-400">{error}</div>
                )}

                {/* Rebuild result */}
                {rebuilt && (
                    <div className="rounded-lg border border-emerald-300 dark:border-emerald-700 bg-emerald-50 dark:bg-emerald-900/20 p-3 space-y-1">
                        <div className="text-sm text-emerald-800 dark:text-emerald-200">
                            {t('settings.repairRebuiltSummary', { count: rebuilt.recoveredProfiles })}
                        </div>
                        {rebuilt.backupPath && (
                            <div className="flex items-start gap-2 text-xs text-emerald-700 dark:text-emerald-300/90">
                                <Archive size={13} className="mt-0.5 flex-shrink-0" />
                                <span className="break-all">{t('settings.keystoreBackupSnapshotPath', { path: rebuilt.backupPath })}</span>
                            </div>
                        )}
                    </div>
                )}

                {/* Actions */}
                <div className="flex flex-wrap gap-2 pt-1">
                    {confirming ? (
                        <div className="w-full rounded-lg border border-amber-300 dark:border-amber-700 bg-amber-50 dark:bg-amber-900/20 p-3 space-y-2">
                            <p className="text-xs text-amber-800 dark:text-amber-200">{t('settings.repairRebuildConfirm')}</p>
                            <div className="flex gap-2">
                                <button
                                    onClick={handleRebuild}
                                    disabled={rebuilding}
                                    className="inline-flex items-center gap-1.5 px-3 py-1.5 rounded-lg bg-amber-500 hover:bg-amber-600 text-white text-xs font-medium transition-colors disabled:opacity-60"
                                >
                                    {rebuilding ? <Loader2 size={14} className="animate-spin" /> : <Wrench size={14} />}
                                    {t('settings.repairRebuildConfirmYes')}
                                </button>
                                <button
                                    onClick={() => setConfirming(false)}
                                    disabled={rebuilding}
                                    className="px-3 py-1.5 rounded-lg text-xs font-medium text-gray-600 dark:text-gray-300 hover:bg-gray-200 dark:hover:bg-gray-700 transition-colors disabled:opacity-50"
                                >
                                    {t('common.cancel')}
                                </button>
                            </div>
                        </div>
                    ) : (
                        <>
                            <button
                                onClick={() => { setConfirming(true); setRebuilt(null); }}
                                disabled={!health?.canRebuildFromDevice}
                                title={!health?.canRebuildFromDevice ? t('settings.repairRebuildUnavailable') : undefined}
                                className="inline-flex items-center gap-1.5 px-3 py-1.5 rounded-lg border border-gray-300 dark:border-gray-600 hover:bg-gray-100 dark:hover:bg-gray-700 text-xs font-medium transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
                            >
                                <Wrench size={14} /> {t('settings.repairRebuild')}
                            </button>
                            <button
                                onClick={onImportClick}
                                className="inline-flex items-center gap-1.5 px-3 py-1.5 rounded-lg border border-gray-300 dark:border-gray-600 hover:bg-gray-100 dark:hover:bg-gray-700 text-xs font-medium transition-colors"
                            >
                                <Upload size={14} /> {t('settings.repairRekeyFromBackup')}
                            </button>
                            <button
                                onClick={handleRestart}
                                disabled={restarting}
                                className="inline-flex items-center gap-1.5 px-3 py-1.5 rounded-lg border border-blue-300 dark:border-blue-700 text-blue-600 dark:text-blue-300 hover:bg-blue-50 dark:hover:bg-blue-900/30 text-xs font-medium transition-colors disabled:opacity-60"
                            >
                                <RefreshCw size={14} className={restarting ? 'animate-spin' : ''} /> {t('settings.restartApp')}
                            </button>
                        </>
                    )}
                </div>

                <p className="text-xs text-gray-500 dark:text-gray-400">{t('settings.repairRebuildDesc')}</p>
            </div>
        </div>
    );
};
