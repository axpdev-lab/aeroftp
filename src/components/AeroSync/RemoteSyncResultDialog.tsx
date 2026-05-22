// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
//
// GAP-3 of the AeroSync connected-remote gap-closure filone.
//
// Surfaces the rich `SyncRunReport` produced by `runRemoteSync` after a
// connected-remote preset finishes. Replaces the copy-only toast that the
// unified modal used to show: deletes, verify failures, retries and delta
// savings now all have a home. Reuses the restored `syncPanel.*` report
// strings so the slice ships with no new i18n keys.

import * as React from 'react';
import {
    FolderSync,
    X,
    ArrowUp,
    ArrowDown,
    Trash2,
    SkipForward,
    Folder,
    RotateCcw,
    ShieldAlert,
    ShieldCheck,
    CheckCircle2,
    AlertTriangle,
    Zap,
} from 'lucide-react';
import { useTranslation } from '../../i18n';
import { useDraggableModal } from '../../hooks/useDraggableModal';
import { formatSize } from '../../utils/formatters';
import { groupErrorsByKind, type SyncRunReport } from '../../utils/remoteSyncRunner';

export interface RemoteSyncResultDialogProps {
    report: SyncRunReport | null;
    onClose: () => void;
}

const formatDuration = (ms: number): string => {
    const secs = Math.floor(ms / 1000);
    if (secs < 60) return `${secs}s`;
    const mins = Math.floor(secs / 60);
    return `${mins}m ${secs % 60}s`;
};

