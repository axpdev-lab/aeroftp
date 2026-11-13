// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Tag, X, Plus, Loader2 } from 'lucide-react';
import { useTranslation } from '../i18n';
import { useDraggableModal } from '../hooks/useDraggableModal';
import { useGuardedClose } from '../hooks/useGuardedClose';
import { GuardedCloseConfirm } from './GuardedCloseConfirm';

interface BoxTagsDialogProps {
  filePath: string;
  currentTags: string[];
  onClose: () => void;
  onUpdated: () => void;
  command?: string;
  providerName?: string;
}

export function BoxTagsDialog({ filePath, currentTags, onClose, onUpdated, command = 'box_set_tags', providerName = 'Box' }: BoxTagsDialogProps) {
  const t = useTranslation();
  const modalDrag = useDraggableModal();
  const [tags, setTags] = useState<string[]>(currentTags);
  const [input, setInput] = useState('');
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const hasChanges = JSON.stringify([...tags].sort()) !== JSON.stringify([...currentTags].sort());
  // A save already sent goes on after a close, and changed tags or a typed tag are unsaved: the backdrop is inert
  // in both cases and the X asks first (Discussion #1527).
  const guarded = useGuardedClose({ guard: saving ? 'busy' : hasChanges || input.trim() ? 'dirty' : null, onClose });

  useEffect(() => {
    const handleKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') guarded.requestClose();
    };
    window.addEventListener('keydown', handleKey);
    return () => window.removeEventListener('keydown', handleKey);
  }, [guarded.requestClose]);

  const addTag = () => {
    const trimmed = input.trim().toLowerCase();
    if (!trimmed || tags.includes(trimmed)) return;
    setTags(prev => [...prev, trimmed]);
    setInput('');
  };

  const removeTag = (tag: string) => {
    setTags(prev => prev.filter(t => t !== tag));
  };

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'Enter') {
      e.preventDefault();
      addTag();
    }
  };

  const handleSave = async () => {
    setSaving(true);
    setError(null);
    try {
      await invoke(command, { path: filePath, tags });
      onUpdated();
      onClose();
    } catch (err) {
      setError(String(err));
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="fixed inset-0 z-[9999] flex items-center justify-center bg-black/50" onClick={guarded.onBackdropClick}>
      <div
        {...modalDrag.panelProps}
        className="bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg shadow-2xl w-[400px] flex flex-col animate-scale-in"
        onClick={e => e.stopPropagation()}
        role="dialog"
        aria-modal="true"
      >
        {/* Header */}
        <div {...modalDrag.dragHandleProps} className="flex items-center justify-between px-4 py-3 border-b border-gray-200 dark:border-gray-700 cursor-grab active:cursor-grabbing">
          <div className="flex items-center gap-2">
            <Tag size={16} className="text-blue-500" />
            <h2 className="text-sm font-semibold text-gray-900 dark:text-gray-100">
              {t('box.tagsTitle')}
            </h2>
          </div>
          <button
            onClick={guarded.requestClose}
            className="p-1.5 rounded hover:bg-gray-100 dark:hover:bg-gray-700 text-gray-600 dark:text-gray-400"
          >
            <X size={14} />
          </button>
        </div>

        {/* Content */}
        <div className="px-4 py-3 space-y-3">
          {/* Tag input */}
          <div className="flex gap-2">
            <input
              type="text"
              value={input}
              onChange={e => setInput(e.target.value)}
              onKeyDown={handleKeyDown}
              placeholder={t('box.tagsPlaceholder')}
              className="flex-1 px-3 py-1.5 text-xs rounded border border-gray-300 dark:border-gray-600 bg-gray-50 dark:bg-gray-700 text-gray-900 dark:text-gray-100 focus:outline-none focus:ring-1 focus:ring-blue-500"
              maxLength={50}
              autoFocus
            />
            <button
              onClick={addTag}
              disabled={!input.trim()}
              className="px-3 py-1.5 text-xs rounded bg-blue-600 text-white hover:bg-blue-700 disabled:opacity-40 disabled:cursor-not-allowed flex items-center gap-1"
            >
              <Plus size={12} />
              {t('box.addTag')}
            </button>
          </div>

          {/* Tag list */}
          <div className="flex flex-wrap gap-1.5 min-h-[32px]">
            {tags.length === 0 ? (
              <span className="text-xs text-gray-500 dark:text-gray-500 italic">{t('box.noTags')}</span>
            ) : (
              tags.map(tag => (
                <span
                  key={tag}
                  className="inline-flex items-center gap-1 px-2 py-0.5 text-xs rounded-full bg-blue-100 text-blue-700 dark:bg-blue-900/40 dark:text-blue-300"
                >
                  {tag}
                  <button
                    onClick={() => removeTag(tag)}
                    className="hover:text-red-500 transition-colors"
                  >
                    <X size={10} />
                  </button>
                </span>
              ))
            )}
          </div>

          {error && (
            <p className="text-xs text-red-500">{error}</p>
          )}
        </div>

        {/* Footer */}
        <div className="flex justify-end gap-2 px-4 py-3 border-t border-gray-200 dark:border-gray-700">
          <button
            onClick={guarded.requestClose}
            disabled={saving}
            className="px-4 py-1.5 text-xs text-gray-600 dark:text-gray-400 hover:bg-gray-100 dark:hover:bg-gray-700 rounded disabled:opacity-50"
          >
            {t('common.cancel')}
          </button>
          <button
            onClick={handleSave}
            disabled={saving || !hasChanges}
            className="px-4 py-1.5 text-xs rounded bg-blue-600 text-white hover:bg-blue-700 disabled:opacity-40 disabled:cursor-not-allowed flex items-center gap-1.5"
          >
            {saving && <Loader2 size={12} className="animate-spin" />}
            {t('common.save')}
          </button>
        </div>
      </div>
      {guarded.confirmOpen && guarded.confirmKind && (
        <GuardedCloseConfirm kind={guarded.confirmKind} canAbort={guarded.canAbort} onKeep={guarded.cancelConfirm} onConfirm={guarded.confirmAndClose} />
      )}
    </div>
  );
}
