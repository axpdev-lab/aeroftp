// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Trash2, RotateCcw, AlertTriangle, X, RefreshCw, Loader2, File, CheckSquare, Square, EyeOff } from 'lucide-react';
import { useTranslation } from '../i18n';
import { formatDate } from '../utils/formatters';
import { useHumanizedLog } from '../hooks/useHumanizedLog';

interface HiddenEntry {
  name: string;
  path: string;
  is_dir: boolean;
  size: number;
  modified: string | null;
}

interface B2HiddenManagerProps {
  onClose: () => void;
  onRefreshFiles?: () => void;
  currentPath: string;
}

/**
 * "Hidden Files" panel for Backblaze B2 native.
 *
 * Unlike S3 or other providers' Trash, B2 has no separate trash bucket: a
 * `delete()` on a B2 native connection writes a hide marker on top of the
 * file. The previous content stays in the bucket and the file disappears
 * from listings. This panel surfaces those hide markers so the user can
 * either restore the file (drop the marker) or hard-delete every version of
 * the path (irreversible).
 */
export function B2HiddenManager({ onClose, onRefreshFiles, currentPath }: B2HiddenManagerProps) {
  const t = useTranslation();
  const humanLog = useHumanizedLog();
  const [items, setItems] = useState<HiddenEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [actionLoading, setActionLoading] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [pendingDeleteConfirm, setPendingDeleteConfirm] = useState(false);

  const loadHidden = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const result = await invoke<HiddenEntry[]>('b2_list_hidden', { path: currentPath || '/' });
      setItems(result);
      setSelected(new Set());
    } catch (err) {
      setError(String(err));
    } finally {
      setLoading(false);
    }
  }, [currentPath]);

  useEffect(() => {
    loadHidden();
  }, [loadHidden]);

  const toggleSelect = (item: HiddenEntry) => {
    setSelected(prev => {
      const next = new Set(prev);
      if (next.has(item.path)) next.delete(item.path);
      else next.add(item.path);
      return next;
    });
  };

  const toggleSelectAll = () => {
    if (selected.size === items.length) setSelected(new Set());
    else setSelected(new Set(items.map(i => i.path)));
  };

  const handleRestore = async () => {
    const paths = Array.from(selected);
    if (paths.length === 0) return;
    const logId = humanLog.logRaw('activity.trash_restore_start', 'INFO', { provider: 'Backblaze B2', count: paths.length });
    setActionLoading('restore');
    try {
      for (const path of paths) {
        await invoke('b2_restore_hidden', { path });
      }
      humanLog.updateEntry(logId, { status: 'success', message: `[Backblaze B2] Restored ${paths.length} hidden file(s)` });
      await loadHidden();
      onRefreshFiles?.();
    } catch (err) {
      humanLog.updateEntry(logId, { status: 'error', message: `[Backblaze B2] Failed to restore hidden file(s)` });
      setError(String(err));
    } finally {
      setActionLoading(null);
    }
  };

  const handlePermanentDelete = () => {
    if (selected.size === 0) return;
    setPendingDeleteConfirm(true);
  };

  const confirmPermanentDelete = async () => {
    setPendingDeleteConfirm(false);
    const paths = Array.from(selected);
    if (paths.length === 0) return;
    const logId = humanLog.logRaw('activity.trash_delete_start', 'INFO', { provider: 'Backblaze B2', count: paths.length });
    setActionLoading('delete');
    try {
      let totalVersions = 0;
      for (const path of paths) {
        const versions = await invoke<number>('b2_permanent_delete', { path });
        totalVersions += versions;
      }
      humanLog.updateEntry(logId, {
        status: 'success',
        message: `[Backblaze B2] Permanently deleted ${paths.length} file(s) (${totalVersions} versions purged)`,
      });
      await loadHidden();
      onRefreshFiles?.();
    } catch (err) {
      humanLog.updateEntry(logId, { status: 'error', message: `[Backblaze B2] Failed to permanently delete` });
      setError(String(err));
    } finally {
      setActionLoading(null);
    }
  };

  useEffect(() => {
    const handleKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        if (pendingDeleteConfirm) setPendingDeleteConfirm(false);
        else onClose();
      }
    };
    window.addEventListener('keydown', handleKey);
    return () => window.removeEventListener('keydown', handleKey);
  }, [onClose, pendingDeleteConfirm]);

  return (
    <div className="fixed inset-0 z-[9999] flex items-center justify-center bg-black/50" onClick={onClose}>
      <div
        className="bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg shadow-2xl w-[640px] max-h-[80vh] flex flex-col animate-scale-in"
        onClick={e => e.stopPropagation()}
        role="dialog"
        aria-modal="true"
        aria-label={t('b2.hidden.title')}
      >
        {/* Header */}
        <div className="flex items-center justify-between px-4 py-3 border-b border-gray-200 dark:border-gray-700">
          <div className="flex items-center gap-2">
            <EyeOff size={18} className="text-red-500" />
            <h2 className="text-sm font-semibold text-gray-900 dark:text-gray-100">
              {t('b2.hidden.title')}
            </h2>
            <span className="text-xs text-gray-500 dark:text-gray-500">({items.length})</span>
          </div>
          <div className="flex items-center gap-1">
            <button onClick={loadHidden} disabled={loading} className="p-1.5 rounded hover:bg-gray-100 dark:hover:bg-gray-700 text-gray-600 dark:text-gray-400" title={t('common.refresh') as string}>
              <RefreshCw size={14} className={loading ? 'animate-spin' : ''} />
            </button>
            <button onClick={onClose} className="p-1.5 rounded hover:bg-gray-100 dark:hover:bg-gray-700 text-gray-600 dark:text-gray-400">
              <X size={14} />
            </button>
          </div>
        </div>

        {/* Info banner */}
        <div className="px-4 py-2 text-[11px] text-red-700 dark:text-red-300 bg-red-50 dark:bg-red-900/10 border-b border-red-100 dark:border-red-900/30">
          {t('b2.hidden.banner')}
        </div>

        {/* Toolbar */}
        {items.length > 0 && (
          <div className="flex items-center gap-2 px-4 py-2 border-b border-gray-200 dark:border-gray-700 bg-gray-50 dark:bg-gray-800">
            <button onClick={toggleSelectAll} className="flex items-center gap-1.5 px-2 py-1 text-xs rounded hover:bg-gray-100 dark:hover:bg-gray-700 text-gray-600 dark:text-gray-400">
              {selected.size === items.length ? <CheckSquare size={12} /> : <Square size={12} />}
              {selected.size === items.length ? t('contextMenu.trashDeselectAll') : t('contextMenu.trashSelectAll')}
            </button>
            <div className="flex-1" />
            <button onClick={handleRestore} disabled={selected.size === 0 || actionLoading !== null} className="flex items-center gap-1.5 px-3 py-1 text-xs rounded bg-blue-600 text-white hover:bg-blue-700 disabled:opacity-40 disabled:cursor-not-allowed">
              {actionLoading === 'restore' ? <Loader2 size={12} className="animate-spin" /> : <RotateCcw size={12} />}
              {t('contextMenu.restoreFromTrash')} {selected.size > 0 && `(${selected.size})`}
            </button>
            <button onClick={handlePermanentDelete} disabled={selected.size === 0 || actionLoading !== null} className="flex items-center gap-1.5 px-3 py-1 text-xs rounded bg-red-600 text-white hover:bg-red-700 disabled:opacity-40 disabled:cursor-not-allowed">
              {actionLoading === 'delete' ? <Loader2 size={12} className="animate-spin" /> : <Trash2 size={12} />}
              {t('contextMenu.permanentDelete')} {selected.size > 0 && `(${selected.size})`}
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
              <EyeOff size={32} className="mb-2 opacity-30" />
              {t('b2.hidden.empty')}
            </div>
          ) : (
            <table className="w-full text-xs">
              <thead className="sticky top-0 bg-gray-50 dark:bg-gray-800 border-b border-gray-200 dark:border-gray-700">
                <tr className="text-left text-gray-500 dark:text-gray-500">
                  <th className="w-8 px-2 py-1.5"></th>
                  <th className="px-2 py-1.5">{t('common.name')}</th>
                  <th className="px-2 py-1.5">{t('common.path')}</th>
                  <th className="px-2 py-1.5 w-32">{t('contextMenu.trashDeletedDate')}</th>
                </tr>
              </thead>
              <tbody>
                {items.map(item => (
                  <tr
                    key={item.path}
                    className={`cursor-pointer hover:bg-gray-100 dark:hover:bg-gray-700 border-b border-gray-200 dark:border-gray-700/30 ${selected.has(item.path) ? 'bg-red-500/10' : ''}`}
                    onClick={() => toggleSelect(item)}
                  >
                    <td className="px-2 py-1.5 text-center">
                      {selected.has(item.path) ? <CheckSquare size={13} className="text-red-500" /> : <Square size={13} className="text-gray-500 dark:text-gray-500" />}
                    </td>
                    <td className="px-2 py-1.5">
                      <div className="flex items-center gap-1.5">
                        <File size={13} className="text-gray-500 dark:text-gray-500 shrink-0" />
                        <span className="truncate text-gray-900 dark:text-gray-100">{item.name}</span>
                      </div>
                    </td>
                    <td className="px-2 py-1.5 text-gray-500 dark:text-gray-500 truncate max-w-[200px]" title={item.path}>
                      {item.path}
                    </td>
                    <td className="px-2 py-1.5 text-gray-500 dark:text-gray-500">
                      {item.modified ? formatDate(item.modified) : '-'}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </div>

        {/* Permanent delete confirmation */}
        {pendingDeleteConfirm && (
          <div className="absolute inset-0 z-10 flex items-center justify-center bg-black/60" onClick={() => setPendingDeleteConfirm(false)}>
            <div
              className="bg-white dark:bg-gray-800 border border-red-300 dark:border-red-700 rounded-lg shadow-2xl w-[420px] p-4"
              onClick={e => e.stopPropagation()}
              role="alertdialog"
              aria-modal="true"
            >
              <div className="flex items-start gap-3 mb-3">
                <AlertTriangle size={20} className="text-red-500 shrink-0 mt-0.5" />
                <div>
                  <h3 className="text-sm font-semibold text-gray-900 dark:text-gray-100 mb-1">
                    {t('b2.hidden.confirmDeleteTitle')}
                  </h3>
                  <p className="text-xs text-gray-600 dark:text-gray-400">
                    {t('b2.hidden.confirmDeleteBody', { count: selected.size })}
                  </p>
                </div>
              </div>
              <div className="flex justify-end gap-2">
                <button onClick={() => setPendingDeleteConfirm(false)} className="px-3 py-1.5 text-xs rounded bg-gray-200 dark:bg-gray-700 text-gray-700 dark:text-gray-300 hover:bg-gray-300 dark:hover:bg-gray-600">
                  {t('common.cancel')}
                </button>
                <button onClick={confirmPermanentDelete} className="px-3 py-1.5 text-xs rounded bg-red-600 text-white hover:bg-red-700">
                  {t('contextMenu.permanentDelete')}
                </button>
              </div>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
