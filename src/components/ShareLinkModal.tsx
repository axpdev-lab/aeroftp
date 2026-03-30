// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Link2, Copy, Check, X, Loader2, AlertTriangle, Key, RefreshCw, Clock, Shield, Eye } from 'lucide-react';
import { useTranslation } from '../i18n';
import { useHumanizedLog } from '../hooks/useHumanizedLog';
import type { ProviderType } from '../types';

/** Backend response from provider_create_share_link */
interface ShareLinkResult {
  url: string;
  password: string | null;
  expires_at: string | null;
}

/** Per-provider capability flags */
interface ShareLinkCapabilities {
  expiration: boolean;
  password: boolean;
  permissions: boolean;
  availablePermissions: string[];
  hasAdvancedOptions: boolean;
}

/** Map of provider capabilities for share link advanced options */
function getShareLinkCapabilities(provider: ProviderType | string): ShareLinkCapabilities {
  const caps: ShareLinkCapabilities = {
    expiration: false,
    password: false,
    permissions: false,
    availablePermissions: [],
    hasAdvancedOptions: false,
  };

  switch (provider) {
    case 'googledrive':
      caps.permissions = true;
      caps.availablePermissions = ['view', 'comment', 'edit'];
      break;
    case 'dropbox':
      caps.expiration = true;
      caps.password = true; // Pro+ only
      caps.permissions = true;
      caps.availablePermissions = ['view', 'edit'];
      break;
    case 'onedrive':
      caps.expiration = true;
      caps.password = true; // Personal only
      caps.permissions = true;
      caps.availablePermissions = ['view', 'edit'];
      break;
    case 'box':
      caps.expiration = true; // Paid only
      caps.password = true; // Paid only
      caps.permissions = true;
      caps.availablePermissions = ['view', 'edit'];
      break;
    case 'pcloud':
      caps.expiration = true; // Premium only
      caps.password = true; // Premium only
      break;
    case 'filen':
      caps.expiration = true;
      caps.password = true;
      break;
    case 'zohoworkdrive':
      caps.expiration = true;
      caps.password = true;
      caps.permissions = true;
      caps.availablePermissions = ['view', 'edit'];
      break;
    case 'kdrive':
      caps.expiration = true;
      caps.password = true;
      caps.permissions = true;
      caps.availablePermissions = ['view', 'edit'];
      break;
    case 'drime':
      caps.expiration = true;
      caps.password = true;
      caps.permissions = true;
      caps.availablePermissions = ['view', 'edit'];
      break;
    case 'webdav':
      caps.expiration = true;
      caps.password = true;
      break;
    case 's3':
      caps.expiration = true;
      break;
    case 'azure':
      caps.expiration = true;
      break;
    case 'opendrive':
      caps.expiration = true;
      break;
    // No advanced options: mega, jottacloud, koofr, yandexdisk, github, filelu
  }

  caps.hasAdvancedOptions = caps.expiration || caps.password || caps.permissions;
  return caps;
}

interface ShareLinkModalProps {
  path: string;
  fileName: string;
  providerName: string;
  providerType?: ProviderType | string;
  providerIcon?: React.ReactNode;
  onClose: () => void;
}

type ModalState = 'options' | 'loading' | 'success' | 'error';

const EXPIRATION_PRESETS = [
  { label: '1 hour', value: 3600 },
  { label: '24 hours', value: 86400 },
  { label: '7 days', value: 604800 },
  { label: '30 days', value: 2592000 },
] as const;

