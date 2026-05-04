// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { useState, useEffect, useCallback, useMemo } from 'react';
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
import { PROVIDER_LOGOS } from './ProviderLogos';
import { Checkbox } from './ui/Checkbox';
import { useDraggableModal } from '../hooks/useDraggableModal';

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
    initialProfileId?: string;
    initialRemotePath?: string;
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

const PROTOCOL_COLORS: Record<string, string> = {
    ftp: 'from-blue-500 to-cyan-400',
    ftps: 'from-green-500 to-emerald-400',
    sftp: 'from-purple-500 to-violet-400',
    webdav: 'from-orange-500 to-amber-400',
    s3: 'from-amber-500 to-yellow-400',
    aerocloud: 'from-sky-400 to-blue-500',
    googledrive: 'from-red-500 to-red-400',
    dropbox: 'from-blue-600 to-blue-400',
    onedrive: 'from-sky-500 to-sky-400',
    mega: 'from-red-600 to-red-500',
    box: 'from-blue-500 to-blue-600',
    pcloud: 'from-green-500 to-teal-400',
    azure: 'from-blue-600 to-indigo-500',
    filen: 'from-emerald-500 to-green-400',
    internxt: 'from-blue-500 to-blue-400',
    kdrive: 'from-blue-500 to-sky-400',
    drime: 'from-green-500 to-emerald-400',
    filelu: 'from-sky-500 to-cyan-400',
    koofr: 'from-green-500 to-emerald-400',
    opendrive: 'from-cyan-500 to-sky-400',
    yandexdisk: 'from-yellow-500 to-amber-400',
    fourshared: 'from-blue-500 to-cyan-400',
    zohoworkdrive: 'from-yellow-500 to-orange-400',
    github: 'from-gray-700 to-gray-500',
    googlephotos: 'from-amber-500 to-amber-400',
    immich: 'from-indigo-500 to-purple-400',
};

const MountProfileIcon: React.FC<{ profile: ServerProfile }> = ({ profile }) => {
    const logoKey = profile.providerId || profile.protocol || '';
    const LogoComponent = PROVIDER_LOGOS[logoKey];
    const hasIcon = !!profile.customIconUrl || !!profile.faviconUrl || !!LogoComponent;

    return (
        <span className={`flex h-10 w-10 shrink-0 items-center justify-center rounded-xl ${
            hasIcon
                ? 'border border-gray-200 bg-[#FFFFF0] dark:border-gray-600 dark:bg-gray-700'
                : `bg-gradient-to-br ${PROTOCOL_COLORS[profile.protocol || 'ftp'] || PROTOCOL_COLORS.ftp} text-white`
        }`}>
            {profile.customIconUrl ? (
                <img src={profile.customIconUrl} alt="" className="h-6 w-6 rounded object-contain" onError={(e) => { (e.target as HTMLImageElement).style.display = 'none'; }} />
            ) : profile.faviconUrl ? (
                <img src={profile.faviconUrl} alt="" className="h-6 w-6 rounded object-contain" onError={(e) => { (e.target as HTMLImageElement).style.display = 'none'; }} />
            ) : LogoComponent ? (
                <LogoComponent size={20} />
            ) : (
                <span className="font-bold">{(profile.name || profile.host || '?').charAt(0).toUpperCase()}</span>
            )}
        </span>
    );
};

interface MountProfileCarouselProps {
    title: string;
    profiles: ServerProfile[];
    selectedName: string;
    onSelect: (profileName: string) => void;
    emptyMessage: string;
    filterPlaceholder: string;
    noMatchesMessage: string;
}

