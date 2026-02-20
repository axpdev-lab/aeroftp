/**
 * CanaryResultDialog — Shows results of a canary (sample) sync analysis
 * Displays sampled file stats, action breakdown, and projection for full sync.
 * User can approve full sync or dismiss.
 */

import React, { useEffect, useMemo } from 'react';
import {
    X, FlaskConical, Upload, Download, AlertTriangle, HardDrive,
    CheckCircle2, XCircle, SkipForward, Trash2, FileSearch, Zap
} from 'lucide-react';
import { useTranslation } from '../../i18n';
import { formatSize } from '../../utils/formatters';

// --- Backend types (from Rust sync_canary_run) ---

export interface CanarySampleResult {
    relative_path: string;
    action: string; // "upload" | "download" | "delete" | "skip"
    success: boolean;
    error: string | null;
    bytes: number;
}

export interface CanarySummary {
    would_upload: number;
    would_download: number;
    would_delete: number;
    conflicts: number;
    errors: number;
    estimated_transfer_size: number;
}

export interface CanaryResult {
    sampled_files: number;
    total_files: number;
    results: CanarySampleResult[];
    summary: CanarySummary;
}

interface CanaryResultDialogProps {
    result: CanaryResult;
    onClose: () => void;
    onApprove: () => void;
}

const ACTION_BADGE: Record<string, { bg: string; text: string; Icon: typeof Upload }> = {
    upload:   { bg: 'bg-blue-500/20',   text: 'text-blue-400',   Icon: Upload },
    download: { bg: 'bg-emerald-500/20', text: 'text-emerald-400', Icon: Download },
    delete:   { bg: 'bg-red-500/20',     text: 'text-red-400',     Icon: Trash2 },
    skip:     { bg: 'bg-gray-500/20',    text: 'text-gray-400',    Icon: SkipForward },
};

