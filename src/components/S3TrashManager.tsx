// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { useState, useEffect, useCallback, useMemo } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Trash2, RotateCcw, AlertTriangle, X, RefreshCw, Loader2, File, Undo2, CornerUpLeft } from 'lucide-react';
import { useTranslation } from '../i18n';
import { useDraggableModal } from '../hooks/useDraggableModal';
import { TrashTable, type TrashRow } from './Trash/TrashTable';
import { useHumanizedLog } from '../hooks/useHumanizedLog';
import { formatSize, formatDate } from '../utils/formatters';

/** One row from `s3_list_trash` (mirrors the Rust `TrashEntry`). */
interface TrashEntry {
  /** Raw backend key: round-tripped verbatim on every operation (never shown). */
  key: string;
  /** Human-facing key (decrypted under a crypt overlay); render this. */
  display_key: string;
  version_id: string;
  is_delete_marker: boolean;
  is_latest: boolean;
  size: number;
  last_modified: string | null;
}

interface EmptyTrashSummary {
  count: number;
  bytes: number;
  dry_run: boolean;
}

interface S3TrashManagerProps {
  onClose: () => void;
  onRefreshFiles?: () => void;
}

/** Restore mode routed to `s3_restore_from_trash`. */
type RestoreMode = 'undelete' | 'copy_forward' | 'purge';

