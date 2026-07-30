// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { useState, useEffect, useCallback, useMemo } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Trash2, RotateCcw, AlertTriangle, X, RefreshCw, Loader2, Folder, File, CheckSquare, Square } from 'lucide-react';
import { useTranslation } from '../i18n';
import { useDraggableModal } from '../hooks/useDraggableModal';
import { TrashTable, type TrashRow } from './Trash/TrashTable';
import { formatSize, formatDate } from '../utils/formatters';
import { useHumanizedLog } from '../hooks/useHumanizedLog';

interface TrashEntry {
  name: string;
  path: string;
  is_dir: boolean;
  size: number;
  modified: string | null;
  metadata: Record<string, string>;
}

interface MegaTrashManagerProps {
  onClose: () => void;
  onRefreshFiles?: () => void;
}

export function MegaTrashManager({ onClose, onRefreshFiles }: MegaTrashManagerProps) {
  const t = useTranslation();
  const modalDrag = useDraggableModal();
  const humanLog = useHumanizedLog();
  const [items, setItems] = useState<TrashEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [actionLoading, setActionLoading] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const loadTrash = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const result = await invoke<TrashEntry[]>('mega_list_trash');
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
      setSelected(new Set(items.map(i => i.name)));
    }
  };

  const handleRestore = async () => {
    const filenames = Array.from(selected);
    if (filenames.length === 0) return;
    const logId = humanLog.logRaw('activity.trash_restore_start', 'INFO', { provider: 'MEGA', count: filenames.length });
    setActionLoading('restore');
    try {
      await invoke('mega_restore_from_trash', { filenames });
      humanLog.updateEntry(logId, { status: 'success', message: `[MEGA] Restored ${filenames.length} item(s) from trash` });
      await loadTrash();
      onRefreshFiles?.();
    } catch (err) {
      humanLog.updateEntry(logId, { status: 'error', message: `[MEGA] Failed to restore from trash` });
      setError(String(err));
    } finally {
      setActionLoading(null);
    }
  };

  // Styled confirmation dialog state (replaces window.confirm)
  const [pendingDeleteConfirm, setPendingDeleteConfirm] = useState(false);

  const handlePermanentDelete = () => {
    if (selected.size === 0) return;
    setPendingDeleteConfirm(true);
  };

  const confirmPermanentDelete = async () => {
    setPendingDeleteConfirm(false);
    const filenames = Array.from(selected);
    if (filenames.length === 0) return;
    const logId = humanLog.logRaw('activity.trash_delete_start', 'INFO', { provider: 'MEGA', count: filenames.length });
    setActionLoading('delete');
    try {
      await invoke('mega_permanent_delete', { filenames });
      humanLog.updateEntry(logId, { status: 'success', message: `[MEGA] Permanently deleted ${filenames.length} item(s) from trash` });
      await loadTrash();
      onRefreshFiles?.();
    } catch (err) {
      humanLog.updateEntry(logId, { status: 'error', message: `[MEGA] Failed to permanently delete from trash` });
      setError(String(err));
    } finally {
      setActionLoading(null);
    }
  };

  useEffect(() => {
    const handleKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', handleKey);
    return () => window.removeEventListener('keydown', handleKey);
  }, [onClose]);

  // Normalized for the shared trash table (sorting, Type column,
  // Ctrl/Shift and rubber-band selection live there).
  const trashRows: TrashRow[] = useMemo(
    () => items.map(item => ({
      id: item.name,
      name: item.name,
      isDir: item.is_dir,
      size: item.size,
      deletedAt: item.modified,
    })),
    [items],
  );

  return (
    <div className="fixed inset-0 z-[9999] flex items-center justify-center bg-black/50" onClick={onClose}>
      <div
        {...modalDrag.panelProps}
        className="bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg shadow-2xl w-[600px] max-h-[80vh] flex flex-col animate-scale-in"
        onClick={e => e.stopPropagation()}
        role="dialog"
        aria-modal="true"
        aria-label={t('contextMenu.trashTitle')}
      >
        {/* Header */}
        <div {...modalDrag.dragHandleProps} className="flex items-center justify-between px-4 py-3 border-b border-gray-200 dark:border-gray-700 cursor-grab active:cursor-grabbing">
          <div className="flex items-center gap-2">
            <Trash2 size={18} className="text-red-500" />
            <h2 className="text-sm font-semibold text-gray-900 dark:text-gray-100">
              {t('contextMenu.trashTitle')} - MEGA
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
              onClick={handlePermanentDelete}
              disabled={selected.size === 0 || actionLoading !== null}
              className="flex items-center gap-1.5 px-3 py-1 text-xs rounded bg-red-600 text-white hover:bg-red-700 disabled:opacity-40 disabled:cursor-not-allowed"
            >
              {actionLoading === 'delete' ? <Loader2 size={12} className="animate-spin" /> : <AlertTriangle size={12} />}
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
            />
          )}
        </div>
      </div>

      {/* Styled confirmation dialog (replaces window.confirm) */}
      {pendingDeleteConfirm && (
        <div className="fixed inset-0 z-[10000] bg-black/50 flex items-center justify-center" role="dialog" aria-modal="true" onClick={() => setPendingDeleteConfirm(false)}>
          <div className="bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg p-6 shadow-2xl max-w-sm animate-scale-in" onClick={e => e.stopPropagation()}>
            <p className="text-gray-900 dark:text-gray-100 mb-4">
              {t('contextMenu.permanentDeleteConfirm', { count: selected.size })}
            </p>
            <div className="flex justify-end gap-2">
              <button
                onClick={() => setPendingDeleteConfirm(false)}
                className="px-4 py-2 text-gray-600 dark:text-gray-400 hover:bg-gray-100 dark:hover:bg-gray-700 rounded-lg"
              >
                {t('common.cancel')}
              </button>
              <button
                onClick={confirmPermanentDelete}
                className="px-4 py-2 text-white rounded-lg bg-red-500 hover:bg-red-600"
              >
                {t('contextMenu.permanentDelete')}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