export const CanaryResultDialog: React.FC<CanaryResultDialogProps> = ({
    result,
    onClose,
    onApprove,
}) => {
    const t = useTranslation();

    // Escape to close
    useEffect(() => {
        const handler = (e: KeyboardEvent) => { if (e.key === 'Escape') onClose(); };
        window.addEventListener('keydown', handler);
        return () => window.removeEventListener('keydown', handler);
    }, [onClose]);

    const samplePercent = result.total_files > 0
        ? Math.round((result.sampled_files / result.total_files) * 100)
        : 0;

    // Projection: scale from sample to total
    const projectionMultiplier = result.sampled_files > 0
        ? result.total_files / result.sampled_files
        : 1;
    const projectedFiles = Math.round(
        (result.summary.would_upload + result.summary.would_download + result.summary.would_delete) * projectionMultiplier
    );
    const projectedSize = result.summary.estimated_transfer_size * projectionMultiplier;

    // Sort results: errors first, then by action
    const sortedResults = useMemo(() => {
        return [...result.results].sort((a, b) => {
            if (a.success !== b.success) return a.success ? 1 : -1;
            return a.action.localeCompare(b.action);
        });
    }, [result.results]);

    return (
        <div className="fixed inset-0 bg-black/60 z-[9999] flex items-center justify-center p-4" onClick={onClose} role="dialog" aria-modal="true" aria-label="Canary Sync Results">
            <div
                className="bg-[var(--color-bg-primary,#fff)] dark:bg-[var(--color-bg-primary,#1f2937)] rounded-xl shadow-2xl w-full max-w-2xl flex flex-col max-h-[85vh]"
                onClick={e => e.stopPropagation()}
            >
                {/* Header */}
                <div className="flex items-center justify-between px-5 py-4 border-b border-gray-200 dark:border-gray-700 flex-shrink-0">
                    <div className="flex items-center gap-2">
                        <FlaskConical size={18} className="text-amber-500" />
                        <h3 className="font-semibold text-sm text-gray-900 dark:text-gray-100">
                            {t('syncPanel.canaryResults') || 'Canary Sync Results'}
                        </h3>
                    </div>
                    <button
                        onClick={onClose}
                        className="text-gray-400 hover:text-gray-600 dark:hover:text-gray-200 transition-colors"
                    >
                        <X size={18} />
                    </button>
                </div>

                {/* Stats Cards */}
                <div className="px-5 pt-4 pb-2 flex-shrink-0">
                    <div className="grid grid-cols-2 sm:grid-cols-5 gap-2">
                        {/* Sampled */}
                        <div className="rounded-lg bg-gray-100 dark:bg-gray-700/50 p-2.5 text-center">
                            <FileSearch size={16} className="mx-auto mb-1 text-purple-400" />
                            <div className="text-xs font-semibold text-gray-800 dark:text-gray-100">
                                {result.sampled_files} / {result.total_files}
                            </div>
                            <div className="text-[10px] text-gray-500 dark:text-gray-400">
                                {t('syncPanel.canarySampled') || 'Sampled'} ({samplePercent}%)
                            </div>
                        </div>

                        {/* Upload */}
                        <div className="rounded-lg bg-blue-500/10 p-2.5 text-center">
                            <Upload size={16} className="mx-auto mb-1 text-blue-400" />
                            <div className="text-xs font-semibold text-blue-500 dark:text-blue-300">
                                {result.summary.would_upload}
                            </div>
                            <div className="text-[10px] text-gray-500 dark:text-gray-400">
                                {t('syncPanel.canaryUpload') || 'Upload'}
                            </div>
                        </div>

                        {/* Download */}
                        <div className="rounded-lg bg-emerald-500/10 p-2.5 text-center">
                            <Download size={16} className="mx-auto mb-1 text-emerald-400" />
                            <div className="text-xs font-semibold text-emerald-500 dark:text-emerald-300">
                                {result.summary.would_download}
                            </div>
                            <div className="text-[10px] text-gray-500 dark:text-gray-400">
                                {t('syncPanel.canaryDownload') || 'Download'}
                            </div>
                        </div>

                        {/* Conflicts */}
                        <div className="rounded-lg bg-amber-500/10 p-2.5 text-center">
                            <AlertTriangle size={16} className="mx-auto mb-1 text-amber-400" />
                            <div className="text-xs font-semibold text-amber-500 dark:text-amber-300">
                                {result.summary.conflicts}
                            </div>
                            <div className="text-[10px] text-gray-500 dark:text-gray-400">
                                {t('syncPanel.canaryConflicts') || 'Conflicts'}
                            </div>
                        </div>

                        {/* Est. Transfer */}
                        <div className="rounded-lg bg-purple-500/10 p-2.5 text-center">
                            <HardDrive size={16} className="mx-auto mb-1 text-purple-400" />
                            <div className="text-xs font-semibold text-purple-500 dark:text-purple-300">
                                {formatSize(result.summary.estimated_transfer_size)}
                            </div>
                            <div className="text-[10px] text-gray-500 dark:text-gray-400">
                                {t('syncPanel.canaryEstTransfer') || 'Est. Transfer'}
                            </div>
                        </div>
                    </div>

                    {/* Errors banner */}
                    {result.summary.errors > 0 && (
                        <div className="mt-2 flex items-center gap-2 p-2 rounded-lg bg-red-500/10 text-red-400 text-xs">
                            <XCircle size={14} />
                            {(t('syncPanel.canaryErrors') || '{count} errors detected').replace('{count}', String(result.summary.errors))}
                        </div>
                    )}
                </div>

                {/* File List Table */}
                <div className="px-5 py-2 flex-1 min-h-0 overflow-hidden">
                    <div className="overflow-auto max-h-64 rounded-lg border border-gray-200 dark:border-gray-700">
                        <table className="w-full text-xs">
                            <thead className="bg-gray-50 dark:bg-gray-800 sticky top-0">
                                <tr>
                                    <th className="text-left px-3 py-2 font-medium text-gray-500 dark:text-gray-400">
                                        {t('syncPanel.colFile') || 'File'}
                                    </th>
                                    <th className="text-left px-3 py-2 font-medium text-gray-500 dark:text-gray-400 w-24">
                                        {t('syncPanel.canaryAction') || 'Action'}
                                    </th>
                                    <th className="text-right px-3 py-2 font-medium text-gray-500 dark:text-gray-400 w-20">
                                        {t('syncPanel.size') || 'Size'}
                                    </th>
                                    <th className="text-center px-3 py-2 font-medium text-gray-500 dark:text-gray-400 w-16">
                                        {t('syncPanel.canaryStatus') || 'Status'}
                                    </th>
                                </tr>
                            </thead>
                            <tbody className="divide-y divide-gray-100 dark:divide-gray-700/50">
                                {sortedResults.map((item, i) => {
                                    const badge = ACTION_BADGE[item.action] || ACTION_BADGE.skip;
                                    return (
                                        <tr
                                            key={i}
                                            className="hover:bg-gray-50 dark:hover:bg-gray-700/30 transition-colors"
                                            title={item.error || undefined}
                                        >
                                            <td className="px-3 py-1.5 text-gray-700 dark:text-gray-300 truncate max-w-[250px]">
                                                {item.relative_path}
                                            </td>
                                            <td className="px-3 py-1.5">
                                                <span className={`inline-flex items-center gap-1 px-1.5 py-0.5 rounded text-[10px] font-medium ${badge.bg} ${badge.text}`}>
                                                    <badge.Icon size={10} />
                                                    {item.action}
                                                </span>
                                            </td>
                                            <td className="px-3 py-1.5 text-right text-gray-500 dark:text-gray-400 tabular-nums">
                                                {item.bytes > 0 ? formatSize(item.bytes) : '\u2014'}
                                            </td>
                                            <td className="px-3 py-1.5 text-center" title={item.error || undefined}>
                                                {item.success ? (
                                                    <CheckCircle2 size={14} className="inline text-emerald-400" />
                                                ) : (
                                                    <XCircle size={14} className="inline text-red-400" />
                                                )}
                                            </td>
                                        </tr>
                                    );
                                })}
                                {sortedResults.length === 0 && (
                                    <tr>
                                        <td colSpan={4} className="px-3 py-4 text-center text-gray-400">
                                            {t('syncPanel.canaryNoResults') || 'No sampled files'}
                                        </td>
                                    </tr>
                                )}
                            </tbody>
                        </table>
                    </div>
                </div>

                {/* Projection */}
                <div className="px-5 py-2 flex-shrink-0">
                    <div className="rounded-lg bg-blue-500/5 dark:bg-blue-500/10 border border-blue-200/50 dark:border-blue-500/20 p-3 text-xs text-gray-600 dark:text-gray-300">
                        <FlaskConical size={12} className="inline mr-1.5 text-blue-400" />
                        {(t('syncPanel.canaryProjection') || 'Based on {percent}% sample, full sync would transfer ~{files} files ({size})')
                            .replace('{percent}', String(samplePercent))
                            .replace('{files}', String(projectedFiles))
                            .replace('{size}', formatSize(projectedSize))
                        }
                    </div>
                </div>

                {/* Footer Buttons */}
                <div className="flex items-center justify-end gap-2 px-5 py-3 border-t border-gray-200 dark:border-gray-700 flex-shrink-0">
                    <button
                        className="text-xs px-4 py-1.5 rounded-lg bg-gray-200 dark:bg-gray-700 hover:bg-gray-300 dark:hover:bg-gray-600 text-gray-700 dark:text-gray-300 transition-colors"
                        onClick={onClose}
                    >
                        {t('common.cancel') || 'Cancel'}
                    </button>
                    <button
                        className="text-xs px-5 py-1.5 rounded-lg bg-emerald-600 hover:bg-emerald-700 text-white font-medium transition-colors shadow-sm"
                        onClick={onApprove}
                    >
                        <Zap size={12} className="inline mr-1" />
                        {t('syncPanel.canaryApproveFullSync') || 'Approve Full Sync'}
                    </button>
                </div>
            </div>
        </div>
    );
};