export function S3TrashManager({ onClose, onRefreshFiles }: S3TrashManagerProps) {
  const t = useTranslation();
  const modalDrag = useDraggableModal();
  const humanLog = useHumanizedLog();
  const [items, setItems] = useState<TrashEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [includeNoncurrent, setIncludeNoncurrent] = useState(false);
  // Row currently running an operation, keyed by version_id, plus the mode.
  const [actionKey, setActionKey] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  // Empty-trash: null (idle), 'previewing' (dry-run in flight), or a summary to confirm.
  const [emptyPreview, setEmptyPreview] = useState<EmptyTrashSummary | null>(null);
  const [emptyLoading, setEmptyLoading] = useState(false);
  // Per-version purge confirmation (irreversible).
  const [pendingPurge, setPendingPurge] = useState<TrashEntry | null>(null);

  const loadTrash = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const result = await invoke<TrashEntry[]>('s3_list_trash', {
        prefix: '',
        includeNoncurrent,
      });
      setItems(result);
    } catch (err) {
      setError(String(err));
    } finally {
      setLoading(false);
    }
  }, [includeNoncurrent]);

  useEffect(() => {
    loadTrash();
  }, [loadTrash]);

  useEffect(() => {
    const handleKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', handleKey);
    return () => window.removeEventListener('keydown', handleKey);
  }, [onClose]);

  const restore = async (entry: TrashEntry, mode: RestoreMode) => {
    setActionKey(entry.version_id);
    setError(null);
    const label = mode === 'purge' ? 'purge' : mode === 'undelete' ? 'undelete' : 'restore';
    const logId = humanLog.logRaw('activity.trash_restore_start', 'INFO', { provider: 'S3', count: 1 });
    try {
      await invoke('s3_restore_from_trash', {
        key: entry.key,
        versionId: entry.version_id,
        mode,
      });
      humanLog.updateEntry(logId, { status: 'success', message: `[S3] ${label} ${entry.display_key}` });
      await loadTrash();
      if (mode !== 'purge') onRefreshFiles?.();
    } catch (err) {
      humanLog.updateEntry(logId, { status: 'error', message: `[S3] Failed to ${label} ${entry.display_key}` });
      setError(String(err));
    } finally {
      setActionKey(null);
    }
  };

  const confirmPurge = async () => {
    const entry = pendingPurge;
    setPendingPurge(null);
    if (entry) await restore(entry, 'purge');
  };

  // Step 1: dry-run to preview the count/bytes, then surface the confirm dialog.
  const previewEmptyTrash = async () => {
    setEmptyLoading(true);
    setError(null);
    try {
      const summary = await invoke<EmptyTrashSummary>('s3_empty_trash', {
        prefix: '',
        includeNoncurrent,
        dryRun: true,
      });
      setEmptyPreview(summary);
    } catch (err) {
      setError(String(err));
    } finally {
      setEmptyLoading(false);
    }
  };

  // Step 2: execute the sweep for real once the user approves.
  const confirmEmptyTrash = async () => {
    setEmptyPreview(null);
    const logId = humanLog.logRaw('activity.trash_empty_start', 'INFO', { provider: 'S3', count: 0 });
    setEmptyLoading(true);
    setError(null);
    try {
      const summary = await invoke<EmptyTrashSummary>('s3_empty_trash', {
        prefix: '',
        includeNoncurrent,
        dryRun: false,
      });
      humanLog.updateEntry(logId, { status: 'success', message: `[S3] Emptied trash (${summary.count} object(s))` });
      await loadTrash();
      onRefreshFiles?.();
    } catch (err) {
      humanLog.updateEntry(logId, { status: 'error', message: `[S3] Failed to empty trash` });
      setError(String(err));
    } finally {
      setEmptyLoading(false);
    }
  };

  const busy = actionKey !== null || emptyLoading;

  // S3's trash is the bucket's version history: every row is an object version
  // or a delete marker, never a folder, and each carries its own actions. It
  // joins the shared table read-only (no bulk selection here) with Kind and
  // Actions as extra columns, which is what buys it the sorting it lacked.
  const byRowId = useMemo(
    () => new Map(items.map(item => [`${item.key}#${item.version_id}`, item])),
    [items],
  );
  const trashRows: TrashRow[] = useMemo(
    () => items.map(item => ({
      id: `${item.key}#${item.version_id}`,
      name: item.display_key,
      isDir: false,
      size: item.is_delete_marker ? null : item.size,
      deletedAt: item.last_modified,
    })),
    [items],
  );
  const s3Columns = useMemo(
    () => [
      {
        key: 'kind',
        header: t('contextMenu.s3TrashKind') || 'Kind',
        className: 'w-24',
        render: (row: TrashRow) => {
          const item = byRowId.get(row.id);
          if (!item) return null;
          return (
            <span className="flex items-center gap-1">
              {item.is_delete_marker ? (
                <span className="text-[10px] bg-red-100 dark:bg-red-900/40 text-red-600 dark:text-red-400 px-1.5 py-0.5 rounded whitespace-nowrap">
                  {t('contextMenu.s3TrashDeleteMarker') || 'Deleted'}
                </span>
              ) : (
                <span className="text-[10px] bg-gray-100 dark:bg-gray-700 text-gray-600 dark:text-gray-300 px-1.5 py-0.5 rounded whitespace-nowrap">
                  {t('contextMenu.s3TrashVersion') || 'Version'}
                </span>
              )}
              {item.is_latest && (
                <span className="text-[10px] bg-sky-100 dark:bg-sky-900/40 text-sky-600 dark:text-sky-400 px-1.5 py-0.5 rounded whitespace-nowrap">
                  {t('contextMenu.s3TrashLatest') || 'Latest'}
                </span>
              )}
            </span>
          );
        },
      },
      {
        key: 'actions',
        header: t('versions.actions') || 'Actions',
        className: 'w-40 text-right',
        render: (row: TrashRow) => {
          const item = byRowId.get(row.id);
          if (!item) return null;
          const rowBusy = actionKey === item.version_id;
          return (
            <span className="flex items-center justify-end gap-1">
              {item.is_delete_marker ? (
                <button
                  onClick={() => restore(item, 'undelete')}
                  disabled={busy}
                  className="flex items-center gap-1 px-2 py-1 text-[11px] rounded bg-sky-600 text-white hover:bg-sky-700 disabled:opacity-40 disabled:cursor-not-allowed"
                  title={t('contextMenu.s3TrashUndeleteHint') || 'Drop this delete marker so the object reappears'}
                >
                  {rowBusy ? <Loader2 size={11} className="animate-spin" /> : <Undo2 size={11} />}
                  {t('contextMenu.s3TrashUndelete') || 'Undelete'}
                </button>
              ) : (
                !item.is_latest && (
                  <button
                    onClick={() => restore(item, 'copy_forward')}
                    disabled={busy}
                    className="flex items-center gap-1 px-2 py-1 text-[11px] rounded bg-sky-600 text-white hover:bg-sky-700 disabled:opacity-40 disabled:cursor-not-allowed"
                    title={t('contextMenu.s3TrashCopyForwardHint') || 'Copy this older version forward to become current'}
                  >
                    {rowBusy ? <Loader2 size={11} className="animate-spin" /> : <CornerUpLeft size={11} />}
                    {t('contextMenu.s3TrashCopyForward') || 'Restore'}
                  </button>
                )
              )}
              <button
                onClick={() => setPendingPurge(item)}
                disabled={busy}
                className="flex items-center gap-1 px-2 py-1 text-[11px] rounded bg-red-600 text-white hover:bg-red-700 disabled:opacity-40 disabled:cursor-not-allowed"
                title={t('contextMenu.s3TrashPurgeHint') || 'Permanently delete this version (irreversible)'}
              >
                {rowBusy ? <Loader2 size={11} className="animate-spin" /> : <Trash2 size={11} />}
                {t('contextMenu.s3TrashPurge') || 'Purge'}
              </button>
            </span>
          );
        },
      },
    ],
    [byRowId, actionKey, busy, restore, t],
  );

  return (
    <div className="fixed inset-0 z-[9999] flex items-center justify-center bg-black/50" onClick={onClose}>
      <div
        {...modalDrag.panelProps}
        className="bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg shadow-2xl w-[760px] max-h-[80vh] flex flex-col animate-scale-in"
        onClick={e => e.stopPropagation()}
        role="dialog"
        aria-modal="true"
        aria-label={t('contextMenu.s3TrashTitle') || 'S3 Trash / Versions'}
      >
        <div {...modalDrag.dragHandleProps} className="flex items-center justify-between px-4 py-3 border-b border-gray-200 dark:border-gray-700 cursor-grab active:cursor-grabbing">
          <div className="flex items-center gap-2">
            <Trash2 size={18} className="text-sky-500" />
            <h2 className="text-sm font-semibold text-gray-900 dark:text-gray-100">
              {t('contextMenu.s3TrashTitle') || 'S3 Trash / Versions'}
            </h2>
            <span className="text-xs text-gray-500 dark:text-gray-500">({items.length})</span>
          </div>
          <div className="flex items-center gap-1">
            <button
              onClick={loadTrash}
              disabled={loading}
              className="p-1.5 rounded hover:bg-gray-100 dark:hover:bg-gray-700 text-gray-600 dark:text-gray-400"
              title="Refresh"
            >
              <RefreshCw size={14} className={loading ? 'animate-spin' : ''} />
            </button>
            <button
              onClick={onClose}
              className="p-1.5 rounded hover:bg-gray-100 dark:hover:bg-gray-700 text-gray-600 dark:text-gray-400"
            >
              <X size={14} />
            </button>
          </div>
        </div>

        <div className="flex items-center gap-3 px-4 py-2 border-b border-gray-200 dark:border-gray-700 bg-gray-50 dark:bg-gray-800">
          <label className="flex items-center gap-1.5 text-xs text-gray-600 dark:text-gray-400 cursor-pointer select-none">
            <input
              type="checkbox"
              checked={includeNoncurrent}
              onChange={e => setIncludeNoncurrent(e.target.checked)}
              className="accent-sky-500"
            />
            {t('contextMenu.s3TrashShowAllVersions') || 'Show all versions'}
          </label>
          <div className="flex-1" />
          <button
            onClick={previewEmptyTrash}
            disabled={busy || items.length === 0}
            className="flex items-center gap-1.5 px-3 py-1 text-xs rounded bg-amber-600 text-white hover:bg-amber-700 disabled:opacity-40 disabled:cursor-not-allowed"
          >
            {emptyLoading ? <Loader2 size={12} className="animate-spin" /> : <AlertTriangle size={12} />}
            {t('contextMenu.emptyTrash')}
          </button>
        </div>

        <div className="flex-1 overflow-y-auto min-h-0">
          {loading ? (
            <div className="flex items-center justify-center py-12 text-gray-600 dark:text-gray-400">
              <Loader2 size={20} className="animate-spin mr-2" />
              {t('contextMenu.trashLoading')}
            </div>
          ) : error ? (
            <div className="flex items-center justify-center py-12 text-red-500">
              <AlertTriangle size={16} className="mr-2" />
              {error}
            </div>
          ) : items.length === 0 ? (
            <div className="flex flex-col items-center justify-center py-12 text-gray-500 dark:text-gray-500 text-center px-6">
              <Trash2 size={32} className="mb-2 opacity-30" />
              {t('contextMenu.s3TrashEmptyOrVersioningOff') || 'Trash empty (or bucket versioning is off)'}
            </div>
          ) : (
            <TrashTable
              rows={trashRows}
              showTypeColumn={false}
              extraColumns={s3Columns}
            />
          )}
        </div>
      </div>

      {/* Per-version purge confirmation */}
      {pendingPurge && (
        <div className="fixed inset-0 z-[10000] bg-black/50 flex items-center justify-center" role="dialog" aria-modal="true" onClick={() => setPendingPurge(null)}>
          <div className="bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg p-6 shadow-2xl max-w-sm animate-scale-in" onClick={e => e.stopPropagation()}>
            <div className="flex items-start gap-2 mb-4">
              <AlertTriangle size={18} className="text-red-500 shrink-0 mt-0.5" />
              <p className="text-gray-900 dark:text-gray-100 text-sm">
                {t('contextMenu.s3TrashPurgeConfirm') || 'Permanently delete this version? This cannot be undone.'}
                <span className="block mt-1 text-xs text-gray-500 dark:text-gray-400 truncate">{pendingPurge.display_key}</span>
              </p>
            </div>
            <div className="flex justify-end gap-2">
              <button
                onClick={() => setPendingPurge(null)}
                className="px-3 py-1.5 text-sm rounded bg-gray-200 hover:bg-gray-300 dark:bg-gray-700 dark:hover:bg-gray-600 text-gray-900 dark:text-gray-100"
              >
                {t('common.cancel')}
              </button>
              <button
                onClick={confirmPurge}
                className="px-3 py-1.5 text-sm rounded bg-red-600 hover:bg-red-700 text-white"
              >
                {t('contextMenu.s3TrashPurge') || 'Purge'}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Empty-trash dry-run summary + confirmation */}
      {emptyPreview && (
        <div className="fixed inset-0 z-[10000] bg-black/50 flex items-center justify-center" role="dialog" aria-modal="true" onClick={() => setEmptyPreview(null)}>
          <div className="bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg p-6 shadow-2xl max-w-md animate-scale-in" onClick={e => e.stopPropagation()}>
            <div className="flex items-start gap-2 mb-4">
              <AlertTriangle size={18} className="text-amber-500 shrink-0 mt-0.5" />
              <div className="text-sm text-gray-900 dark:text-gray-100">
                <p>
                  {(t('contextMenu.s3TrashEmptySummary') || 'This will permanently delete {count} object(s) ({bytes}). This cannot be undone.')
                    .replace('{count}', String(emptyPreview.count))
                    .replace('{bytes}', formatSize(emptyPreview.bytes))}
                </p>
              </div>
            </div>
            <div className="flex justify-end gap-2">
              <button
                onClick={() => setEmptyPreview(null)}
                className="px-3 py-1.5 text-sm rounded bg-gray-200 hover:bg-gray-300 dark:bg-gray-700 dark:hover:bg-gray-600 text-gray-900 dark:text-gray-100"
              >
                {t('common.cancel')}
              </button>
              <button
                onClick={confirmEmptyTrash}
                disabled={emptyPreview.count === 0}
                className="px-3 py-1.5 text-sm rounded bg-red-600 hover:bg-red-700 text-white disabled:opacity-40 disabled:cursor-not-allowed"
              >
                {t('contextMenu.emptyTrash')}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
