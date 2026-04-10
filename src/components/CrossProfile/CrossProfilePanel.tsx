// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { useEffect, useMemo, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { ArrowLeftRight, ArrowRight, ArrowRightLeft, Play, RefreshCw, X } from 'lucide-react';
import { ServerProfile } from '../../types';
import { useTranslation } from '../../i18n';
import { secureGetWithFallback } from '../../utils/secureStorage';
import { formatSize } from '../../utils/formatters';
import { PROVIDER_LOGOS } from '../ProviderLogos';
import { Checkbox } from '../ui/Checkbox';
import { TransferActionBar } from './TransferActionBar';

const getDefaultRemotePath = (profile: ServerProfile | null): string =>
    profile?.initialPath?.trim() ? profile.initialPath.trim() : '/';

interface CrossProfilePlanEntry {
    source_path: string;
    dest_path: string;
    display_name: string;
    size: number;
    is_dir: boolean;
}

interface CrossProfilePlan {
    plan_id: string;
    source_profile_id: string;
    dest_profile_id: string;
    source_profile: string;
    dest_profile: string;
    entries: CrossProfilePlanEntry[];
    total_files: number;
    total_bytes: number;
}

interface TransferSummary {
    transfer_id: string;
    planned_files: number;
    transferred_files: number;
    skipped_files: number;
    failed_files: number;
    total_bytes: number;
    duration_ms: number;
}

interface CrossProfilePanelProps {
    onClose: () => void;
}

interface ProfileRailProps {
    title: string;
    profiles: ServerProfile[];
    selected: ServerProfile | null;
    onSelect: (profile: ServerProfile) => void;
    emptyMessage: string;
}

const ProfileIcon: React.FC<{ profile: ServerProfile }> = ({ profile }) => {
    const logoKey = profile.providerId || profile.protocol || '';
    const LogoComponent = PROVIDER_LOGOS[logoKey];
    const hasLogo = !!LogoComponent;
    const hasCustomIcon = !!profile.customIconUrl;
    const hasFavicon = !!profile.faviconUrl;
    const hasIcon = hasCustomIcon || hasFavicon;

    return (
        <span className={`flex h-10 w-10 shrink-0 items-center justify-center rounded-lg ${
            hasIcon || hasLogo
                ? 'bg-[#FFFFF0] dark:bg-gray-600 border border-gray-200 dark:border-gray-500'
                : `bg-gradient-to-br ${PROTOCOL_COLORS[profile.protocol || 'ftp'] || PROTOCOL_COLORS.ftp} text-white`
        }`}>
            {hasCustomIcon ? (
                <img src={profile.customIconUrl} alt="" className="w-6 h-6 rounded object-contain" onError={(e) => { (e.target as HTMLImageElement).style.display = 'none'; }} />
            ) : hasFavicon ? (
                <img src={profile.faviconUrl} alt="" className="w-6 h-6 rounded object-contain" onError={(e) => { (e.target as HTMLImageElement).style.display = 'none'; }} />
            ) : hasLogo ? (
                <LogoComponent size={20} />
            ) : (
                <span className="font-bold">{(profile.name || profile.host || '?').charAt(0).toUpperCase()}</span>
            )}
        </span>
    );
};

const ProfileRail: React.FC<ProfileRailProps> = ({ title, profiles, selected, onSelect, emptyMessage }) => (
    <div className="rounded-lg border border-gray-200 dark:border-gray-700">
        <div className="flex items-center justify-between border-b border-gray-200 px-3 py-2 dark:border-gray-700">
            <span className="text-xs font-semibold uppercase tracking-wide text-gray-500 dark:text-gray-400">
                {title}
            </span>
            {selected && (
                <span className="text-xs text-gray-500 dark:text-gray-400 truncate max-w-[140px]">
                    {selected.name}
                </span>
            )}
        </div>
        <div className="max-h-60 overflow-y-auto p-2 space-y-1">
            {profiles.map((profile) => {
                const isSelected = selected?.id === profile.id;
                return (
                    <button
                        key={profile.id}
                        type="button"
                        onClick={() => onSelect(profile)}
                        className={`w-full rounded-lg px-2 py-2 text-left transition flex items-center gap-3 ${
                            isSelected
                                ? 'bg-blue-50 ring-1 ring-blue-500 dark:bg-blue-900/30 dark:ring-blue-400'
                                : 'hover:bg-gray-50 dark:hover:bg-gray-700/50'
                        }`}
                    >
                        <ProfileIcon profile={profile} />
                        <div className="min-w-0 flex-1">
                            <div className="font-medium flex items-center gap-2">
                                <span className="truncate text-sm text-gray-900 dark:text-white">{profile.name}</span>
                                <span className="text-xs px-1.5 py-0.5 rounded font-medium uppercase bg-gray-200 dark:bg-gray-600 text-gray-600 dark:text-gray-300">
                                    {profile.protocol || 'ftp'}
                                </span>
                            </div>
                            {profile.initialPath?.trim() && (
                                <div className="text-xs text-gray-500 dark:text-gray-400 truncate mt-0.5">
                                    {profile.initialPath.trim()}
                                </div>
                            )}
                        </div>
                    </button>
                );
            })}
            {profiles.length === 0 && (
                <div className="rounded-lg border border-dashed border-gray-300 px-3 py-4 text-center text-sm text-gray-500 dark:border-gray-600 dark:text-gray-400">
                    {emptyMessage}
                </div>
            )}
        </div>
    </div>
);

