// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
//
// CO-6: Journal History launcher inside AeroSync. Replaces the journal
// view that used to live inside the legacy SyncPanel; reuses the
// existing `list_sync_journals_cmd` / `load_sync_journal_cmd` /
// `delete_sync_journal_cmd` / `cleanup_old_journals_cmd` /
// `clear_all_journals_cmd` Tauri commands plus the per-pair
// `verify_journal_signature` (gated by `get_journal_signing_key`).

import React, { useCallback, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import {
    X, History, Trash2, AlertTriangle, CheckCircle2, RefreshCw, ShieldCheck, ShieldAlert, Eraser,
} from 'lucide-react';
import { useTranslation } from '../../i18n';
import { useDraggableModal } from '../../hooks/useDraggableModal';
import type {
    JournalSummary,
    SyncJournal,
    SyncJournalEntry,
} from '../../types';
import { formatDate } from '../../utils/formatters';
import { logger } from '../../utils/logger';

interface JournalHistoryDialogProps {
    isOpen: boolean;
    onClose: () => void;
    /** When provided, the list is filtered to journals that share this
     *  local path. The dialog still lists every journal returned by
     *  the backend; the filter is purely visual. */
    localPath?: string;
    /** When provided, the list is filtered to journals that share this
     *  remote path. Same semantics as `localPath`. */
    remotePath?: string;
}

type VerifyState = 'idle' | 'verifying' | 'ok' | 'failed' | 'no-signature';

function formatBytes(n: number): string {
    if (!Number.isFinite(n) || n <= 0) return '0 B';
    if (n < 1024) return `${n} B`;
    if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KiB`;
    if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MiB`;
    return `${(n / 1024 / 1024 / 1024).toFixed(2)} GiB`;
}

function totalBytes(entries: SyncJournalEntry[]): number {
    return entries.reduce((sum, e) => sum + (e.bytes_transferred || 0), 0);
}

const ENTRY_STATUS_TINT: Record<string, string> = {
    completed: 'text-emerald-600 dark:text-emerald-300',
    failed: 'text-rose-600 dark:text-rose-300',
    verify_failed: 'text-amber-600 dark:text-amber-300',
    skipped: 'text-gray-500 dark:text-gray-400',
    pending: 'text-gray-500 dark:text-gray-400',
    in_progress: 'text-blue-600 dark:text-blue-300',
};

export const JournalHistoryDialog: React.FC<JournalHistoryDialogProps> = ({
    isOpen,
    onClose,
    localPath,
    remotePath,
}) => {
    const t = useTranslation();
    const modalDrag = useDraggableModal();
    const [summaries, setSummaries] = useState<JournalSummary[]>([]);
    const [loading, setLoading] = useState(false);
    const [selected, setSelected] = useState<JournalSummary | null>(null);
    const [journal, setJournal] = useState<SyncJournal | null>(null);
    const [journalLoading, setJournalLoading] = useState(false);
    const [verifyState, setVerifyState] = useState<Record<string, VerifyState>>({});
    const [busy, setBusy] = useState(false);

    useEffect(() => {
        if (!isOpen) return;
        const handler = (e: KeyboardEvent) => { if (e.key === 'Escape') onClose(); };
        window.addEventListener('keydown', handler);
        return () => window.removeEventListener('keydown', handler);
    }, [isOpen, onClose]);

    const loadList = useCallback(async () => {
        setLoading(true);
        try {
            const list = await invoke<JournalSummary[]>('list_sync_journals_cmd');
            list.sort((a, b) => (b.updated_at || '').localeCompare(a.updated_at || ''));
            setSummaries(list);
        } catch (e) {
            logger.error('[JournalHistory] list failed:', e);
            setSummaries([]);
        } finally {
            setLoading(false);
        }
    }, []);

    useEffect(() => {
        if (!isOpen) return;
        void loadList();
        setSelected(null);
        setJournal(null);
        setVerifyState({});
    }, [isOpen, loadList]);

    const openJournal = async (s: JournalSummary) => {
        setSelected(s);
        setJournal(null);
        setJournalLoading(true);
        try {
            const j = await invoke<SyncJournal | null>('load_sync_journal_cmd', {
                localPath: s.local_path,
                remotePath: s.remote_path,
            });
            setJournal(j);
        } catch (e) {
            logger.error('[JournalHistory] load failed:', e);
        } finally {
            setJournalLoading(false);
        }
    };

    const deleteJournal = async (s: JournalSummary) => {
        try {
            await invoke('delete_sync_journal_cmd', {
                localPath: s.local_path,
                remotePath: s.remote_path,
            });
            if (selected
                && selected.local_path === s.local_path
                && selected.remote_path === s.remote_path) {
                setSelected(null);
                setJournal(null);
            }
            await loadList();
        } catch (e) {
            logger.error('[JournalHistory] delete failed:', e);
        }
    };

    const cleanupOld = async () => {
        setBusy(true);
        try {
            await invoke('cleanup_old_journals_cmd', { maxAgeDays: 30 });
            await loadList();
        } catch (e) {
            logger.error('[JournalHistory] cleanup failed:', e);
        } finally {
            setBusy(false);
        }
    };

    const clearAll = async () => {
        if (!window.confirm(t('aerosync.history.confirmClearAll') || 'Clear ALL sync journals?')) return;
        setBusy(true);
        try {
            await invoke('clear_all_journals_cmd');
            setSelected(null);
            setJournal(null);
            await loadList();
        } catch (e) {
            logger.error('[JournalHistory] clear-all failed:', e);
        } finally {
            setBusy(false);
        }
    };

    const verifyJournal = async (s: JournalSummary) => {
        const key = `${s.local_path}|${s.remote_path}`;
        setVerifyState(prev => ({ ...prev, [key]: 'verifying' }));
        try {
            const signingKey = await invoke<string>('get_journal_signing_key', {
                localPath: s.local_path,
                remotePath: s.remote_path,
            });
            const ok = await invoke<boolean>('verify_journal_signature', {
                localPath: s.local_path,
                remotePath: s.remote_path,
                signingKey,
            });
            setVerifyState(prev => ({ ...prev, [key]: ok ? 'ok' : 'failed' }));
        } catch (e) {
            // Missing signature file is the common case for journals that were
            // never signed: surface a distinct state so the dialog can show
            // "no signature" instead of "failed".
            const msg = String(e || '').toLowerCase();
            const noSig = msg.includes('signature file') || msg.includes('no such file');
            setVerifyState(prev => ({ ...prev, [key]: noSig ? 'no-signature' : 'failed' }));
        }
    };

    if (!isOpen) return null;

    const filtered = summaries.filter(s => {
        if (localPath && s.local_path !== localPath) return false;
        if (remotePath && s.remote_path !== remotePath) return false;
        return true;
    });

    const renderVerifyBadge = (state: VerifyState) => {
        if (state === 'verifying') {
            return (
                <span className="rounded bg-gray-100 px-1.5 py-0.5 text-[10px] font-semibold text-gray-600 dark:bg-gray-700 dark:text-gray-300">
                    ...
                </span>
            );
        }
        if (state === 'ok') {
            return (
                <span className="inline-flex items-center gap-1 rounded bg-emerald-100 px-1.5 py-0.5 text-[10px] font-semibold text-emerald-700 dark:bg-emerald-900/40 dark:text-emerald-300">
                    <ShieldCheck size={10} /> {t('aerosync.history.verifyOk') || 'VERIFIED'}
                </span>
            );
        }
        if (state === 'failed') {
            return (
                <span className="inline-flex items-center gap-1 rounded bg-rose-100 px-1.5 py-0.5 text-[10px] font-semibold text-rose-700 dark:bg-rose-900/40 dark:text-rose-300">
                    <ShieldAlert size={10} /> {t('aerosync.history.verifyFailed') || 'TAMPERED'}
                </span>
            );
        }
        if (state === 'no-signature') {
            return (
                <span className="rounded bg-gray-100 px-1.5 py-0.5 text-[10px] font-semibold text-gray-500 dark:bg-gray-700 dark:text-gray-400">
                    {t('aerosync.history.verifyNoSignature') || 'NO SIG'}
                </span>
            );
        }
        return null;
    };

    return (
        <div
            className="fixed inset-0 z-[9998] flex items-start justify-center pt-[5vh] bg-black/50 animate-scale-in"
            role="dialog"
            aria-modal="true"
            aria-label={t('aerosync.history.title') || 'AeroSync history'}
            onClick={onClose}
        >
            <div
                {...modalDrag.panelProps}
                className="relative bg-white dark:bg-gray-800 rounded-lg shadow-2xl w-full max-w-4xl max-h-[90vh] overflow-hidden flex flex-col"
                onClick={(e) => e.stopPropagation()}
            >
                <div
                    {...modalDrag.dragHandleProps}
                    className="flex items-center justify-between p-4 border-b border-gray-200 dark:border-gray-700 cursor-grab active:cursor-grabbing"
                >
                    <div className="flex items-center gap-2 pointer-events-none">
                        <History size={18} className="text-blue-500" />
                        <h2 className="text-base font-semibold">
                            {t('aerosync.history.title') || 'AeroSync history'}
                        </h2>
                    </div>
                    <div className="flex items-center gap-1">
                        <button
                            type="button"
                            onClick={() => void loadList()}
                            disabled={loading || busy}
                            className="p-1.5 hover:bg-gray-100 dark:hover:bg-gray-700 rounded-lg transition-colors disabled:opacity-50"
                            title={t('aerosync.history.refresh') || 'Refresh'}
                        >
                            <RefreshCw size={14} className={loading ? 'animate-spin' : ''} />
                        </button>
                        <button
                            type="button"
                            onClick={() => void cleanupOld()}
                            disabled={loading || busy || summaries.length === 0}
                            className="p-1.5 hover:bg-gray-100 dark:hover:bg-gray-700 rounded-lg transition-colors disabled:opacity-50"
                            title={t('aerosync.history.cleanupOld') || 'Remove entries older than 30 days'}
                        >
                            <Eraser size={14} />
                        </button>
                        <button
                            type="button"
                            onClick={() => void clearAll()}
                            disabled={loading || busy || summaries.length === 0}
                            className="p-1.5 hover:bg-gray-100 dark:hover:bg-gray-700 rounded-lg transition-colors disabled:opacity-50 text-rose-600 dark:text-rose-300"
                            title={t('aerosync.history.clearAll') || 'Clear all journals'}
                        >
                            <Trash2 size={14} />
                        </button>
                        <button
                            type="button"
                            onClick={onClose}
                            aria-label={t('common.close') || 'Close'}
                            className="p-1.5 hover:bg-gray-100 dark:hover:bg-gray-700 rounded-lg transition-colors"
                        >
                            <X size={16} />
                        </button>
                    </div>
                </div>

                <div className="flex flex-1 min-h-0">
                    <div className="w-1/2 border-r border-gray-200 dark:border-gray-700 flex flex-col">
                        {loading ? (
                            <div className="flex-1 flex items-center justify-center text-sm text-gray-500 dark:text-gray-400">
                                {t('common.loading') || 'Loading...'}
                            </div>
                        ) : filtered.length === 0 ? (
                            <div className="flex-1 flex items-center justify-center text-sm text-gray-500 dark:text-gray-400 italic px-4 text-center">
                                {t('aerosync.history.empty') || 'No sync journals yet. They are created on the first preset execution against this path pair.'}
                            </div>
                        ) : (
                            <ul className="overflow-y-auto flex-1 text-sm">
                                {filtered.map(s => {
                                    const key = `${s.local_path}|${s.remote_path}`;
                                    const active = !!selected
                                        && selected.local_path === s.local_path
                                        && selected.remote_path === s.remote_path;
                                    const pct = s.total_entries > 0
                                        ? Math.round((s.completed_entries / s.total_entries) * 100)
                                        : 0;
                                    return (
                                        <li
                                            key={key}
                                            className={`px-3 py-2 border-b border-gray-100 dark:border-gray-700/60 cursor-pointer transition-colors ${
                                                active
                                                    ? 'bg-blue-50 dark:bg-blue-900/30'
                                                    : 'hover:bg-gray-50 dark:hover:bg-gray-700/50'
                                            }`}
                                            onClick={() => void openJournal(s)}
                                        >
                                            <div className="flex items-center justify-between gap-2">
                                                <div className="text-xs text-gray-500 dark:text-gray-400">
                                                    {formatDate(s.updated_at)}
                                                </div>
                                                <div className="flex items-center gap-1">
                                                    {s.completed ? (
                                                        <span className="inline-flex items-center gap-1 rounded bg-emerald-100 px-1.5 py-0.5 text-[10px] font-semibold text-emerald-700 dark:bg-emerald-900/40 dark:text-emerald-300">
                                                            <CheckCircle2 size={10} /> {t('aerosync.history.statusCompleted') || 'COMPLETED'}
                                                        </span>
                                                    ) : (
                                                        <span className="inline-flex items-center gap-1 rounded bg-amber-100 px-1.5 py-0.5 text-[10px] font-semibold text-amber-700 dark:bg-amber-900/40 dark:text-amber-300">
                                                            <AlertTriangle size={10} /> {t('aerosync.history.statusInProgress') || 'IN PROGRESS'}
                                                        </span>
                                                    )}
                                                    {renderVerifyBadge(verifyState[key] || 'idle')}
                                                </div>
                                            </div>
                                            <div className="mt-1 text-xs font-mono text-gray-700 dark:text-gray-200 truncate" title={s.local_path}>
                                                {s.local_path}
                                            </div>
                                            <div className="text-xs font-mono text-gray-700 dark:text-gray-200 truncate" title={s.remote_path}>
                                                {s.remote_path}
                                            </div>
                                            <div className="mt-1 flex items-center justify-between gap-2 text-[11px] text-gray-500 dark:text-gray-400">
                                                <span>
                                                    {s.completed_entries} / {s.total_entries} ({pct}%)
                                                </span>
                                                <div className="flex items-center gap-1">
                                                    <button
                                                        type="button"
                                                        onClick={(e) => {
                                                            e.stopPropagation();
                                                            void verifyJournal(s);
                                                        }}
                                                        className="px-1.5 py-0.5 text-[10px] rounded border border-gray-300 dark:border-gray-600 hover:bg-gray-100 dark:hover:bg-gray-700"
                                                        title={t('aerosync.history.verify') || 'Verify signature'}
                                                    >
                                                        {t('aerosync.history.verify') || 'Verify'}
                                                    </button>
                                                    <button
                                                        type="button"
                                                        onClick={(e) => {
                                                            e.stopPropagation();
                                                            void deleteJournal(s);
                                                        }}
                                                        className="px-1.5 py-0.5 text-[10px] rounded border border-rose-200 text-rose-600 dark:border-rose-800 dark:text-rose-300 hover:bg-rose-50 dark:hover:bg-rose-900/20"
                                                        title={t('aerosync.history.delete') || 'Delete journal'}
                                                    >
                                                        <Trash2 size={10} />
                                                    </button>
                                                </div>
                                            </div>
                                        </li>
                                    );
                                })}
                            </ul>
                        )}
                    </div>
                    <div className="w-1/2 flex flex-col overflow-hidden">
                        {!selected ? (
                            <div className="flex-1 flex items-center justify-center text-sm text-gray-500 dark:text-gray-400 italic px-4 text-center">
                                {t('aerosync.history.selectPrompt') || 'Pick a journal on the left to inspect its per-file entries.'}
                            </div>
                        ) : journalLoading ? (
                            <div className="flex-1 flex items-center justify-center text-sm text-gray-500 dark:text-gray-400">
                                {t('common.loading') || 'Loading...'}
                            </div>
                        ) : !journal ? (
                            <div className="flex-1 flex items-center justify-center text-sm text-gray-500 dark:text-gray-400 italic px-4 text-center">
                                {t('aerosync.history.journalMissing') || 'Journal not found.'}
                            </div>
                        ) : (
                            <>
                                <div className="px-3 py-2 border-b border-gray-200 dark:border-gray-700 text-xs text-gray-600 dark:text-gray-300 space-y-0.5">
                                    <div>
                                        <span className="font-semibold">{t('aerosync.history.colDirection') || 'Direction'}:</span> {journal.direction}
                                    </div>
                                    <div>
                                        <span className="font-semibold">{t('aerosync.history.created') || 'Created'}:</span> {formatDate(journal.created_at)}
                                    </div>
                                    <div>
                                        <span className="font-semibold">{t('aerosync.history.bytes') || 'Bytes'}:</span> {formatBytes(totalBytes(journal.entries))}
                                    </div>
                                </div>
                                <div className="flex-1 overflow-y-auto">
                                    <table className="w-full text-[11px]">
                                        <thead className="sticky top-0 bg-gray-100 dark:bg-gray-800 text-left text-gray-600 dark:text-gray-300">
                                            <tr>
                                                <th className="px-2 py-1 font-medium">
                                                    {t('aerosync.history.colPath') || 'Path'}
                                                </th>
                                                <th className="px-2 py-1 font-medium">
                                                    {t('aerosync.history.colAction') || 'Action'}
                                                </th>
                                                <th className="px-2 py-1 font-medium">
                                                    {t('aerosync.history.colStatus') || 'Status'}
                                                </th>
                                                <th className="px-2 py-1 font-medium text-right">
                                                    {t('aerosync.history.colBytes') || 'Bytes'}
                                                </th>
                                            </tr>
                                        </thead>
                                        <tbody>
                                            {journal.entries.length === 0 ? (
                                                <tr>
                                                    <td colSpan={4} className="px-2 py-3 text-center text-gray-500 dark:text-gray-400 italic">
                                                        {t('aerosync.history.entriesEmpty') || 'No entries recorded.'}
                                                    </td>
                                                </tr>
                                            ) : journal.entries.map((e, i) => (
                                                <tr key={i} className="border-t border-gray-100 dark:border-gray-700/60">
                                                    <td className="px-2 py-1 font-mono text-gray-800 dark:text-gray-200 break-all" title={e.relative_path}>
                                                        {e.relative_path}
                                                    </td>
                                                    <td className="px-2 py-1 text-gray-700 dark:text-gray-300">
                                                        {e.action}
                                                    </td>
                                                    <td className={`px-2 py-1 ${ENTRY_STATUS_TINT[e.status] || 'text-gray-700 dark:text-gray-300'}`}>
                                                        {e.status}
                                                        {e.last_error && (
                                                            <span className="ml-1 inline-flex items-center text-rose-500" title={e.last_error.message || e.last_error.kind}>
                                                                <AlertTriangle size={10} />
                                                            </span>
                                                        )}
                                                    </td>
                                                    <td className="px-2 py-1 text-right font-mono text-gray-600 dark:text-gray-300">
                                                        {e.bytes_transferred > 0 ? formatBytes(e.bytes_transferred) : '-'}
                                                    </td>
                                                </tr>
                                            ))}
                                        </tbody>
                                    </table>
                                </div>
                            </>
                        )}
                    </div>
                </div>
            </div>
        </div>
    );
};

export default JournalHistoryDialog;
