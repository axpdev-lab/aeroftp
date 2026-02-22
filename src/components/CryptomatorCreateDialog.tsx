import * as React from 'react';
import { useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useTranslation } from '../i18n';
import { X, Lock, Loader2, Shield, Eye, EyeOff } from 'lucide-react';

interface CryptomatorCreateDialogProps {
  outputDir: string;
  onClose: () => void;
  onCreated?: () => void;
}

export default function CryptomatorCreateDialog({ outputDir, onClose, onCreated }: CryptomatorCreateDialogProps) {
  const t = useTranslation();
  const [vaultName, setVaultName] = useState('NewVault');
  const [password, setPassword] = useState('');
  const [confirmPassword, setConfirmPassword] = useState('');
  const [showPassword, setShowPassword] = useState(false);
  const [creating, setCreating] = useState(false);
  const [error, setError] = useState<string | null>(null);

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
    try {
      const vaultPath = `${outputDir}/${vaultName.trim()}`;
      await invoke('cryptomator_create', { vaultPath, password });
      onCreated?.();
      onClose();
    } catch (err) {
      setError(String(err));
    } finally {
      setCreating(false);
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
        className="w-full max-w-lg rounded-xl shadow-2xl bg-[var(--color-bg-primary)] border border-[var(--color-border)] p-6"
        onClick={e => e.stopPropagation()}
        role="dialog"
        aria-modal="true"
        aria-label={t('cryptomator.createVault') || 'Create Cryptomator Vault'}
      >
        {/* Header */}
        <div className="flex items-center justify-between mb-4">
          <div className="flex items-center gap-2">
            <Lock size={20} className="text-emerald-500" />
            <h2 className="text-lg font-semibold text-[var(--color-text-primary)]">
              {t('cryptomator.createVault') || 'Create Cryptomator Vault'}
            </h2>
          </div>
          <button onClick={onClose} className="p-1 rounded hover:bg-[var(--color-bg-tertiary)]">
            <X size={18} className="text-[var(--color-text-secondary)]" />
          </button>
        </div>

        {/* Vault Name */}
        <div className="mb-4">
          <label className="block text-sm font-medium text-[var(--color-text-secondary)] mb-1">
            {t('cryptomator.vaultName') || 'Vault Name'}
          </label>
          <input
            type="text"
            value={vaultName}
            onChange={e => setVaultName(e.target.value)}
            className="w-full px-3 py-2 rounded-lg border border-[var(--color-border)] bg-[var(--color-bg-secondary)] text-[var(--color-text-primary)] focus:ring-2 focus:ring-emerald-500 focus:border-transparent"
            placeholder="MyVault"
            autoFocus
          />
          <p className="mt-1 text-xs text-[var(--color-text-tertiary)]">
            {outputDir}/{vaultName || '...'}
          </p>
        </div>

        {/* Password */}
        <div className="mb-3">
          <label className="block text-sm font-medium text-[var(--color-text-secondary)] mb-1">
            {t('common.password') || 'Password'}
          </label>
          <div className="relative">
            <input
              type={showPassword ? 'text' : 'password'}
              value={password}
              onChange={e => setPassword(e.target.value)}
              className="w-full px-3 py-2 pr-10 rounded-lg border border-[var(--color-border)] bg-[var(--color-bg-secondary)] text-[var(--color-text-primary)] focus:ring-2 focus:ring-emerald-500 focus:border-transparent"
              placeholder={t('cryptomator.minPassword') || 'Min 8 characters'}
            />
            <button
              type="button"
              onClick={() => setShowPassword(!showPassword)}
              className="absolute right-2 top-1/2 -translate-y-1/2 p-1 rounded hover:bg-[var(--color-bg-tertiary)]"
            >
              {showPassword ? <EyeOff size={16} className="text-[var(--color-text-secondary)]" /> : <Eye size={16} className="text-[var(--color-text-secondary)]" />}
            </button>
          </div>
          {password.length > 0 && password.length < 8 && (
            <p className="mt-1 text-xs text-red-500">{t('cryptomator.minPassword') || 'Minimum 8 characters'}</p>
          )}
        </div>

        {/* Confirm Password */}
        <div className="mb-4">
          <label className="block text-sm font-medium text-[var(--color-text-secondary)] mb-1">
            {t('cryptomator.confirmPassword') || 'Confirm Password'}
          </label>
          <input
            type={showPassword ? 'text' : 'password'}
            value={confirmPassword}
            onChange={e => setConfirmPassword(e.target.value)}
            className="w-full px-3 py-2 rounded-lg border border-[var(--color-border)] bg-[var(--color-bg-secondary)] text-[var(--color-text-primary)] focus:ring-2 focus:ring-emerald-500 focus:border-transparent"
            placeholder={t('cryptomator.repeatPassword') || 'Repeat password'}
          />
          {confirmPassword.length > 0 && password !== confirmPassword && (
            <p className="mt-1 text-xs text-red-500">{t('cryptomator.passwordMismatch') || 'Passwords do not match'}</p>
          )}
        </div>

        {/* Error message */}
        {error && (
          <div className="mb-4 px-3 py-2 rounded-lg bg-red-500/10 border border-red-500/20">
            <p className="text-xs text-red-600 dark:text-red-400">{error}</p>
          </div>
        )}

        {/* Info badge */}
        <div className="mb-5 flex items-center gap-2 px-3 py-2 rounded-lg bg-emerald-500/10 border border-emerald-500/20">
          <Shield size={16} className="text-emerald-500 flex-shrink-0" />
          <span className="text-xs text-emerald-600 dark:text-emerald-400">
            Cryptomator Format 8 — AES-256-GCM + AES-SIV
          </span>
        </div>

        {/* Buttons */}
        <div className="flex justify-end gap-2">
          <button
            onClick={onClose}
            className="px-4 py-2 rounded-lg border border-[var(--color-border)] text-[var(--color-text-secondary)] hover:bg-[var(--color-bg-tertiary)]"
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
        </div>
      </div>
    </div>
  );
}