const MountProfileCarousel: React.FC<MountProfileCarouselProps> = ({ title, profiles, selectedName, onSelect, emptyMessage, filterPlaceholder, noMatchesMessage }) => {
    const [filter, setFilter] = useState('');
    const listRef = React.useRef<HTMLDivElement | null>(null);
    const normalizedFilter = filter.trim().toLowerCase();
    const visibleProfiles = useMemo(() => {
        if (!normalizedFilter) return profiles;
        return profiles.filter((profile) => {
            const haystack = [
                profile.name,
                profile.protocol,
                profile.host,
                profile.username,
                profile.initialPath,
            ].filter(Boolean).join(' ').toLowerCase();
            return haystack.includes(normalizedFilter);
        });
    }, [profiles, normalizedFilter]);
    const orderedProfiles = useMemo(() => {
        const selectedIdx = visibleProfiles.findIndex((profile) => profile.name === selectedName);
        if (selectedIdx <= 0) return visibleProfiles;
        return [visibleProfiles[selectedIdx], ...visibleProfiles.slice(0, selectedIdx), ...visibleProfiles.slice(selectedIdx + 1)];
    }, [visibleProfiles, selectedName]);

    useEffect(() => {
        listRef.current?.scrollTo({ top: 0, behavior: 'smooth' });
    }, [selectedName, normalizedFilter]);

    return (
        <div className="rounded-xl border border-gray-200 bg-white/70 shadow-sm dark:border-gray-700 dark:bg-gray-900/40">
            <div className="flex flex-wrap items-center justify-between gap-2 border-b border-gray-200 px-3 py-2 dark:border-gray-700">
                <div>
                    <span className="text-xs font-semibold uppercase tracking-wide text-gray-500 dark:text-gray-400">
                        {title}
                    </span>
                    {selectedName && (
                        <span className="ml-2 max-w-[220px] truncate align-middle text-xs text-sky-600 dark:text-sky-300">
                            {selectedName}
                        </span>
                    )}
                </div>
                <input
                    type="search"
                    value={filter}
                    onChange={(e) => setFilter(e.target.value)}
                    placeholder={filterPlaceholder}
                    className="h-9 min-w-[180px] rounded-lg border border-gray-200 bg-gray-50 px-3 text-sm text-gray-900 outline-none transition focus:ring-2 focus:ring-sky-500/30 dark:border-gray-700 dark:bg-gray-900 dark:text-gray-100"
                />
            </div>
            <div ref={listRef} className="max-h-[8.25rem] space-y-1 overflow-y-auto p-2">
                {orderedProfiles.map((profile) => {
                    const isSelected = profile.name === selectedName;
                    return (
                        <button
                            key={profile.id}
                            type="button"
                            onClick={() => onSelect(profile.name)}
                            className={`relative w-full rounded-xl border px-3 py-2 text-left transition ${
                                isSelected
                                    ? 'border-sky-400 bg-sky-50 shadow-sm ring-1 ring-sky-400 dark:border-sky-400 dark:bg-sky-900/30'
                                    : 'border-transparent hover:border-gray-200 hover:bg-gray-50 dark:hover:border-gray-700 dark:hover:bg-gray-800/70'
                            }`}
                        >
                            <div className="flex items-center gap-3">
                                <MountProfileIcon profile={profile} />
                                <div className="min-w-0 flex-1">
                                    <div className="flex items-center gap-2">
                                        <span className="truncate text-sm font-medium text-gray-900 dark:text-gray-100">{profile.name}</span>
                                        <span className="rounded bg-gray-200 px-1.5 py-0.5 text-[10px] font-semibold uppercase text-gray-600 dark:bg-gray-700 dark:text-gray-300">
                                            {profile.protocol || 'ftp'}
                                        </span>
                                    </div>
                                    <div className="mt-0.5 truncate text-xs text-gray-500 dark:text-gray-400">
                                        {profile.initialPath?.trim() || profile.host || '/'}
                                    </div>
                                </div>
                            </div>
                        </button>
                    );
                })}
                {profiles.length === 0 && (
                    <div className="w-full rounded-lg border border-dashed border-gray-300 px-3 py-4 text-center text-sm text-gray-500 dark:border-gray-600 dark:text-gray-400">
                        {emptyMessage}
                    </div>
                )}
                {profiles.length > 0 && orderedProfiles.length === 0 && (
                    <div className="w-full rounded-lg border border-dashed border-gray-300 px-3 py-4 text-center text-sm text-gray-500 dark:border-gray-600 dark:text-gray-400">
                        {noMatchesMessage}
                    </div>
                )}
            </div>
        </div>
    );
};

