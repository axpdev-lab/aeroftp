// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { useState, useEffect, useCallback, useMemo } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Trash2, RotateCcw, AlertTriangle, X, RefreshCw, Loader2, Folder, File, CheckSquare, Square } from 'lucide-react';
import { useTranslation } from '../i18n';
import { useDraggableModal } from '../hooks/useDraggableModal';
import { TrashTable, type TrashRow } from './Trash/TrashTable';
import { useHumanizedLog } from '../hooks/useHumanizedLog';
import { formatSize } from '../utils/formatters';

interface NextcloudTrashItem {
  id: string;
  name: string;
  original_path: string;
  deleted_at: number;
  size: number;
  is_dir: boolean;
}

interface NextcloudTrashManagerProps {
  providerName?: string;
  onClose: () => void;
  onRefreshFiles?: () => void;
}

export function NextcloudTrashManager({ providerName, onClose, onRefreshFiles }: NextcloudTrashManagerProps) {
  const t = useTranslation();
  const modalDrag = useDraggableModal();
  const humanLog = useHumanizedLog();
  const [items, setItems] = useState<NextcloudTrashItem[]>([]);
  const [loading, setLoading] = useState(true);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [actionLoading, setActionLoading] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const loadTrash = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const result = await invoke<NextcloudTrashItem[]>('webdav_list_trash');
      result.sort((a, b) => b.deleted_at - a.deleted_at);
      setItems(result);
      setSelected(new Set());
    } catch (err) {
      setError(String(err));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    loadTrash();
  }, [loadTrash]);


  const toggleSelectAll = () => {
    if (selected.size === items.length) {
      setSelected(new Set());
    } else {
      setSelected(new Set(items.map(i => i.id)));
    }
  };

  const getSelectedIds = (): string[] => {
    return items.filter(i => selected.has(i.id)).map(i => i.id);
  };

  const handleRestore = async () => {
    const ids = getSelectedIds();
    if (ids.length === 0) return;
    const selectedCount = ids.length;
    const logId = humanLog.logRaw('activity.trash_restore_start', 'INFO', { provider: 'Nextcloud', count: selectedCount });
    setActionLoading('restore');
    try {
      await invoke('webdav_restore_trash', { ids });
      humanLog.updateEntry(logId, { status: 'success', message: `[Nextcloud] Restored ${selectedCount} item(s) from trash` });
      await loadTrash();
      onRefreshFiles?.();
    } catch (err) {
      humanLog.updateEntry(logId, { status: 'error', message: `[Nextcloud] Failed to restore from trash` });
      setError(String(err));
    } finally {
      setActionLoading(null);
    }
  };

  const handleDeleteSelected = async () => {
    const ids = getSelectedIds();
    if (ids.length === 0) return;
    const selectedCount = ids.length;
    const logId = humanLog.logRaw('activity.trash_delete_start', 'INFO', { provider: 'Nextcloud', count: selectedCount });
    setActionLoading('delete');
    try {
      await invoke('webdav_delete_trash', { ids });
      humanLog.updateEntry(logId, { status: 'success', message: `[Nextcloud] Permanently deleted ${selectedCount} item(s) from trash` });
      await loadTrash();
    } catch (err) {
      humanLog.updateEntry(logId, { status: 'error', message: `[Nextcloud] Failed to permanently delete from trash` });
      setError(String(err));
    } finally {
      setActionLoading(null);
    }
  };

  const [pendingEmptyConfirm, setPendingEmptyConfirm] = useState(false);

  const handleEmptyTrash = () => {
    if (items.length === 0) return;
    setPendingEmptyConfirm(true);
  };

  const confirmEmptyTrash = async () => {
    setPendingEmptyConfirm(false);
    const totalCount = items.length;
    const logId = humanLog.logRaw('activity.trash_empty_start', 'INFO', { provider: 'Nextcloud', count: totalCount });
    setActionLoading('empty');
    try {
      await invoke('webdav_empty_trash');
      humanLog.updateEntry(logId, { status: 'success', message: `[Nextcloud] Emptied trash (${totalCount} item(s))` });
      await loadTrash();
      onRefreshFiles?.();
    } catch (err) {
      humanLog.updateEntry(logId, { status: 'error', message: `[Nextcloud] Failed to empty trash` });
      setError(String(err));
    } finally {
      setActionLoading(null);
    }
  };

  const formatDeletedDate = (timestamp: number): string => {
    if (timestamp === 0) return '\u2014';
    const date = new Date(timestamp * 1000);
    return date.toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' });
  };

  useEffect(() => {
    const handleKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', handleKey);
    return () => window.removeEventListener('keydown', handleKey);
  }, [onClose]);

  const label = providerName || 'Nextcloud';

  // Normalized for the shared trash table (sorting, Type column,
  // Ctrl/Shift and rubber-band selection live there).
  const trashRows: TrashRow[] = useMemo(
    () => items.map(item => ({
      id: item.id,
      name: item.name,
      isDir: item.is_dir,
      size: item.size,
      deletedAtMs: item.deleted_at * 1000, deletedAtLabel: formatDeletedDate(item.deleted_at),
    })),
    [items],
  );

  // Nextcloud is the one provider that reports where the item used to live, and
  // that column survives the move to the shared table as an extra column.
  const originalPathById = useMemo(
    () => new Map(items.map(item => [item.id, item.original_path])),
    [items],
  );
  const originalPathColumn = useMemo(
    () => [{
      key: 'originalPath',
      header: t('contextMenu.trashOriginalPath'),
      className: 'truncate max-w-[160px]',
      render: (row: TrashRow) => {
        const path = originalPathById.get(row.id) || '/';
        return <span title={path}>{path}</span>;
      },
    }],
    [originalPathById, t],
  );

  return (
    <div className="fixed inset-0 z-[9999] flex items-center justify-center bg-black/50" onClick={onClose}>
      <div
        {...modalDrag.panelProps}
        className="bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg shadow-2xl w-[640px] max-h-[80vh] flex flex-col animate-scale-in"
        onClick={e => e.stopPropagation()}
        role="dialog"
        aria-modal="true"
        aria-label={t('contextMenu.trashTitle')}
      >
        {/* Header */}
        <div {...modalDrag.dragHandleProps} className="flex items-center justify-between px-4 py-3 border-b border-gray-200 dark:border-gray-700 cursor-grab active:cursor-grabbing">
          <div className="flex items-center gap-2">
            <Trash2 size={18} className="text-orange-500" />
            <h2 className="text-sm font-semibold text-gray-900 dark:text-gray-100">
              {t('contextMenu.trashTitle')} - {label}
            </h2>
            <span className="text-xs text-gray-500 dark:text-gray-500">
              ({items.length})
            </span>
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

        {/* Toolbar */}
        {items.length > 0 && (
          <div className="flex items-center gap-2 px-4 py-2 border-b border-gray-200 dark:border-gray-700 bg-gray-50 dark:bg-gray-800">
            <button
              onClick={toggleSelectAll}
              className="flex items-center gap-1.5 px-2 py-1 text-xs rounded hover:bg-gray-100 dark:hover:bg-gray-700 text-gray-600 dark:text-gray-400"
            >
              {selected.size === items.length ? <CheckSquare size={12} /> : <Square size={12} />}
              {selected.size === items.length ? t('contextMenu.trashDeselectAll') : t('contextMenu.trashSelectAll')}
            </button>
            <div className="flex-1" />
            <button
              onClick={handleRestore}
              disabled={selected.size === 0 || actionLoading !== null}
              className="flex items-center gap-1.5 px-3 py-1 text-xs rounded bg-blue-600 text-white hover:bg-blue-700 disabled:opacity-40 disabled:cursor-not-allowed"
            >
              {actionLoading === 'restore' ? <Loader2 size={12} className="animate-spin" /> : <RotateCcw size={12} />}
              {t('contextMenu.restoreFromTrash')} {selected.size > 0 && `(${selected.size})`}
            </button>
            <button
              onClick={handleDeleteSelected}
              disabled={selected.size === 0 || actionLoading !== null}
              title={t('contextMenu.permanentDeleteHint')}
              className="flex items-center gap-1.5 px-3 py-1 text-xs rounded bg-orange-600 text-white hover:bg-orange-700 disabled:opacity-40 disabled:cursor-not-allowed"
            >
              {actionLoading === 'delete' ? <Loader2 size={12} className="animate-spin" /> : <Trash2 size={12} />}
              {t('contextMenu.permanentDelete')} {selected.size > 0 && `(${selected.size})`}
            </button>
            <button
              onClick={handleEmptyTrash}
              disabled={actionLoading !== null}
              title={t('contextMenu.emptyTrashHint')}
              className="flex items-center gap-1.5 px-3 py-1 text-xs rounded bg-red-600 text-white hover:bg-red-700 disabled:opacity-40 disabled:cursor-not-allowed"
            >
              {actionLoading === 'empty' ? <Loader2 size={12} className="animate-spin" /> : <AlertTriangle size={12} />}
              {t('contextMenu.emptyTrash')}
            </button>
          </div>
        )}

        {/* Content */}
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
            <div className="flex flex-col items-center justify-center py-12 text-gray-500 dark:text-gray-500">
              <Trash2 size={32} className="mb-2 opacity-30" />
              {t('contextMenu.trashEmpty')}
            </div>
          ) : (
            <TrashTable
              rows={trashRows}
              selected={selected}
              setSelected={setSelected}
              rowTintClass="bg-blue-500/10"
              accentClass="text-blue-500"
              extraColumns={originalPathColumn}
            />
          )}
        </div>
      </div>

      {/* Empty trash confirmation dialog */}
      {pendingEmptyConfirm && (
        <div className="fixed inset-0 z-[10000] bg-black/50 flex items-center justify-center" role="dialog" aria-modal="true" onClick={() => setPendingEmptyConfirm(false)}>
          <div className="bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg p-6 shadow-2xl max-w-sm animate-scale-in" onClick={e => e.stopPropagation()}>
            <p className="text-gray-900 dark:text-gray-100 mb-4">
              {t('contextMenu.emptyTrashConfirm')}
            </p>
            <div className="flex justify-end gap-2">
              <button
                onClick={() => setPendingEmptyConfirm(false)}
                className="px-4 py-2 text-gray-600 dark:text-gray-400 hover:bg-gray-100 dark:hover:bg-gray-700 rounded-lg"
              >
                {t('common.cancel')}
              </button>
              <button
                onClick={confirmEmptyTrash}
                className="px-4 py-2 text-white rounded-lg bg-red-500 hover:bg-red-600"
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
