// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

/**
 * GitHubWriteModeIndicator Component
 * Compact status bar badge showing the current GitHub write mode.
 * Three states: direct (green), branch workflow (yellow), readonly (red).
 * In branch mode, includes a "Create PR" button.
 */

import React, { useCallback, useState } from 'react';
import { CheckCircle, GitBranch, GitPullRequest, Lock, Globe, LockKeyhole } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-shell';
import { useTranslation } from '../i18n';

interface GitHubWriteModeIndicatorProps {
  writeMode: 'direct' | 'branch' | 'readonly';
  workingBranch?: string;
  isPrivate?: boolean;
  onError?: (title: string, message: string) => void;
  protocol?: string;
}

export const GitHubWriteModeIndicator: React.FC<GitHubWriteModeIndicatorProps> = ({
  writeMode,
  workingBranch,
  isPrivate,
  onError,
  protocol,
}) => {
  const t = useTranslation();
  const [showPrPrompt, setShowPrPrompt] = useState(false);
  const [prTitle, setPrTitle] = useState('');
  const [prBody, setPrBody] = useState('');
  const [creating, setCreating] = useState(false);

  const handleCreatePr = useCallback(async () => {
    if (!prTitle.trim()) return;
    setCreating(true);
    try {
      const command = protocol === 'gitlab' ? 'gitlab_create_merge_request' : 'github_create_pr';
      const url = await invoke<string>(command, {
        title: prTitle.trim(),
        body: prBody.trim(),
      });
      setShowPrPrompt(false);
      setPrTitle('');
      setPrBody('');
      if (url) {
        await open(url);
      }
    } catch (err) {
      console.error('Failed to create PR:', err);
      if (onError) onError('Pull Request', String(err));
    } finally {
      setCreating(false);
    }
  }, [prTitle, prBody, protocol, onError]);

  const visibilityBadge = (
    <span
      className={`inline-flex items-center gap-0.5 text-xs ${isPrivate ? 'text-yellow-500' : 'text-gray-400'}`}
      title={isPrivate ? 'Private repository' : 'Public repository'}
    >
      {isPrivate ? <LockKeyhole size={11} /> : <Globe size={11} />}
      <span>{isPrivate ? 'Private' : 'Public'}</span>
    </span>
  );

  switch (writeMode) {
    case 'direct':
      return (
        <span className="inline-flex items-center gap-2 whitespace-nowrap">
          {visibilityBadge}
          <span
            className="inline-flex items-center gap-1 text-xs text-green-500 whitespace-nowrap"
            title={t('github.writeModeDirectTooltip') || 'Changes are committed directly to the branch'}
          >
            <CheckCircle size={12} />
            <span>{t('github.writeModeDirect') || 'Direct'}</span>
          </span>
        </span>
      );

    case 'branch':
      return (
        <>
          <span className="inline-flex items-center gap-2 whitespace-nowrap">
          {visibilityBadge}
          <span
            className="inline-flex items-center gap-1 text-xs text-yellow-500 whitespace-nowrap"
            title={
              t('github.writeModeBranchTooltip', { branch: workingBranch || '' }) ||
              `Changes will be committed to ${workingBranch || 'working branch'}`
            }
          >
            <GitBranch size={12} />
            <span>
              {workingBranch
                ? (t('github.writeModeBranchName', { branch: workingBranch }) || workingBranch)
                : (t('github.writeModeBranch') || 'Branch')
              }
            </span>
            <button
              onClick={() => setShowPrPrompt(true)}
              className="ml-1 inline-flex items-center gap-0.5 px-1 py-0.5 rounded text-[10px] font-medium
                bg-yellow-500/20 hover:bg-yellow-500/30 text-yellow-400 transition-colors"
              title={t('github.createPr') || 'Create Pull Request'}
            >
              <GitPullRequest size={10} />
              <span>{protocol === 'gitlab' ? 'MR' : 'PR'}</span>
            </button>
          </span>
          </span>

          {showPrPrompt && (
            <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50"
              onClick={(e) => { if (e.target === e.currentTarget) setShowPrPrompt(false); }}
            >
              <div className="bg-[var(--color-bg-secondary)] border border-[var(--color-border)] rounded-lg shadow-xl p-4 w-96 animate-scale-in">
                <h3 className="text-sm font-semibold text-[var(--color-text-primary)] mb-3 flex items-center gap-2">
                  <GitPullRequest size={16} />
                  {protocol === 'gitlab'
                    ? (t('gitlab.createMergeRequest') || 'Create Merge Request')
                    : (t('github.createPr') || 'Create Pull Request')
                  }
                </h3>

                <input
                  type="text"
                  placeholder={t('github.prTitle') || 'PR title'}
                  value={prTitle}
                  onChange={(e) => setPrTitle(e.target.value)}
                  className="w-full px-2 py-1.5 text-sm rounded border border-[var(--color-border)]
                    bg-[var(--color-bg-primary)] text-[var(--color-text-primary)]
                    placeholder:text-[var(--color-text-tertiary)] mb-2"
                  autoFocus
                  onKeyDown={(e) => { if (e.key === 'Escape') setShowPrPrompt(false); }}
                />

                <textarea
                  placeholder={t('github.prBody') || 'Description (optional)'}
                  value={prBody}
                  onChange={(e) => setPrBody(e.target.value)}
                  rows={4}
                  className="w-full px-2 py-1.5 text-sm rounded border border-[var(--color-border)]
                    bg-[var(--color-bg-primary)] text-[var(--color-text-primary)]
                    placeholder:text-[var(--color-text-tertiary)] mb-3 resize-none"
                  onKeyDown={(e) => { if (e.key === 'Escape') setShowPrPrompt(false); }}
                />

                <div className="flex justify-end gap-2">
                  <button
                    onClick={() => setShowPrPrompt(false)}
                    className="px-3 py-1.5 text-xs rounded border border-[var(--color-border)]
                      text-[var(--color-text-secondary)] hover:bg-[var(--color-bg-tertiary)] transition-colors"
                    disabled={creating}
                  >
                    {t('common.cancel') || 'Cancel'}
                  </button>
                  <button
                    onClick={handleCreatePr}
                    disabled={!prTitle.trim() || creating}
                    className="px-3 py-1.5 text-xs rounded bg-green-600 hover:bg-green-700
                      text-white font-medium transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
                  >
                    {creating
                      ? (t('common.creating') || 'Creating...')
                      : (t('github.createPr') || 'Create PR')
                    }
                  </button>
                </div>
              </div>
            </div>
          )}
        </>
      );

    case 'readonly':
      return (
        <span className="inline-flex items-center gap-2 whitespace-nowrap">
          {visibilityBadge}
          <span
            className="inline-flex items-center gap-1 text-xs text-red-400 whitespace-nowrap"
            title={t('github.writeModeReadOnlyTooltip') || 'No write access to this repository'}
          >
            <Lock size={12} />
            <span>{t('github.writeModeReadOnly') || 'Read-only'}</span>
          </span>
        </span>
      );
  }
};