export function ShareLinkModal({ path, fileName, providerName, providerType, providerIcon, onClose }: ShareLinkModalProps) {
  const t = useTranslation();
  const humanLog = useHumanizedLog();
  const caps = React.useMemo(() => getShareLinkCapabilities(providerType || ''), [providerType]);

  const [state, setState] = useState<ModalState>(caps.hasAdvancedOptions ? 'options' : 'loading');
  const [shareUrl, setShareUrl] = useState('');
  const [sharePassword, setSharePassword] = useState<string | null>(null);
  const [error, setError] = useState('');
  const [linkCopied, setLinkCopied] = useState(false);
  const [passwordCopied, setPasswordCopied] = useState(false);
  const [allCopied, setAllCopied] = useState(false);

  // Options form state
  const [optPassword, setOptPassword] = useState('');
  const [optExpiration, setOptExpiration] = useState<number | null>(null);
  const [optPermissions, setOptPermissions] = useState<string>('view');
  const [showAdvanced, setShowAdvanced] = useState(false);

  const humanLogRef = React.useRef(humanLog);
  humanLogRef.current = humanLog;

  const generateLink = useCallback(async (opts?: { password?: string; expiresInSecs?: number | null; permissions?: string }) => {
    setState('loading');
    setError('');
    setLinkCopied(false);
    setPasswordCopied(false);
    setAllCopied(false);

    const log = humanLogRef.current;
    const logId = log.logRaw('activity.share_link_creating', 'INFO', { provider: providerName, filename: fileName }, 'running');

    try {
      const params: Record<string, unknown> = { path };
      if (opts?.expiresInSecs) params.expiresInSecs = opts.expiresInSecs;
      if (opts?.password) params.password = opts.password;
      if (opts?.permissions && opts.permissions !== 'view') params.permissions = opts.permissions;

      const result = await invoke<ShareLinkResult>('provider_create_share_link', params);
      setShareUrl(result.url);
      setSharePassword(result.password || null);
      setState('success');

      // Auto-copy link to clipboard
      await invoke('copy_to_clipboard', { text: result.url }).catch(() => {});
      setLinkCopied(true);
      setTimeout(() => setLinkCopied(false), 2000);

      log.updateEntry(logId, { status: 'success', message: `[${providerName}] Share link created: ${result.url}` });
    } catch (err) {
      setError(String(err));
      setState('error');
      log.updateEntry(logId, { status: 'error', message: `[${providerName}] Share link failed` });
    }
  }, [path, providerName, fileName]);

  // Auto-generate for providers without advanced options
  const didRun = React.useRef(false);
  useEffect(() => {
    if (didRun.current || caps.hasAdvancedOptions) return;
    didRun.current = true;
    generateLink();
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    const handleKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', handleKey);
    return () => window.removeEventListener('keydown', handleKey);
  }, [onClose]);

  const handleGenerate = () => {
    generateLink({
      password: optPassword || undefined,
      expiresInSecs: optExpiration,
      permissions: optPermissions,
    });
  };

  const copyToClipboard = async (text: string, type: 'link' | 'password' | 'all') => {
    try {
      await invoke('copy_to_clipboard', { text });
      if (type === 'link') {
        setLinkCopied(true);
        setTimeout(() => setLinkCopied(false), 2000);
      } else if (type === 'password') {
        setPasswordCopied(true);
        setTimeout(() => setPasswordCopied(false), 2000);
      } else {
        setAllCopied(true);
        setTimeout(() => setAllCopied(false), 2000);
      }
    } catch {
      // Fallback: try navigator.clipboard
      try { await navigator.clipboard.writeText(text); } catch { /* ignore */ }
    }
  };

  return (
    <div className="fixed inset-0 z-[9999] flex items-center justify-center bg-black/50" onClick={onClose}>
      <div
        className="bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg shadow-2xl w-[480px] flex flex-col animate-scale-in"
        onClick={e => e.stopPropagation()}
        role="dialog"
        aria-modal="true"
        aria-label={t('shareLinkModal.title')}
      >
        {/* Header */}
        <div className="flex items-center justify-between px-4 py-3 border-b border-gray-200 dark:border-gray-700">
          <div className="flex items-center gap-2">
            {providerIcon || <Link2 size={18} className="text-blue-500" />}
            <h2 className="text-sm font-semibold text-gray-900 dark:text-gray-100">
              {t('shareLinkModal.title')} - {providerName}
            </h2>
          </div>
          <button
            onClick={onClose}
            className="p-1.5 rounded hover:bg-gray-100 dark:hover:bg-gray-700 text-gray-600 dark:text-gray-400"
          >
            <X size={14} />
          </button>
        </div>

        {/* Content */}
        <div className="px-4 py-4">
          {/* File name */}
          <div className="text-xs text-gray-500 dark:text-gray-400 mb-3 truncate" title={path}>
            {fileName}
          </div>

          {/* OPTIONS phase */}
          {state === 'options' && (
            <div className="space-y-3">
              {/* Quick generate button */}
              <button
                onClick={() => generateLink()}
                className="w-full flex items-center justify-center gap-2 px-4 py-2.5 text-sm bg-blue-600 text-white rounded-lg hover:bg-blue-700 transition-colors"
              >
                <Link2 size={14} />
                {t('shareLinkModal.generateDefault')}
              </button>

              {/* Advanced toggle */}
              <button
                onClick={() => setShowAdvanced(!showAdvanced)}
                className="w-full text-xs text-gray-500 dark:text-gray-400 hover:text-gray-700 dark:hover:text-gray-300 flex items-center justify-center gap-1 py-1"
              >
                <Shield size={11} />
                {showAdvanced ? t('shareLinkModal.hideAdvanced') : t('shareLinkModal.showAdvanced')}
              </button>

              {showAdvanced && (
                <div className="space-y-3 pt-1 border-t border-gray-200 dark:border-gray-700">
                  {/* Expiration */}
                  {caps.expiration && (
                    <div>
                      <label className="flex items-center gap-1.5 text-xs font-medium text-gray-700 dark:text-gray-300 mb-1.5">
                        <Clock size={12} />
                        {t('shareLinkModal.expiration')}
                      </label>
                      <div className="flex flex-wrap gap-1.5">
                        <button
                          onClick={() => setOptExpiration(null)}
                          className={`px-2.5 py-1 text-xs rounded-md transition-colors ${
                            optExpiration === null
                              ? 'bg-blue-100 dark:bg-blue-900/30 text-blue-700 dark:text-blue-400 border border-blue-300 dark:border-blue-700'
                              : 'bg-gray-100 dark:bg-gray-700 text-gray-600 dark:text-gray-400 border border-gray-200 dark:border-gray-600 hover:bg-gray-200 dark:hover:bg-gray-600'
                          }`}
                        >
                          {t('shareLinkModal.expirationNever')}
                        </button>
                        {EXPIRATION_PRESETS.map(preset => (
                          <button
                            key={preset.value}
                            onClick={() => setOptExpiration(preset.value)}
                            className={`px-2.5 py-1 text-xs rounded-md transition-colors ${
                              optExpiration === preset.value
                                ? 'bg-blue-100 dark:bg-blue-900/30 text-blue-700 dark:text-blue-400 border border-blue-300 dark:border-blue-700'
                                : 'bg-gray-100 dark:bg-gray-700 text-gray-600 dark:text-gray-400 border border-gray-200 dark:border-gray-600 hover:bg-gray-200 dark:hover:bg-gray-600'
                            }`}
                          >
                            {preset.label}
                          </button>
                        ))}
                      </div>
                    </div>
                  )}

                  {/* Password */}
                  {caps.password && (
                    <div>
                      <label className="flex items-center gap-1.5 text-xs font-medium text-gray-700 dark:text-gray-300 mb-1.5">
                        <Key size={12} />
                        {t('shareLinkModal.setPassword')}
                      </label>
                      <input
                        type="text"
                        value={optPassword}
                        onChange={e => setOptPassword(e.target.value)}
                        placeholder={t('shareLinkModal.passwordPlaceholder')}
                        className="w-full text-xs px-3 py-2 bg-gray-50 dark:bg-gray-700 border border-gray-200 dark:border-gray-600 rounded-lg text-gray-900 dark:text-gray-100 placeholder-gray-400 dark:placeholder-gray-500"
                      />
                      <p className="text-[10px] text-gray-400 dark:text-gray-500 mt-1">
                        {t('shareLinkModal.passwordHint')}
                      </p>
                    </div>
                  )}

                  {/* Permissions */}
                  {caps.permissions && caps.availablePermissions.length > 1 && (
                    <div>
                      <label className="flex items-center gap-1.5 text-xs font-medium text-gray-700 dark:text-gray-300 mb-1.5">
                        <Eye size={12} />
                        {t('shareLinkModal.permissions')}
                      </label>
                      <div className="flex gap-1.5">
                        {caps.availablePermissions.map(perm => (
                          <button
                            key={perm}
                            onClick={() => setOptPermissions(perm)}
                            className={`px-2.5 py-1 text-xs rounded-md capitalize transition-colors ${
                              optPermissions === perm
                                ? 'bg-blue-100 dark:bg-blue-900/30 text-blue-700 dark:text-blue-400 border border-blue-300 dark:border-blue-700'
                                : 'bg-gray-100 dark:bg-gray-700 text-gray-600 dark:text-gray-400 border border-gray-200 dark:border-gray-600 hover:bg-gray-200 dark:hover:bg-gray-600'
                            }`}
                          >
                            {t(`shareLinkModal.perm_${perm}`)}
                          </button>
                        ))}
                      </div>
                    </div>
                  )}

                  {/* Generate with options */}
                  <button
                    onClick={handleGenerate}
                    className="w-full flex items-center justify-center gap-2 px-4 py-2 text-xs bg-blue-600 text-white rounded-lg hover:bg-blue-700 transition-colors"
                  >
                    <Link2 size={12} />
                    {t('shareLinkModal.generateWithOptions')}
                  </button>
                </div>
              )}
            </div>
          )}

          {state === 'loading' && (
            <div className="flex flex-col items-center justify-center py-8 text-gray-600 dark:text-gray-400">
              <Loader2 size={28} className="animate-spin mb-3 text-blue-500" />
              <p className="text-sm font-medium">{t('shareLinkModal.generating')}</p>
              <p className="text-xs text-gray-500 mt-1">{t('shareLinkModal.generatingDesc')}</p>
            </div>
          )}

          {state === 'error' && (
            <div className="py-6">
              <div className="flex items-start gap-3 p-3 bg-red-50 dark:bg-red-900/20 border border-red-200 dark:border-red-800 rounded-lg">
                <AlertTriangle size={18} className="text-red-500 shrink-0 mt-0.5" />
                <div className="flex-1 min-w-0">
                  <p className="text-sm font-medium text-red-700 dark:text-red-400">{t('shareLinkModal.errorTitle')}</p>
                  <p className="text-xs text-red-600 dark:text-red-500 mt-1 break-words">{error}</p>
                </div>
              </div>
              <div className="flex justify-end gap-2 mt-4">
                <button
                  onClick={onClose}
                  className="px-3 py-1.5 text-xs text-gray-600 dark:text-gray-400 hover:bg-gray-100 dark:hover:bg-gray-700 rounded-lg"
                >
                  {t('shareLinkModal.close')}
                </button>
                <button
                  onClick={() => caps.hasAdvancedOptions ? setState('options') : generateLink()}
                  className="flex items-center gap-1.5 px-3 py-1.5 text-xs bg-blue-600 text-white rounded-lg hover:bg-blue-700"
                >
                  <RefreshCw size={12} />
                  {t('shareLinkModal.retry')}
                </button>
              </div>
            </div>
          )}

          {state === 'success' && (
            <div className="space-y-3">
              {/* Link field */}
              <div>
                <label className="text-xs font-medium text-gray-700 dark:text-gray-300 mb-1 block">
                  {t('shareLinkModal.linkReady')}
                </label>
                <div className="flex items-center gap-2">
                  <input
                    type="text"
                    readOnly
                    value={shareUrl}
                    className="flex-1 text-xs px-3 py-2 bg-gray-50 dark:bg-gray-700 border border-gray-200 dark:border-gray-600 rounded-lg text-gray-900 dark:text-gray-100 select-all cursor-text"
                    onClick={e => (e.target as HTMLInputElement).select()}
                  />
                  <button
                    onClick={() => copyToClipboard(shareUrl, 'link')}
                    className={`flex items-center gap-1.5 px-3 py-2 text-xs rounded-lg transition-all ${
                      linkCopied
                        ? 'bg-green-100 dark:bg-green-900/30 text-green-700 dark:text-green-400'
                        : 'bg-blue-600 text-white hover:bg-blue-700'
                    }`}
                  >
                    {linkCopied ? <Check size={12} /> : <Copy size={12} />}
                    {linkCopied ? t('shareLinkModal.linkCopied') : t('shareLinkModal.copyLink')}
                  </button>
                </div>
              </div>

              {/* Password field (if server returned one) */}
              {sharePassword && (
                <div>
                  <div className="flex items-center gap-1.5 mb-1">
                    <Key size={12} className="text-amber-500" />
                    <label className="text-xs font-medium text-gray-700 dark:text-gray-300">
                      {t('shareLinkModal.password')}
                    </label>
                  </div>
                  <div className="flex items-center gap-2">
                    <input
                      type="text"
                      readOnly
                      value={sharePassword}
                      className="flex-1 text-xs px-3 py-2 bg-amber-50 dark:bg-amber-900/20 border border-amber-200 dark:border-amber-800 rounded-lg text-gray-900 dark:text-gray-100 font-mono select-all cursor-text"
                      onClick={e => (e.target as HTMLInputElement).select()}
                    />
                    <button
                      onClick={() => copyToClipboard(sharePassword, 'password')}
                      className={`flex items-center gap-1.5 px-3 py-2 text-xs rounded-lg transition-all ${
                        passwordCopied
                          ? 'bg-green-100 dark:bg-green-900/30 text-green-700 dark:text-green-400'
                          : 'bg-amber-600 text-white hover:bg-amber-700'
                      }`}
                    >
                      {passwordCopied ? <Check size={12} /> : <Copy size={12} />}
                      {passwordCopied ? t('shareLinkModal.passwordCopied') : t('shareLinkModal.copyPassword')}
                    </button>
                  </div>
                  <p className="text-[10px] text-amber-600 dark:text-amber-500 mt-1.5">
                    {t('shareLinkModal.passwordRequired')}
                  </p>

                  {/* Copy All button */}
                  <button
                    onClick={() => copyToClipboard(`${shareUrl}\nPassword: ${sharePassword}`, 'all')}
                    className={`mt-2 w-full flex items-center justify-center gap-1.5 px-3 py-2 text-xs rounded-lg transition-all ${
                      allCopied
                        ? 'bg-green-100 dark:bg-green-900/30 text-green-700 dark:text-green-400'
                        : 'bg-gray-100 dark:bg-gray-700 text-gray-700 dark:text-gray-300 hover:bg-gray-200 dark:hover:bg-gray-600'
                    }`}
                  >
                    {allCopied ? <Check size={12} /> : <Copy size={12} />}
                    {allCopied ? t('shareLinkModal.allCopied') : t('shareLinkModal.copyAll')}
                  </button>
                </div>
              )}
            </div>
          )}
        </div>

        {/* Footer (only on success) */}
        {state === 'success' && (
          <div className="px-4 py-3 border-t border-gray-200 dark:border-gray-700 flex justify-end">
            <button
              onClick={onClose}
              className="px-4 py-1.5 text-xs text-gray-600 dark:text-gray-400 hover:bg-gray-100 dark:hover:bg-gray-700 rounded-lg"
            >
              {t('shareLinkModal.close')}
            </button>
          </div>
        )}
      </div>
    </div>
  );
}
