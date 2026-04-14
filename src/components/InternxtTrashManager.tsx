// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Trash2, AlertTriangle, X, RefreshCw, Loader2, Folder, File, CheckSquare, Square } from 'lucide-react';
import { useTranslation } from '../i18n';
import { useHumanizedLog } from '../hooks/useHumanizedLog';
import { formatSize, formatDate } from '../utils/formatters';

interface InternxtTrashItem {
  name: string;
  path: string;
  is_dir: boolean;
  size: number;
  modified: string | null;
}

interface InternxtTrashManagerProps {
  onClose: () => void;
  onRefreshFiles?: () => void;
}

export function InternxtTrashManager({ onClose, onRefreshFiles }: InternxtTrashManagerProps) {
  const t = useTranslation();
  const humanLog = useHumanizedLog();
  const [items, setItems] = useState<InternxtTrashItem[]>([]);
  const [loading, setLoading] = useState(true);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [actionLoading, setActionLoading] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const loadTrash = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const result = await invoke<InternxtTrashItem[]>('internxt_list_trash');
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

  const toggleSelect = (item: InternxtTrashItem) => {
    const id = item.path;
    setSelected(prev => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
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

  // Internxt API only supports listing trash — restore and permanent delete
  // are not available via the current API. This component is read-only.

  useEffect(() => {
    const handleKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', handleKey);
    return () => window.removeEventListener('keydown', handleKey);
  }, [onClose]);

  return (
    <div className="fixed inset-0 z-[9999] flex items-center justify-center bg-black/50" onClick={onClose}>
      <div
        className="bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg shadow-2xl w-[600px] max-h-[80vh] flex flex-col animate-scale-in"
        onClick={e => e.stopPropagation()}
        role="dialog"
        aria-modal="true"
        aria-label={t('contextMenu.internxtTrashTitle')}
      >
        <div className="flex items-center justify-between px-4 py-3 border-b border-gray-200 dark:border-gray-700">
          <div className="flex items-center gap-2">
            <Trash2 size={18} className="text-blue-500" />
            <h2 className="text-sm font-semibold text-gray-900 dark:text-gray-100">
              {t('contextMenu.internxtTrashTitle')}
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

        {/* Internxt trash is read-only: no restore or permanent delete API available */}

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
                  <th className="px-2 py-1.5 w-32">{t('contextMenu.trashDeletedDate')}</th>
                </tr>
              </thead>
              <tbody>
                {items.map(item => (
                  <tr
                    key={item.path}
                    className={`cursor-pointer hover:bg-gray-100 dark:hover:bg-gray-700 border-b border-gray-200 dark:border-gray-700/30 ${
                      selected.has(item.path) ? 'bg-blue-500/10' : ''
                    }`}
                    onClick={() => toggleSelect(item)}
                  >
                    <td className="px-2 py-1.5 text-center">
                      {selected.has(item.path) ? (
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
                    <td className="px-2 py-1.5 text-right text-gray-600 dark:text-gray-400 tabular-nums">
                      {item.is_dir ? '—' : formatSize(item.size)}
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

      {/* No delete confirm dialog — Internxt trash is read-only */}
    </div>
  );
}