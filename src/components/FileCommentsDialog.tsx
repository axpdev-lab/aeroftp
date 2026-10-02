// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { useState, useEffect, useRef, useCallback } from 'react';
import { X, MessageSquare, Send, Loader2, Trash2 } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';
import { useTranslation } from '../i18n';
import { useModalFocusTrap } from '../hooks/useModalFocusTrap';
import { useDraggableModal } from '../hooks/useDraggableModal';
import { getUiLocale } from '../utils/formatters';
import {
  addComment,
  deleteComment,
  listComments,
  type CommentsProvider,
  type FileComment,
} from '../utils/boxDriveSocial';


interface FileCommentsDialogProps {
  provider: CommentsProvider;
  filePath: string;
  fileName: string;
  onClose: () => void;
}

/**
 * The comments of a Box or Google Drive file: read them, add one, delete one.
 * Google Drive used to have an add-only dialog and Box had none.
 */
export function FileCommentsDialog({ provider, filePath, fileName, onClose }: FileCommentsDialogProps) {
  const t = useTranslation();
  const modalDrag = useDraggableModal();
  const [comments, setComments] = useState<FileComment[] | null>(null);
  const [message, setMessage] = useState('');
  const [sending, setSending] = useState(false);
  const [deletingId, setDeletingId] = useState<string | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const panelRef = useRef<HTMLDivElement>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  const reload = useCallback(async () => {
    try {
      setComments(await listComments(invoke, provider, filePath));
      setLoadError(null);
    } catch (err) {
      // A failed load is not a file without comments: keep what is on screen and say why.
      setLoadError(String(err));
    }
  }, [provider, filePath]);

  useEffect(() => {
    void reload();
  }, [reload]);

  useEffect(() => {
    const handleKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', handleKey);
    return () => window.removeEventListener('keydown', handleKey);
  }, [onClose]);

  useModalFocusTrap(panelRef, textareaRef);

  const handleSubmit = async () => {
    if (!message.trim() || sending) return;
    setSending(true);
    setError(null);
    setNotice(null);
    try {
      await addComment(invoke, provider, filePath, message.trim());
      setMessage('');
      setNotice(t('box.commentAdded'));
      await reload();
    } catch (err) {
      setError(String(err));
    } finally {
      setSending(false);
    }
  };

  const handleDelete = async (id: string) => {
    setDeletingId(id);
    setError(null);
    setNotice(null);
    try {
      await deleteComment(invoke, provider, filePath, id);
      setComments((prev) => (prev ?? []).filter((c) => c.id !== id));
      setNotice(t('box.commentDeleted'));
    } catch (err) {
      setError(String(err));
    } finally {
      setDeletingId(null);
    }
  };

  return (
    <div className="fixed inset-0 z-50 flex items-start justify-center pt-[5vh]">
      <div className="absolute inset-0 bg-black/50" onClick={onClose} />
      <div
        {...modalDrag.panelProps}
        ref={panelRef}
        className="relative bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg shadow-2xl w-full max-w-md overflow-hidden animate-scale-in"
        role="dialog"
        aria-modal="true"
        aria-label={t('fileComments.title')}
      >
        <div {...modalDrag.dragHandleProps} className="flex items-center justify-between px-5 py-3 border-b border-gray-200 dark:border-gray-700 cursor-grab active:cursor-grabbing">
          <div className="flex items-center gap-2">
            <MessageSquare size={16} className="text-blue-500" />
            <h2 className="text-sm font-semibold text-gray-900 dark:text-gray-100">{t('fileComments.title')}</h2>
          </div>
          <button onClick={onClose} className="p-1 rounded hover:bg-gray-200 dark:hover:bg-gray-700" aria-label={t('common.close')}>
            <X size={16} className="text-gray-500" />
          </button>
        </div>

        <div className="px-5 py-2 border-b border-gray-200 dark:border-gray-700/50">
          <p className="text-xs text-gray-500 dark:text-gray-400 truncate" title={filePath}>{fileName}</p>
        </div>

        <div className="px-5 py-3 max-h-72 overflow-y-auto space-y-2">
          {loadError && <p className="text-xs text-red-500 py-2">{loadError}</p>}
          {comments === null ? (
            !loadError && <div className="flex justify-center py-4"><Loader2 size={16} className="animate-spin text-gray-400" /></div>
          ) : comments.length === 0 ? (
            <p className="text-xs text-gray-500 dark:text-gray-400 py-2">{t('fileComments.empty')}</p>
          ) : (
            comments.map((c) => (
              <div key={c.id} className="group rounded-lg border border-gray-200 dark:border-gray-700 p-2">
                <div className="flex items-start justify-between gap-2">
                  <div className="min-w-0">
                    <div className="text-[11px] text-gray-500 dark:text-gray-400">
                      {c.author ?? '?'}
                      {c.createdAt && ` · ${new Date(c.createdAt).toLocaleString(getUiLocale())}`}
                    </div>
                    <p className="text-xs text-gray-900 dark:text-gray-100 whitespace-pre-wrap break-words">{c.text}</p>
                  </div>
                  <button
                    onClick={() => void handleDelete(c.id)}
                    disabled={deletingId === c.id}
                    className="p-1 rounded text-red-500 hover:bg-red-500/10 disabled:opacity-50"
                    title={t('common.delete')}
                    aria-label={t('common.delete')}
                  >
                    {deletingId === c.id ? <Loader2 size={12} className="animate-spin" /> : <Trash2 size={12} />}
                  </button>
                </div>
              </div>
            ))
          )}
        </div>

        <div className="px-5 py-3 border-t border-gray-200 dark:border-gray-700/50">
          <textarea
            ref={textareaRef}
            value={message}
            onChange={(e) => setMessage(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) {
                e.preventDefault();
                void handleSubmit();
              }
            }}
            placeholder={t('box.commentPlaceholder')}
            rows={3}
            className="w-full px-3 py-2 text-sm bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg resize-none focus:outline-none focus:ring-2 focus:ring-blue-500 text-gray-900 dark:text-gray-100 placeholder-gray-400"
          />
          {error && <p className="mt-2 text-xs text-red-500">{error}</p>}
          {notice && !error && <p className="mt-2 text-xs text-green-500">{notice}</p>}
        </div>

        <div className="flex justify-end gap-2 px-5 py-3 border-t border-gray-200 dark:border-gray-700">
          <button
            onClick={onClose}
            className="px-3 py-1.5 text-xs rounded-lg border border-gray-300 dark:border-gray-600 text-gray-700 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-800"
          >
            {t('common.close')}
          </button>
          <button
            onClick={() => void handleSubmit()}
            disabled={!message.trim() || sending}
            className="flex items-center gap-1.5 px-3 py-1.5 text-xs rounded-lg bg-blue-500 text-white hover:bg-blue-600 disabled:opacity-50 disabled:cursor-not-allowed"
          >
            {sending ? <Loader2 size={12} className="animate-spin" /> : <Send size={12} />}
            {t('box.addComment')}
          </button>
        </div>
      </div>
    </div>
  );
}
