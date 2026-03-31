// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Trash2, RotateCcw, X, RefreshCw, Loader2, File, CheckSquare, Square, Clock } from 'lucide-react';
import { useTranslation } from '../i18n';
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

function formatDeletedAgo(seconds: number | null): string {
  if (seconds === null) return '';
  if (seconds < 60) return `${seconds}s ago`;
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m ago`;
  if (seconds < 86400) return `${Math.floor(seconds / 3600)}h ago`;
  return `${Math.floor(seconds / 86400)}d ago`;
}

export function FileLuTrashManager({ onClose, onRefreshFiles }: FileLuTrashManagerProps) {
  const t = useTranslation();
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

  return (
    <div className="fixed inset-0 z-50 flex items-start justify-center pt-[5vh] bg-black/50 backdrop-blur-sm">
      <div className="relative w-full max-w-xl mx-4 rounded-lg shadow-2xl bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 animate-scale-in">

        {/* Header */}
        <div className="flex items-center justify-between px-5 py-4 border-b border-gray-200 dark:border-gray-700">
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
            <ul className="divide-y divide-gray-200 dark:divide-gray-700">
              {items.map(item => {
                const code = item.file_code ?? '';
                const isSelected = selected.has(code);
                return (
                  <li
                    key={code}
                    className={`flex items-center gap-3 px-5 py-3 cursor-pointer hover:bg-gray-50 dark:bg-gray-800 transition-colors ${isSelected ? 'bg-gray-50 dark:bg-gray-800' : ''}`}
                    onClick={() => toggleSelect(code)}
                  >
                    <div className="flex-shrink-0 text-gray-500 dark:text-gray-400">
                      {isSelected ? <CheckSquare size={15} className="text-blue-500" /> : <Square size={15} />}
                    </div>
                    <File size={15} className="flex-shrink-0 text-gray-500 dark:text-gray-400" />
                    <div className="flex-1 min-w-0">
                      <p className="text-sm text-gray-900 dark:text-gray-100 truncate">{item.name ?? code}</p>
                      <div className="flex items-center gap-2 mt-0.5">
                        <Clock size={11} className="text-gray-500 dark:text-gray-400" />
                        <span className="text-xs text-gray-500 dark:text-gray-400">
                          {item.deleted ?? formatDeletedAgo(item.deleted_ago_sec)}
                        </span>
                        <span className="text-xs text-gray-500 dark:text-gray-400 opacity-60 font-mono">{code}</span>
                      </div>
                    </div>
                    <div className="flex gap-1.5 flex-shrink-0">
                      <button
                        onClick={e => { e.stopPropagation(); invoke('filelu_restore_file', { fileCode: code }).then(loadTrash).then(() => onRefreshFiles?.()); }}
                        className="p-1 rounded text-emerald-500 hover:bg-emerald-500/10 transition-colors"
                        title={t('filelu.restore')}
                      >
                        <RotateCcw size={13} />
                      </button>
                      {PERMANENT_DELETE_ENABLED && (
                        <button
                          onClick={e => { e.stopPropagation(); invoke('filelu_permanent_delete', { fileCode: code }).then(loadTrash).catch(err => setError(String(err))); }}
                          className="p-1 rounded text-red-500 hover:bg-red-500/10 transition-colors"
                          title={t('filelu.permanentDeleteOne')}
                        >
                          <Trash2 size={13} />
                        </button>
                      )}
                    </div>
                  </li>
                );
              })}
            </ul>
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
