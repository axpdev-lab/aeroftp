/**
 * GitHubCommitDialog Component
 * Modal dialog for entering a commit message when uploading or deleting files on GitHub.
 * Auto-generates a default message, supports branch/readonly mode awareness.
 */

import React, { useState, useEffect, useRef } from 'react';
import { GitCommit, X, FileUp, Trash2, AlertTriangle, GitBranch } from 'lucide-react';
import { useTranslation } from '../i18n';

interface GitHubCommitDialogProps {
  isOpen: boolean;
  files: { local: string; remote: string }[];
  operation: 'upload' | 'delete';
  branch: string;
  writeMode: 'direct' | 'branch' | 'readonly';
  workingBranch?: string;
  onCommit: (message: string) => void;
  onCancel: () => void;
}

export const GitHubCommitDialog: React.FC<GitHubCommitDialogProps> = ({
  isOpen,
  files,
  operation,
  branch,
  writeMode,
  workingBranch,
  onCommit,
  onCancel,
}) => {
  const t = useTranslation();
  const [message, setMessage] = useState('');
  const inputRef = useRef<HTMLInputElement>(null);

  // Auto-generate default commit message and focus input on open
  useEffect(() => {
    if (isOpen) {
      const fileName = files.length === 1
        ? files[0].remote.split('/').pop() || files[0].remote
        : `${files.length} items`;
      const verb = operation === 'delete' ? 'Delete' : 'Update';
      setMessage(`${verb} ${fileName} via AeroFTP`);
      setTimeout(() => inputRef.current?.select(), 100);
    }
  }, [isOpen, files, operation]);

  // Hide scrollbars when dialog is open (WebKitGTK fix)
  useEffect(() => {
    if (isOpen) {
      document.documentElement.classList.add('modal-open');
      return () => {
        document.documentElement.classList.remove('modal-open');
      };
    }
  }, [isOpen]);

  // Keyboard handler
  useEffect(() => {
    if (!isOpen) return;
    const handleKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.preventDefault();
        onCancel();
      }
    };
    window.addEventListener('keydown', handleKey);
    return () => window.removeEventListener('keydown', handleKey);
  }, [isOpen, onCancel]);

  if (!isOpen) return null;

  const isReadOnly = writeMode === 'readonly';
  const targetBranch = writeMode === 'branch' && workingBranch ? workingBranch : branch;

  const handleSubmit = () => {
    if (isReadOnly || !message.trim()) return;
    onCommit(message.trim());
  };

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      handleSubmit();
    }
  };

  const OperationIcon = operation === 'delete' ? Trash2 : FileUp;

  return (
    <div
      className="fixed inset-0 z-50 flex items-start justify-center pt-[5vh]"
      role="dialog"
      aria-modal="true"
      aria-label={t('github.commitDialogTitle') || 'Commit Message'}
    >
      {/* Backdrop */}
      <div className="absolute inset-0 bg-black/50 backdrop-blur-sm" onClick={onCancel} />

      {/* Dialog */}
      <div
        className="relative w-full max-w-md overflow-hidden rounded-xl border shadow-2xl animate-scale-in"
        style={{
          backgroundColor: 'var(--color-bg-secondary)',
          borderColor: 'var(--color-border)',
        }}
        onClick={e => e.stopPropagation()}
      >
        {/* Header */}
        <div
          className="flex items-center justify-between px-5 py-3 border-b"
          style={{ borderColor: 'var(--color-border)' }}
        >
          <div className="flex items-center gap-2">
            <GitCommit size={16} style={{ color: 'var(--color-accent)' }} />
            <h2
              className="text-sm font-semibold"
              style={{ color: 'var(--color-text-primary)' }}
            >
              {t('github.commitDialogTitle') || 'Commit Message'}
            </h2>
          </div>
          <button
            onClick={onCancel}
            className="p-1 rounded transition-colors hover:opacity-80"
            style={{ color: 'var(--color-text-secondary)' }}
          >
            <X size={16} />
          </button>
        </div>

        {/* Branch indicator */}
        <div
          className="flex items-center gap-2 px-5 py-2 text-xs border-b"
          style={{ borderColor: 'var(--color-border)' }}
        >
          <GitBranch size={12} style={{ color: 'var(--color-text-secondary)' }} />
          <span style={{ color: 'var(--color-text-secondary)' }}>
            {writeMode === 'branch'
              ? (t('github.commitToBranch', { branch: targetBranch }) || `Commit to ${targetBranch}`)
              : (t('github.commitToBranch', { branch }) || `Commit to ${branch}`)
            }
          </span>
          <GitHubWriteModeBadge writeMode={writeMode} />
        </div>

        {/* Readonly warning */}
        {isReadOnly && (
          <div
            className="flex items-center gap-2 px-5 py-2 text-xs border-b"
            style={{
              borderColor: 'var(--color-border)',
              backgroundColor: 'rgba(239, 68, 68, 0.1)',
            }}
          >
            <AlertTriangle size={12} className="text-red-400 flex-shrink-0" />
            <span className="text-red-400">
              {t('github.readonlyWarning') || 'This branch is read-only. You cannot commit changes.'}
            </span>
          </div>
        )}

        {/* File list */}
        <div className="px-5 py-3">
          <div className="text-xs font-medium mb-2" style={{ color: 'var(--color-text-secondary)' }}>
            {operation === 'delete'
              ? (t('github.filesToDelete', { count: files.length }) || `${files.length} file(s) to delete`)
              : (t('github.filesToUpload', { count: files.length }) || `${files.length} file(s) to upload`)
            }
          </div>
          <div
            className="max-h-[120px] overflow-y-auto rounded-lg p-2 space-y-1"
            style={{ backgroundColor: 'var(--color-bg-primary)' }}
          >
            {files.slice(0, 50).map((file, i) => (
              <div
                key={i}
                className="flex items-center gap-2 text-xs py-0.5"
                style={{ color: 'var(--color-text-primary)' }}
              >
                <OperationIcon
                  size={12}
                  className={operation === 'delete' ? 'text-red-400 flex-shrink-0' : 'text-green-400 flex-shrink-0'}
                />
                <span className="truncate" title={file.remote}>
                  {file.remote.split('/').pop() || file.remote}
                </span>
              </div>
            ))}
            {files.length > 50 && (
              <div className="text-xs py-0.5" style={{ color: 'var(--color-text-secondary)' }}>
                ...{t('github.andMore', { count: files.length - 50 }) || `and ${files.length - 50} more`}
              </div>
            )}
          </div>
        </div>

        {/* Commit message input */}
        <div className="px-5 pb-3">
          <label
            className="block text-xs font-medium mb-1.5"
            style={{ color: 'var(--color-text-secondary)' }}
          >
            {t('github.commitMessage') || 'Commit message'}
          </label>
          <input
            ref={inputRef}
            type="text"
            value={message}
            onChange={e => setMessage(e.target.value)}
            onKeyDown={handleKeyDown}
            disabled={isReadOnly}
            placeholder={t('github.commitMessagePlaceholder') || 'Update via AeroFTP'}
            className="w-full px-3 py-2 text-sm rounded-lg border focus:outline-none focus:ring-2 disabled:opacity-50 disabled:cursor-not-allowed"
            style={{
              backgroundColor: 'var(--color-bg-primary)',
              borderColor: 'var(--color-border)',
              color: 'var(--color-text-primary)',
            }}
          />
        </div>

        {/* Footer */}
        <div
          className="flex justify-end gap-2 px-5 py-3 border-t"
          style={{ borderColor: 'var(--color-border)' }}
        >
          <button
            onClick={onCancel}
            className="px-3 py-1.5 text-xs rounded-lg border transition-colors hover:opacity-80"
            style={{
              borderColor: 'var(--color-border)',
              color: 'var(--color-text-primary)',
            }}
          >
            {t('common.cancel') || 'Cancel'}
          </button>
          <button
            onClick={handleSubmit}
            disabled={isReadOnly || !message.trim()}
            className="flex items-center gap-1.5 px-3 py-1.5 text-xs rounded-lg text-white transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
            style={{
              backgroundColor: isReadOnly ? undefined : 'var(--color-accent)',
            }}
          >
            <GitCommit size={12} />
            {t('github.commit') || 'Commit'}
          </button>
        </div>
      </div>
    </div>
  );
};

/**
 * Inline write mode badge used inside the commit dialog header.
 */
const GitHubWriteModeBadge: React.FC<{ writeMode: 'direct' | 'branch' | 'readonly' }> = ({ writeMode }) => {
  const t = useTranslation();

  const config = {
    direct: { label: t('github.writeModeDirect') || 'Direct', color: 'text-green-500', bg: 'bg-green-500/10' },
    branch: { label: t('github.writeModeBranch') || 'Branch', color: 'text-yellow-500', bg: 'bg-yellow-500/10' },
    readonly: { label: t('github.writeModeReadOnly') || 'Read-only', color: 'text-red-400', bg: 'bg-red-400/10' },
  }[writeMode];

  return (
    <span className={`ml-auto px-1.5 py-0.5 rounded text-[10px] font-medium ${config.color} ${config.bg}`}>
      {config.label}
    </span>
  );
};