const TransferBarsSpinner: React.FC<{ className?: string }> = ({ className = 'h-6 w-6' }) => (
    <svg
        className={className}
        style={{ color: 'var(--color-accent)' }}
        fill="currentColor"
        viewBox="0 0 24 24"
        xmlns="http://www.w3.org/2000/svg"
        aria-hidden="true"
    >
        <rect x="1" y="4" width="6" height="14" opacity="1">
            <animate id="spinner_aqiq" begin="0;spinner_xVBj.end-0.25s" attributeName="y" dur="0.75s" values="1;5" fill="freeze" />
            <animate begin="0;spinner_xVBj.end-0.25s" attributeName="height" dur="0.75s" values="22;14" fill="freeze" />
            <animate begin="0;spinner_xVBj.end-0.25s" attributeName="opacity" dur="0.75s" values="1;.2" fill="freeze" />
        </rect>
        <rect x="9" y="4" width="6" height="14" opacity=".4">
            <animate begin="spinner_aqiq.begin+0.15s" attributeName="y" dur="0.75s" values="1;5" fill="freeze" />
            <animate begin="spinner_aqiq.begin+0.15s" attributeName="height" dur="0.75s" values="22;14" fill="freeze" />
            <animate begin="spinner_aqiq.begin+0.15s" attributeName="opacity" dur="0.75s" values="1;.2" fill="freeze" />
        </rect>
        <rect x="17" y="4" width="6" height="14" opacity=".3">
            <animate id="spinner_xVBj" begin="spinner_aqiq.begin+0.3s" attributeName="y" dur="0.75s" values="1;5" fill="freeze" />
            <animate begin="spinner_aqiq.begin+0.3s" attributeName="height" dur="0.75s" values="22;14" fill="freeze" />
            <animate begin="spinner_aqiq.begin+0.3s" attributeName="opacity" dur="0.75s" values="1;.2" fill="freeze" />
        </rect>
    </svg>
);

