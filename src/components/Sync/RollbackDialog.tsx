// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

/**
 * RollbackDialog — View and manage sync snapshots for rollback
 * Shows snapshot list with file counts and timestamps
 */

import React, { useState, useEffect, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import {
    X, Undo2, Camera, Trash2, Clock, File, Plus
} from 'lucide-react';
import { RestoreSnapshotResult, SyncSnapshot } from '../../types';
import { useTranslation } from '../../i18n';
import { formatSize } from '../../utils/formatters';
import { logger } from '../../utils/logger';

interface RollbackDialogProps {
    isOpen: boolean;
    onClose: () => void;
    localPath: string;
    remotePath: string;
    isProvider: boolean;
    versioningStrategy?: 'disabled' | 'trash_can' | 'simple' | 'staggered';
}

export const RollbackDialog: React.FC<RollbackDialogProps> = ({
    isOpen,
    onClose,
    localPath,
    remotePath,
    isProvider,
    versioningStrategy,
}) => {
    const t = useTranslation();
    const [snapshots, setSnapshots] = useState<SyncSnapshot[]>([]);
    const [loading, setLoading] = useState(true);
    const [creating, setCreating] = useState(false);
    const [selectedId, setSelectedId] = useState<string | null>(null);
    const [restoring, setRestoring] = useState(false);
    const [confirmRestore, setConfirmRestore] = useState(false);
    const [restoreResult, setRestoreResult] = useState<string | null>(null);

    const cancelledRef = useRef(false);

    useEffect(() => {
        if (!isOpen) return;
        const handler = (e: KeyboardEvent) => { if (e.key === 'Escape') onClose(); };
        window.addEventListener('keydown', handler);
        return () => window.removeEventListener('keydown', handler);
    }, [isOpen, onClose]);

    useEffect(() => {
        if (!isOpen) return;
        cancelledRef.current = false;
        loadSnapshots();
        return () => { cancelledRef.current = true; };
    }, [isOpen, localPath, remotePath]);

    const loadSnapshots = async () => {
        setLoading(true);
        try {
            const snaps = await invoke<SyncSnapshot[]>('list_sync_snapshots_cmd', {
                localPath, remotePath,
            });
            if (!cancelledRef.current) setSnapshots(snaps);
        } catch (e) {
            logger.error('[RollbackDialog] loadSnapshots failed:', e);
            if (!cancelledRef.current) setSnapshots([]);
        } finally {
            if (!cancelledRef.current) setLoading(false);
        }
    };

    const handleCreate = async () => {
        setCreating(true);
        try {
            await invoke('create_sync_snapshot_cmd', {
                localPath, remotePath,
            });
            await loadSnapshots();
        } catch (e) { logger.error('[RollbackDialog] create snapshot failed:', e); }
        finally {
            setCreating(false);
        }
    };

    const handleDelete = async (snapshotId: string) => {
        try {
            await invoke('delete_sync_snapshot_cmd', {
                localPath, remotePath, snapshotId,
            });
            setSnapshots(prev => prev.filter(s => s.id !== snapshotId));
            if (selectedId === snapshotId) setSelectedId(null);
        } catch (e) { logger.error('[RollbackDialog] delete snapshot failed:', e); }
    };

    const handleRestore = async () => {
        if (!selectedId) return;
        setConfirmRestore(false);
        setRestoring(true);
        setRestoreResult(null);
        try {
            const result = await invoke<RestoreSnapshotResult>('restore_sync_snapshot_cmd', {
                snapshotId: selectedId,
                localPath,
                remotePath,
                isProvider,
                versioningStrategy,
            });
            const messages = [
                result.restored_from_remote > 0 ? `local ${result.restored_from_remote}` : null,
                result.restored_to_remote > 0 ? `remote ${result.restored_to_remote}` : null,
                result.skipped > 0 ? `skipped ${result.skipped}` : null,
                result.failed.length > 0 ? `failed ${result.failed.length}` : null,
            ].filter(Boolean);
            setRestoreResult(messages.length > 0 ? `Restore completed: ${messages.join(', ')}` : 'Nothing to restore');
            await loadSnapshots();
        } catch (e: any) {
            setRestoreResult(`Restore failed: ${e?.toString() || 'unknown error'}`);
        } finally {
            setRestoring(false);
        }
    };

    const selectedSnapshot = snapshots.find(s => s.id === selectedId);

    if (!isOpen) return null;

    return (
        <div className="fixed inset-0 bg-black/60 z-[9999] flex items-center justify-center p-4" onClick={onClose} role="dialog" aria-modal="true" aria-label="Rollback Snapshot">
            <div
                className="bg-white dark:bg-gray-800 rounded-lg shadow-2xl w-full max-w-2xl max-h-[80vh] flex flex-col animate-scale-in"
                onClick={e => e.stopPropagation()}
            >
                {/* Header */}
                <div className="flex items-center justify-between px-5 py-4 border-b border-gray-200 dark:border-gray-700">
                    <div className="flex items-center gap-2">
                        <Undo2 size={18} className="text-amber-500" />
                        <h3 className="font-semibold text-sm">{t('syncPanel.rollback')}</h3>
                    </div>
                    <button onClick={onClose} className="text-gray-400 hover:text-gray-200">
                        <X size={18} />
                    </button>
                </div>

                {/* Content */}
                <div className="flex-1 overflow-y-auto px-5 py-4">
                    {loading ? (
                        <div className="text-center text-gray-400 py-8 text-sm">{t('common.loading')}</div>
                    ) : snapshots.length === 0 ? (
                        <div className="text-center text-gray-400 py-8 text-sm">
                            <Camera size={32} className="mx-auto mb-3 opacity-30" />
                            {t('syncPanel.rollbackEmpty')}
                        </div>
                    ) : (
                        <div className="space-y-2">
                            {snapshots.map(snap => {
                                const fileCount = Object.keys(snap.files).length;
                                const totalSize = Object.values(snap.files).reduce((sum, f) => sum + f.size, 0);
                                const isSelected = selectedId === snap.id;

                                return (
                                    <div
                                        key={snap.id}
                                        className={`p-3 rounded-lg border cursor-pointer transition-colors ${
                                            isSelected
                                                ? 'border-amber-500 bg-amber-500/10'
                                                : 'border-gray-300 dark:border-gray-600 hover:border-gray-400 dark:hover:border-gray-500'
                                        }`}
                                        onClick={() => setSelectedId(isSelected ? null : snap.id)}
                                    >
                                        <div className="flex items-center justify-between">
                                            <div className="flex items-center gap-2">
                                                <Camera size={14} className="text-amber-500" />
                                                <span className="text-xs font-medium">
                                                    {new Date(snap.created_at).toLocaleString()}
                                                </span>
                                            </div>
                                            <div className="flex items-center gap-3">
                                                <span className="text-[10px] text-gray-400">
                                                    {fileCount} files, {formatSize(totalSize)}
                                                </span>
                                                <button
                                                    onClick={e => { e.stopPropagation(); handleDelete(snap.id); }}
                                                    className="text-gray-400 hover:text-red-400"
                                                    title={t('syncPanel.rollbackDeleteSnapshot')}
                                                >
                                                    <Trash2 size={12} />
                                                </button>
                                            </div>
                                        </div>

                                        {/* Expanded detail */}
                                        {isSelected && (
                                            <div className="mt-3 pt-2 border-t border-gray-200/20 max-h-48 overflow-y-auto">
                                                <div className="text-[10px] text-gray-400 mb-1">
                                                    {t('syncPanel.rollbackFiles')}:
                                                </div>
                                                {Object.entries(snap.files).slice(0, 50).map(([path, entry]) => (
                                                    <div key={path} className="flex items-center gap-2 text-xs py-0.5">
                                                        <File size={10} className="text-gray-500 flex-shrink-0" />
                                                        <span className="flex-1 truncate text-gray-400" title={path}>
                                                            {path}
                                                        </span>
                                                        <span className="text-gray-500 text-[10px] flex-shrink-0">
                                                            {formatSize(entry.size)}
                                                        </span>
                                                        <span className="text-[10px] text-gray-600 flex-shrink-0">
                                                            {t(`syncPanel.action${entry.action_taken.charAt(0).toUpperCase() + entry.action_taken.slice(1)}`) || entry.action_taken}
                                                        </span>
                                                    </div>
                                                ))}
                                                {Object.keys(snap.files).length > 50 && (
                                                    <div className="text-[10px] text-gray-500 mt-1">
                                                        +{Object.keys(snap.files).length - 50} more...
                                                    </div>
                                                )}
                                            </div>
                                        )}
                                    </div>
                                );
                            })}
                        </div>
                    )}
                </div>

                {/* Footer */}
                <div className="flex items-center justify-between px-5 py-3 border-t border-gray-200 dark:border-gray-700">
                    <button
                        className="flex items-center gap-1 text-xs px-3 py-1.5 rounded-lg bg-amber-500/15 text-amber-400 border border-amber-500/25 hover:bg-amber-500/25"
                        onClick={handleCreate}
                        disabled={creating}
                    >
                        <Plus size={12} /> {creating ? '...' : t('syncPanel.rollbackCreate')}
                    </button>
                    <div className="flex items-center gap-2">
                        {restoreResult && (
                            <span className="text-[11px] text-amber-400 mr-2">{restoreResult}</span>
                        )}
                        <button
                            className="text-xs px-4 py-1.5 rounded-lg bg-amber-500 text-white hover:bg-amber-600 disabled:opacity-50 disabled:cursor-not-allowed"
                            disabled={!selectedId || restoring}
                            onClick={() => setConfirmRestore(true)}
                        >
                            <Undo2 size={12} className="inline mr-1" />
                            {restoring ? '...' : t('syncPanel.rollbackRestore')}
                        </button>
                        <button
                            className="text-xs px-4 py-1.5 rounded-lg bg-gray-200 dark:bg-gray-700 hover:bg-gray-300 dark:hover:bg-gray-600"
                            onClick={onClose}
                        >
                            {t('common.close')}
                        </button>
                    </div>
                </div>
            </div>

            {/* Restore confirmation overlay */}
            {confirmRestore && selectedSnapshot && (
                <div className="absolute inset-0 bg-black/50 rounded-lg flex items-center justify-center p-6">
                    <div className="bg-gray-800 rounded-lg border border-amber-500/40 p-5 max-w-sm w-full">
                        <div className="flex items-center gap-2 mb-3">
                            <Undo2 size={16} className="text-amber-500" />
                            <span className="font-semibold text-sm">{t('syncPanel.rollbackConfirmTitle')}</span>
                        </div>
                        <p className="text-xs text-gray-300 mb-4">
                            {t('syncPanel.rollbackConfirmBody', { count: Object.keys(selectedSnapshot.files).length })}
                        </p>
                        <div className="flex justify-end gap-2">
                            <button
                                className="px-3 py-1.5 text-xs rounded-lg bg-gray-700 hover:bg-gray-600 text-gray-300"
                                onClick={() => setConfirmRestore(false)}
                            >
                                {t('common.cancel')}
                            </button>
                            <button
                                className="px-3 py-1.5 text-xs rounded-lg bg-amber-500 hover:bg-amber-600 text-white font-medium"
                                onClick={handleRestore}
                            >
                                <Undo2 size={12} className="inline mr-1" /> {t('syncPanel.rollbackRestore')}
                            </button>
                        </div>
                    </div>
                </div>
            )}
        </div>
    );
};
