/**
 * RollbackDialog — View and manage sync snapshots for rollback
 * Shows snapshot list with file counts and timestamps
 */

import React, { useState, useEffect, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import {
    X, Undo2, Camera, Trash2, Clock, File, Plus
} from 'lucide-react';
import { SyncSnapshot } from '../../types';
import { useTranslation } from '../../i18n';
import { formatSize } from '../../utils/formatters';
import { logger } from '../../utils/logger';

interface RollbackDialogProps {
    isOpen: boolean;
    onClose: () => void;
    localPath: string;
    remotePath: string;
}

export const RollbackDialog: React.FC<RollbackDialogProps> = ({
    isOpen,
    onClose,
    localPath,
    remotePath,
}) => {
    const t = useTranslation();
    const [snapshots, setSnapshots] = useState<SyncSnapshot[]>([]);
    const [loading, setLoading] = useState(true);
    const [creating, setCreating] = useState(false);
    const [selectedId, setSelectedId] = useState<string | null>(null);

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

    const selectedSnapshot = snapshots.find(s => s.id === selectedId);

    if (!isOpen) return null;

    return (
        <div className="fixed inset-0 bg-black/60 z-[9999] flex items-center justify-center p-4" onClick={onClose} role="dialog" aria-modal="true" aria-label="Rollback Snapshot">
            <div
                className="bg-white dark:bg-gray-800 rounded-xl shadow-2xl w-full max-w-2xl max-h-[80vh] flex flex-col"
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
                        <button
                            className="text-xs px-4 py-1.5 rounded-lg bg-amber-500 text-white hover:bg-amber-600 disabled:opacity-50 disabled:cursor-not-allowed"
                            disabled={true}
                            /* Restore not yet implemented — i18n key: syncPanel.rollbackRestoreComingSoon */
                            title={t('syncPanel.rollbackRestoreComingSoon') || 'Coming in v2.3'}
                        >
                            <Undo2 size={12} className="inline mr-1" />
                            {t('syncPanel.rollbackRestore')}
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
        </div>
    );
};
