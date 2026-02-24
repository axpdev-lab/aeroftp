import * as React from 'react';
import { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Trash2, RotateCcw, AlertTriangle, X, RefreshCw, Loader2, Folder, File, CheckSquare, Square } from 'lucide-react';
import { useTranslation } from '../i18n';
import { formatSize } from '../utils/formatters';

/** RemoteEntry as returned by Rust (includes metadata with Zoho file ID) */
interface TrashEntry {
  name: string;
  path: string;
  is_dir: boolean;
  size: number;
  modified: string | null;
  metadata: Record<string, string>;
}

interface ZohoTrashManagerProps {
  onClose: () => void;
  onRefreshFiles?: () => void;
}

export function ZohoTrashManager({ onClose, onRefreshFiles }: ZohoTrashManagerProps) {
  const t = useTranslation();
  const [items, setItems] = useState<TrashEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [actionLoading, setActionLoading] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const loadTrash = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const result = await invoke<TrashEntry[]>('zoho_list_trash');
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

  const getItemId = (item: TrashEntry): string => item.metadata?.id || item.path;

  const toggleSelect = (item: TrashEntry) => {
    const id = getItemId(item);
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
      setSelected(new Set(items.map(getItemId)));
    }
  };

  const getSelectedIds = (): string[] => Array.from(selected);

  const handleRestore = async () => {
    const ids = getSelectedIds();
    if (ids.length === 0) return;
    setActionLoading('restore');
    try {
      await invoke('zoho_restore_from_trash', { fileIds: ids });
      await loadTrash();
      onRefreshFiles?.();
    } catch (err) {
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
    const ids = getSelectedIds();
    if (ids.length === 0) return;
    setActionLoading('delete');
    try {
      await invoke('zoho_permanent_delete', { fileIds: ids });
      await loadTrash();
      onRefreshFiles?.();
    } catch (err) {
      setError(String(err));
    } finally {
      setActionLoading(null);
    }
  };

  // Escape key handler
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
        className="bg-[var(--color-bg-primary)] border border-[var(--color-border)] rounded-lg shadow-2xl w-[600px] max-h-[80vh] flex flex-col animate-scale-in"
        onClick={e => e.stopPropagation()}
        role="dialog"
        aria-modal="true"
        aria-label={t('contextMenu.trashTitle')}
      >
        {/* Header */}
        <div className="flex items-center justify-between px-4 py-3 border-b border-[var(--color-border)]">
          <div className="flex items-center gap-2">
            <Trash2 size={18} className="text-[var(--color-text-secondary)]" />
            <h2 className="text-sm font-semibold text-[var(--color-text-primary)]">
              {t('contextMenu.trashTitle')} — Zoho WorkDrive
            </h2>
            <span className="text-xs text-[var(--color-text-tertiary)]">
              ({items.length})
            </span>
          </div>
          <div className="flex items-center gap-1">
            <button
              onClick={loadTrash}
              disabled={loading}
              className="p-1.5 rounded hover:bg-[var(--color-bg-tertiary)] text-[var(--color-text-secondary)]"
              title="Refresh"
            >
              <RefreshCw size={14} className={loading ? 'animate-spin' : ''} />
            </button>
            <button
              onClick={onClose}
              className="p-1.5 rounded hover:bg-[var(--color-bg-tertiary)] text-[var(--color-text-secondary)]"
            >
              <X size={14} />
            </button>
          </div>
        </div>

        {/* Toolbar */}
        {items.length > 0 && (
          <div className="flex items-center gap-2 px-4 py-2 border-b border-[var(--color-border)] bg-[var(--color-bg-secondary)]">
            <button
              onClick={toggleSelectAll}
              className="flex items-center gap-1.5 px-2 py-1 text-xs rounded hover:bg-[var(--color-bg-tertiary)] text-[var(--color-text-secondary)]"
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
            <div className="flex items-center justify-center py-12 text-[var(--color-text-secondary)]">
              <Loader2 size={20} className="animate-spin mr-2" />
              {t('contextMenu.trashLoading')}
            </div>
          ) : error ? (
            <div className="flex items-center justify-center py-12 text-red-500">
              <AlertTriangle size={16} className="mr-2" />
              {error}
            </div>
          ) : items.length === 0 ? (
            <div className="flex flex-col items-center justify-center py-12 text-[var(--color-text-tertiary)]">
              <Trash2 size={32} className="mb-2 opacity-30" />
              {t('contextMenu.trashEmpty')}
            </div>
          ) : (
            <table className="w-full text-xs">
              <thead className="sticky top-0 bg-[var(--color-bg-secondary)] border-b border-[var(--color-border)]">
                <tr className="text-left text-[var(--color-text-tertiary)]">
                  <th className="w-8 px-2 py-1.5"></th>
                  <th className="px-2 py-1.5">{t('common.name')}</th>
                  <th className="px-2 py-1.5 w-20 text-right">{t('common.size')}</th>
                  <th className="px-2 py-1.5 w-32">{t('contextMenu.trashDeletedDate')}</th>
                </tr>
              </thead>
              <tbody>
                {items.map(item => {
                  const itemId = getItemId(item);
                  return (
                    <tr
                      key={itemId}
                      className={`cursor-pointer hover:bg-[var(--color-bg-tertiary)] border-b border-[var(--color-border)]/30 ${
                        selected.has(itemId) ? 'bg-[var(--color-accent)]/10' : ''
                      }`}
                      onClick={() => toggleSelect(item)}
                    >
                      <td className="px-2 py-1.5 text-center">
                        {selected.has(itemId) ? (
                          <CheckSquare size={13} className="text-[var(--color-accent)]" />
                        ) : (
                          <Square size={13} className="text-[var(--color-text-tertiary)]" />
                        )}
                      </td>
                      <td className="px-2 py-1.5">
                        <div className="flex items-center gap-1.5">
                          {item.is_dir ? (
                            <Folder size={13} className="text-yellow-500 shrink-0" />
                          ) : (
                            <File size={13} className="text-[var(--color-text-tertiary)] shrink-0" />
                          )}
                          <span className="truncate text-[var(--color-text-primary)]">{item.name}</span>
                        </div>
                      </td>
                      <td className="px-2 py-1.5 text-right text-[var(--color-text-secondary)] tabular-nums">
                        {item.is_dir ? '—' : formatSize(item.size)}
                      </td>
                      <td className="px-2 py-1.5 text-[var(--color-text-tertiary)]">
                        {item.modified || '—'}
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          )}
        </div>
      </div>

      {/* Styled confirmation dialog (replaces window.confirm) */}
      {pendingDeleteConfirm && (
        <div className="fixed inset-0 z-[10000] bg-black/50 flex items-center justify-center" role="dialog" aria-modal="true" onClick={() => setPendingDeleteConfirm(false)}>
          <div className="bg-[var(--color-bg-primary)] border border-[var(--color-border)] rounded-xl p-6 shadow-2xl max-w-sm animate-scale-in" onClick={e => e.stopPropagation()}>
            <p className="text-[var(--color-text-primary)] mb-4">
              {t('contextMenu.permanentDeleteConfirm', { count: selected.size })}
            </p>
            <div className="flex justify-end gap-2">
              <button
                onClick={() => setPendingDeleteConfirm(false)}
                className="px-4 py-2 text-[var(--color-text-secondary)] hover:bg-[var(--color-bg-tertiary)] rounded-lg"
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
