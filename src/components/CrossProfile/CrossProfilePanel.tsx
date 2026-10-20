// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { useEffect, useMemo, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen, UnlistenFn } from '@tauri-apps/api/event';
import { ArrowLeftRight, ArrowRight, ArrowRightLeft, Play, RefreshCw, X } from 'lucide-react';
import { ServerProfile, TransferEvent } from '../../types';
import { useTranslation } from '../../i18n';
import { loadSavedServerProfiles } from '../../utils/serverProfileStore';
import { formatBytes } from '../../utils/formatters';
import { Checkbox } from '../ui/Checkbox';
import { ProfileRail } from '../common/ProfileRail';
import { TransferActionBar } from './TransferActionBar';
import { TransferProgressBar } from '../TransferProgressBar';
import { useDraggableModal } from '../../hooks/useDraggableModal';
import { useGuardedClose } from '../../hooks/useGuardedClose';
import { GuardedCloseConfirm } from '../GuardedCloseConfirm';

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
    directories: CrossProfilePlanEntry[];
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
    /** Profile id to pre-select as the source (typically the active connection). */
    initialSourceProfileId?: string;
    /** Optional source path to seed the source input (typically the current remote path). */
    initialSourcePath?: string;
    /** Profile id to pre-select as the destination (e.g. second selection from My Servers). */
    initialDestProfileId?: string;
    /** Optional destination path to seed the destination input. */
    initialDestPath?: string;
}

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

