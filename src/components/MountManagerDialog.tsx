// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import {
    X,
    HardDrive,
    Play,
    Square as StopIcon,
    FolderOpen,
    Trash2,
    Plus,
    RefreshCw,
    Lock,
    Save,
    Edit3,
    AlertTriangle,
    Loader2,
} from 'lucide-react';
import { useTranslation } from '../i18n';
import { loadSavedServerProfiles } from '../utils/serverProfileStore';
import type { ServerProfile } from '../types';

interface MountConfig {
    id: string;
    name: string;
    profile: string;
    remote_path: string;
    mountpoint: string;
    read_only: boolean;
    cache_ttl: number;
    allow_other: boolean;
    auto_start: boolean;
    created_at: string;
}

interface MountStatus {
    id: string;
    state: 'stopped' | 'starting' | 'running' | 'failed' | 'unmounting';
    pid: number | null;
    started_at: string | null;
    error: string | null;
}

interface MountListResponse {
    storage_mode: 'sidecar' | 'vault';
    mounts: { config: MountConfig; status: MountStatus }[];
}

interface MountManagerDialogProps {
    onClose: () => void;
}

const newConfigDraft = (): MountConfig => ({
    id: '',
    name: '',
    profile: '',
    remote_path: '/',
    mountpoint: '',
    read_only: false,
    cache_ttl: 30,
    allow_other: false,
    auto_start: false,
    created_at: '',
});

const isWindows = (): boolean =>
    typeof navigator !== 'undefined' && /win/i.test(navigator.platform);