export function MountManagerDialog({ onClose, initialProfileId, initialRemotePath }: MountManagerDialogProps) {
    const t = useTranslation();
    const modalDrag = useDraggableModal();
    const [storageMode, setStorageMode] = useState<'sidecar' | 'vault'>('sidecar');
    const [rows, setRows] = useState<{ config: MountConfig; status: MountStatus }[]>([]);
    const [profiles, setProfiles] = useState<ServerProfile[]>([]);
    const [loading, setLoading] = useState(true);
    const [error, setError] = useState<string | null>(null);
    const [busy, setBusy] = useState<string | null>(null); // mount id of in-flight action
    const [draft, setDraft] = useState<MountConfig | null>(null);
    const [draftSaving, setDraftSaving] = useState(false);
    const [autostartBlocked, setAutostartBlocked] = useState<string | null>(null);
    const initialDraftAppliedRef = React.useRef(false);

    const makeDraftForProfile = useCallback(async (profile: ServerProfile | undefined, remotePath?: string) => {
        const next = newConfigDraft();
        if (profile) {
            next.name = profile.name;
            next.profile = profile.name;
            next.remote_path = remotePath?.trim() || profile.initialPath?.trim() || '/';
            try {
                next.mountpoint = await invoke<string>('mount_suggest_path', { profile: profile.name });
            } catch {
                // fallback handled in form
            }
        }
        return next;
    }, []);

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
        loadSavedServerProfiles()
            .then(async (loaded) => {
                setProfiles(loaded);
                if (initialProfileId && !initialDraftAppliedRef.current) {
                    const match = loaded.find((profile) => profile.id === initialProfileId);
                    if (match) {
                        initialDraftAppliedRef.current = true;
                        setDraft(await makeDraftForProfile(match, initialRemotePath));
                    }
                }
            })
            .catch(() => setProfiles([]));
        invoke<string | null>('mount_autostart_blocked')
            .then(reason => setAutostartBlocked(reason))
            .catch(() => setAutostartBlocked(null));
    }, [refresh, initialProfileId, initialRemotePath, makeDraftForProfile]);

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
        setDraft(await makeDraftForProfile(profiles[0]));
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
        const selectedProfile = profiles.find(profile => profile.name === profileName);
        const updates: Partial<MountConfig> = {
            profile: profileName,
            name: profileName,
            remote_path: selectedProfile?.initialPath?.trim() || '/',
        };

        try {
            updates.mountpoint = await invoke<string>('mount_suggest_path', { profile: profileName });
        } catch {
            // Keep the field editable if the backend cannot suggest a platform-specific path.
        }
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
            className="fixed inset-0 z-[9999] flex items-start justify-center bg-black/60 px-4 pt-[5vh] backdrop-blur-sm"
            onClick={() => !draft && onClose()}
        >
            <div
                {...modalDrag.panelProps}
                className="w-full max-w-3xl overflow-hidden rounded-xl border border-gray-200 bg-white shadow-2xl dark:border-gray-700 dark:bg-gray-800 max-h-[90vh] flex flex-col animate-scale-in"
                onClick={e => e.stopPropagation()}
            >
                <div
                    {...modalDrag.dragHandleProps}
                    className="flex cursor-grab items-center justify-between border-b border-gray-200 bg-white/90 px-4 py-3 active:cursor-grabbing dark:border-gray-700 dark:bg-gray-800/90"
                >
                    <div className="flex items-center gap-3 pointer-events-none">
                        <div className="flex h-9 w-9 items-center justify-center rounded-xl bg-sky-100 text-sky-600 dark:bg-sky-900/40 dark:text-sky-300">
                            <HardDrive size={18} />
                        </div>
                        <div>
                            <h2 className="text-base font-semibold text-gray-900 dark:text-gray-100">
                                {t('mountManager.title')}
                            </h2>
                            <p className="text-xs text-gray-500 dark:text-gray-400">
                                {storageMode === 'vault' ? t('mountManager.storageVault') : t('mountManager.storageSidecar')}
                            </p>
                        </div>
                    </div>
                    <div className="flex items-center gap-2">
                        <div className="flex items-center rounded-lg bg-gray-100 p-0.5 text-xs dark:bg-gray-900/70">
                            <button
                                onClick={() => switchStorage('sidecar')}
                                className={`px-2 py-1 rounded ${
                                    storageMode === 'sidecar'
                                        ? 'bg-white text-gray-900 shadow dark:bg-gray-700 dark:text-gray-100'
                                        : 'text-gray-600 hover:text-gray-900 dark:text-gray-400 dark:hover:text-gray-200'
                                }`}
                                title={t('mountManager.storageModeSidecarHint')}
                            >
                                {t('mountManager.storageSidecar')}
                            </button>
                            <button
                                onClick={() => switchStorage('vault')}
                                className={`px-2 py-1 rounded flex items-center gap-1 ${
                                    storageMode === 'vault'
                                        ? 'bg-white text-gray-900 shadow dark:bg-gray-700 dark:text-gray-100'
                                        : 'text-gray-600 hover:text-gray-900 dark:text-gray-400 dark:hover:text-gray-200'
                                }`}
                                title={t('mountManager.storageModeVaultHint')}
                            >
                                <Lock size={11} />
                                {t('mountManager.storageVault')}
                            </button>
                        </div>
                        <button
                            onClick={refresh}
                            className="rounded-lg p-1.5 transition hover:bg-gray-100 dark:hover:bg-gray-700"
                            title={t('common.refresh')}
                        >
                            <RefreshCw size={14} className={loading ? 'animate-spin' : ''} />
                        </button>
                        <button
                            onClick={onClose}
                            className="rounded-lg p-1.5 transition hover:bg-gray-100 dark:hover:bg-gray-700"
                        >
                            <X size={16} />
                        </button>
                    </div>
                </div>

                <div className="px-4 py-3 flex items-center justify-between border-b border-gray-200 bg-gray-50/70 dark:border-gray-700 dark:bg-gray-900/30">
                    <p className="text-xs text-gray-600 dark:text-gray-400 max-w-md">
                        {t('mountManager.subtitle')}
                    </p>
                    <button
                        onClick={startNewDraft}
                        disabled={!!draft}
                        className="flex items-center gap-1.5 rounded-lg bg-sky-500 px-4 py-2 text-sm font-medium text-white shadow-sm transition hover:bg-sky-600 disabled:opacity-50"
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
                                    className="flex items-center gap-2 rounded-xl border border-gray-200 bg-gray-50 p-2.5 dark:border-gray-700 dark:bg-gray-900/35"
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
                                                    className="rounded-lg p-1.5 transition hover:bg-gray-200 disabled:opacity-50 dark:hover:bg-gray-700"
                                                    title={t('mountManager.openInExplorer')}
                                                >
                                                    <FolderOpen size={14} />
                                                </button>
                                                <button
                                                    onClick={() => handleStop(config.id)}
                                                    disabled={busy === config.id}
                                                    className="rounded-lg p-1.5 transition hover:bg-gray-200 disabled:opacity-50 dark:hover:bg-gray-700"
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
                                                className="rounded-lg p-1.5 text-emerald-600 transition hover:bg-emerald-100 disabled:opacity-50 dark:text-emerald-400 dark:hover:bg-emerald-900/30"
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
                                            className="rounded-lg p-1.5 transition hover:bg-gray-200 disabled:opacity-30 dark:hover:bg-gray-700"
                                            title={t('common.edit')}
                                        >
                                            <Edit3 size={14} />
                                        </button>
                                        <button
                                            onClick={() => handleDelete(config.id, config.name)}
                                            disabled={running || busy === config.id}
                                            className="rounded-lg p-1.5 text-red-600 transition hover:bg-red-100 disabled:opacity-30 dark:text-red-400 dark:hover:bg-red-900/30"
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
                        <div className="mt-3 space-y-3">
                            <MountProfileCarousel
                                title={draft.id ? t('mountManager.editMount') : t('mountManager.newMount')}
                                profiles={profiles}
                                selectedName={draft.profile}
                                onSelect={onProfileChange}
                                emptyMessage={t('mountManager.selectProfile')}
                                filterPlaceholder={t('mountManager.filterProfiles')}
                                noMatchesMessage={t('mountManager.noProfileMatches')}
                            />
                            <div className="grid grid-cols-2 gap-3">
                                <label className="text-xs">
                                    <span className="block text-gray-600 dark:text-gray-400 mb-1">
                                        {t('mountManager.fieldName')}
                                    </span>
                                    <input
                                        type="text"
                                        value={draft.name}
                                        onChange={e => updateDraft({ name: e.target.value })}
                                        className="h-9 w-full rounded-lg border border-gray-200 bg-gray-50 px-3 text-sm text-gray-900 outline-none transition focus:ring-2 focus:ring-sky-500/30 dark:border-gray-700 dark:bg-gray-900 dark:text-gray-100"
                                        placeholder={draft.profile || t('mountManager.fieldNamePlaceholder')}
                                    />
                                </label>
                                <div className="hidden md:block" aria-hidden="true" />
                                <label className="text-xs col-span-2">
                                    <span className="block text-gray-600 dark:text-gray-400 mb-1">
                                        {t('mountManager.fieldRemotePath')}
                                    </span>
                                    <input
                                        type="text"
                                        value={draft.remote_path}
                                        onChange={e => updateDraft({ remote_path: e.target.value })}
                                        className="h-9 w-full rounded-lg border border-gray-200 bg-gray-50 px-3 font-mono text-sm text-gray-900 outline-none transition focus:ring-2 focus:ring-sky-500/30 dark:border-gray-700 dark:bg-gray-900 dark:text-gray-100"
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
                                            className="h-9 flex-1 rounded-lg border border-gray-200 bg-gray-50 px-3 font-mono text-sm text-gray-900 outline-none transition focus:ring-2 focus:ring-sky-500/30 dark:border-gray-700 dark:bg-gray-900 dark:text-gray-100"
                                            placeholder={isWindows() ? 'Z:' : '/home/user/aeroftp-mounts/myprofile'}
                                        />
                                        {isWindows() && (
                                            <button
                                                onClick={pickDriveLetter}
                                                className="h-9 rounded-lg bg-gray-100 px-4 text-sm font-medium transition hover:bg-gray-200 dark:bg-gray-700 dark:hover:bg-gray-600"
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
                                        className="h-9 w-full rounded-lg border border-gray-200 bg-gray-50 px-3 text-sm text-gray-900 outline-none transition focus:ring-2 focus:ring-sky-500/30 dark:border-gray-700 dark:bg-gray-900 dark:text-gray-100"
                                    />
                                </label>
                                <div className="flex flex-col gap-1 text-xs justify-end">
                                    <Checkbox
                                        checked={draft.read_only}
                                        onChange={(checked) => updateDraft({ read_only: checked })}
                                        label={<span className="text-xs text-gray-700 dark:text-gray-300">{t('mountManager.fieldReadOnly')}</span>}
                                    />
                                    {!isWindows() && (
                                        <Checkbox
                                            checked={draft.allow_other}
                                            onChange={(checked) => updateDraft({ allow_other: checked })}
                                            label={<span className="text-xs text-gray-700 dark:text-gray-300">{t('mountManager.fieldAllowOther')}</span>}
                                        />
                                    )}
                                    <div className={autostartBlocked ? 'opacity-50' : ''} title={autostartBlocked ?? ''}>
                                        <Checkbox
                                            checked={draft.auto_start && !autostartBlocked}
                                            disabled={!!autostartBlocked}
                                            onChange={(checked) => updateDraft({ auto_start: checked })}
                                            label={<span className="text-xs text-gray-700 dark:text-gray-300">{t('mountManager.fieldAutoStart')}</span>}
                                        />
                                    </div>
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
                                    className="rounded-lg bg-gray-100 px-4 py-2 text-sm font-medium text-gray-700 transition hover:bg-gray-200 dark:bg-gray-700 dark:text-gray-300 dark:hover:bg-gray-600"
                                >
                                    {t('common.cancel')}
                                </button>
                                <button
                                    onClick={saveDraft}
                                    disabled={!draft.profile || !draft.mountpoint || draftSaving}
                                    className="flex items-center gap-1.5 rounded-lg bg-sky-500 px-4 py-2 text-sm font-medium text-white shadow-sm transition hover:bg-sky-600 disabled:opacity-50"
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
