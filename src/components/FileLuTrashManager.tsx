// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { useState, useEffect, useCallback, useMemo } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Trash2, RotateCcw, X, RefreshCw, Loader2, File, CheckSquare, Square, Clock } from 'lucide-react';
import { useTranslation } from '../i18n';
import { useDraggableModal } from '../hooks/useDraggableModal';
import { TrashTable, type TrashRow } from './Trash/TrashTable';
import { fileLuDeletedAtLabel } from './Trash/fileLuDeletedLabel';
import { useHumanizedLog } from '../hooks/useHumanizedLog';

// FileLu confirmed permanent delete endpoint: api/file/permanent_delete?key=X&file_code=Y
const PERMANENT_DELETE_ENABLED = true;

interface DeletedFileEntry {
  file_code: string | null;
  name: string | null;
  deleted: string | null;
  deleted_ago_sec: number | null;
}

interface FileLuTrashManagerProps {
  onClose: () => void;
  onRefreshFiles?: () => void;
}

export function FileLuTrashManager({ onClose, onRefreshFiles }: FileLuTrashManagerProps) {
  const t = useTranslation();
  const modalDrag = useDraggableModal();
  const humanLog = useHumanizedLog();
  const [items, setItems] = useState<DeletedFileEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [actionLoading, setActionLoading] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const loadTrash = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const result = await invoke<DeletedFileEntry[]>('filelu_list_deleted');
      setItems(result);
      setSelected(new Set());
    } catch (err) {
      setError(String(err));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => { loadTrash(); }, [loadTrash]);

  const toggleSelect = (code: string) => {
    setSelected(prev => {
      const next = new Set(prev);
      next.has(code) ? next.delete(code) : next.add(code);
      return next;
    });
  };

  const toggleAll = () => {
    const all = items.map(i => i.file_code).filter(Boolean) as string[];
    setSelected(prev => prev.size === all.length ? new Set() : new Set(all));
  };

  const restoreSelected = async () => {
    if (selected.size === 0) return;
    const selectedCount = selected.size;
    const logId = humanLog.logRaw('activity.trash_restore_start', 'INFO', { provider: 'FileLu', count: selectedCount });
    setActionLoading('restore');
    try {
      for (const code of selected) {
        await invoke('filelu_restore_file', { fileCode: code });
      }
      humanLog.updateEntry(logId, { status: 'success', message: `[FileLu] Restored ${selectedCount} item(s) from trash` });
      await loadTrash();
      onRefreshFiles?.();
    } catch (err) {
      humanLog.updateEntry(logId, { status: 'error', message: `[FileLu] Failed to restore from trash` });
      setError(String(err));
    } finally {
      setActionLoading(null);
    }
  };

  const deleteSelected = async () => {
    if (selected.size === 0) return;
    const selectedCount = selected.size;
    const logId = humanLog.logRaw('activity.trash_delete_start', 'INFO', { provider: 'FileLu', count: selectedCount });
    setActionLoading('delete');
    try {
      for (const code of selected) {
        await invoke('filelu_permanent_delete', { fileCode: code });
      }
      humanLog.updateEntry(logId, { status: 'success', message: `[FileLu] Permanently deleted ${selectedCount} item(s) from trash` });
      await loadTrash();
    } catch (err) {
      humanLog.updateEntry(logId, { status: 'error', message: `[FileLu] Failed to permanently delete from trash` });
      setError(String(err));
    } finally {
      setActionLoading(null);
    }
  };

  const allCodes = items.map(i => i.file_code).filter(Boolean) as string[];
  const allSelected = allCodes.length > 0 && selected.size === allCodes.length;

  // FileLu reports no size and no folders in its trash, and its rows carry a
  // file code plus per-row restore/delete buttons: both ride along as extra
  // columns so this view gains the shared sorting and selection like the rest.
  const trashRows: TrashRow[] = useMemo(
    // An entry with no file code cannot be selected, restored or deleted — the
    // code IS the handle — and giving them all the id '' would collapse them
    // into one row with a duplicate React key. Dropped, like the two existing
    // `.filter(Boolean)` call sites in this file already do.
    () => items
      .filter((item): item is DeletedFileEntry & { file_code: string } => !!item.file_code)
      .map(item => ({
        id: item.file_code,
        name: item.name ?? item.file_code,
        isDir: false,
        size: null,
        deletedAt: item.deleted,
        deletedAtLabel: fileLuDeletedAtLabel(item.deleted, item.deleted_ago_sec),
      })),
    [items],
  );

  // Per-row restore/delete, in the same shape as restoreSelected/deleteSelected
  // above: an activity-log entry that settles, and a failure the user can see.
  // The inline handlers these replace had no rejection handler at all, so a
  // failed restore became an unhandled rejection and a silently stale list.
  const restoreOne = useCallback(async (code: string) => {
    const logId = humanLog.logRaw('activity.trash_restore_start', 'INFO', { provider: 'FileLu', count: 1 });
    setActionLoading('restore');
    try {
      await invoke('filelu_restore_file', { fileCode: code });
      humanLog.updateEntry(logId, { status: 'success', message: `[FileLu] Restored 1 item from trash` });
      await loadTrash();
      onRefreshFiles?.();
    } catch (err) {
      humanLog.updateEntry(logId, { status: 'error', message: `[FileLu] Failed to restore from trash` });
      setError(String(err));
    } finally {
      setActionLoading(null);
    }
  }, [humanLog, loadTrash, onRefreshFiles]);

  const deleteOne = useCallback(async (code: string) => {
    const logId = humanLog.logRaw('activity.trash_delete_start', 'INFO', { provider: 'FileLu', count: 1 });
    setActionLoading('delete');
    try {
      await invoke('filelu_permanent_delete', { fileCode: code });
      humanLog.updateEntry(logId, { status: 'success', message: `[FileLu] Permanently deleted 1 item from trash` });
      await loadTrash();
    } catch (err) {
      humanLog.updateEntry(logId, { status: 'error', message: `[FileLu] Failed to permanently delete from trash` });
      setError(String(err));
    } finally {
      setActionLoading(null);
    }
  }, [humanLog, loadTrash]);

  const fileLuColumns = useMemo(
    () => [
      {
        key: 'code',
        header: '',
        className: 'font-mono text-[10px] opacity-60 whitespace-nowrap',
        render: (row: TrashRow) => row.id,
      },
      {
        key: 'actions',
        header: '',
        className: 'w-16',
        render: (row: TrashRow) => (
          <span className="flex gap-1.5">
            <button
              onClick={e => { e.stopPropagation(); void restoreOne(row.id); }}
              disabled={actionLoading !== null}
              className="p-1 rounded text-emerald-500 hover:bg-emerald-500/10 transition-colors disabled:opacity-40"
              title={t('filelu.restore')}
            >
              <RotateCcw size={13} />
            </button>
            {PERMANENT_DELETE_ENABLED && (
              <button
                onClick={e => { e.stopPropagation(); void deleteOne(row.id); }}
                disabled={actionLoading !== null}
                className="p-1 rounded text-red-500 hover:bg-red-500/10 transition-colors disabled:opacity-40"
                title={t('filelu.permanentDeleteOne')}
              >
                <Trash2 size={13} />
              </button>
            )}
          </span>
        ),
      },
    ],
    [restoreOne, deleteOne, actionLoading, t],
  );

  return (
    <div className="fixed inset-0 z-50 flex items-start justify-center pt-[5vh] bg-black/50 backdrop-blur-sm">
      <div
        {...modalDrag.panelProps}
        className="relative w-full max-w-xl mx-4 rounded-lg shadow-2xl bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 animate-scale-in"
      >

        {/* Header (drag handle) */}
        <div {...modalDrag.dragHandleProps} className="flex items-center justify-between px-5 py-4 border-b border-gray-200 dark:border-gray-700 cursor-grab active:cursor-grabbing">
          <div className="flex items-center gap-2">
            <Trash2 size={18} className="text-red-500" />
            <h2 className="text-base font-semibold text-gray-900 dark:text-gray-100">
              {t('filelu.trashTitle')}
            </h2>
            {!loading && (
              <span className="text-xs text-gray-500 dark:text-gray-400 ml-1">
                ({items.length})
              </span>
            )}
          </div>
          <div className="flex items-center gap-2">
            <button
              onClick={loadTrash}
              disabled={loading}
              className="p-1.5 rounded text-gray-500 dark:text-gray-400 hover:text-gray-900 dark:text-gray-100 hover:bg-gray-50 dark:bg-gray-800 transition-colors"
              title={t('common.refresh')}
            >
              <RefreshCw size={14} className={loading ? 'animate-spin' : ''} />
            </button>
            <button onClick={onClose} className="p-1.5 rounded text-gray-500 dark:text-gray-400 hover:text-gray-900 dark:text-gray-100 hover:bg-gray-50 dark:bg-gray-800 transition-colors">
              <X size={14} />
            </button>
          </div>
        </div>

        {/* Toolbar */}
        {items.length > 0 && (
          <div className="flex items-center gap-2 px-5 py-2.5 border-b border-gray-200 dark:border-gray-700 bg-gray-50 dark:bg-gray-800">
            <button onClick={toggleAll} className="flex items-center gap-1.5 text-xs text-gray-500 dark:text-gray-400 hover:text-gray-900 dark:text-gray-100 transition-colors">
              {allSelected ? <CheckSquare size={14} /> : <Square size={14} />}
              {t('common.selectAll')}
            </button>
            <span className="text-gray-300 dark:text-gray-600">|</span>
            {selected.size > 0 && (
              <>
                <button
                  onClick={restoreSelected}
                  disabled={actionLoading !== null}
                  className="flex items-center gap-1.5 text-xs text-emerald-500 hover:text-emerald-400 transition-colors disabled:opacity-50"
                >
                  {actionLoading === 'restore' ? <Loader2 size={13} className="animate-spin" /> : <RotateCcw size={13} />}
                  {t('filelu.restoreSelected')} ({selected.size})
                </button>
                {PERMANENT_DELETE_ENABLED && (
                  <button
                    onClick={deleteSelected}
                    disabled={actionLoading !== null}
                    className="flex items-center gap-1.5 text-xs text-red-500 hover:text-red-400 transition-colors disabled:opacity-50"
                  >
                    {actionLoading === 'delete' ? <Loader2 size={13} className="animate-spin" /> : <Trash2 size={13} />}
                    {t('filelu.permanentDelete')} ({selected.size})
                  </button>
                )}
              </>
            )}
          </div>
        )}

        {/* Body */}
        <div className="max-h-[50vh] overflow-y-auto">
          {loading ? (
            <div className="flex items-center justify-center py-12">
              <Loader2 size={22} className="animate-spin text-blue-500" />
            </div>
          ) : error ? (
            <div className="flex flex-col items-center gap-2 py-10 text-sm text-red-500 px-6 text-center">
              <span>{error}</span>
              <button onClick={loadTrash} className="mt-2 text-xs underline text-gray-500 dark:text-gray-400">{t('common.retry')}</button>
            </div>
          ) : items.length === 0 ? (
            <div className="flex flex-col items-center gap-2 py-12 text-gray-500 dark:text-gray-400">
              <Trash2 size={32} className="opacity-30" />
              <span className="text-sm">{t('filelu.trashEmpty')}</span>
            </div>
          ) : (
            <TrashTable
              rows={trashRows}
              selected={selected}
              setSelected={setSelected}
              extraColumns={fileLuColumns}
            />
          )}
        </div>

        {/* Footer */}
        <div className="px-5 py-3 border-t border-gray-200 dark:border-gray-700 text-xs text-gray-500 dark:text-gray-400">
          {t('filelu.trashAutoExpiry') || 'Trashed files are automatically deleted by FileLu after 7 days'}
        </div>
      </div>
    </div>
  );
}
