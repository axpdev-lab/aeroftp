// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// AeroCloud first-run setup: local folder, then a saved server profile and
// the remote folder on it, then the sync schedule. AeroCloud connects only
// through saved profiles, so there is no connection form here: a user without
// a compatible profile is sent to My Servers before the wizard starts.

import * as React from 'react';
import { useEffect, useMemo, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import {
    AlertCircle, Check, ChevronRight, Clock, Cloud, Folder, FolderOpen,
    Info, Loader2, Server, ServerOff, Settings, Zap,
} from 'lucide-react';
import { ServerProfile, getServerCryptOverlay } from '../types';
import { useTranslation } from '../i18n';
import { pickFile } from '../utils/pickPath';
import { normCryptScope } from '../utils/cryptScope';
import { loadSavedServerProfilesStrict } from '../utils/serverProfileStore';
import { Checkbox } from './ui/Checkbox';
import { ProfileIcon, ProfileRail } from './common/ProfileRail';
import { AeroCloudMaturityBadge, isAeroCloudProtocol, useAeroCloudCaveat } from './AeroCloudProtocols';
import type { CloudConfig } from './CloudPanel';

const INPUT_CLASS = 'w-full px-3 py-2 text-sm rounded-lg border border-gray-300 dark:border-gray-600 bg-gray-50 dark:bg-gray-700 text-gray-900 dark:text-gray-100 placeholder-gray-400 dark:placeholder-gray-500 focus:border-cyan-500 focus:ring-2 focus:ring-cyan-500/20 focus:outline-none';
const SECONDARY_BUTTON_CLASS = 'px-4 py-2 text-sm rounded-lg border border-gray-300 dark:border-gray-600 text-gray-700 dark:text-gray-200 hover:bg-gray-100 dark:hover:bg-gray-700 transition-colors';
const PRIMARY_BUTTON_CLASS = 'px-4 py-2 text-sm rounded-lg bg-blue-500 hover:bg-blue-600 text-white transition-colors flex items-center gap-1.5 disabled:opacity-50 disabled:cursor-not-allowed';

/**
 * The root AeroCloud works in on a profile: the profile's starting folder,
 * absolute, without a trailing slash ('' for the server root), with the
 * `{username}` template some WebDAV presets keep in that path resolved.
 */
export function profileRoot(profile: ServerProfile): string {
    // Every occurrence, as the backend resolves it (`replaceAll` needs ES2021).
    const base = (profile.initialPath || '')
        .split('{username}').join(profile.username || '')
        .trim()
        .replace(/\/+$/, '');
    if (!base) return '';
    return base.startsWith('/') ? base : `/${base}`;
}

/**
 * The base AeroCloud works in on a profile: the folder its crypt overlay is
 * bound to (plaintext-absolute, as the app reads every binding) when it has
 * one, otherwise the profile's starting folder, '' for the server root. The
 * app anchors an opened overlay the same way, so AeroCloud stays inside the
 * encrypted folder.
 */
export function aeroCloudBase(profile: ServerProfile): string {
    const scope = getServerCryptOverlay(profile) ? normCryptScope(profile.aeroCryptOverlay?.remoteScope) : '';
    return scope || profileRoot(profile);
}

/**
 * The remote folder AeroCloud stores for a sub-folder of its base, as the
 * sync resolves it: a plaintext absolute path in the provider's namespace,
 * which the crypt overlay (when bound) encrypts below its anchor.
 */
export function aeroCloudRemoteFolder(profile: ServerProfile, subfolder: string): string {
    const sub = subfolder.trim().replace(/^\/+|\/+$/g, '');
    const base = aeroCloudBase(profile);
    return sub ? `${base}/${sub}/` : `${base}/`;
}

const STEPS = ['cloud.stepFolder', 'cloud.stepServer', 'cloud.stepSettings'] as const;

interface CloudSetupWizardProps {
    onComplete: (config: CloudConfig) => void;
    onCancel: () => void;
    /** Leaves AeroCloud for My Servers, offered when no profile can be used. */
    onOpenServers?: () => void;
}

export const CloudSetupWizard: React.FC<CloudSetupWizardProps> = ({ onComplete, onCancel, onOpenServers }) => {
    const t = useTranslation();
    const caveatFor = useAeroCloudCaveat();
    const [profiles, setProfiles] = useState<ServerProfile[] | null>(null);
    const [step, setStep] = useState(1);
    const [cloudName, setCloudName] = useState(t('cloud.cloudNamePlaceholder'));
    const [localFolder, setLocalFolder] = useState('');
    const [profile, setProfile] = useState<ServerProfile | null>(null);
    const [subfolder, setSubfolder] = useState('cloud');
    const [syncOnChange, setSyncOnChange] = useState(true);
    const [syncInterval, setSyncInterval] = useState(24);
    const [syncUnit, setSyncUnit] = useState<'hours' | 'minutes'>('hours');
    const [saving, setSaving] = useState(false);
    const [error, setError] = useState<string | null>(null);
    const [loadError, setLoadError] = useState<string | null>(null);

    // The strict read: a profile store it could not reach is an error to show,
    // not an empty list that would tell the user to save a server they have.
    const loadProfiles = () => {
        setProfiles(null);
        setLoadError(null);
        loadSavedServerProfilesStrict()
            .then(setProfiles)
            .catch((e) => setLoadError(String(e)));
    };

    useEffect(() => {
        invoke<string>('get_default_cloud_folder').then(setLocalFolder).catch(() => setLocalFolder(''));
        loadProfiles();
    }, []);

    const compatible = useMemo(() => (profiles ?? []).filter(p => isAeroCloudProtocol(p.protocol)), [profiles]);
    const hiddenCount = (profiles?.length ?? 0) - compatible.length;

    const selectLocalFolder = async () => {
        const selected = await pickFile({ directory: true, multiple: false, title: t('cloud.localFolder') });
        if (selected) setLocalFolder(selected as string);
    };

    const canAdvance = step === 1 ? !!localFolder.trim() : !!profile;
    const rootLabel = `${profile ? aeroCloudBase(profile) : ''}/`;
    const remoteLabel = `${rootLabel}${subfolder.trim().replace(/^\/+|\/+$/g, '')}`.replace(/\/?$/, '/');

    const handleComplete = async () => {
        if (!profile) return;
        setSaving(true);
        setError(null);
        try {
            const config = await invoke<CloudConfig>('setup_aerocloud', {
                cloudName,
                localFolder,
                remoteFolder: aeroCloudRemoteFolder(profile, subfolder),
                serverProfile: profile.name,
                serverProfileId: profile.id,
                syncOnChange,
                syncIntervalSecs: syncUnit === 'hours' ? syncInterval * 3600 : syncInterval * 60,
                protocolType: profile.protocol,
            });
            // Badge shell extension on Linux; a failure only loses the badges.
            invoke('install_shell_extension_cmd').catch(() => undefined);
            onComplete(config);
        } catch (e) {
            setError(t('cloud.setupFailed', { error: String(e) }));
        } finally {
            setSaving(false);
        }
    };

    if (loadError) {
        return (
            <div className="flex flex-col items-center text-center px-6 py-8">
                <span className="flex h-14 w-14 items-center justify-center rounded-full bg-red-500/10 text-red-500 mb-4">
                    <AlertCircle size={28} />
                </span>
                <p className="text-sm text-gray-600 dark:text-gray-300 max-w-md break-words">{t('cloud.profilesLoadFailed', { error: loadError })}</p>
                <div className="flex gap-3 mt-6">
                    <button onClick={onCancel} className={SECONDARY_BUTTON_CLASS}>{t('common.close')}</button>
                    <button onClick={loadProfiles} className={PRIMARY_BUTTON_CLASS}>{t('common.retry')}</button>
                </div>
            </div>
        );
    }

    if (profiles === null) {
        return (
            <div className="flex items-center justify-center gap-2 py-16 text-sm text-gray-500 dark:text-gray-400">
                <Loader2 className="animate-spin" size={18} /> {t('common.loading')}
            </div>
        );
    }

    if (compatible.length === 0) {
        return (
            <div className="flex flex-col items-center text-center px-6 py-8">
                <span className="flex h-14 w-14 items-center justify-center rounded-full bg-cyan-500/10 text-cyan-500 mb-4">
                    <ServerOff size={28} />
                </span>
                <h3 className="text-base font-semibold text-gray-900 dark:text-white mb-2">{t('cloud.noProfilesTitle')}</h3>
                <p className="text-sm text-gray-500 dark:text-gray-400 max-w-md">
                    {hiddenCount > 0 ? t('cloud.noCompatibleProfilesDesc') : t('cloud.noProfilesDesc')}
                </p>
                <div className="flex gap-3 mt-6">
                    <button onClick={onCancel} className={SECONDARY_BUTTON_CLASS}>{t('common.close')}</button>
                    {onOpenServers && (
                        <button onClick={onOpenServers} className={PRIMARY_BUTTON_CLASS}>
                            <Server size={16} /> {t('cloud.openMyServers')}
                        </button>
                    )}
                </div>
            </div>
        );
    }

    const caveat = caveatFor(profile?.protocol);

    return (
        <div>
            <ol className="flex items-start justify-center mb-6">
                {STEPS.map((label, i) => {
                    const n = i + 1;
                    const done = step > n;
                    const current = step === n;
                    return (
                        <li key={label} className="flex items-start">
                            {i > 0 && <span className={`mt-4 h-0.5 w-12 sm:w-16 ${step >= n ? 'bg-cyan-500' : 'bg-gray-200 dark:bg-gray-700'}`} />}
                            <button
                                type="button"
                                onClick={() => done && setStep(n)}
                                disabled={!done}
                                className="flex flex-col items-center gap-1.5 w-24 disabled:cursor-default"
                            >
                                <span className={`flex h-8 w-8 items-center justify-center rounded-full text-sm font-semibold border-2 transition-colors ${
                                    done ? 'bg-cyan-500 border-cyan-500 text-white'
                                        : current ? 'border-cyan-500 text-cyan-600 dark:text-cyan-400 bg-cyan-500/10'
                                            : 'border-gray-300 dark:border-gray-600 text-gray-400 dark:text-gray-500'
                                }`}>
                                    {done ? <Check size={16} /> : n}
                                </span>
                                <span className={`text-xs ${current ? 'text-gray-900 dark:text-white font-medium' : 'text-gray-500 dark:text-gray-400'}`}>{t(label)}</span>
                            </button>
                        </li>
                    );
                })}
            </ol>

            <div className="min-h-[280px]">
                {step === 1 && (
                    <div className="space-y-6">
                        <div>
                            <label className="flex items-center gap-2 text-sm font-semibold text-gray-900 dark:text-white mb-1"><Cloud size={16} /> {t('cloud.cloudName')}</label>
                            <p className="text-xs text-gray-500 dark:text-gray-400 mb-2">{t('cloud.cloudNameDesc')}</p>
                            <input type="text" value={cloudName} onChange={(e) => setCloudName(e.target.value)}
                                placeholder={t('cloud.cloudNamePlaceholder')} className={INPUT_CLASS} />
                        </div>
                        <div>
                            <label className="flex items-center gap-2 text-sm font-semibold text-gray-900 dark:text-white mb-1"><FolderOpen size={16} /> {t('cloud.localFolder')}</label>
                            <p className="text-xs text-gray-500 dark:text-gray-400 mb-2">{t('cloud.localFolderDesc')}</p>
                            <div className="flex items-center gap-2">
                                <div className="flex-1 px-3 py-2 text-sm font-mono rounded-lg border border-gray-300 dark:border-gray-600 bg-gray-50 dark:bg-gray-700 text-gray-700 dark:text-gray-300 truncate" title={localFolder}>
                                    {localFolder || <span className="text-gray-400 italic">{t('cloud.noFolderSelected')}</span>}
                                </div>
                                <button onClick={selectLocalFolder} className={`${SECONDARY_BUTTON_CLASS} flex items-center gap-1.5 shrink-0`}>
                                    <Folder size={16} /> {t('common.browse')}
                                </button>
                            </div>
                        </div>
                    </div>
                )}

                {step === 2 && (
                    <div className="space-y-4">
                        <p className="text-sm text-gray-500 dark:text-gray-400">{t('cloud.selectProfileDesc')}</p>
                        <ProfileRail
                            title={t('cloud.serverProfile')}
                            profiles={compatible}
                            selected={profile}
                            onSelect={setProfile}
                            emptyMessage={t('cloud.noProfilesTitle')}
                            noMatchMessage={t('cloud.noProfileMatch')}
                            searchPlaceholder={t('cloud.searchProfiles')}
                            renderBadges={(p) => <AeroCloudMaturityBadge protocol={p.protocol} />}
                            listClassName="max-h-56"
                            role="choice"
                        />
                        {hiddenCount > 0 && (
                            <p className="flex items-center gap-1.5 text-xs text-gray-500 dark:text-gray-400">
                                <Info size={13} className="shrink-0" /> {t('cloud.unsupportedProfilesHidden', { count: hiddenCount })}
                            </p>
                        )}
                        <div className={profile ? '' : 'opacity-50'}>
                            <label className="block text-sm font-medium text-gray-900 dark:text-white mb-1">{t('cloud.remoteFolder')}</label>
                            <div className="flex items-stretch font-mono text-sm">
                                <span className="flex items-center px-3 rounded-l-lg border border-r-0 border-gray-300 dark:border-gray-600 bg-gray-100 dark:bg-gray-800 text-gray-500 dark:text-gray-400 max-w-[55%] truncate" title={rootLabel}>
                                    {rootLabel}
                                </span>
                                <input type="text" value={subfolder} onChange={(e) => setSubfolder(e.target.value)} disabled={!profile}
                                    placeholder={t('cloud.remoteSubfolderPlaceholder')} aria-label={t('cloud.remoteFolder')}
                                    className={`${INPUT_CLASS} rounded-l-none font-mono min-w-0`} />
                            </div>
                            <p className="text-xs text-gray-500 dark:text-gray-400 mt-1">{t('cloud.remoteFolderDesc')}</p>
                        </div>
                        {caveat && (
                            <p className="flex items-start gap-1.5 text-xs text-gray-500 dark:text-gray-400">
                                <Info size={13} className="shrink-0 mt-0.5" /> {caveat}
                            </p>
                        )}
                    </div>
                )}

                {step === 3 && profile && (
                    <div className="space-y-4">
                        <div className="flex items-center gap-3 px-3 py-2.5 rounded-lg bg-gray-50 dark:bg-gray-700/50">
                            <Checkbox checked={syncOnChange} onChange={setSyncOnChange}
                                label={<span className="flex items-center gap-2 text-sm"><Zap size={16} /> {t('cloud.syncOnChange')}</span>} />
                        </div>
                        <div className="flex items-center gap-2 px-3 py-2.5 rounded-lg bg-gray-50 dark:bg-gray-700/50 text-sm">
                            <Clock size={16} className="text-gray-500 dark:text-gray-400" />
                            <span className="flex-1">{t('cloud.syncInterval')}</span>
                            <input type="number" min="1" max={syncUnit === 'hours' ? 168 : 1440} value={syncInterval}
                                onChange={(e) => setSyncInterval(Math.max(1, parseInt(e.target.value) || 1))}
                                className="w-20 px-2 py-1.5 text-sm text-center rounded-lg border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-800 text-gray-900 dark:text-gray-100" />
                            <select value={syncUnit} onChange={(e) => {
                                const unit = e.target.value as 'hours' | 'minutes';
                                if (unit === 'hours' && syncUnit === 'minutes') setSyncInterval(Math.max(1, Math.round(syncInterval / 60)));
                                else if (unit === 'minutes' && syncUnit === 'hours') setSyncInterval(syncInterval * 60);
                                setSyncUnit(unit);
                            }} className="px-2 py-1.5 text-sm rounded-lg border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-800 text-gray-900 dark:text-gray-100">
                                <option value="minutes">{t('settings.minutes')}</option>
                                <option value="hours">{t('cloud.hours')}</option>
                            </select>
                        </div>

                        <div className="rounded-lg border border-cyan-500/30 bg-cyan-500/5 p-4 space-y-2.5 text-sm">
                            <h4 className="flex items-center gap-2 font-semibold text-cyan-700 dark:text-cyan-400"><Settings size={15} /> {t('cloud.summary')}</h4>
                            <div className="flex items-center gap-2.5">
                                <ProfileIcon profile={profile} />
                                <div className="min-w-0">
                                    <div className="flex items-center gap-2 font-medium text-gray-900 dark:text-white">
                                        <span className="truncate">{profile.name}</span>
                                        <span className="text-xs px-1.5 py-0.5 rounded uppercase bg-gray-200 dark:bg-gray-600 text-gray-600 dark:text-gray-300">{profile.protocol}</span>
                                        <AeroCloudMaturityBadge protocol={profile.protocol} />
                                    </div>
                                    <div className="text-xs text-gray-500 dark:text-gray-400">{cloudName}</div>
                                </div>
                            </div>
                            <p className="flex items-center gap-2 text-gray-600 dark:text-gray-300"><Folder size={14} className="shrink-0" /> {t('cloud.localFolder')}: <code className="truncate text-xs">{localFolder}</code></p>
                            <p className="flex items-center gap-2 text-gray-600 dark:text-gray-300"><Server size={14} className="shrink-0" /> {t('cloud.remoteFolder')}: <code className="truncate text-xs">{remoteLabel}</code></p>
                        </div>
                        {error && (
                            <div className="flex items-start gap-2 rounded-lg bg-red-50 dark:bg-red-900/30 p-3 text-sm text-red-700 dark:text-red-300">
                                <AlertCircle size={16} className="shrink-0 mt-0.5" /> <span className="break-words min-w-0">{error}</span>
                            </div>
                        )}
                    </div>
                )}
            </div>

            <div className="flex items-center justify-between mt-6 pt-4 border-t border-gray-200 dark:border-gray-700">
                <button onClick={onCancel} className={SECONDARY_BUTTON_CLASS}>{t('common.cancel')}</button>
                <div className="flex gap-3">
                    {step > 1 && (
                        <button onClick={() => setStep(step - 1)} className={SECONDARY_BUTTON_CLASS}>{t('common.back')}</button>
                    )}
                    {step < STEPS.length ? (
                        <button onClick={() => setStep(step + 1)} disabled={!canAdvance} className={PRIMARY_BUTTON_CLASS}>
                            {t('common.next')} <ChevronRight size={16} />
                        </button>
                    ) : (
                        <button onClick={handleComplete} disabled={saving} className={PRIMARY_BUTTON_CLASS}>
                            {saving ? <Loader2 className="animate-spin" size={16} /> : <Cloud size={16} />}
                            {t('cloud.enableCloud')}
                        </button>
                    )}
                </div>
            </div>
        </div>
    );
};
