/**
 * GitHubWriteModeIndicator Component
 * Compact status bar badge showing the current GitHub write mode.
 * Three states: direct (green), branch workflow (yellow), readonly (red).
 */

import React from 'react';
import { CheckCircle, GitBranch, Lock } from 'lucide-react';
import { useTranslation } from '../i18n';

interface GitHubWriteModeIndicatorProps {
  writeMode: 'direct' | 'branch' | 'readonly';
  workingBranch?: string;
}

export const GitHubWriteModeIndicator: React.FC<GitHubWriteModeIndicatorProps> = ({
  writeMode,
  workingBranch,
}) => {
  const t = useTranslation();

  switch (writeMode) {
    case 'direct':
      return (
        <span
          className="inline-flex items-center gap-1 text-xs text-green-500"
          title={t('github.writeModeDirectTooltip') || 'Changes are committed directly to the branch'}
        >
          <CheckCircle size={12} />
          <span>{t('github.writeModeDirect') || 'Direct'}</span>
        </span>
      );

    case 'branch':
      return (
        <span
          className="inline-flex items-center gap-1 text-xs text-yellow-500"
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
        </span>
      );

    case 'readonly':
      return (
        <span
          className="inline-flex items-center gap-1 text-xs text-red-400"
          title={t('github.writeModeReadOnlyTooltip') || 'No write access to this repository'}
        >
          <Lock size={12} />
          <span>{t('github.writeModeReadOnly') || 'Read-only'}</span>
        </span>
      );
  }
};