export const CrossProfilePanel: React.FC<CrossProfilePanelProps> = ({ onClose }) => {
    const t = useTranslation();
    const [profiles, setProfiles] = useState<ServerProfile[]>([]);
    const [sourceProfile, setSourceProfile] = useState<ServerProfile | null>(null);
    const [destProfile, setDestProfile] = useState<ServerProfile | null>(null);
    const [sourcePath, setSourcePath] = useState('/');
    const [destPath, setDestPath] = useState('/');
    const [recursive, setRecursive] = useState(true);
    const [skipExisting, setSkipExisting] = useState(false);

    const [plan, setPlan] = useState<CrossProfilePlan | null>(null);
    const [summary, setSummary] = useState<TransferSummary | null>(null);
    const [loading, setLoading] = useState(false);
    const [error, setError] = useState<string | null>(null);
    const [phase, setPhase] = useState<'setup' | 'plan' | 'executing' | 'done'>('setup');

    useEffect(() => {
        (async () => {
            const saved = await secureGetWithFallback<ServerProfile[]>('server_profiles', 'aeroftp-saved-servers');
            if (saved) {
                setProfiles(saved);
            }
        })();
    }, []);

    useEffect(() => {
        if (sourceProfile) {
            setSourcePath(getDefaultRemotePath(sourceProfile));
        }
    }, [sourceProfile]);

    useEffect(() => {
        if (destProfile) {
            setDestPath(getDefaultRemotePath(destProfile));
        }
    }, [destProfile]);

    const sourceChoices = useMemo(
        () => profiles.filter((profile) => profile.id !== destProfile?.id),
        [profiles, destProfile?.id]
    );

    const destChoices = useMemo(
        () => profiles.filter((profile) => profile.id !== sourceProfile?.id),
        [profiles, sourceProfile?.id]
    );

    const handlePlan = async () => {
        if (!sourceProfile?.id || !destProfile?.id) return;

        setLoading(true);
        setError(null);
        setPlan(null);

        try {
            const result = await invoke<CrossProfilePlan>('cross_profile_plan', {
                request: {
                    source_profile_id: sourceProfile.id,
                    dest_profile_id: destProfile.id,
                    source_path: sourcePath,
                    dest_path: destPath,
                    recursive,
                    skip_existing: skipExisting,
                },
            });
            setPlan(result);
            setPhase('plan');
        } catch (e) {
            setError(String(e));
        } finally {
            setLoading(false);
        }
    };

    const handleExecute = async () => {
        if (!plan) return;

        setLoading(true);
        setError(null);
        setPhase('executing');

        try {
            const result = await invoke<TransferSummary>('cross_profile_execute', {
                request: {
                    plan_id: plan.plan_id,
                },
            });
            setSummary(result);
            setPhase('done');
        } catch (e) {
            setError(String(e));
            setPhase('plan');
        } finally {
            setLoading(false);
        }
    };

    const handleCancel = async () => {
        if (!plan) return;

        try {
            await invoke('cross_profile_cancel', { transferId: plan.plan_id });
        } catch (_) {
            // Cancellation is best-effort; backend progress/error surfaces the authoritative state.
        }
    };

    const handleReset = () => {
        setPlan(null);
        setSummary(null);
        setPhase('setup');
        setError(null);
    };

    const handleSwapProfiles = () => {
        setSourceProfile(destProfile);
        setDestProfile(sourceProfile);
        setSourcePath(destPath);
        setDestPath(sourcePath);
        setPlan(null);
        setSummary(null);
        setError(null);
        setPhase('setup');
    };

    return (
        <div className="pointer-events-none fixed inset-x-0 top-16 z-50 flex justify-center px-4">
            <div className="pointer-events-auto flex max-h-[calc(100vh-8rem)] w-full max-w-3xl flex-col overflow-hidden rounded-lg bg-white shadow-2xl dark:bg-gray-800">
                {/* Header */}
                <div className="flex items-center justify-between border-b border-gray-200 p-4 dark:border-gray-700">
                    <div className="flex items-center gap-2">
                        <ArrowRightLeft className="h-5 w-5 text-indigo-500" />
                        <div>
                            <h2 className="text-lg font-semibold text-gray-900 dark:text-white">
                                {t('transfer.crossProfile.title')}
                            </h2>
                            <p className="text-xs text-gray-500 dark:text-gray-400">
                                {t('transfer.crossProfile.subtitle')}
                            </p>
                        </div>
                    </div>
                    <button
                        onClick={onClose}
                        className="p-2 hover:bg-gray-100 dark:hover:bg-gray-700 rounded-lg transition-colors"
                        aria-label={t('transfer.crossProfile.closeAria')}
                    >
                        <X size={18} />
                    </button>
                </div>

                <div className="flex-1 overflow-y-auto p-4 space-y-4">
                    {error && (
                        <div className="rounded-lg bg-red-50 p-3 text-sm text-red-700 dark:bg-red-900/30 dark:text-red-300">
                            {error}
                        </div>
                    )}

                    {phase === 'setup' && (
                        <>
                            <div className="grid gap-4 md:grid-cols-[1fr_auto_1fr]">
                                <div className="rounded-lg border border-gray-200 p-3 dark:border-gray-700">
                                    <div className="mb-2 flex items-center justify-between">
                                        <label className="text-sm font-medium text-gray-700 dark:text-gray-300">
                                            {t('transfer.crossProfile.sourcePath')}
                                        </label>
                                        <span className="text-[11px] uppercase tracking-wide text-gray-400 dark:text-gray-500">
                                            {t('transfer.crossProfile.defaultFromProfile')}
                                        </span>
                                    </div>
                                    <input
                                        type="text"
                                        value={sourcePath}
                                        onChange={(e) => setSourcePath(e.target.value)}
                                        className="w-full rounded-lg border border-gray-300 bg-white px-3 py-2 text-sm text-gray-900 dark:border-gray-600 dark:bg-gray-700 dark:text-white"
                                        placeholder={t('transfer.crossProfile.sourcePathPlaceholder')}
                                    />
                                </div>
                                <div className="flex items-center justify-center">
                                    <button
                                        type="button"
                                        onClick={handleSwapProfiles}
                                        disabled={!sourceProfile && !destProfile}
                                        className="rounded-full border border-gray-300 bg-white p-2 text-gray-500 transition hover:border-indigo-400 hover:text-indigo-500 disabled:cursor-not-allowed disabled:opacity-50 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-300 dark:hover:border-indigo-500 dark:hover:text-indigo-400"
                                        title={t('transfer.crossProfile.swapProfiles')}
                                    >
                                        <ArrowLeftRight className="h-4 w-4" />
                                    </button>
                                </div>
                                <div className="rounded-lg border border-gray-200 p-3 dark:border-gray-700">
                                    <div className="mb-2 flex items-center justify-between">
                                        <label className="text-sm font-medium text-gray-700 dark:text-gray-300">
                                            {t('transfer.crossProfile.destinationPath')}
                                        </label>
                                        <span className="text-[11px] uppercase tracking-wide text-gray-400 dark:text-gray-500">
                                            {t('transfer.crossProfile.defaultFromProfile')}
                                        </span>
                                    </div>
                                    <input
                                        type="text"
                                        value={destPath}
                                        onChange={(e) => setDestPath(e.target.value)}
                                        className="w-full rounded-lg border border-gray-300 bg-white px-3 py-2 text-sm text-gray-900 dark:border-gray-600 dark:bg-gray-700 dark:text-white"
                                        placeholder={t('transfer.crossProfile.destinationPathPlaceholder')}
                                    />
                                </div>
                            </div>

                            <div className="grid gap-4 md:grid-cols-2">
                                <ProfileRail
                                    title={t('transfer.crossProfile.sourceProfile')}
                                    profiles={sourceChoices}
                                    selected={sourceProfile}
                                    onSelect={setSourceProfile}
                                    emptyMessage={t('transfer.crossProfile.noProfiles')}
                                />
                                <ProfileRail
                                    title={t('transfer.crossProfile.destinationProfile')}
                                    profiles={destChoices}
                                    selected={destProfile}
                                    onSelect={setDestProfile}
                                    emptyMessage={t('transfer.crossProfile.noProfiles')}
                                />
                            </div>

                            <div className="flex flex-wrap items-center gap-6 px-1">
                                <Checkbox
                                    checked={recursive}
                                    onChange={setRecursive}
                                    label={<span className="text-sm text-gray-700 dark:text-gray-300">{t('transfer.crossProfile.recursive')}</span>}
                                />
                                <Checkbox
                                    checked={skipExisting}
                                    onChange={setSkipExisting}
                                    label={<span className="text-sm text-gray-700 dark:text-gray-300">{t('transfer.crossProfile.skipExisting')}</span>}
                                />
                            </div>

                            <TransferActionBar
                                onPlan={handlePlan}
                                canPlan={!!sourceProfile && !!destProfile}
                                loading={loading}
                            />
                        </>
                    )}

                    {phase === 'plan' && plan && (
                        <>
                            <div className="rounded-lg bg-blue-50 p-3 dark:bg-blue-900/30">
                                <div className="text-sm font-medium text-blue-700 dark:text-blue-300">
                                    {t('transfer.crossProfile.planReady', {
                                        count: plan.total_files,
                                        size: formatSize(plan.total_bytes),
                                    })}
                                </div>
                                <div className="mt-1 text-xs text-blue-600/80 dark:text-blue-200/80">
                                    {plan.source_profile} <ArrowRight className="mx-1 inline h-3.5 w-3.5" /> {plan.dest_profile}
                                </div>
                            </div>

                            <div className="overflow-hidden rounded-lg border border-gray-200 dark:border-gray-700">
                                <div className="max-h-64 overflow-y-auto">
                                    <table className="w-full text-sm">
                                        <thead className="sticky top-0 bg-gray-50 dark:bg-gray-700">
                                            <tr>
                                                <th className="px-3 py-2 text-left text-gray-600 dark:text-gray-300">{t('transfer.crossProfile.fileColumn')}</th>
                                                <th className="px-3 py-2 text-right text-gray-600 dark:text-gray-300">{t('transfer.crossProfile.sizeColumn')}</th>
                                            </tr>
                                        </thead>
                                        <tbody>
                                            {plan.entries.map((entry, i) => (
                                                <tr key={i} className="border-t border-gray-100 dark:border-gray-700">
                                                    <td
                                                        className="max-w-[420px] truncate px-3 py-1.5 text-gray-900 dark:text-white"
                                                        title={entry.dest_path}
                                                    >
                                                        {entry.display_name || entry.source_path || entry.dest_path}
                                                    </td>
                                                    <td className="px-3 py-1.5 text-right text-gray-500 dark:text-gray-400">
                                                        {formatSize(entry.size)}
                                                    </td>
                                                </tr>
                                            ))}
                                        </tbody>
                                    </table>
                                </div>
                            </div>

                            <div className="flex gap-2">
                                <button
                                    onClick={handleExecute}
                                    disabled={loading}
                                    className="flex flex-1 items-center justify-center gap-2 rounded-lg bg-blue-600 px-4 py-2 text-white hover:bg-blue-700 disabled:opacity-50"
                                >
                                    <Play className="h-4 w-4" />
                                    {t('transfer.crossProfile.executeTransfer')}
                                </button>
                                <button
                                    onClick={handleReset}
                                    className="rounded-lg border border-gray-300 px-4 py-2 text-gray-700 hover:bg-gray-50 dark:border-gray-600 dark:text-gray-300 dark:hover:bg-gray-700"
                                >
                                    {t('common.back')}
                                </button>
                            </div>
                        </>
                    )}

                    {phase === 'executing' && (
                        <div className="space-y-4 py-4">
                            <div className="rounded-lg border border-blue-200 bg-blue-50 p-4 dark:border-blue-800 dark:bg-blue-900/20">
                                <div className="flex items-start gap-3">
                                    <TransferBarsSpinner className="mt-0.5 h-6 w-6 shrink-0" />
                                    <div>
                                        <div className="text-sm font-medium text-blue-700 dark:text-blue-300">
                                            {t('transfer.crossProfile.transferInProgress')}
                                        </div>
                                        <p className="mt-1 text-sm text-blue-600/80 dark:text-blue-200/80">
                                            {t('transfer.crossProfile.transferHint')}
                                        </p>
                                    </div>
                                </div>
                            </div>

                            <div className="flex justify-end">
                                <button
                                    onClick={handleCancel}
                                    className="rounded-lg border border-red-300 px-4 py-1.5 text-sm text-red-600 hover:bg-red-50 dark:border-red-700 dark:text-red-400 dark:hover:bg-red-900/30"
                                >
                                    {t('common.cancel')}
                                </button>
                            </div>
                        </div>
                    )}

                    {phase === 'done' && summary && (
                        <>
                            <div className={`rounded-lg p-3 ${summary.failed_files > 0 ? 'bg-yellow-50 dark:bg-yellow-900/30' : 'bg-green-50 dark:bg-green-900/30'}`}>
                                <div className={`text-sm font-medium ${summary.failed_files > 0 ? 'text-yellow-700 dark:text-yellow-300' : 'text-green-700 dark:text-green-300'}`}>
                                    {t('transfer.crossProfile.summary', {
                                        transferred: summary.transferred_files,
                                        skipped: summary.skipped_files,
                                        failed: summary.failed_files,
                                    })}
                                </div>
                                <div className="mt-1 text-xs text-gray-500 dark:text-gray-400">
                                    {t('transfer.crossProfile.summaryMeta', {
                                        size: formatSize(summary.total_bytes),
                                        seconds: (summary.duration_ms / 1000).toFixed(1),
                                    })}
                                </div>
                            </div>
                            <button
                                onClick={handleReset}
                                className="flex w-full items-center justify-center gap-2 rounded-lg border border-gray-300 px-4 py-2 text-gray-700 hover:bg-gray-50 dark:border-gray-600 dark:text-gray-300 dark:hover:bg-gray-700"
                            >
                                <RefreshCw className="h-4 w-4" />
                                {t('transfer.crossProfile.newTransfer')}
                            </button>
                        </>
                    )}
                </div>
            </div>
        </div>
    );
};

// Protocol gradient colors — same as SavedServers
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
