// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Trash2, RotateCcw, AlertTriangle, X, RefreshCw, Loader2, Folder, File, CheckSquare, Square } from 'lucide-react';
import { useTranslation } from '../i18n';
import { useHumanizedLog } from '../hooks/useHumanizedLog';
import type { RemoteFile } from '../types';
import { formatSize, formatDate } from '../utils/formatters';

interface KDriveTrashManagerProps {
  onClose: () => void;
  onRefreshFiles?: () => void;
}

export function KDriveTrashManager({ onClose, onRefreshFiles }: KDriveTrashManagerProps) {
  const t = useTranslation();
  const humanLog = useHumanizedLog();
  const [items, setItems] = useState<RemoteFile[]>([]);
  const [loading, setLoading] = useState(true);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [actionLoading, setActionLoading] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [pendingEmptyConfirm, setPendingEmptyConfirm] = useState(false);

  const loadTrash = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const result = await invoke<RemoteFile[]>('kdrive_list_trash');
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

  useEffect(() => {
    const handleKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', handleKey);
    return () => window.removeEventListener('keydown', handleKey);
  }, [onClose]);

  const toggleSelect = (item: RemoteFile) => {
    setSelected(prev => {
      const next = new Set(prev);
      if (next.has(item.path)) next.delete(item.path);
      else next.add(item.path);
      return next;
    });
  };

  const toggleSelectAll = () => {
    if (selected.size === items.length) {
      setSelected(new Set());
    } else {
      setSelected(new Set(items.map(i => i.path)));
    }
  };

  const getSelectedItems = (): RemoteFile[] => {
    return items.filter(item => selected.has(item.path));
  };

  const handleRestore = async () => {
    const sel = getSelectedItems();
    if (sel.length === 0) return;
    const selectedCount = sel.length;
    const logId = humanLog.logRaw('activity.trash_restore_start', 'INFO', { provider: 'kDrive', count: selectedCount });
    setActionLoading('restore');
    setError(null);
    try {
      for (const item of sel) {
        const fileId = item.metadata?.file_id ?? '';
        await invoke('kdrive_restore_from_trash', { fileId });
      }
      humanLog.updateEntry(logId, { status: 'success', message: `[kDrive] Restored ${selectedCount} item(s) from trash` });
      await loadTrash();
      onRefreshFiles?.();
    } catch (err) {
      humanLog.updateEntry(logId, { status: 'error', message: `[kDrive] Failed to restore from trash` });
      setError(String(err));
    } finally {
      setActionLoading(null);
    }
  };

  const handlePermanentDelete = async () => {
    const sel = getSelectedItems();
    if (sel.length === 0) return;
    const selectedCount = sel.length;
    const logId = humanLog.logRaw('activity.trash_delete_start', 'INFO', { provider: 'kDrive', count: selectedCount });
    setActionLoading('delete');
    setError(null);
    try {
      for (const item of sel) {
        const fileId = item.metadata?.file_id ?? '';
        await invoke('kdrive_permanently_delete_trash', { fileId });
      }
      humanLog.updateEntry(logId, { status: 'success', message: `[kDrive] Permanently deleted ${selectedCount} item(s) from trash` });
      await loadTrash();
      onRefreshFiles?.();
    } catch (err) {
      humanLog.updateEntry(logId, { status: 'error', message: `[kDrive] Failed to permanently delete from trash` });
      setError(String(err));
    } finally {
      setActionLoading(null);
    }
  };

  const confirmEmptyTrash = async () => {
    setPendingEmptyConfirm(false);
    const totalCount = items.length;
    const logId = humanLog.logRaw('activity.trash_empty_start', 'INFO', { provider: 'kDrive', count: totalCount });
    setActionLoading('empty');
    setError(null);
    try {
      await invoke('kdrive_empty_trash');
      humanLog.updateEntry(logId, { status: 'success', message: `[kDrive] Emptied trash (${totalCount} item(s))` });
      await loadTrash();
      onRefreshFiles?.();
    } catch (err) {
      humanLog.updateEntry(logId, { status: 'error', message: `[kDrive] Failed to empty trash` });
      setError(String(err));
    } finally {
      setActionLoading(null);
    }
  };

  return (
    <div className="fixed inset-0 z-[9999] flex items-center justify-center bg-black/50" onClick={onClose}>
      <div
        className="bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg shadow-2xl w-[680px] max-h-[80vh] flex flex-col animate-scale-in"
        onClick={e => e.stopPropagation()}
        role="dialog"
        aria-modal="true"
        aria-label={t('contextMenu.kdriveTrashTitle')}
      >
        <div className="flex items-center justify-between px-4 py-3 border-b border-gray-200 dark:border-gray-700">
          <div className="flex items-center gap-2">
            <Trash2 size={18} className="text-sky-500" />
            <h2 className="text-sm font-semibold text-gray-900 dark:text-gray-100">
              {t('contextMenu.kdriveTrashTitle')}
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
              className="flex items-center gap-1.5 px-3 py-1 text-xs rounded bg-sky-600 text-white hover:bg-sky-700 disabled:opacity-40 disabled:cursor-not-allowed"
            >
              {actionLoading === 'restore' ? <Loader2 size={12} className="animate-spin" /> : <RotateCcw size={12} />}
              {t('contextMenu.restoreFromTrash')} {selected.size > 0 && `(${selected.size})`}
            </button>
            <button
              onClick={handlePermanentDelete}
              disabled={selected.size === 0 || actionLoading !== null}
              className="flex items-center gap-1.5 px-3 py-1 text-xs rounded bg-red-600 text-white hover:bg-red-700 disabled:opacity-40 disabled:cursor-not-allowed"
            >
              {actionLoading === 'delete' ? <Loader2 size={12} className="animate-spin" /> : <Trash2 size={12} />}
              {t('contextMenu.permanentDelete')}
            </button>
            <button
              onClick={() => setPendingEmptyConfirm(true)}
              disabled={items.length === 0 || actionLoading !== null}
              className="flex items-center gap-1.5 px-3 py-1 text-xs rounded bg-amber-600 text-white hover:bg-amber-700 disabled:opacity-40 disabled:cursor-not-allowed"
            >
              {actionLoading === 'empty' ? <Loader2 size={12} className="animate-spin" /> : <AlertTriangle size={12} />}
              {t('contextMenu.emptyTrash')}
            </button>
          </div>
        )}

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
                  <th className="px-2 py-1.5 w-20 text-right">{t('common.size')}</th>
                  <th className="px-2 py-1.5 w-40">{t('contextMenu.trashDeletedDate')}</th>
                </tr>
              </thead>
              <tbody>
                {items.map(item => (
                  <tr
                    key={item.path}
                    className={`cursor-pointer hover:bg-gray-100 dark:hover:bg-gray-700 border-b border-gray-200 dark:border-gray-700/30 ${
                      selected.has(item.path) ? 'bg-sky-500/10' : ''
                    }`}
                    onClick={() => toggleSelect(item)}
                  >
                    <td className="px-2 py-1.5 text-center">
                      {selected.has(item.path) ? (
                        <CheckSquare size={13} className="text-sky-500" />
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
                    <td className="px-2 py-1.5 text-right text-gray-600 dark:text-gray-400 tabular-nums">
                      {item.is_dir ? '-' : formatSize(item.size || 0)}
                    </td>
                    <td className="px-2 py-1.5 text-gray-500 dark:text-gray-500">
                      {item.modified ? formatDate(item.modified) : '—'}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </div>
      </div>

      {pendingEmptyConfirm && (
        <div className="fixed inset-0 z-[10000] bg-black/50 flex items-center justify-center" role="dialog" aria-modal="true" onClick={() => setPendingEmptyConfirm(false)}>
          <div className="bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-xl p-6 shadow-2xl max-w-sm animate-scale-in" onClick={e => e.stopPropagation()}>
            <p className="text-gray-900 dark:text-gray-100 mb-4">
              {t('contextMenu.emptyTrashConfirm')}
            </p>
            <div className="flex justify-end gap-2">
              <button
                onClick={() => setPendingEmptyConfirm(false)}
                className="px-3 py-1.5 text-sm rounded bg-gray-200 hover:bg-gray-300 dark:bg-gray-700 dark:hover:bg-gray-600 text-gray-900 dark:text-gray-100"
              >
                {t('common.cancel')}
              </button>
              <button
                onClick={confirmEmptyTrash}
                className="px-3 py-1.5 text-sm rounded bg-red-600 hover:bg-red-700 text-white"
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
