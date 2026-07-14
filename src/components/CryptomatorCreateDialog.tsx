// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useTranslation } from '../i18n';
import { X, Lock, Loader2, Shield } from 'lucide-react';
import { useDraggableModal } from '../hooks/useDraggableModal';
import { PasswordInput } from './common/PasswordInput';
import { PasswordMatchHint } from './common/PasswordMatchHint';
import { InlinePasswordGenerator } from './common/InlinePasswordGenerator';
import { PasswordStrengthBar } from './vault/PasswordStrengthBar';

interface CryptomatorCreateDialogProps {
  outputDir: string;
  /** Host paths of the items the user selected to encrypt into the new vault.
   *  When present, they are recursively ingested after the skeleton is created
   *  (folders become real subdirectories). Empty/undefined => empty vault. */
  files?: string[];
  onClose: () => void;
  onCreated?: () => void;
}

interface IngestReport {
  files: number;
  dirs: number;
  skipped: string[];
}

export default function CryptomatorCreateDialog({ outputDir, files, onClose, onCreated }: CryptomatorCreateDialogProps) {
  const t = useTranslation();
  const modalDrag = useDraggableModal();
  const [vaultName, setVaultName] = useState('NewVault');
  const [password, setPassword] = useState('');
  const [confirmPassword, setConfirmPassword] = useState('');
  const [creating, setCreating] = useState(false);
  const [status, setStatus] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [skipped, setSkipped] = useState<string[] | null>(null);

  const selectionCount = files?.length ?? 0;
  const isValid = vaultName.trim().length > 0 && password.length >= 8 && password === confirmPassword;

  const handleCreate = async () => {
    if (!isValid) return;
    // Validate vault name: reject path separators, null bytes, and traversal
    if (/[\/\\:\0]|\.\./.test(vaultName.trim())) {
      setError('Invalid characters in vault name');
      return;
    }
    setCreating(true);
    setError(null);
    setStatus(null);
    try {
      const vaultPath = `${outputDir}/${vaultName.trim()}`;
      await invoke('cryptomator_create', { vaultPath, password });

      // Ingest the user's selection into the freshly created vault (#322).
      if (files && files.length > 0) {
        setStatus(t('cryptomator.encryptingSelection') || 'Encrypting selected items...');
        const info = await invoke<{ vaultId: string }>('cryptomator_unlock', { vaultPath, password });
        try {
          const report = await invoke<IngestReport>('cryptomator_encrypt_paths', {
            vaultId: info.vaultId,
            dirId: '',
            inputPaths: files,
          });
          if (report.skipped && report.skipped.length > 0) {
            // Keep the dialog open to surface a partial-failure summary instead
            // of silently dropping items; the vault itself was created.
            onCreated?.();
            setSkipped(report.skipped);
            return;
          }
        } finally {
          await invoke('cryptomator_lock', { vaultId: info.vaultId }).catch(() => { });
        }
      }

      onCreated?.();
      onClose();
    } catch (err) {
      setError(String(err));
    } finally {
      setCreating(false);
      setStatus(null);
    }
  };

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'Escape') onClose();
    if (e.key === 'Enter' && isValid && !creating) handleCreate();
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/50"
      onClick={onClose}
      onKeyDown={handleKeyDown}
    >
      <div
        {...modalDrag.panelProps}
        className="w-full max-w-lg rounded-lg shadow-2xl bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 p-6 animate-scale-in"
        onClick={e => e.stopPropagation()}
        role="dialog"
        aria-modal="true"
        aria-label={t('cryptomator.createVault') || 'Create Cryptomator Vault'}
      >
        {/* Header */}
        <div {...modalDrag.dragHandleProps} className="flex items-center justify-between mb-4 cursor-grab active:cursor-grabbing">
          <div className="flex items-center gap-2">
            <Lock size={20} className="text-emerald-500" />
            <h2 className="text-lg font-semibold text-gray-900 dark:text-gray-100">
              {t('cryptomator.createVault') || 'Create Cryptomator Vault'}
            </h2>
          </div>
          <button onClick={onClose} className="p-1 rounded hover:bg-gray-100 dark:hover:bg-gray-700">
            <X size={18} className="text-gray-600 dark:text-gray-400" />
          </button>
        </div>

        {/* Vault Name */}
        <div className="mb-4">
          <label className="block text-sm font-medium text-gray-600 dark:text-gray-400 mb-1">
            {t('cryptomator.vaultName') || 'Vault Name'}
          </label>
          <input
            type="text"
            value={vaultName}
            onChange={e => setVaultName(e.target.value)}
            className="w-full px-3 py-2 rounded-lg border border-gray-300 dark:border-gray-600 bg-gray-50 dark:bg-gray-700 text-gray-900 dark:text-gray-100 focus:ring-2 focus:ring-emerald-500 focus:border-transparent"
            placeholder="MyVault"
            autoFocus
          />
          <p className="mt-1 text-xs text-gray-500 dark:text-gray-500">
            {outputDir}/{vaultName || '...'}
          </p>
        </div>

        {/* Password */}
        <div className="mb-3">
          <label className="block text-sm font-medium text-gray-600 dark:text-gray-400 mb-1">
            {t('common.password') || 'Password'}
          </label>
          <div className="relative">
            <PasswordInput
              value={password}
              onChange={setPassword}
              className="w-full px-3 py-2 pr-20 rounded-lg border border-gray-300 dark:border-gray-600 bg-gray-50 dark:bg-gray-700 text-gray-900 dark:text-gray-100 focus:ring-2 focus:ring-emerald-500 focus:border-transparent"
              placeholder={t('cryptomator.minPassword') || 'Min 8 characters'}
              ariaLabel={t('common.password') || 'Password'}
            />
            <InlinePasswordGenerator onGenerated={value => { setPassword(value); setConfirmPassword(value); }} className="absolute right-9 top-1/2 -translate-y-1/2" />
          </div>
          {password.length > 0 && (
            <div className="mt-2">
              <PasswordStrengthBar password={password} />
            </div>
          )}
          {password.length > 0 && password.length < 8 && (
            <p className="mt-1 text-xs text-red-500">{t('cryptomator.minPassword') || 'Minimum 8 characters'}</p>
          )}
        </div>

        {/* Confirm Password */}
        <div className="mb-4">
          <label className="block text-sm font-medium text-gray-600 dark:text-gray-400 mb-1">
            {t('cryptomator.confirmPassword') || 'Confirm Password'}
          </label>
          <PasswordInput
            value={confirmPassword}
            onChange={setConfirmPassword}
            className="w-full px-3 py-2 pr-10 rounded-lg border border-gray-300 dark:border-gray-600 bg-gray-50 dark:bg-gray-700 text-gray-900 dark:text-gray-100 focus:ring-2 focus:ring-emerald-500 focus:border-transparent"
            placeholder={t('cryptomator.repeatPassword') || 'Repeat password'}
            ariaLabel={t('cryptomator.confirmPassword') || 'Confirm Password'}
          />
          <PasswordMatchHint password={password} confirm={confirmPassword} />
        </div>

        {/* Error message */}
        {error && (
          <div className="mb-4 px-3 py-2 rounded-lg bg-red-500/10 border border-red-500/20">
            <p className="text-xs text-red-600 dark:text-red-400">{error}</p>
          </div>
        )}

        {/* Ingest status while encrypting the selection */}
        {status && (
          <div className="mb-4 flex items-center gap-2 px-3 py-2 rounded-lg bg-emerald-500/10 border border-emerald-500/20">
            <Loader2 size={16} className="animate-spin text-emerald-500 flex-shrink-0" />
            <span className="text-xs text-emerald-600 dark:text-emerald-400">{status}</span>
          </div>
        )}

        {/* Partial-failure summary: the vault was created but some items were skipped */}
        {skipped && (
          <div className="mb-4 px-3 py-2 rounded-lg bg-amber-500/10 border border-amber-500/20">
            <p className="text-xs font-medium text-amber-700 dark:text-amber-400 mb-1">
              {t('cryptomator.ingestSkipped') || 'Vault created, but some items could not be added:'}
            </p>
            <ul className="max-h-32 overflow-y-auto text-xs text-amber-600 dark:text-amber-400 list-disc list-inside">
              {skipped.map((s, i) => <li key={i} className="break-all">{s}</li>)}
            </ul>
          </div>
        )}

        {/* Info badge: how many selected items will go in (when launched from a selection) */}
        {selectionCount > 0 && !skipped && (
          <div className="mb-3 flex items-center gap-2 px-3 py-2 rounded-lg bg-emerald-500/10 border border-emerald-500/20">
            <Lock size={16} className="text-emerald-500 flex-shrink-0" />
            <span className="text-xs text-emerald-600 dark:text-emerald-400">
              {(t('cryptomator.selectionInfo', { count: selectionCount }) || `${selectionCount} item(s) will be encrypted into the vault`)}
            </span>
          </div>
        )}

        {/* Info badge */}
        <div className="mb-5 flex items-center gap-2 px-3 py-2 rounded-lg bg-emerald-500/10 border border-emerald-500/20">
          <Shield size={16} className="text-emerald-500 flex-shrink-0" />
          <span className="text-xs text-emerald-600 dark:text-emerald-400">
            Cryptomator Format 8: AES-256-GCM + AES-SIV
          </span>
        </div>

        {/* Buttons */}
        <div className="flex justify-end gap-2">
          {skipped ? (
            <button
              onClick={onClose}
              className="px-4 py-2 rounded-lg bg-emerald-600 hover:bg-emerald-700 text-white flex items-center gap-2"
            >
              {t('cryptomator.done') || 'Done'}
            </button>
          ) : (
            <>
              <button
                onClick={onClose}
                className="px-4 py-2 rounded-lg border border-gray-200 dark:border-gray-700 text-gray-600 dark:text-gray-400 hover:bg-gray-100 dark:hover:bg-gray-700"
              >
                {t('common.cancel') || 'Cancel'}
              </button>
              <button
                onClick={handleCreate}
                disabled={!isValid || creating}
                className="px-4 py-2 rounded-lg bg-emerald-600 hover:bg-emerald-700 text-white disabled:opacity-50 disabled:cursor-not-allowed flex items-center gap-2"
              >
                {creating ? (
                  <>
                    <Loader2 size={16} className="animate-spin" />
                    {t('common.creating') || 'Creating...'}
                  </>
                ) : (
                  <>
                    <Lock size={16} />
                    {t('common.create') || 'Create'}
                  </>
                )}
              </button>
            </>
          )}
        </div>
      </div>
    </div>
  );
}
