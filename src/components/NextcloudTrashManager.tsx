// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Trash2, RotateCcw, AlertTriangle, X, RefreshCw, Loader2, Folder, File, CheckSquare, Square } from 'lucide-react';
import { useTranslation } from '../i18n';
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

  const toggleSelect = (item: NextcloudTrashItem) => {
    setSelected(prev => {
      const next = new Set(prev);
      if (next.has(item.id)) next.delete(item.id);
      else next.add(item.id);
      return next;
    });
  };

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

  return (
    <div className="fixed inset-0 z-[9999] flex items-center justify-center bg-black/50" onClick={onClose}>
      <div
        className="bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg shadow-2xl w-[640px] max-h-[80vh] flex flex-col animate-scale-in"
        onClick={e => e.stopPropagation()}
        role="dialog"
        aria-modal="true"
        aria-label={t('contextMenu.trashTitle')}
      >
        {/* Header */}
        <div className="flex items-center justify-between px-4 py-3 border-b border-gray-200 dark:border-gray-700">
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
              className="flex items-center gap-1.5 px-3 py-1 text-xs rounded bg-orange-600 text-white hover:bg-orange-700 disabled:opacity-40 disabled:cursor-not-allowed"
            >
              {actionLoading === 'delete' ? <Loader2 size={12} className="animate-spin" /> : <Trash2 size={12} />}
              {t('contextMenu.permanentDelete')} {selected.size > 0 && `(${selected.size})`}
            </button>
            <button
              onClick={handleEmptyTrash}
              disabled={actionLoading !== null}
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
            <table className="w-full text-xs">
              <thead className="sticky top-0 bg-gray-50 dark:bg-gray-800 border-b border-gray-200 dark:border-gray-700">
                <tr className="text-left text-gray-500 dark:text-gray-500">
                  <th className="w-8 px-2 py-1.5"></th>
                  <th className="px-2 py-1.5">{t('common.name')}</th>
                  <th className="px-2 py-1.5">{t('contextMenu.trashOriginalPath') || 'Original path'}</th>
                  <th className="px-2 py-1.5 w-20 text-right">{t('common.size')}</th>
                  <th className="px-2 py-1.5 w-36">{t('contextMenu.trashDeletedDate')}</th>
                </tr>
              </thead>
              <tbody>
                {items.map(item => (
                  <tr
                    key={item.id}
                    className={`cursor-pointer hover:bg-gray-100 dark:hover:bg-gray-700 border-b border-gray-200 dark:border-gray-700/30 ${
                      selected.has(item.id) ? 'bg-blue-500/10' : ''
                    }`}
                    onClick={() => toggleSelect(item)}
                  >
                    <td className="px-2 py-1.5 text-center">
                      {selected.has(item.id) ? (
                        <CheckSquare size={13} className="text-blue-500" />
                      ) : (
                        <Square size={13} className="text-gray-500 dark:text-gray-500" />
                      )}
                    </td>
                    <td className="px-2 py-1.5">
                      <div className="flex items-center gap-1.5">
                        {item.is_dir ? (
                          <Folder size={13} className="text-yellow-500 shrink-0" />
                        ) : (
                          <File size={13} className="text-gray-500 dark:text-gray-500 shrink-0" />
                        )}
                        <span className="truncate text-gray-900 dark:text-gray-100">{item.name}</span>
                      </div>
                    </td>
                    <td className="px-2 py-1.5 text-gray-500 dark:text-gray-500 truncate max-w-[160px]" title={item.original_path}>
                      {item.original_path || '/'}
                    </td>
                    <td className="px-2 py-1.5 text-right text-gray-600 dark:text-gray-400 tabular-nums">
                      {item.is_dir ? '\u2014' : formatSize(item.size)}
                    </td>
                    <td className="px-2 py-1.5 text-gray-500 dark:text-gray-500">
                      {formatDeletedDate(item.deleted_at)}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
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
