// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Link2, Copy, Check, X, Loader2, AlertTriangle, Key, RefreshCw } from 'lucide-react';
import { useTranslation } from '../i18n';
import { useHumanizedLog } from '../hooks/useHumanizedLog';

interface ShareLinkModalProps {
  path: string;
  fileName: string;
  providerName: string;
  providerIcon?: React.ReactNode;
  onClose: () => void;
}

type ModalState = 'loading' | 'success' | 'error';

export function ShareLinkModal({ path, fileName, providerName, providerIcon, onClose }: ShareLinkModalProps) {
  const t = useTranslation();
  const humanLog = useHumanizedLog();
  const [state, setState] = useState<ModalState>('loading');
  const [shareUrl, setShareUrl] = useState('');
  const [sharePassword, setSharePassword] = useState<string | null>(null);
  const [error, setError] = useState('');
  const [linkCopied, setLinkCopied] = useState(false);
  const [passwordCopied, setPasswordCopied] = useState(false);
  const [allCopied, setAllCopied] = useState(false);

  const humanLogRef = React.useRef(humanLog);
  humanLogRef.current = humanLog;

  const generateLink = useCallback(async () => {
    setState('loading');
    setError('');
    setLinkCopied(false);
    setPasswordCopied(false);
    setAllCopied(false);

    const log = humanLogRef.current;
    const logId = log.logRaw('activity.share_link_creating', 'INFO', { provider: providerName, filename: fileName }, 'running');

    try {
      const result = await invoke<string>('provider_create_share_link', { path });
      // Backend may return "url\npassword" when server enforces passwords
      const parts = result.split('\n');
      const url = parts[0];
      const pwd = parts.length > 1 ? parts[1] : null;

      setShareUrl(url);
      setSharePassword(pwd);
      setState('success');

      // Auto-copy link to clipboard
      await invoke('copy_to_clipboard', { text: url }).catch(() => {});
      setLinkCopied(true);
      setTimeout(() => setLinkCopied(false), 2000);

      log.updateEntry(logId, { status: 'success', message: `[${providerName}] Share link created: ${url}` });
    } catch (err) {
      setError(String(err));
      setState('error');
      log.updateEntry(logId, { status: 'error', message: `[${providerName}] Share link failed` });
    }
  }, [path, providerName, fileName]);

  const didRun = React.useRef(false);
  useEffect(() => {
    if (didRun.current) return;
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
                  onClick={generateLink}
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

              {/* Password field (if server required it) */}
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