export const CrossProfilePanel: React.FC<CrossProfilePanelProps> = ({ onClose, initialSourceProfileId, initialSourcePath, initialDestProfileId, initialDestPath }) => {
    const t = useTranslation();
    const modalDrag = useDraggableModal();
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

    // Live transfer progress: populated while phase === 'executing'.
    // Values come from `transfer_event` Tauri events filtered by plan.plan_id.
    interface LiveProgress {
        currentFile: string;
        currentIndex: number;      // 1-based count of the file being transferred
        totalFiles: number;
        bytesTransferred: number;
        percentage: number;        // based on file count (granularity is per-file)
        speedBps: number;
    }
    const [liveProgress, setLiveProgress] = useState<LiveProgress | null>(null);

    // Skip the auto-reset of source/dest path on the first render after seeding -
    // otherwise the second useEffect would overwrite the seeded path with the
    // profile's default.
    const seededSourceRef = useRef(false);
    const seededDestRef = useRef(false);

    useEffect(() => {
        (async () => {
            const saved = await loadSavedServerProfiles();
            if (saved.length > 0) {
                setProfiles(saved);
                // Pre-select the profile matching the active connection so users
                // who open the dialog while connected don't have to re-pick the
                // server they're already looking at.
                if (initialSourceProfileId) {
                    const match = saved.find((p) => p.id === initialSourceProfileId);
                    if (match) {
                        seededSourceRef.current = true;
                        setSourceProfile(match);
                        setSourcePath(initialSourcePath?.trim() || getDefaultRemotePath(match));
                    }
                }
                if (initialDestProfileId && initialDestProfileId !== initialSourceProfileId) {
                    const match = saved.find((p) => p.id === initialDestProfileId);
                    if (match) {
                        seededDestRef.current = true;
                        setDestProfile(match);
                        setDestPath(initialDestPath?.trim() || getDefaultRemotePath(match));
                    }
                }
            }
        })();
        // eslint-disable-next-line react-hooks/exhaustive-deps
    }, []);

    useEffect(() => {
        if (!sourceProfile) return;
        if (seededSourceRef.current) {
            seededSourceRef.current = false;
            return;
        }
        setSourcePath(getDefaultRemotePath(sourceProfile));
    }, [sourceProfile]);

    useEffect(() => {
        if (!destProfile) return;
        if (seededDestRef.current) {
            seededDestRef.current = false;
            return;
        }
        setDestPath(getDefaultRemotePath(destProfile));
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

    // Subscribe to transfer_event while executing. We scope the listener to this
    // plan_id so other concurrent transfers don't pollute our in-panel progress.
    useEffect(() => {
        if (phase !== 'executing' || !plan) return;

        let unlisten: UnlistenFn | null = null;
        let cancelled = false;

        (async () => {
            const fn = await listen<TransferEvent>('transfer_event', (event) => {
                const data = event.payload;
                if (data.transfer_id !== plan.plan_id) return;
                if (data.direction !== 'cross-profile') return;

                // Helper: cumulative bytes for the first N plan entries.
                const cumBytesUpTo = (completed: number): number =>
                    plan.entries.slice(0, completed).reduce((sum, e) => sum + (e.size || 0), 0);

                // The speed is the backend's: the bytes moved over the last
                // few seconds (SpeedMeter, transfer_speed.rs). It used to be
                // recomputed here as the bytes of the completed files over
                // the time since the start, i.e. the average of the whole
                // copy, which barely moves after a few minutes (#658 test).
                const speedOf = (data: TransferEvent) => data.progress?.speed_bps ?? 0;

                // Byte-based percentage matches what the user actually sees transferring
                // (e.g. "183 MB / 202 MB" reads 91%, not the file-count 17/69 = 24%).
                // Falls back to file count when total_bytes isn't known (empty plan edge case).
                const bytePct = (cumBytes: number, fileCount: number, total: number): number => {
                    if (plan.total_bytes > 0) {
                        return Math.min(100, Math.round((cumBytes * 100) / plan.total_bytes));
                    }
                    return Math.min(100, Math.round((fileCount * 100) / Math.max(total, 1)));
                };

                if (data.event_type === 'file_start' && data.progress) {
                    const completed = data.progress.transferred;
                    const cumBytes = cumBytesUpTo(completed);
                    setLiveProgress({
                        currentFile: data.filename,
                        currentIndex: completed + 1,
                        totalFiles: data.progress.total,
                        bytesTransferred: cumBytes,
                        percentage: bytePct(cumBytes, completed, data.progress.total),
                        speedBps: speedOf(data),
                    });
                } else if (data.event_type === 'file_complete' && data.progress) {
                    const total = data.progress.total || 1;
                    const done = data.progress.transferred;
                    const cumBytes = cumBytesUpTo(done);
                    setLiveProgress({
                        currentFile: data.filename,
                        currentIndex: done,
                        totalFiles: total,
                        bytesTransferred: cumBytes,
                        percentage: bytePct(cumBytes, done, total),
                        speedBps: speedOf(data),
                    });
                } else if (data.event_type === 'file_skip' && data.progress) {
                    const total = data.progress.total || 1;
                    const done = data.progress.transferred;
                    const cumBytes = cumBytesUpTo(done);
                    setLiveProgress({
                        currentFile: data.filename,
                        currentIndex: done,
                        totalFiles: total,
                        bytesTransferred: cumBytes,
                        percentage: bytePct(cumBytes, done, total),
                        speedBps: speedOf(data),
                    });
                }
            });
            // If cleanup already ran while `listen()` was in-flight, drop the
            // freshly-registered listener instead of leaving it orphaned -
            // otherwise a second execute() would stack another subscription.
            if (cancelled) {
                fn();
            } else {
                unlisten = fn;
            }
        })();

        return () => {
            cancelled = true;
            if (unlisten) {
                unlisten();
                unlisten = null;
            }
        };
    }, [phase, plan]);

    const handleExecute = async () => {
        if (!plan) return;

        setLoading(true);
        setError(null);
        setPhase('executing');
        setLiveProgress({
            currentFile: '',
            currentIndex: 0,
            totalFiles: plan.total_files,
            bytesTransferred: 0,
            percentage: 0,
            speedBps: 0,
        });

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
            setLiveProgress(null);
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

    // Guarded close: a transfer in progress must not be lost to a stray
    // click-outside; the backdrop is inert and X/ESC offer wait vs stop.
    const guarded = useGuardedClose({
        guard: phase === 'executing' ? 'busy' : null,
        onClose,
        onAbort: handleCancel,
    });

    return (
        <div
            className="fixed inset-0 z-50 flex items-start justify-center px-4 pt-[5vh] bg-black/60 backdrop-blur-sm animate-scale-in"
            onClick={(e) => {
                if (e.target === e.currentTarget) guarded.requestBackdropClose();
            }}
        >
            <div
                {...modalDrag.panelProps}
                className="flex max-h-[calc(100vh-8rem)] w-full max-w-3xl flex-col overflow-hidden rounded-lg bg-white shadow-2xl dark:bg-gray-800"
            >
                {/* Header */}
                <div
                    {...modalDrag.dragHandleProps}
                    className="flex cursor-grab items-center justify-between border-b border-gray-200 p-4 active:cursor-grabbing dark:border-gray-700"
                >
                    <div className="flex items-center gap-2 pointer-events-none">
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
                        onClick={guarded.requestClose}
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
                                    role="source"
                                />
                                <ProfileRail
                                    title={t('transfer.crossProfile.destinationProfile')}
                                    profiles={destChoices}
                                    selected={destProfile}
                                    onSelect={setDestProfile}
                                    emptyMessage={t('transfer.crossProfile.noProfiles')}
                                    role="destination"
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
                                        size: formatBytes(plan.total_bytes),
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
                                            {[...plan.directories, ...plan.entries].map((entry, i) => (
                                                <tr key={i} className="border-t border-gray-100 dark:border-gray-700">
                                                    <td
                                                        className="max-w-[420px] truncate px-3 py-1.5 text-gray-900 dark:text-white"
                                                        title={entry.dest_path}
                                                    >
                                                        {entry.display_name || entry.source_path || entry.dest_path}{entry.is_dir ? "/" : ""}
                                                    </td>
                                                    <td className="px-3 py-1.5 text-right text-gray-500 dark:text-gray-400">
                                                        {entry.is_dir ? "/" : formatBytes(entry.size)}
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
                        <div className="space-y-4 py-2">
                            <div className="rounded-lg border border-blue-200 bg-blue-50 p-4 dark:border-blue-800 dark:bg-blue-900/20">
                                <div className="flex items-start gap-3">
                                    <TransferBarsSpinner className="mt-0.5 h-6 w-6 shrink-0" />
                                    <div className="min-w-0 flex-1">
                                        <div className="text-sm font-medium text-blue-700 dark:text-blue-300">
                                            {t('transfer.crossProfile.transferInProgress')}
                                        </div>
                                        {plan && (
                                            <div className="mt-1 text-xs text-blue-600/80 dark:text-blue-200/80">
                                                {plan.source_profile}
                                                <ArrowRight className="mx-1 inline h-3.5 w-3.5" />
                                                {plan.dest_profile}
                                            </div>
                                        )}
                                    </div>
                                </div>
                            </div>

                            {liveProgress && (
                                <div className="rounded-lg border border-gray-200 p-3 dark:border-gray-700 space-y-2">
                                    <div className="flex items-baseline justify-between gap-2">
                                        <span className="truncate text-sm font-medium text-gray-900 dark:text-white" title={liveProgress.currentFile}>
                                            {liveProgress.currentFile || t('transfer.crossProfile.preparing')}
                                        </span>
                                        <span className="shrink-0 text-xs tabular-nums text-gray-500 dark:text-gray-400">
                                            {liveProgress.currentIndex}/{liveProgress.totalFiles}
                                        </span>
                                    </div>
                                    <TransferProgressBar
                                        percentage={liveProgress.percentage}
                                        filename={liveProgress.currentFile}
                                        speedBps={liveProgress.speedBps}
                                        etaSeconds={
                                            liveProgress.speedBps > 0 && plan && plan.total_bytes > liveProgress.bytesTransferred
                                                ? Math.round((plan.total_bytes - liveProgress.bytesTransferred) / liveProgress.speedBps)
                                                : 0
                                        }
                                        transferredBytes={liveProgress.bytesTransferred}
                                        totalBytes={plan?.total_bytes}
                                        currentFile={liveProgress.currentIndex}
                                        totalFiles={liveProgress.totalFiles}
                                        size="md"
                                        variant={liveProgress.percentage > 0 ? 'gradient' : 'indeterminate'}
                                    />
                                    {/* Bytes and speed are in the bar's details row
                                        above; this row printed them a second time. */}
                                    <div className="text-xs text-gray-500 dark:text-gray-400 tabular-nums">
                                        {liveProgress.percentage}%
                                    </div>
                                </div>
                            )}

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
                                        size: formatBytes(summary.total_bytes),
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
            {guarded.confirmOpen && guarded.confirmKind && (
                <GuardedCloseConfirm
                    kind={guarded.confirmKind}
                    onKeep={guarded.cancelConfirm}
                    onConfirm={guarded.confirmAndClose}
                />
            )}
        </div>
    );
};
