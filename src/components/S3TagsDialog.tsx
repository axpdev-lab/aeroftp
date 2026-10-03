// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { useState, useEffect, useRef } from 'react';
import { X, Tag, Plus, Trash2, Loader2 } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';
import { useTranslation } from '../i18n';
import { useDraggableModal } from '../hooks/useDraggableModal';
import { useModalFocusTrap } from '../hooks/useModalFocusTrap';
import { loadS3Tags, saveS3Tags, S3_MAX_TAGS, type TagRow } from '../utils/cloudTiers';

interface S3TagsDialogProps {
  path: string;
  name: string;
  onClose: () => void;
  onSaved: () => void;
}

/** Key/value tags of one S3 object, at most ten as AWS allows. */
export function S3TagsDialog({ path, name, onClose, onSaved }: S3TagsDialogProps) {
  const t = useTranslation();
  const modalDrag = useDraggableModal();
  const panelRef = useRef<HTMLDivElement>(null);
  useModalFocusTrap(panelRef);
  const [rows, setRows] = useState<TagRow[] | null>(null);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [attempt, setAttempt] = useState(0);

  // A failed load leaves `rows` null, which keeps Save disabled: an empty
  // editor would save as "no tags" and delete the ones the object has.
  useEffect(() => {
    let cancelled = false;
    loadS3Tags(invoke, path)
      .then((r) => { if (!cancelled) setRows(r); })
      .catch((err) => { if (!cancelled) setLoadError(String(err)); });
    return () => { cancelled = true; };
  }, [path, attempt]);

  useEffect(() => {
    const handleKey = (e: KeyboardEvent) => { if (e.key === 'Escape') onClose(); };
    window.addEventListener('keydown', handleKey);
    return () => window.removeEventListener('keydown', handleKey);
  }, [onClose]);

  const update = (i: number, field: keyof TagRow, value: string) =>
    setRows((prev) => (prev ?? []).map((r, j) => (j === i ? { ...r, [field]: value } : r)));

  const save = async () => {
    if (!rows) return;
    setSaving(true);
    setError(null);
    try {
      await saveS3Tags(invoke, path, rows);
      onSaved();
      onClose();
    } catch (err) {
      setError(String(err));
    } finally {
      setSaving(false);
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
        aria-label={t('s3.objectTags')}
      >
        <div {...modalDrag.dragHandleProps} className="flex items-center justify-between px-5 py-3 border-b border-gray-200 dark:border-gray-700 cursor-grab active:cursor-grabbing">
          <div className="flex items-center gap-2">
            <Tag size={16} className="text-cyan-500" />
            <h2 className="text-sm font-semibold text-gray-900 dark:text-gray-100">{t('s3.objectTags')}</h2>
          </div>
          <button onClick={onClose} className="p-1 rounded hover:bg-gray-200 dark:hover:bg-gray-700" aria-label={t('common.close')}>
            <X size={16} className="text-gray-500" />
          </button>
        </div>
        <div className="px-5 py-2 border-b border-gray-200 dark:border-gray-700/50">
          <p className="text-xs text-gray-500 dark:text-gray-400 truncate" title={path}>{name}</p>
        </div>
        <div className="px-5 py-3 max-h-72 overflow-y-auto space-y-2">
          {rows === null ? (
            loadError ? (
              <div className="text-center py-4">
                <p className="text-xs text-red-500">{loadError}</p>
                <button
                  onClick={() => { setLoadError(null); setAttempt((n) => n + 1); }}
                  className="mt-2 text-xs text-blue-500 hover:underline"
                >
                  {t('common.retry')}
                </button>
              </div>
            ) : (
              <div className="flex justify-center py-4"><Loader2 size={16} className="animate-spin text-gray-400" /></div>
            )
          ) : (
            <>
              {rows.length === 0 && <p className="text-xs text-gray-500 dark:text-gray-400">{t('s3.noTags')}</p>}
              {rows.map((r, i) => (
                <div key={i} className="flex items-center gap-1.5">
                  <input
                    value={r.key}
                    onChange={(e) => update(i, 'key', e.target.value)}
                    placeholder={t('s3.tagKey')}
                    aria-label={t('s3.tagKey')}
                    className="w-2/5 px-2 py-1 text-xs bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded"
                  />
                  <input
                    value={r.value}
                    onChange={(e) => update(i, 'value', e.target.value)}
                    placeholder={t('s3.tagValue')}
                    aria-label={t('s3.tagValue')}
                    className="flex-1 px-2 py-1 text-xs bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded"
                  />
                  <button
                    onClick={() => setRows((prev) => (prev ?? []).filter((_, j) => j !== i))}
                    className="p-1 rounded text-red-500 hover:bg-red-500/10"
                    title={t('common.remove')}
                    aria-label={t('common.remove')}
                  >
                    <Trash2 size={12} />
                  </button>
                </div>
              ))}
              <button
                onClick={() => setRows((prev) => [...(prev ?? []), { key: '', value: '' }])}
                disabled={rows.length >= S3_MAX_TAGS}
                className="flex items-center gap-1 text-xs text-blue-500 hover:underline disabled:opacity-50 disabled:no-underline"
              >
                <Plus size={12} /> {t('common.add')}
              </button>
            </>
          )}
          {error && <p className="text-xs text-red-500">{error}</p>}
        </div>
        <div className="flex justify-end gap-2 px-5 py-3 border-t border-gray-200 dark:border-gray-700">
          <button onClick={onClose} className="px-3 py-1.5 text-xs rounded-lg border border-gray-300 dark:border-gray-600 text-gray-700 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-800">
            {t('common.cancel')}
          </button>
          <button
            onClick={() => void save()}
            disabled={saving || rows === null}
            className="flex items-center gap-1.5 px-3 py-1.5 text-xs rounded-lg bg-blue-500 text-white hover:bg-blue-600 disabled:opacity-50 disabled:cursor-not-allowed"
          >
            {saving && <Loader2 size={12} className="animate-spin" />}
            {t('common.save')}
          </button>
        </div>
      </div>
    </div>
  );
}