export const RemoteSyncResultDialog: React.FC<RemoteSyncResultDialogProps> = ({
    report,
    onClose,
}) => {
    const t = useTranslation();
    const modalDrag = useDraggableModal();

    React.useEffect(() => {
        if (!report) return;
        const handler = (e: KeyboardEvent) => {
            if (e.key === 'Escape') onClose();
        };
        window.addEventListener('keydown', handler);
        return () => window.removeEventListener('keydown', handler);
    }, [report, onClose]);

    if (!report) return null;

    const hasErrors = report.errors.length > 0;
    const grouped = groupErrorsByKind(report.errors);

    const stats: Array<{ icon: React.ReactNode; label: string; value: number; show: boolean }> = [
        { icon: <ArrowUp size={14} className="text-blue-500" />, label: t('syncPanel.reportUploaded'), value: report.uploaded, show: report.uploaded > 0 },
        { icon: <ArrowDown size={14} className="text-amber-500" />, label: t('syncPanel.reportDownloaded'), value: report.downloaded, show: report.downloaded > 0 },
        { icon: <Trash2 size={14} className="text-rose-500" />, label: t('syncPanel.reportDeleted'), value: report.deleted, show: report.deleted > 0 },
        { icon: <Folder size={14} className="text-emerald-500" />, label: t('syncPanel.reportDirsCreated'), value: report.dirsCreated, show: report.dirsCreated > 0 },
        { icon: <SkipForward size={14} className="text-gray-400" />, label: t('syncPanel.reportSkipped'), value: report.skipped, show: report.skipped > 0 },
        { icon: <RotateCcw size={14} className="text-amber-500" />, label: t('syncPanel.reportRetried'), value: report.retried, show: report.retried > 0 },
        { icon: <ShieldAlert size={14} className="text-rose-500" />, label: t('syncPanel.reportVerifyFailed'), value: report.verifyFailed, show: report.verifyFailed > 0 },
    ];
    const visibleStats = stats.filter((s) => s.show);

    const errorKindLabel = (kind: string): string => {
        const key = `syncPanel.errorKind.${kind}`;
        const val = t(key);
        return val !== key ? val : kind.replace(/_/g, ' ');
    };

    return (
        <div
            className="fixed inset-0 z-[9999] flex items-start justify-center pt-[8vh] bg-black/50 animate-scale-in"
            role="dialog"
            aria-modal="true"
            aria-label={t('aerosync.title') || 'AeroSync'}
            onClick={onClose}
        >
            <div
                {...modalDrag.panelProps}
                className="relative bg-white dark:bg-gray-800 rounded-lg shadow-2xl w-full max-w-md overflow-hidden flex flex-col max-h-[80vh]"
                onClick={(e) => e.stopPropagation()}
            >
                <div
                    {...modalDrag.dragHandleProps}
                    className="flex items-center justify-between p-4 border-b border-gray-200 dark:border-gray-700 cursor-grab active:cursor-grabbing"
                >
                    <div className="flex items-center gap-2 pointer-events-none">
                        <FolderSync size={18} className="text-blue-500" />
                        <h2 className="text-base font-semibold">{t('aerosync.title') || 'AeroSync'}</h2>
                    </div>
                    <button
                        type="button"
                        onClick={onClose}
                        aria-label={t('common.close') || 'Close'}
                        className="p-1.5 hover:bg-gray-100 dark:hover:bg-gray-700 rounded-lg transition-colors"
                    >
                        <X size={16} />
                    </button>
                </div>

                <div className="px-4 py-3 space-y-3 text-sm overflow-y-auto">
                    <div
                        className={`flex items-center gap-2 rounded-md px-3 py-2 text-[13px] font-medium ${
                            hasErrors
                                ? 'bg-amber-50 text-amber-900 dark:bg-amber-900/30 dark:text-amber-100'
                                : 'bg-emerald-50 text-emerald-900 dark:bg-emerald-900/30 dark:text-emerald-100'
                        }`}
                    >
                        {hasErrors ? (
                            <AlertTriangle size={16} className="shrink-0" />
                        ) : (
                            <CheckCircle2 size={16} className="shrink-0" />
                        )}
                        <span>
                            {hasErrors
                                ? t('syncPanel.reportPartial')
                                : t('syncPanel.reportSuccess')}
                        </span>
                    </div>

                    {visibleStats.length > 0 && (
                        <div className="grid grid-cols-2 gap-2">
                            {visibleStats.map((s) => (
                                <div
                                    key={s.label}
                                    className="flex items-center gap-2 rounded-md border border-gray-200 dark:border-gray-700 px-2.5 py-1.5"
                                >
                                    {s.icon}
                                    <span className="text-[12px] text-gray-500 dark:text-gray-400">
                                        {s.label}
                                    </span>
                                    <span className="ml-auto font-semibold tabular-nums">
                                        {s.value}
                                    </span>
                                </div>
                            ))}
                        </div>
                    )}

                    <div className="flex justify-between text-[12px] text-gray-500 dark:text-gray-400 px-1">
                        <span>
                            {t('syncPanel.reportTransferred')}:{' '}
                            <span className="font-mono text-gray-700 dark:text-gray-300">
                                {formatSize(report.totalBytes)}
                            </span>
                        </span>
                        <span>
                            {t('syncPanel.reportDuration')}:{' '}
                            <span className="font-mono text-gray-700 dark:text-gray-300">
                                {formatDuration(report.durationMs)}
                            </span>
                        </span>
                    </div>

                    {report.postSyncVerification && (
                        <div
                            className={`flex items-center gap-2 rounded-md border px-3 py-2 text-[12px] ${
                                report.postSyncVerification.mismatches > 0
                                || report.postSyncVerification.failed > 0
                                    ? 'border-amber-200 bg-amber-50 text-amber-900 dark:border-amber-800 dark:bg-amber-900/30 dark:text-amber-100'
                                    : 'border-emerald-200 bg-emerald-50 text-emerald-900 dark:border-emerald-800 dark:bg-emerald-900/30 dark:text-emerald-100'
                            }`}
                        >
                            <ShieldCheck size={14} className="shrink-0" />
                            <span>
                                {t('syncPanel.maniacPostSyncReport', {
                                    ok: report.postSyncVerification.ok,
                                    mismatches: report.postSyncVerification.mismatches,
                                    failed: report.postSyncVerification.failed,
                                })}
                            </span>
                        </div>
                    )}

                    {report.delta_savings && report.delta_savings.files_using_delta > 0 && (
                        <div className="flex items-center gap-2 rounded-md border border-blue-200 bg-blue-50 px-3 py-2 text-[12px] text-blue-900 dark:border-blue-800 dark:bg-blue-900/30 dark:text-blue-100">
                            <Zap size={14} className="shrink-0" />
                            <span>
                                {t('syncPanel.deltaSummary', {
                                    files: report.delta_savings.files_using_delta,
                                    bytes: formatSize(Math.max(0, report.delta_savings.bytes_saved)),
                                    speedup: (report.delta_savings.average_speedup ?? 1).toFixed(1),
                                })}
                            </span>
                        </div>
                    )}

                    {hasErrors && (
                        <div className="space-y-1.5">
                            <div className="text-[12px] font-semibold text-gray-600 dark:text-gray-300">
                                {t('syncPanel.errorBreakdown')}
                            </div>
                            {[...grouped.entries()].map(([kind, list]) => (
                                <div
                                    key={kind}
                                    className="rounded-md border border-rose-200 bg-rose-50 px-2.5 py-1.5 dark:border-rose-900 dark:bg-rose-900/20"
                                >
                                    <div className="flex items-center justify-between text-[12px]">
                                        <span className="font-medium text-rose-800 dark:text-rose-200">
                                            {errorKindLabel(kind)}
                                        </span>
                                        <span className="tabular-nums text-rose-600 dark:text-rose-300">
                                            {list.length}
                                            {list.some((e) => e.retryable)
                                                ? ` · ${t('syncPanel.retryable')}`
                                                : ''}
                                        </span>
                                    </div>
                                    <ul className="mt-1 space-y-0.5">
                                        {list.slice(0, 5).map((e, i) => (
                                            <li
                                                key={`${e.file_path ?? ''}-${i}`}
                                                className="truncate text-[11px] text-rose-700 dark:text-rose-300"
                                                title={e.message}
                                            >
                                                {e.file_path ? `${e.file_path}: ` : ''}
                                                {e.message}
                                            </li>
                                        ))}
                                        {list.length > 5 && (
                                            <li className="text-[11px] text-rose-500">
                                                +{list.length - 5}
                                            </li>
                                        )}
                                    </ul>
                                </div>
                            ))}
                        </div>
                    )}
                </div>

                <div className="flex justify-end px-4 py-3 border-t border-gray-200 dark:border-gray-700">
                    <button
                        type="button"
                        onClick={onClose}
                        className="px-3 py-1.5 rounded-md bg-blue-600 text-white text-sm font-medium hover:bg-blue-700 transition-colors"
                    >
                        {t('syncPanel.close') || 'Close'}
                    </button>
                </div>
            </div>
        </div>
    );
};

export default RemoteSyncResultDialog;
