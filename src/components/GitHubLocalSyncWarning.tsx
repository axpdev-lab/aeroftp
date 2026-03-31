// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { AlertTriangle, GitBranch, Upload, X } from 'lucide-react';
import { useTranslation } from '../i18n';

interface GitHubLocalSyncWarningProps {
  unpushedCount: number;
  branch: string;
  onPushFirst: () => void;
  onContinue: () => void;
  onCancel: () => void;
}

export function GitHubLocalSyncWarning({
  unpushedCount,
  branch,
  onPushFirst,
  onContinue,
  onCancel,
}: GitHubLocalSyncWarningProps) {
  const t = useTranslation();

  React.useEffect(() => {
    const handleKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onCancel();
    };
    window.addEventListener('keydown', handleKey);
    return () => window.removeEventListener('keydown', handleKey);
  }, [onCancel]);

  return (
    <div className="fixed inset-0 z-[9999] flex items-start justify-center pt-[15vh]">
      <div className="absolute inset-0 bg-black/50" onClick={onCancel} />
      <div
        className="relative border border-amber-200 dark:border-amber-700 rounded-lg shadow-2xl w-[480px] animate-scale-in overflow-hidden"
        style={{ backgroundColor: 'var(--color-bg-secondary)' }}
        role="dialog"
        aria-modal="true"
      >
        {/* Header */}
        <div className="flex items-center gap-3 px-5 py-4 border-b border-amber-100 dark:border-amber-800 bg-amber-50 dark:bg-amber-900/20">
          <AlertTriangle size={20} className="text-amber-500 flex-shrink-0" />
          <div className="flex-1">
            <h2 className="text-sm font-semibold text-amber-800 dark:text-amber-200">
              {t('github.unpushedCommitsTitle')}
            </h2>
            <p className="text-xs text-amber-600 dark:text-amber-400 mt-0.5">
              {unpushedCount} unpushed commit{unpushedCount > 1 ? 's' : ''} on <code className="bg-amber-100 dark:bg-amber-800/50 px-1 rounded">{branch}</code>
            </p>
          </div>
          <button onClick={onCancel} className="p-1 rounded hover:bg-amber-200 dark:hover:bg-amber-700 text-amber-500">
            <X size={16} />
          </button>
        </div>

        {/* Body */}
        <div className="px-5 py-4 text-sm text-gray-700 dark:text-gray-300 space-y-3">
          <p>
            {t('github.unpushedCommitsDesc')}
          </p>
          <div className="flex items-center gap-2 px-3 py-2 rounded-lg text-xs" style={{ backgroundColor: 'var(--color-bg-primary)', color: 'var(--color-text-secondary)' }}>
            <GitBranch size={14} />
            <span>{t('github.unpushedCommitsHint')}</span>
          </div>
        </div>

        {/* Actions */}
        <div className="flex items-center justify-end gap-2 px-5 py-3 border-t border-gray-200 dark:border-gray-700">
          <button
            onClick={onCancel}
            className="px-3 py-1.5 text-sm text-gray-600 dark:text-gray-400 hover:bg-gray-100 dark:hover:bg-gray-700 rounded transition-colors"
          >
            {t('common.cancel')}
          </button>
          <button
            onClick={onContinue}
            className="px-3 py-1.5 text-sm text-amber-600 dark:text-amber-400 hover:bg-amber-50 dark:hover:bg-amber-900/30 rounded transition-colors"
          >
            {t('github.continueAnyway')}
          </button>
          <button
            onClick={onPushFirst}
            className="px-4 py-1.5 text-sm bg-blue-500 hover:bg-blue-600 text-white rounded transition-colors flex items-center gap-1.5"
          >
            <Upload size={13} />
            {t('github.pushFirst')}
          </button>
        </div>
      </div>
    </div>
  );
}