export function MountManagerDialog({ onClose }: MountManagerDialogProps) {
    const t = useTranslation();
    const [storageMode, setStorageMode] = useState<'sidecar' | 'vault'>('sidecar');
    const [rows, setRows] = useState<{ config: MountConfig; status: MountStatus }[]>([]);
    const [profiles, setProfiles] = useState<ServerProfile[]>([]);
    const [loading, setLoading] = useState(true);
    const [error, setError] = useState<string | null>(null);
    const [busy, setBusy] = useState<string | null>(null); // mount id of in-flight action
    const [draft, setDraft] = useState<MountConfig | null>(null);
    const [draftSaving, setDraftSaving] = useState(false);
    const [autostartBlocked, setAutostartBlocked] = useState<string | null>(null);

    const refresh = useCallback(async () => {
        setLoading(true);
        setError(null);
        try {
            const result = await invoke<MountListResponse>('mount_list');
            setStorageMode(result.storage_mode);
            setRows(result.mounts);
        } catch (e) {
            setError(String(e));
        } finally {
            setLoading(false);
        }
    }, []);

    useEffect(() => {
        refresh();
        loadSavedServerProfiles().then(setProfiles).catch(() => setProfiles([]));
        invoke<string | null>('mount_autostart_blocked')
            .then(reason => setAutostartBlocked(reason))
            .catch(() => setAutostartBlocked(null));
    }, [refresh]);

    useEffect(() => {
        const onKey = (e: KeyboardEvent) => {
            if (e.key === 'Escape' && !draft) onClose();
        };
        window.addEventListener('keydown', onKey);
        return () => window.removeEventListener('keydown', onKey);
    }, [onClose, draft]);

    const handleStart = async (id: string) => {
        setBusy(id);
        try {
            await invoke('mount_start', { id });
            await refresh();
        } catch (e) {
            setError(String(e));
        } finally {
            setBusy(null);
        }
    };

    const handleStop = async (id: string) => {
        setBusy(id);
        try {
            await invoke('mount_stop', { id });
            await refresh();
        } catch (e) {
            setError(String(e));
        } finally {
            setBusy(null);
        }
    };

    const handleOpen = async (id: string) => {
        setBusy(id);
        try {
            await invoke('mount_open_in_explorer', { id });
        } catch (e) {
            setError(String(e));
        } finally {
            setBusy(null);
        }
    };

    const handleDelete = async (id: string, name: string) => {
        if (!confirm(t('mountManager.confirmDelete', { name }))) return;
        setBusy(id);
        try {
            // Best-effort autostart cleanup; do not block deletion if the OS
            // entry is missing or the helper command fails.
            try {
                await invoke('mount_uninstall_autostart', { id });
            } catch {
                // ignore
            }
            await invoke('mount_delete_config', { id });
            await refresh();
        } catch (e) {
            setError(String(e));
        } finally {
            setBusy(null);
        }
    };

    const startNewDraft = async () => {
        const next = newConfigDraft();
        if (profiles[0]) {
            next.profile = profiles[0].name;
            try {
                next.mountpoint = await invoke<string>('mount_suggest_path', { profile: next.profile });
            } catch {
                // fallback handled in form
            }
        }
        setDraft(next);
    };

    const startEditDraft = (cfg: MountConfig) => {
        setDraft({ ...cfg });
    };

    const saveDraft = async () => {
        if (!draft) return;
        setDraftSaving(true);
        try {
            const saved = await invoke<MountConfig>('mount_save_config', { config: draft });
            // Sync autostart with the persisted state. If the user toggled it,
            // install or uninstall the OS-level entry. Errors here are
            // surfaced but do not block the save.
            const previous = rows.find(r => r.config.id === saved.id)?.config;
            const wasAutostart = !!previous?.auto_start;
            if (saved.auto_start && !wasAutostart) {
                try {
                    await invoke('mount_install_autostart', { id: saved.id });
                } catch (e) {
                    setError(String(e));
                }
            } else if (!saved.auto_start && wasAutostart) {
                try {
                    await invoke('mount_uninstall_autostart', { id: saved.id });
                } catch (e) {
                    setError(String(e));
                }
            }
            setDraft(null);
            await refresh();
        } catch (e) {
            setError(String(e));
        } finally {
            setDraftSaving(false);
        }
    };

    const updateDraft = (patch: Partial<MountConfig>) => {
        if (!draft) return;
        setDraft({ ...draft, ...patch });
    };

    const onProfileChange = async (profileName: string) => {
        if (!draft) return;
        const updates: Partial<MountConfig> = { profile: profileName };
        if (!draft.mountpoint || draft.mountpoint === '') {
            try {
                updates.mountpoint = await invoke<string>('mount_suggest_path', { profile: profileName });
            } catch {
                // ignore
            }
        }
        if (!draft.name) updates.name = profileName;
        updateDraft(updates);
    };

    const switchStorage = async (target: 'sidecar' | 'vault') => {
        if (target === storageMode) return;
        try {
            await invoke('mount_set_storage_mode', { mode: target });
            setStorageMode(target);
            await refresh();
        } catch (e) {
            setError(String(e));
        }
    };

    const pickDriveLetter = async () => {
        try {
            const letter = await invoke<string>('mount_pick_drive_letter');
            updateDraft({ mountpoint: letter });
        } catch (e) {
            setError(String(e));
        }
    };

    return (
        <div
            className="fixed inset-0 z-[9999] flex items-start justify-center bg-black/50 pt-[5vh]"
            onClick={() => !draft && onClose()}
        >
            <div
                className="w-full max-w-3xl mx-4 bg-white dark:bg-gray-900 rounded-lg shadow-2xl border border-gray-200 dark:border-gray-700 max-h-[90vh] flex flex-col animate-scale-in"
                onClick={e => e.stopPropagation()}
            >
                <div className="flex items-center justify-between px-4 py-3 border-b border-gray-200 dark:border-gray-700">
                    <div className="flex items-center gap-2">
                        <HardDrive size={18} className="text-sky-500" />
                        <h2 className="text-base font-semibold text-gray-900 dark:text-gray-100">
                            {t('mountManager.title')}
                        </h2>
                    </div>
                    <div className="flex items-center gap-2">
                        <div className="flex items-center text-xs bg-gray-100 dark:bg-gray-800 rounded-md p-0.5">
                            <button
                                onClick={() => switchStorage('sidecar')}
                                className={`px-2 py-1 rounded ${
                                    storageMode === 'sidecar'
                                        ? 'bg-white dark:bg-gray-700 shadow text-gray-900 dark:text-gray-100'
                                        : 'text-gray-600 dark:text-gray-400'
                                }`}
                                title={t('mountManager.storageModeSidecarHint')}
                            >
                                {t('mountManager.storageSidecar')}
                            </button>
                            <button
                                onClick={() => switchStorage('vault')}
                                className={`px-2 py-1 rounded flex items-center gap-1 ${
                                    storageMode === 'vault'
                                        ? 'bg-white dark:bg-gray-700 shadow text-gray-900 dark:text-gray-100'
                                        : 'text-gray-600 dark:text-gray-400'
                                }`}
                                title={t('mountManager.storageModeVaultHint')}
                            >
                                <Lock size={11} />
                                {t('mountManager.storageVault')}
                            </button>
                        </div>
                        <button
                            onClick={refresh}
                            className="p-1.5 hover:bg-gray-100 dark:hover:bg-gray-800 rounded"
                            title={t('common.refresh')}
                        >
                            <RefreshCw size={14} className={loading ? 'animate-spin' : ''} />
                        </button>
                        <button
                            onClick={onClose}
                            className="p-1.5 hover:bg-gray-100 dark:hover:bg-gray-800 rounded"
                        >
                            <X size={16} />
                        </button>
                    </div>
                </div>

                <div className="px-4 py-3 flex items-center justify-between border-b border-gray-200 dark:border-gray-700">
                    <p className="text-xs text-gray-600 dark:text-gray-400 max-w-md">
                        {t('mountManager.subtitle')}
                    </p>
                    <button
                        onClick={startNewDraft}
                        disabled={!!draft}
                        className="px-3 py-1.5 bg-sky-500 hover:bg-sky-600 disabled:opacity-50 text-white text-xs rounded flex items-center gap-1.5"
                    >
                        <Plus size={14} />
                        {t('mountManager.addMount')}
                    </button>
                </div>

                {error && (
                    <div className="mx-4 mt-3 p-2 bg-red-50 dark:bg-red-900/20 border border-red-200 dark:border-red-800 text-xs text-red-700 dark:text-red-300 rounded flex items-center gap-2">
                        <AlertTriangle size={13} />
                        <span>{error}</span>
                    </div>
                )}

                <div className="flex-1 overflow-y-auto px-4 py-3 space-y-2">
                    {loading ? (
                        <div className="text-center text-xs text-gray-500 dark:text-gray-400 py-8">
                            {t('common.loading')}
                        </div>
                    ) : rows.length === 0 && !draft ? (
                        <div className="text-center text-xs text-gray-500 dark:text-gray-400 py-12">
                            <HardDrive size={32} className="mx-auto mb-2 opacity-40" />
                            <p>{t('mountManager.empty')}</p>
                        </div>
                    ) : (
                        rows.map(({ config, status }) => {
                            const running = status.state === 'running';
                            return (
                                <div
                                    key={config.id}
                                    className="flex items-center gap-2 p-2.5 rounded border border-gray-200 dark:border-gray-700 bg-gray-50 dark:bg-gray-800/40"
                                >
                                    <div
                                        className={`w-2 h-2 rounded-full shrink-0 ${
                                            running ? 'bg-emerald-500' : 'bg-gray-400 dark:bg-gray-600'
                                        }`}
                                    />
                                    <div className="flex-1 min-w-0">
                                        <div className="text-sm font-medium text-gray-900 dark:text-gray-100 truncate">
                                            {config.name}
                                        </div>
                                        <div className="text-[11px] text-gray-500 dark:text-gray-400 truncate">
                                            {config.profile} {config.remote_path} {`->`} {config.mountpoint}
                                            {config.read_only && (
                                                <span className="ml-2 px-1 py-0.5 bg-gray-200 dark:bg-gray-700 rounded text-[10px]">
                                                    {t('mountManager.readOnly')}
                                                </span>
                                            )}
                                        </div>
                                    </div>
                                    <div className="flex items-center gap-1">
                                        {running ? (
                                            <>
                                                <button
                                                    onClick={() => handleOpen(config.id)}
                                                    disabled={busy === config.id}
                                                    className="p-1.5 hover:bg-gray-200 dark:hover:bg-gray-700 rounded disabled:opacity-50"
                                                    title={t('mountManager.openInExplorer')}
                                                >
                                                    <FolderOpen size={14} />
                                                </button>
                                                <button
                                                    onClick={() => handleStop(config.id)}
                                                    disabled={busy === config.id}
                                                    className="p-1.5 hover:bg-gray-200 dark:hover:bg-gray-700 rounded disabled:opacity-50"
                                                    title={t('mountManager.stop')}
                                                >
                                                    {busy === config.id ? (
                                                        <Loader2 size={14} className="animate-spin" />
                                                    ) : (
                                                        <StopIcon size={14} />
                                                    )}
                                                </button>
                                            </>
                                        ) : (
                                            <button
                                                onClick={() => handleStart(config.id)}
                                                disabled={busy === config.id}
                                                className="p-1.5 hover:bg-emerald-100 dark:hover:bg-emerald-900/30 rounded text-emerald-600 dark:text-emerald-400 disabled:opacity-50"
                                                title={t('mountManager.start')}
                                            >
                                                {busy === config.id ? (
                                                    <Loader2 size={14} className="animate-spin" />
                                                ) : (
                                                    <Play size={14} />
                                                )}
                                            </button>
                                        )}
                                        <button
                                            onClick={() => startEditDraft(config)}
                                            disabled={running || !!draft}
                                            className="p-1.5 hover:bg-gray-200 dark:hover:bg-gray-700 rounded disabled:opacity-30"
                                            title={t('common.edit')}
                                        >
                                            <Edit3 size={14} />
                                        </button>
                                        <button
                                            onClick={() => handleDelete(config.id, config.name)}
                                            disabled={running || busy === config.id}
                                            className="p-1.5 hover:bg-red-100 dark:hover:bg-red-900/30 rounded text-red-600 dark:text-red-400 disabled:opacity-30"
                                            title={t('common.delete')}
                                        >
                                            <Trash2 size={14} />
                                        </button>
                                    </div>
                                </div>
                            );
                        })
                    )}

                    {draft && (
                        <div className="mt-3 p-3 rounded border border-sky-300 dark:border-sky-700 bg-sky-50/50 dark:bg-sky-900/10 space-y-2">
                            <div className="text-xs font-medium text-sky-700 dark:text-sky-300 mb-1">
                                {draft.id ? t('mountManager.editMount') : t('mountManager.newMount')}
                            </div>
                            <div className="grid grid-cols-2 gap-2">
                                <label className="text-xs">
                                    <span className="block text-gray-600 dark:text-gray-400 mb-1">
                                        {t('mountManager.fieldName')}
                                    </span>
                                    <input
                                        type="text"
                                        value={draft.name}
                                        onChange={e => updateDraft({ name: e.target.value })}
                                        className="w-full px-2 py-1 border border-gray-300 dark:border-gray-700 bg-white dark:bg-gray-900 rounded text-xs"
                                        placeholder={draft.profile || t('mountManager.fieldNamePlaceholder')}
                                    />
                                </label>
                                <label className="text-xs">
                                    <span className="block text-gray-600 dark:text-gray-400 mb-1">
                                        {t('mountManager.fieldProfile')}
                                    </span>
                                    <select
                                        value={draft.profile}
                                        onChange={e => onProfileChange(e.target.value)}
                                        className="w-full px-2 py-1 border border-gray-300 dark:border-gray-700 bg-white dark:bg-gray-900 rounded text-xs"
                                    >
                                        <option value="">{t('mountManager.selectProfile')}</option>
                                        {profiles.map(p => (
                                            <option key={p.id} value={p.name}>
                                                {p.name}
                                            </option>
                                        ))}
                                    </select>
                                </label>
                                <label className="text-xs col-span-2">
                                    <span className="block text-gray-600 dark:text-gray-400 mb-1">
                                        {t('mountManager.fieldRemotePath')}
                                    </span>
                                    <input
                                        type="text"
                                        value={draft.remote_path}
                                        onChange={e => updateDraft({ remote_path: e.target.value })}
                                        className="w-full px-2 py-1 border border-gray-300 dark:border-gray-700 bg-white dark:bg-gray-900 rounded text-xs font-mono"
                                        placeholder="/"
                                    />
                                </label>
                                <label className="text-xs col-span-2">
                                    <span className="block text-gray-600 dark:text-gray-400 mb-1">
                                        {isWindows()
                                            ? t('mountManager.fieldDriveLetter')
                                            : t('mountManager.fieldMountpoint')}
                                    </span>
                                    <div className="flex gap-1">
                                        <input
                                            type="text"
                                            value={draft.mountpoint}
                                            onChange={e => updateDraft({ mountpoint: e.target.value })}
                                            className="flex-1 px-2 py-1 border border-gray-300 dark:border-gray-700 bg-white dark:bg-gray-900 rounded text-xs font-mono"
                                            placeholder={isWindows() ? 'Z:' : '/home/user/aeroftp-mounts/myprofile'}
                                        />
                                        {isWindows() && (
                                            <button
                                                onClick={pickDriveLetter}
                                                className="px-2 py-1 text-xs bg-gray-200 dark:bg-gray-700 hover:bg-gray-300 dark:hover:bg-gray-600 rounded"
                                            >
                                                {t('mountManager.pickDrive')}
                                            </button>
                                        )}
                                    </div>
                                </label>
                                <label className="text-xs">
                                    <span className="block text-gray-600 dark:text-gray-400 mb-1">
                                        {t('mountManager.fieldCacheTtl')}
                                    </span>
                                    <input
                                        type="number"
                                        min={1}
                                        max={3600}
                                        value={draft.cache_ttl}
                                        onChange={e => updateDraft({ cache_ttl: parseInt(e.target.value, 10) || 30 })}
                                        className="w-full px-2 py-1 border border-gray-300 dark:border-gray-700 bg-white dark:bg-gray-900 rounded text-xs"
                                    />
                                </label>
                                <div className="flex flex-col gap-1 text-xs justify-end">
                                    <label className="flex items-center gap-1.5 cursor-pointer">
                                        <input
                                            type="checkbox"
                                            checked={draft.read_only}
                                            onChange={e => updateDraft({ read_only: e.target.checked })}
                                        />
                                        {t('mountManager.fieldReadOnly')}
                                    </label>
                                    {!isWindows() && (
                                        <label className="flex items-center gap-1.5 cursor-pointer">
                                            <input
                                                type="checkbox"
                                                checked={draft.allow_other}
                                                onChange={e => updateDraft({ allow_other: e.target.checked })}
                                            />
                                            {t('mountManager.fieldAllowOther')}
                                        </label>
                                    )}
                                    <label
                                        className={`flex items-center gap-1.5 cursor-pointer ${
                                            autostartBlocked ? 'opacity-50 cursor-not-allowed' : ''
                                        }`}
                                        title={autostartBlocked ?? ''}
                                    >
                                        <input
                                            type="checkbox"
                                            disabled={!!autostartBlocked}
                                            checked={draft.auto_start && !autostartBlocked}
                                            onChange={e => updateDraft({ auto_start: e.target.checked })}
                                        />
                                        {t('mountManager.fieldAutoStart')}
                                    </label>
                                    {autostartBlocked && (
                                        <span className="text-[10px] text-amber-600 dark:text-amber-400 leading-tight">
                                            {autostartBlocked}
                                        </span>
                                    )}
                                </div>
                            </div>

                            <div className="flex justify-end gap-2 pt-1">
                                <button
                                    onClick={() => setDraft(null)}
                                    className="px-3 py-1.5 text-xs text-gray-700 dark:text-gray-300 hover:bg-gray-200 dark:hover:bg-gray-700 rounded"
                                >
                                    {t('common.cancel')}
                                </button>
                                <button
                                    onClick={saveDraft}
                                    disabled={!draft.profile || !draft.mountpoint || draftSaving}
                                    className="px-3 py-1.5 bg-sky-500 hover:bg-sky-600 disabled:opacity-50 text-white text-xs rounded flex items-center gap-1.5"
                                >
                                    {draftSaving ? <Loader2 size={12} className="animate-spin" /> : <Save size={12} />}
                                    {t('common.save')}
                                </button>
                            </div>
                        </div>
                    )}
                </div>
            </div>
        </div>
    );
}
