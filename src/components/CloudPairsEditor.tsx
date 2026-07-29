// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * CloudPairsEditor: CRUD dialog for AeroCloud sync pairs (cloud_pairs.json store).
 * Adapted from MultiPathEditor pattern but for full per-pair AeroCloud fields.
 * Separate from AeroSync's MultiPathEditor / PathPair (do not touch AeroSync code).
 */

import React, { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import {
    X, Plus, Trash2, Folder, Globe, ToggleLeft, ToggleRight, Server, Edit2,
    ArrowLeftRight, ArrowRight, ArrowLeft, Shield, Shrink
} from 'lucide-react';
import { CloudPathPair, CloudPairsConfig, CloudSyncDirection } from '../types';
import { useTranslation } from '../i18n';
import { logger } from '../utils/logger';
import { loadSavedServerProfiles } from '../utils/serverProfileStore';
import { pickFile } from '../utils/pickPath';

const MAX_CLOUD_PAIRS = 32;

interface CloudPairsEditorProps {
    isOpen: boolean;
    onClose: () => void;
}

type VersioningSelectValue =
    | 'disabled'
    | 'trash_can_30'
    | 'trash_can_7'
    | 'trash_can_90'
    | 'simple_5'
    | 'staggered';

function toVersioningSelectValue(strategy: any): VersioningSelectValue {
    if (!strategy || strategy.type === 'disabled') return 'disabled';
    if (strategy.type === 'simple') return 'simple_5';
    if (strategy.type === 'staggered') return 'staggered';
    if (strategy.type === 'trash_can') {
        const d = strategy.max_age_days ?? 30;
        if (d === 7) return 'trash_can_7';
        if (d === 90) return 'trash_can_90';
        return 'trash_can_30';
    }
    return 'trash_can_30';
}

function fromVersioningSelectValue(v: VersioningSelectValue): any {
    switch (v) {
        case 'disabled': return { type: 'disabled' };
        case 'simple_5': return { type: 'simple', max_copies: 5 };
        case 'staggered': return { type: 'staggered' };
        case 'trash_can_7': return { type: 'trash_can', max_age_days: 7 };
        case 'trash_can_90': return { type: 'trash_can', max_age_days: 90 };
        default: return { type: 'trash_can', max_age_days: 30 };
    }
}

function normalizeCompressLevel(value: unknown): number {
    const n = Number(value);
    if (!Number.isFinite(n)) return 3;
    return Math.min(22, Math.max(1, Math.round(n)));
}

function normalizePair(pair: CloudPathPair): CloudPathPair {
    return {
        ...pair,
        compress_enabled: pair.compress_enabled === true,
        compress_level: normalizeCompressLevel(pair.compress_level ?? 3),
    };
}

export const CloudPairsEditor: React.FC<CloudPairsEditorProps> = ({ isOpen, onClose }) => {
    const t = useTranslation();
    const [pairs, setPairs] = useState<CloudPathPair[]>([]);
    const [loading, setLoading] = useState(true);
    const [saving, setSaving] = useState(false);
    const [savedServers, setSavedServers] = useState<any[]>([]);
    const [legacyConfig, setLegacyConfig] = useState<any>(null);

    useEffect(() => {
        if (!isOpen) return;
        const handler = (e: KeyboardEvent) => { if (e.key === 'Escape') onClose(); };
        window.addEventListener('keydown', handler);
        return () => window.removeEventListener('keydown', handler);
    }, [isOpen, onClose]);

    useEffect(() => {
        if (!isOpen) return;
        setLoading(true);

        (async () => {
            try {
                // Load pairs
                const cfg = await invoke<CloudPairsConfig>('get_cloud_pairs_config').catch(() => ({ pairs: [], parallel_pairs: false } as CloudPairsConfig));
                setPairs((cfg.pairs || []).map(normalizePair));

                // Load saved servers for profile picker
                const servers = await loadSavedServerProfiles().catch(() => []);
                setSavedServers(servers || []);

                // Load legacy for possible import
                const leg = await invoke<any>('get_cloud_config').catch(() => null);
                if (leg && leg.enabled && (cfg.pairs || []).length === 0) {
                    setLegacyConfig(leg);
                } else {
                    setLegacyConfig(null);
                }
            } catch (e) {
                logger.error('[CloudPairsEditor] load failed:', e);
                setPairs([]);
            } finally {
                setLoading(false);
            }
        })();
    }, [isOpen]);

    const handleAdd = async () => {
        if (pairs.length >= MAX_CLOUD_PAIRS) return;

        // Seed sensible defaults; prefer legacy values if present for first pair UX
        const base = legacyConfig || {};
        const newPair: CloudPathPair = {
            id: (typeof crypto !== 'undefined' && crypto.randomUUID) ? crypto.randomUUID() : `pair-${Date.now()}`,
            name: base.cloud_name || `Pair ${pairs.length + 1}`,
            local_path: base.local_folder || '',
            remote_path: base.remote_folder || '/cloud/',
            enabled: true,
            server_profile: base.server_profile || '',
            protocol_type: base.protocol_type || 'ftp',
            connection_params: base.connection_params || {},
            sync_direction: (base.sync_direction as CloudSyncDirection) || 'bidirectional',
            preserve_remote_deletes: base.preserve_remote_deletes !== false,
            compress_enabled: base.compress_enabled === true,
            compress_level: normalizeCompressLevel(base.compress_level ?? 3),
            conflict_strategy: base.conflict_strategy || 'ask_user',
            versioning_strategy: base.versioning_strategy || { type: 'trash_can', max_age_days: 30 },
            excluded_folders: base.excluded_folders || [],
            exclude_patterns: base.exclude_patterns || [],
            last_sync: null,
        };

        try {
            const updated = await invoke<CloudPairsConfig>('add_cloud_pair', { pair: newPair });
            setPairs(updated.pairs || []);
            // Clear legacy hint after first real add
            if (legacyConfig) setLegacyConfig(null);
        } catch (e) {
            logger.error('[CloudPairsEditor] add_cloud_pair failed:', e);
            // fallback local add (will save on explicit Save)
            setPairs(prev => [...prev, newPair]);
        }
    };

    const handleRemove = async (pairId: string) => {
        try {
            const updated = await invoke<CloudPairsConfig>('remove_cloud_pair', { pairId });
            setPairs(updated.pairs || []);
        } catch (e) {
            logger.error('[CloudPairsEditor] remove_cloud_pair failed:', e);
            setPairs(prev => prev.filter(p => p.id !== pairId));
        }
    };

    const handleToggle = async (pairId: string) => {
        const previous = pairs;
        const updated = pairs.map(p =>
            p.id === pairId ? { ...p, enabled: !p.enabled } : p
        );
        setPairs(updated);
        try {
            // persist immediately for toggle (like multi path)
            await invoke('save_cloud_pairs_config_cmd', {
                config: { pairs: updated, parallel_pairs: false },
            });
        } catch (e) {
            logger.error('[CloudPairsEditor] toggle save failed, rolling back:', e);
            setPairs(previous);
        }
    };

    const updatePairField = (pairId: string, field: keyof CloudPathPair, value: any) => {
        setPairs(prev => prev.map(p => p.id === pairId ? { ...p, [field]: value } : p));
    };

    const handleDirectionChange = (pairId: string, dir: CloudSyncDirection) => {
        updatePairField(pairId, 'sync_direction', dir);
    };

    const handleProfileChange = (pairId: string, profile: string) => {
        updatePairField(pairId, 'server_profile', profile);
        // FIX-1: also capture the real protocol from the saved server so the
        // pair is persisted with correct protocol_type (not default 'ftp').
        // Worker will still defensively resolve, but this makes stored data correct.
        const server = savedServers.find((s: any) => (s.name || s.host) === profile);
        if (server && server.protocol) {
            updatePairField(pairId, 'protocol_type', server.protocol);
        }
    };

    const handleVersioningChange = (pairId: string, val: VersioningSelectValue) => {
        const strat = fromVersioningSelectValue(val);
        updatePairField(pairId, 'versioning_strategy', strat);
    };

    const handleExcludeTextChange = (pairId: string, text: string) => {
        const arr = text.split('\n').map(l => l.trim()).filter(Boolean);
        updatePairField(pairId, 'exclude_patterns', arr);
    };

    const selectLocalFolder = async (pairId: string) => {
        const selected = await pickFile({ directory: true, multiple: false, title: 'Select local folder for pair' });
        if (selected) {
            updatePairField(pairId, 'local_path', selected as string);
        }
    };

    const handleSave = async () => {
        setSaving(true);
        try {
            await invoke('save_cloud_pairs_config_cmd', {
                config: { pairs, parallel_pairs: false },
            });
            onClose();
        } catch (e) {
            logger.error('[CloudPairsEditor] save failed:', e);
            // keep open so user can retry
        } finally {
            setSaving(false);
        }
    };

    const handleImportLegacy = async () => {
        if (!legacyConfig) return;
        const imported: CloudPathPair = {
            id: (typeof crypto !== 'undefined' && crypto.randomUUID) ? crypto.randomUUID() : `legacy-${Date.now()}`,
            name: legacyConfig.cloud_name || 'Primary (imported)',
            local_path: legacyConfig.local_folder || '',
            remote_path: legacyConfig.remote_folder || '/',
            enabled: true,
            server_profile: legacyConfig.server_profile || '',
            protocol_type: legacyConfig.protocol_type || 'ftp',
            connection_params: legacyConfig.connection_params || {},
            sync_direction: (legacyConfig.sync_direction as CloudSyncDirection) || 'bidirectional',
            preserve_remote_deletes: legacyConfig.preserve_remote_deletes !== false,
            compress_enabled: legacyConfig.compress_enabled === true,
            compress_level: normalizeCompressLevel(legacyConfig.compress_level ?? 3),
            conflict_strategy: legacyConfig.conflict_strategy || 'ask_user',
            versioning_strategy: legacyConfig.versioning_strategy || { type: 'trash_can', max_age_days: 30 },
            excluded_folders: legacyConfig.excluded_folders || [],
            exclude_patterns: legacyConfig.exclude_patterns || [],
            last_sync: legacyConfig.last_sync || null,
        };
        try {
            const updated = await invoke<CloudPairsConfig>('add_cloud_pair', { pair: imported });
            setPairs(updated.pairs || []);
            setLegacyConfig(null);
        } catch (e) {
            logger.error('[CloudPairsEditor] import legacy failed:', e);
            setPairs(prev => [...prev, imported]);
            setLegacyConfig(null);
        }
    };

    if (!isOpen) return null;

    const dirLabel = (d: CloudSyncDirection) =>
        d === 'bidirectional' ? t('cloud.directionBidirectional') || 'Bidirectional' :
        d === 'local_to_remote' ? t('cloud.directionSendOnly') || 'Send only' :
        t('cloud.directionReceiveOnly') || 'Receive only';

    return (
        <div className="fixed inset-0 bg-black/60 z-[9999] flex items-center justify-center p-4" onClick={onClose} role="dialog" aria-modal="true" aria-label="AeroCloud Pairs Editor">
            <div
                className="bg-white dark:bg-gray-800 rounded-lg shadow-2xl w-full max-w-3xl max-h-[85vh] flex flex-col animate-scale-in"
                onClick={e => e.stopPropagation()}
            >
                {/* Header */}
                <div className="flex items-center justify-between px-5 py-4 border-b border-gray-200 dark:border-gray-700">
                    <div className="flex items-center gap-2">
                        <Server size={18} className="text-cyan-500" />
                        <h3 className="font-semibold text-sm">{t('cloud.pairsTitle') || 'AeroCloud Pairs'}</h3>
                        <span className="text-xs px-2 py-0.5 rounded bg-cyan-500/10 text-cyan-600 dark:text-cyan-400">{pairs.length} / {MAX_CLOUD_PAIRS}</span>
                    </div>
                    <button onClick={onClose} className="text-gray-400 hover:text-gray-200">
                        <X size={18} />
                    </button>
                </div>

                {/* Legacy import banner */}
                {legacyConfig && pairs.length === 0 && (
                    <div className="mx-5 mt-3 p-3 bg-amber-50 dark:bg-amber-900/20 border border-amber-200 dark:border-amber-700 rounded text-xs flex items-center justify-between">
                        <span>{t('cloud.importLegacyHint') || 'Import your existing single AeroCloud config as the first pair (keeps back-compat).'}</span>
                        <button
                            onClick={handleImportLegacy}
                            className="px-3 py-1 bg-amber-500 hover:bg-amber-600 text-white rounded text-xs"
                        >
                            {t('cloud.importLegacy') || 'Import current as pair'}
                        </button>
                    </div>
                )}

                {/* Content */}
                <div className="flex-1 overflow-y-auto px-5 py-4 space-y-3">
                    {loading ? (
                        <div className="text-center text-gray-400 py-8 text-sm">{t('common.loading')}</div>
                    ) : pairs.length === 0 ? (
                        <div className="text-center text-gray-400 py-8 text-sm space-y-2">
                            <div>{t('cloud.pairsEmpty') || 'No AeroCloud pairs configured yet.'}</div>
                            <div className="text-xs">{t('cloud.pairsEmptyHint') || 'Add pairs for independent syncs (each with own profile, direction, excludes).'}</div>
                        </div>
                    ) : (
                        pairs.map((pair, idx) => {
                            const vSel = toVersioningSelectValue(pair.versioning_strategy);
                            const excludeText = (pair.exclude_patterns || []).join('\n');
                            return (
                                <div
                                    key={pair.id}
                                    className={`p-3 rounded-lg border ${pair.enabled ? 'border-gray-300 dark:border-gray-600' : 'border-gray-200 dark:border-gray-700 opacity-60'}`}
                                >
                                    <div className="flex items-center justify-between mb-2">
                                        <input
                                            className="text-sm font-medium bg-transparent border-b border-transparent hover:border-gray-400 focus:border-cyan-500 outline-none px-1 py-0.5 w-64"
                                            value={pair.name}
                                            onChange={e => updatePairField(pair.id, 'name', e.target.value)}
                                            placeholder={t('cloud.pairNamePlaceholder') || 'Pair name'}
                                            spellCheck={false}
                                        />
                                        <div className="flex items-center gap-2">
                                            <button
                                                onClick={() => handleToggle(pair.id)}
                                                className="text-gray-400 hover:text-gray-200"
                                                title={pair.enabled ? t('cloud.disable') : t('cloud.enable')}
                                            >
                                                {pair.enabled
                                                    ? <ToggleRight size={18} className="text-green-500" />
                                                    : <ToggleLeft size={18} />}
                                            </button>
                                            <button
                                                onClick={() => handleRemove(pair.id)}
                                                className="text-gray-400 hover:text-red-400"
                                                title={t('common.delete') || 'Remove'}
                                            >
                                                <Trash2 size={14} />
                                            </button>
                                        </div>
                                    </div>

                                    {/* Paths */}
                                    <div className="grid grid-cols-1 md:grid-cols-2 gap-2 mb-2">
                                        <div className="flex items-center gap-2">
                                            <Folder size={14} className="text-gray-400" />
                                            <input
                                                className="flex-1 text-xs bg-gray-100 dark:bg-gray-700 rounded px-2 py-1 font-mono"
                                                value={pair.local_path}
                                                onChange={e => updatePairField(pair.id, 'local_path', e.target.value)}
                                                placeholder="/local/path"
                                                spellCheck={false}
                                            />
                                            <button onClick={() => selectLocalFolder(pair.id)} className="text-xs px-2 py-1 bg-gray-200 dark:bg-gray-600 rounded hover:bg-gray-300 dark:hover:bg-gray-500">...</button>
                                        </div>
                                        <div className="flex items-center gap-2">
                                            <Globe size={14} className="text-gray-400" />
                                            <input
                                                className="flex-1 text-xs bg-gray-100 dark:bg-gray-700 rounded px-2 py-1 font-mono"
                                                value={pair.remote_path}
                                                onChange={e => updatePairField(pair.id, 'remote_path', e.target.value)}
                                                placeholder="/remote/"
                                                spellCheck={false}
                                            />
                                        </div>
                                    </div>

                                    {/* Profile + Direction + Preserve */}
                                    <div className="grid grid-cols-1 md:grid-cols-3 gap-2 mb-2">
                                        <div>
                                            <label className="block text-[10px] text-gray-500 mb-0.5">{t('cloud.serverProfile') || 'Server Profile'}</label>
                                            <select
                                                value={pair.server_profile}
                                                onChange={e => handleProfileChange(pair.id, e.target.value)}
                                                className="w-full text-xs bg-gray-100 dark:bg-gray-700 rounded px-2 py-1"
                                            >
                                                <option value="">{t('cloud.selectServer') || 'Select saved server...'}</option>
                                                {savedServers.map(s => (
                                                    <option key={s.id || s.name} value={s.name || s.host}>{s.name || s.host}</option>
                                                ))}
                                            </select>
                                        </div>

                                        <div>
                                            <label className="block text-[10px] text-gray-500 mb-0.5">{t('cloud.syncDirection') || 'Direction'}</label>
                                            <select
                                                value={pair.sync_direction}
                                                onChange={e => handleDirectionChange(pair.id, e.target.value as CloudSyncDirection)}
                                                className="w-full text-xs bg-gray-100 dark:bg-gray-700 rounded px-2 py-1"
                                            >
                                                <option value="bidirectional">{t('cloud.directionBidirectional') || 'Send & Receive'}</option>
                                                <option value="local_to_remote">{t('cloud.directionSendOnly') || 'Send Only'}</option>
                                                <option value="remote_to_local">{t('cloud.directionReceiveOnly') || 'Receive Only'}</option>
                                            </select>
                                        </div>

                                        <div className="flex items-end">
                                            <label className="flex items-center gap-2 text-xs cursor-pointer select-none">
                                                <input
                                                    type="checkbox"
                                                    checked={!!pair.preserve_remote_deletes}
                                                    onChange={e => updatePairField(pair.id, 'preserve_remote_deletes', e.target.checked)}
                                                />
                                                <span className="inline-flex items-center gap-1">
                                                    <Shield size={12} /> {t('cloud.preserveRemoteDeletes') || 'Preserve deletes (additive)'}
                                                </span>
                                            </label>
                                        </div>
                                    </div>

                                    <div className="grid grid-cols-1 md:grid-cols-2 gap-2 mb-2">
                                        <label className="flex items-center gap-2 text-xs cursor-pointer select-none bg-gray-100 dark:bg-gray-700 rounded px-2 py-1.5">
                                            <input
                                                type="checkbox"
                                                checked={pair.compress_enabled === true}
                                                onChange={e => updatePairField(pair.id, 'compress_enabled', e.target.checked)}
                                            />
                                            <span className="inline-flex items-center gap-1 min-w-0">
                                                <Shrink size={12} /> <span className="truncate">{t('settings.aerocloudCompress') || 'AeroCompress'}</span>
                                            </span>
                                        </label>

                                        <label className="flex items-center gap-2 text-xs bg-gray-100 dark:bg-gray-700 rounded px-2 py-1.5">
                                            <span className="inline-flex items-center gap-1 text-gray-500 dark:text-gray-400 min-w-0">
                                                <Shrink size={12} /> <span className="truncate">{t('settings.aerocloudCompressLevel') || 'Compression level'}</span>
                                            </span>
                                            <input
                                                type="number"
                                                min={1}
                                                max={22}
                                                step={1}
                                                disabled={pair.compress_enabled !== true}
                                                className="w-16 text-xs bg-white dark:bg-gray-800 rounded px-2 py-1 tabular-nums disabled:opacity-50"
                                                value={normalizeCompressLevel(pair.compress_level ?? 3)}
                                                onChange={e => updatePairField(pair.id, 'compress_level', normalizeCompressLevel(e.target.value))}
                                            />
                                        </label>
                                    </div>

                                    {/* Advanced: conflict + versioning + excludes */}
                                    <div className="pt-2 border-t border-gray-200 dark:border-gray-700 grid grid-cols-1 md:grid-cols-2 gap-2">
                                        <div>
                                            <label className="block text-[10px] text-gray-500 mb-0.5">{t('cloud.conflictStrategy') || 'Conflicts'}</label>
                                            <select
                                                value={pair.conflict_strategy || 'ask_user'}
                                                onChange={e => updatePairField(pair.id, 'conflict_strategy', e.target.value)}
                                                className="w-full text-xs bg-gray-100 dark:bg-gray-700 rounded px-2 py-1"
                                            >
                                                <option value="ask_user">{t('cloud.conflictAskUser') || 'Ask me'}</option>
                                                <option value="keep_both">{t('cloud.conflictKeepBoth') || 'Keep both'}</option>
                                                <option value="prefer_local">{t('cloud.conflictPreferLocal') || 'Prefer local'}</option>
                                                <option value="prefer_remote">{t('cloud.conflictPreferRemote') || 'Prefer remote'}</option>
                                                <option value="prefer_newer">{t('cloud.conflictPreferNewer') || 'Prefer newer'}</option>
                                            </select>
                                        </div>

                                        <div>
                                            <label className="block text-[10px] text-gray-500 mb-0.5">{t('cloud.versioningStrategy') || 'Versioning'}</label>
                                            <select
                                                value={vSel}
                                                onChange={e => handleVersioningChange(pair.id, e.target.value as VersioningSelectValue)}
                                                className="w-full text-xs bg-gray-100 dark:bg-gray-700 rounded px-2 py-1"
                                            >
                                                <option value="disabled">{t('cloud.versioningDisabled') || 'Disabled'}</option>
                                                <option value="trash_can_30">{t('cloud.versioningTrashCan') || 'Trash (30d)'}</option>
                                                <option value="trash_can_7">{t('cloud.versioningTrashCan7') || 'Trash (7d)'}</option>
                                                <option value="trash_can_90">{t('cloud.versioningTrashCan90') || 'Trash (90d)'}</option>
                                                <option value="simple_5">{t('cloud.versioningSimple') || 'Simple 5'}</option>
                                                <option value="staggered">{t('cloud.versioningStaggered') || 'Staggered'}</option>
                                            </select>
                                        </div>

                                        <div className="md:col-span-2">
                                            <label className="block text-[10px] text-gray-500 mb-0.5">{t('cloud.excludePatterns') || 'Exclude patterns'} <span className="text-gray-400">(one per line)</span></label>
                                            <textarea
                                                className="w-full text-xs font-mono bg-gray-100 dark:bg-gray-700 rounded px-2 py-1 h-16"
                                                value={excludeText}
                                                onChange={e => handleExcludeTextChange(pair.id, e.target.value)}
                                                placeholder="node_modules&#10;.git&#10;*.tmp"
                                            />
                                        </div>
                                    </div>

                                    <div className="text-[10px] text-gray-400 mt-1">
                                        {pair.last_sync ? `${t('cloud.lastSync') || 'Last'}: ${new Date(pair.last_sync).toLocaleString()}` : (t('cloud.neverSynced') || 'Never synced')}
                                    </div>
                                </div>
                            );
                        })
                    )}
                </div>

                {/* Footer */}
                <div className="flex items-center justify-between px-5 py-3 border-t border-gray-200 dark:border-gray-700">
                    <button
                        className="flex items-center gap-1 text-xs px-3 py-1.5 rounded-lg bg-cyan-500/15 text-cyan-400 border border-cyan-500/25 hover:bg-cyan-500/25 disabled:opacity-40"
                        onClick={handleAdd}
                        disabled={pairs.length >= MAX_CLOUD_PAIRS}
                    >
                        <Plus size={12} /> {t('cloud.pairsAdd') || 'Add pair'}
                    </button>
                    <div className="flex items-center gap-2">
                        <button
                            className="text-xs px-4 py-1.5 rounded-lg bg-gray-200 dark:bg-gray-700 hover:bg-gray-300 dark:hover:bg-gray-600"
                            onClick={onClose}
                            disabled={saving}
                        >
                            {t('common.cancel')}
                        </button>
                        <button
                            className="text-xs px-4 py-1.5 rounded-lg bg-cyan-500 text-white hover:bg-cyan-600 disabled:opacity-50"
                            onClick={handleSave}
                            disabled={saving || loading}
                        >
                            {saving ? (t('common.saving') || 'Saving...') : t('common.save')}
                        </button>
                    </div>
                </div>
            </div>
        </div>
    );
};

export default CloudPairsEditor;
