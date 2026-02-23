/**
 * MultiPathEditor — CRUD dialog for sync path pairs
 * Allows adding/removing multiple local↔remote path pairs
 */

import React, { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import {
    X, Plus, Trash2, FolderTree, Folder, Globe, ToggleLeft, ToggleRight
} from 'lucide-react';
import { PathPair, MultiPathConfig } from '../../types';
import { useTranslation } from '../../i18n';
import { logger } from '../../utils/logger';

const MAX_PATH_PAIRS = 50;

interface MultiPathEditorProps {
    isOpen: boolean;
    onClose: () => void;
    localPath: string;
    remotePath: string;
}

export const MultiPathEditor: React.FC<MultiPathEditorProps> = ({
    isOpen,
    onClose,
    localPath,
    remotePath,
}) => {
    const t = useTranslation();
    const [pairs, setPairs] = useState<PathPair[]>([]);
    const [loading, setLoading] = useState(true);

    useEffect(() => {
        if (!isOpen) return;
        const handler = (e: KeyboardEvent) => { if (e.key === 'Escape') onClose(); };
        window.addEventListener('keydown', handler);
        return () => window.removeEventListener('keydown', handler);
    }, [isOpen, onClose]);

    useEffect(() => {
        if (!isOpen) return;
        setLoading(true);
        invoke<MultiPathConfig>('get_multi_path_config', { localPath, remotePath })
            .then(config => setPairs(config.pairs))
            .catch(() => setPairs([]))
            .finally(() => setLoading(false));
    }, [isOpen, localPath, remotePath]);

    const handleAdd = async () => {
        if (pairs.length >= MAX_PATH_PAIRS) return;
        const newPair: PathPair = {
            id: crypto.randomUUID?.() || `${Date.now()}-${Math.random().toString(36).slice(2, 9)}`,
            name: `Path ${pairs.length + 1}`,
            local_path: localPath,
            remote_path: remotePath,
            enabled: true,
            exclude_overrides: [],
        };
        try {
            await invoke('add_path_pair', {
                localPath, remotePath, pair: newPair,
            });
            setPairs(prev => [...prev, newPair]);
        } catch (e) { logger.error('[MultiPathEditor] add_path_pair failed:', e); }
    };

    const handleRemove = async (pairId: string) => {
        try {
            await invoke('remove_path_pair', {
                localPath, remotePath, pairId,
            });
            setPairs(prev => prev.filter(p => p.id !== pairId));
        } catch (e) { logger.error('[MultiPathEditor] remove_path_pair failed:', e); }
    };

    const handleToggle = async (pairId: string) => {
        const previous = pairs;
        const updated = pairs.map(p =>
            p.id === pairId ? { ...p, enabled: !p.enabled } : p
        );
        setPairs(updated);
        try {
            await invoke('save_multi_path_config_cmd', {
                localPath, remotePath,
                config: { pairs: updated, parallel_pairs: false },
            });
        } catch (e) {
            logger.error('[MultiPathEditor] toggle save failed, rolling back:', e);
            setPairs(previous);
        }
    };

    const handleNameChange = (pairId: string, name: string) => {
        setPairs(prev => prev.map(p => p.id === pairId ? { ...p, name } : p));
    };

    const handlePathChange = (pairId: string, field: 'local_path' | 'remote_path', value: string) => {
        setPairs(prev => prev.map(p => p.id === pairId ? { ...p, [field]: value } : p));
    };

    const handleSave = async () => {
        try {
            await invoke('save_multi_path_config_cmd', {
                localPath, remotePath,
                config: { pairs, parallel_pairs: false },
            });
            onClose();
        } catch (e) { logger.error('[MultiPathEditor] save failed:', e); }
    };

    if (!isOpen) return null;

    return (
        <div className="fixed inset-0 bg-black/60 z-[9999] flex items-center justify-center p-4" onClick={onClose} role="dialog" aria-modal="true" aria-label="Multi-Path Editor">
            <div
                className="bg-white dark:bg-gray-800 rounded-xl shadow-2xl w-full max-w-2xl max-h-[80vh] flex flex-col animate-scale-in"
                onClick={e => e.stopPropagation()}
            >
                {/* Header */}
                <div className="flex items-center justify-between px-5 py-4 border-b border-gray-200 dark:border-gray-700">
                    <div className="flex items-center gap-2">
                        <FolderTree size={18} className="text-blue-500" />
                        <h3 className="font-semibold text-sm">{t('syncPanel.multiPath')}</h3>
                    </div>
                    <button onClick={onClose} className="text-gray-400 hover:text-gray-200">
                        <X size={18} />
                    </button>
                </div>

                {/* Content */}
                <div className="flex-1 overflow-y-auto px-5 py-4 space-y-3">
                    {loading ? (
                        <div className="text-center text-gray-400 py-8 text-sm">{t('common.loading')}</div>
                    ) : pairs.length === 0 ? (
                        <div className="text-center text-gray-400 py-8 text-sm">
                            {t('syncPanel.multiPathEmpty')}
                        </div>
                    ) : (
                        pairs.map(pair => (
                            <div
                                key={pair.id}
                                className={`p-3 rounded-lg border ${pair.enabled ? 'border-gray-300 dark:border-gray-600' : 'border-gray-200 dark:border-gray-700 opacity-50'}`}
                            >
                                <div className="flex items-center justify-between mb-2">
                                    <input
                                        className="text-xs font-medium bg-transparent border-b border-transparent hover:border-gray-400 focus:border-blue-500 outline-none px-1 py-0.5"
                                        value={pair.name}
                                        onChange={e => handleNameChange(pair.id, e.target.value)}
                                        spellCheck={false}
                                    />
                                    <div className="flex items-center gap-2">
                                        <button
                                            onClick={() => handleToggle(pair.id)}
                                            className="text-gray-400 hover:text-gray-200"
                                            title={pair.enabled ? 'Disable' : 'Enable'}
                                        >
                                            {pair.enabled
                                                ? <ToggleRight size={18} className="text-green-500" />
                                                : <ToggleLeft size={18} />}
                                        </button>
                                        <button
                                            onClick={() => handleRemove(pair.id)}
                                            className="text-gray-400 hover:text-red-400"
                                        >
                                            <Trash2 size={14} />
                                        </button>
                                    </div>
                                </div>
                                <div className="space-y-1">
                                    <div className="flex items-center gap-2">
                                        <Folder size={12} className="text-gray-400 flex-shrink-0" />
                                        <input
                                            className="flex-1 text-xs bg-gray-100 dark:bg-gray-700 rounded px-2 py-1"
                                            value={pair.local_path}
                                            onChange={e => handlePathChange(pair.id, 'local_path', e.target.value)}
                                            placeholder="Local path"
                                            spellCheck={false}
                                        />
                                    </div>
                                    <div className="flex items-center gap-2">
                                        <Globe size={12} className="text-gray-400 flex-shrink-0" />
                                        <input
                                            className="flex-1 text-xs bg-gray-100 dark:bg-gray-700 rounded px-2 py-1"
                                            value={pair.remote_path}
                                            onChange={e => handlePathChange(pair.id, 'remote_path', e.target.value)}
                                            placeholder="Remote path"
                                            spellCheck={false}
                                        />
                                    </div>
                                </div>
                            </div>
                        ))
                    )}
                </div>

                {/* Footer */}
                <div className="flex items-center justify-between px-5 py-3 border-t border-gray-200 dark:border-gray-700">
                    <button
                        className="flex items-center gap-1 text-xs px-3 py-1.5 rounded-lg bg-blue-500/15 text-blue-400 border border-blue-500/25 hover:bg-blue-500/25 disabled:opacity-40"
                        onClick={handleAdd}
                        disabled={pairs.length >= MAX_PATH_PAIRS}
                    >
                        <Plus size={12} /> {t('syncPanel.multiPathAdd')}
                    </button>
                    <div className="flex items-center gap-2">
                        <button
                            className="text-xs px-4 py-1.5 rounded-lg bg-gray-200 dark:bg-gray-700 hover:bg-gray-300 dark:hover:bg-gray-600"
                            onClick={onClose}
                        >
                            {t('common.cancel')}
                        </button>
                        <button
                            className="text-xs px-4 py-1.5 rounded-lg bg-blue-500 text-white hover:bg-blue-600"
                            onClick={handleSave}
                        >
                            {t('common.save')}
                        </button>
                    </div>
                </div>
            </div>
        </div>
    );
};
