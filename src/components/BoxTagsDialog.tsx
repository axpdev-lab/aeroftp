import * as React from 'react';
import { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Tag, X, Plus, Loader2 } from 'lucide-react';
import { useTranslation } from '../i18n';

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
  const [tags, setTags] = useState<string[]>(currentTags);
  const [input, setInput] = useState('');
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const handleKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', handleKey);
    return () => window.removeEventListener('keydown', handleKey);
  }, [onClose]);

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

  const hasChanges = JSON.stringify(tags.sort()) !== JSON.stringify([...currentTags].sort());

  return (
    <div className="fixed inset-0 z-[9999] flex items-center justify-center bg-black/50" onClick={onClose}>
      <div
        className="bg-[var(--color-bg-primary)] border border-[var(--color-border)] rounded-lg shadow-2xl w-[400px] flex flex-col animate-scale-in"
        onClick={e => e.stopPropagation()}
        role="dialog"
        aria-modal="true"
      >
        {/* Header */}
        <div className="flex items-center justify-between px-4 py-3 border-b border-[var(--color-border)]">
          <div className="flex items-center gap-2">
            <Tag size={16} className="text-blue-500" />
            <h2 className="text-sm font-semibold text-[var(--color-text-primary)]">
              {t('box.tagsTitle')}
            </h2>
          </div>
          <button
            onClick={onClose}
            className="p-1.5 rounded hover:bg-[var(--color-bg-tertiary)] text-[var(--color-text-secondary)]"
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
              className="flex-1 px-3 py-1.5 text-xs rounded border border-[var(--color-border)] bg-[var(--color-bg-secondary)] text-[var(--color-text-primary)] focus:outline-none focus:ring-1 focus:ring-blue-500"
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
              <span className="text-xs text-[var(--color-text-tertiary)] italic">{t('box.noTags')}</span>
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
        <div className="flex justify-end gap-2 px-4 py-3 border-t border-[var(--color-border)]">
          <button
            onClick={onClose}
            className="px-4 py-1.5 text-xs text-[var(--color-text-secondary)] hover:bg-[var(--color-bg-tertiary)] rounded"
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
    </div>
  );
}
