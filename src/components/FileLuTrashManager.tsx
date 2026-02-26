import * as React from 'react';
import { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Trash2, RotateCcw, X, RefreshCw, Loader2, File, CheckSquare, Square, Clock } from 'lucide-react';
import { useTranslation } from '../i18n';

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
    setActionLoading('restore');
    try {
      for (const code of selected) {
        await invoke('filelu_restore_file', { fileCode: code });
      }
      await loadTrash();
      onRefreshFiles?.();
    } catch (err) {
      setError(String(err));
    } finally {
      setActionLoading(null);
    }
  };

  const deleteSelected = async () => {
    if (selected.size === 0) return;
    setActionLoading('delete');
    try {
      for (const code of selected) {
        await invoke('filelu_permanent_delete', { fileCode: code });
      }
      await loadTrash();
    } catch (err) {
      setError(String(err));
    } finally {
      setActionLoading(null);
    }
  };

  const allCodes = items.map(i => i.file_code).filter(Boolean) as string[];
  const allSelected = allCodes.length > 0 && selected.size === allCodes.length;

  return (
    <div className="fixed inset-0 z-50 flex items-start justify-center pt-[5vh] bg-black/50 backdrop-blur-sm">
      <div className="relative w-full max-w-xl mx-4 rounded-xl shadow-2xl bg-[var(--color-bg-primary)] border border-[var(--color-border)] animate-scale-in">

        {/* Header */}
        <div className="flex items-center justify-between px-5 py-4 border-b border-[var(--color-border)]">
          <div className="flex items-center gap-2">
            <Trash2 size={18} className="text-red-500" />
            <h2 className="text-base font-semibold text-[var(--color-text-primary)]">
              {t('filelu.trashTitle')}
            </h2>
            {!loading && (
              <span className="text-xs text-[var(--color-text-muted)] ml-1">
                ({items.length})
              </span>
            )}
          </div>
          <div className="flex items-center gap-2">
            <button
              onClick={loadTrash}
              disabled={loading}
              className="p-1.5 rounded text-[var(--color-text-muted)] hover:text-[var(--color-text-primary)] hover:bg-[var(--color-bg-secondary)] transition-colors"
              title={t('common.refresh')}
            >
              <RefreshCw size={14} className={loading ? 'animate-spin' : ''} />
            </button>
            <button onClick={onClose} className="p-1.5 rounded text-[var(--color-text-muted)] hover:text-[var(--color-text-primary)] hover:bg-[var(--color-bg-secondary)] transition-colors">
              <X size={14} />
            </button>
          </div>
        </div>

        {/* Toolbar */}
        {items.length > 0 && (
          <div className="flex items-center gap-2 px-5 py-2.5 border-b border-[var(--color-border)] bg-[var(--color-bg-secondary)]">
            <button onClick={toggleAll} className="flex items-center gap-1.5 text-xs text-[var(--color-text-muted)] hover:text-[var(--color-text-primary)] transition-colors">
              {allSelected ? <CheckSquare size={14} /> : <Square size={14} />}
              {t('common.selectAll')}
            </button>
            <span className="text-[var(--color-border)]">|</span>
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
                <button
                  onClick={deleteSelected}
                  disabled={actionLoading !== null}
                  className="flex items-center gap-1.5 text-xs text-red-500 hover:text-red-400 transition-colors disabled:opacity-50"
                >
                  {actionLoading === 'delete' ? <Loader2 size={13} className="animate-spin" /> : <Trash2 size={13} />}
                  {t('filelu.permanentDelete')} ({selected.size})
                </button>
              </>
            )}
          </div>
        )}

        {/* Body */}
        <div className="max-h-[50vh] overflow-y-auto">
          {loading ? (
            <div className="flex items-center justify-center py-12">
              <Loader2 size={22} className="animate-spin text-[var(--color-accent)]" />
            </div>
          ) : error ? (
            <div className="flex flex-col items-center gap-2 py-10 text-sm text-red-500 px-6 text-center">
              <span>{error}</span>
              <button onClick={loadTrash} className="mt-2 text-xs underline text-[var(--color-text-muted)]">{t('common.retry')}</button>
            </div>
          ) : items.length === 0 ? (
            <div className="flex flex-col items-center gap-2 py-12 text-[var(--color-text-muted)]">
              <Trash2 size={32} className="opacity-30" />
              <span className="text-sm">{t('filelu.trashEmpty')}</span>
            </div>
          ) : (
            <ul className="divide-y divide-[var(--color-border)]">
              {items.map(item => {
                const code = item.file_code ?? '';
                const isSelected = selected.has(code);
                return (
                  <li
                    key={code}
                    className={`flex items-center gap-3 px-5 py-3 cursor-pointer hover:bg-[var(--color-bg-secondary)] transition-colors ${isSelected ? 'bg-[var(--color-bg-secondary)]' : ''}`}
                    onClick={() => toggleSelect(code)}
                  >
                    <div className="flex-shrink-0 text-[var(--color-text-muted)]">
                      {isSelected ? <CheckSquare size={15} className="text-[var(--color-accent)]" /> : <Square size={15} />}
                    </div>
                    <File size={15} className="flex-shrink-0 text-[var(--color-text-muted)]" />
                    <div className="flex-1 min-w-0">
                      <p className="text-sm text-[var(--color-text-primary)] truncate">{item.name ?? code}</p>
                      <div className="flex items-center gap-2 mt-0.5">
                        <Clock size={11} className="text-[var(--color-text-muted)]" />
                        <span className="text-xs text-[var(--color-text-muted)]">
                          {item.deleted ?? formatDeletedAgo(item.deleted_ago_sec)}
                        </span>
                        <span className="text-xs text-[var(--color-text-muted)] opacity-60 font-mono">{code}</span>
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
                      <button
                        onClick={e => { e.stopPropagation(); invoke('filelu_permanent_delete', { fileCode: code }).then(loadTrash); }}
                        className="p-1 rounded text-red-500 hover:bg-red-500/10 transition-colors"
                        title={t('filelu.permanentDeleteOne')}
                      >
                        <Trash2 size={13} />
                      </button>
                    </div>
                  </li>
                );
              })}
            </ul>
          )}
        </div>

        {/* Footer */}
        <div className="px-5 py-3 border-t border-[var(--color-border)] text-xs text-[var(--color-text-muted)]">
          {t('filelu.trashFooter')}
        </div>
      </div>
    </div>
  );
}
