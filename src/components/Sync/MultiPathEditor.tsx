// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * MultiPathEditor: CRUD dialog for sync path pairs
 * Allows adding/removing multiple local↔remote path pairs
 */

import React, { useState, useEffect, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import {
    X, Plus, Trash2, FolderTree, Folder, Globe, ToggleLeft, ToggleRight
} from 'lucide-react';
import { PathPair, MultiPathConfig } from '../../types';
import { useTranslation } from '../../i18n';
import { logger } from '../../utils/logger';
import { useDraggableModal } from '../../hooks/useDraggableModal';

const MAX_PATH_PAIRS = 50;

interface MultiPathEditorProps {
    isOpen: boolean;
    /** Render inside an AeroSync tab instead of a separate modal. */
    embedded?: boolean;
    onClose: () => void;
    localPath: string;
    remotePath: string;
}

export const MultiPathEditor: React.FC<MultiPathEditorProps> = ({
    isOpen,
    embedded = false,
    onClose,
    localPath,
    remotePath,
}) => {
    const t = useTranslation();
    const modalDrag = useDraggableModal();
    const [pairs, setPairs] = useState<PathPair[]>([]);
    // Only backend-confirmed lists are a Cancel baseline; drafts stay separate.
    const persistedPairs = useRef<PathPair[]>([]);
    const persistenceScope = JSON.stringify([localPath, remotePath]);
    const persistedScope = useRef(persistenceScope);
    const hasConfirmedConfig = useRef(false);
    const writeInFlight = useRef(false);
    const [writing, setWriting] = useState(false);
    const [loading, setLoading] = useState(true);

    useEffect(() => {
        if (!isOpen || embedded) return;
        const handler = (e: KeyboardEvent) => { if (e.key === 'Escape') onClose(); };
        window.addEventListener('keydown', handler);
        return () => window.removeEventListener('keydown', handler);
    }, [isOpen, embedded, onClose]);

    useEffect(() => {
        if (!isOpen) return;
        let active = true;
        if (persistedScope.current !== persistenceScope) {
            persistedScope.current = persistenceScope;
            persistedPairs.current = [];
            hasConfirmedConfig.current = false;
            setPairs([]);
        }
        setLoading(true);
        invoke<MultiPathConfig>('get_multi_path_config', { localPath, remotePath })
            .then(config => {
                if (!active) return;
                persistedPairs.current = config.pairs;
                hasConfirmedConfig.current = true;
                setPairs(config.pairs);
            })
            .catch(() => { if (active) setPairs(persistedPairs.current); })
            .finally(() => { if (active) setLoading(false); });
        return () => { active = false; };
    }, [isOpen, localPath, remotePath, persistenceScope]);

    const beginWrite = () => {
        if (loading || !hasConfirmedConfig.current || writeInFlight.current) return false;
        writeInFlight.current = true;
        setWriting(true);
        return true;
    };
    const endWrite = () => {
        writeInFlight.current = false;
        setWriting(false);
    };
    const handleCancel = () => {
        if (loading || writeInFlight.current) return;
        setPairs(persistedPairs.current);
        onClose();
    };

    const handleAdd = async () => {
        if (pairs.length >= MAX_PATH_PAIRS || !beginWrite()) return;
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
            if (persistedScope.current !== persistenceScope) return;
            persistedPairs.current = [...persistedPairs.current, newPair];
            setPairs(prev => [...prev, newPair]);
        } catch (e) { logger.error('[MultiPathEditor] add_path_pair failed:', e); }
        finally { endWrite(); }
    };

    const handleRemove = async (pairId: string) => {
        if (!beginWrite()) return;
        try {
            await invoke('remove_path_pair', {
                localPath, remotePath, pairId,
            });
            if (persistedScope.current !== persistenceScope) return;
            persistedPairs.current = persistedPairs.current.filter(p => p.id !== pairId);
            setPairs(prev => prev.filter(p => p.id !== pairId));
        } catch (e) { logger.error('[MultiPathEditor] remove_path_pair failed:', e); }
        finally { endWrite(); }
    };

    const handleToggle = async (pairId: string) => {
        if (!beginWrite()) return;
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
            if (persistedScope.current !== persistenceScope) return;
            persistedPairs.current = updated;
        } catch (e) {
            logger.error('[MultiPathEditor] toggle save failed, rolling back:', e);
            if (persistedScope.current === persistenceScope) setPairs(previous);
        } finally { endWrite(); }
    };

    const handleNameChange = (pairId: string, name: string) => {
        setPairs(prev => prev.map(p => p.id === pairId ? { ...p, name } : p));
    };

    const handlePathChange = (pairId: string, field: 'local_path' | 'remote_path', value: string) => {
        setPairs(prev => prev.map(p => p.id === pairId ? { ...p, [field]: value } : p));
    };

    const handleSave = async () => {
        if (!beginWrite()) return;
        try {
            await invoke('save_multi_path_config_cmd', {
                localPath, remotePath,
                config: { pairs, parallel_pairs: false },
            });
            if (persistedScope.current !== persistenceScope) return;
            persistedPairs.current = pairs;
            onClose();
        } catch (e) { logger.error('[MultiPathEditor] save failed:', e); }
        finally { endWrite(); }
    };

    if (!isOpen) return null;

    return (
        <div className={embedded ? "relative flex flex-col" : "fixed inset-0 bg-black/60 z-[9999] flex items-center justify-center p-4"} onClick={embedded ? undefined : onClose} role={embedded ? undefined : "dialog"} aria-modal={embedded ? undefined : true} aria-label={embedded ? undefined : t('syncPanel.multiPathEditorLabel')}>
            <div
                {...(embedded ? {} : modalDrag.panelProps)}
                className={embedded ? "flex flex-1 min-h-0 flex-col" : "bg-white dark:bg-gray-800 rounded-lg shadow-2xl w-full max-w-2xl max-h-[80vh] flex flex-col animate-scale-in"}
                onClick={e => e.stopPropagation()}
            >
                {/* Header */}
                <div {...(embedded ? {} : modalDrag.dragHandleProps)} className={embedded ? "flex items-center justify-between px-5 py-4 border-b border-gray-200 dark:border-gray-700" : "flex items-center justify-between px-5 py-4 border-b border-gray-200 dark:border-gray-700 cursor-grab active:cursor-grabbing"}>
                    <div className="flex items-center gap-2">
                        <FolderTree size={18} className="text-blue-500" />
                        <h3 className="font-semibold text-sm">{t('syncPanel.multiPath')}</h3>
                    </div>
                    <button hidden={embedded} onClick={onClose} className="text-gray-400 hover:text-gray-200">
                        <X size={18} />
                    </button>
                </div>

                <fieldset disabled={loading || writing || !hasConfirmedConfig.current} className="contents">
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
                                            title={pair.enabled ? t('syncPanel.multiPathDisablePair') : t('syncPanel.multiPathEnablePair')}
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
                                            placeholder={t('syncPanel.multiPathLocalPlaceholder')}
                                            spellCheck={false}
                                        />
                                    </div>
                                    <div className="flex items-center gap-2">
                                        <Globe size={12} className="text-gray-400 flex-shrink-0" />
                                        <input
                                            className="flex-1 text-xs bg-gray-100 dark:bg-gray-700 rounded px-2 py-1"
                                            value={pair.remote_path}
                                            onChange={e => handlePathChange(pair.id, 'remote_path', e.target.value)}
                                            placeholder={t('syncPanel.multiPathRemotePlaceholder')}
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
                            onClick={handleCancel}
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
                </fieldset>
            </div>
        </div>
    );
};
