// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * FileAssociationsPanel
 *
 * Compact OS-backed File associations section for Settings > File Handling.
 * - Shows current default status for AeroFTP-owned formats and common archives.
 * - Linux: direct xdg-mime apply (immediate).
 * - Windows: opens Default apps; never writes protected UserChoice.
 * - macOS: best-effort (no live Mac validation on this host).
 *
 * Source of truth = OS. No persisted checkbox state.
 */

import React, { useCallback, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { RefreshCw, CheckCircle2, AlertCircle, ExternalLink, Archive, FileCheck } from 'lucide-react';
import { Checkbox } from '../ui/Checkbox';
import { useTranslation } from '../../i18n';

interface FileAssociationItem {
  key: string;
  label: string;
  extensions: string[];
  isDefault: boolean;
  isAvailable: boolean;
  currentHandler?: string | null;
  action: string; // 'direct' | 'os_confirmation' | 'best_effort' | 'unsupported'
}

interface FileAssociationStatus {
  platform: string;
  canApplyDirectly: boolean;
  requiresOsConfirmation: boolean;
  items: FileAssociationItem[];
}

interface ApplyResult {
  applied: string[];
  requiresConfirmation: string[];
  platformNote?: string | null;
  error?: string | null;
}

interface FileAssociationsPanelProps {
  compact?: boolean;
}

export const FileAssociationsPanel: React.FC<FileAssociationsPanelProps> = ({ compact = true }) => {
  const t = useTranslation();
  const [status, setStatus] = useState<FileAssociationStatus | null>(null);
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);
  const [applyingKey, setApplyingKey] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);

  const loadStatus = useCallback(async (isRefresh = false) => {
    if (isRefresh) setRefreshing(true);
    setError(null);
    try {
      const s = await invoke<FileAssociationStatus>('file_associations_status');
      setStatus(s);
    } catch (e: any) {
      setError(String(e?.message || e || 'Failed to load association status'));
    } finally {
      if (isRefresh) setRefreshing(false);
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    loadStatus(false);
  }, [loadStatus]);

  const refresh = useCallback(() => {
    void loadStatus(true);
  }, [loadStatus]);

  const makeDefault = useCallback(async (key: string) => {
    setApplyingKey(key);
    setError(null);
    setNote(null);
    try {
      const res = await invoke<ApplyResult>('file_associations_set_default', { keys: [key] });
      if (res.platformNote) setNote(res.platformNote);
      if (res.error) setError(res.error);
      // Always refresh to reflect real OS state (especially on Win after user returns)
      await loadStatus(true);
    } catch (e: any) {
      setError(String(e?.message || e || 'Failed to apply association'));
    } finally {
      setApplyingKey(null);
    }
  }, [loadStatus]);

  const openOsSettings = useCallback(async () => {
    setError(null);
    try {
      await invoke('file_associations_open_settings');
      setNote(t('settings.fileAssociationWindowsConfirm') || 'Windows Default apps opened. Choose AeroFTP for the desired extensions, then return and Refresh.');
    } catch (e: any) {
      setError(String(e?.message || e || 'Failed to open OS settings'));
    }
  }, [t]);

  if (loading && !status) {
    return (
      <div className="text-sm text-gray-500 py-2">{t('settings.loading') || 'Loading…'}</div>
    );
  }

  const platform = status?.platform || 'unknown';
  const isLinux = platform === 'linux';
  const isWindows = platform === 'windows';
  const isMac = platform === 'macos';

  const aeroKeys = ['aerovault', 'aerozip', 'profile', 'keystore', 'script'];
  const archiveKeys = ['zip', 'sevenz', 'rar', 'tar', 'single_stream'];

  const aeroItems = (status?.items || []).filter(i => aeroKeys.includes(i.key));
  const archiveItems = (status?.items || []).filter(i => archiveKeys.includes(i.key));

  const renderRow = (item: FileAssociationItem) => {
    const isDef = item.isDefault;
    const extChips = item.extensions.slice(0, 4).map((ext, idx) => (
      <span key={idx} className="inline-block px-1.5 py-0.5 text-[10px] rounded bg-gray-100 dark:bg-gray-700 text-gray-600 dark:text-gray-300 mr-1 font-mono">
        {ext}
      </span>
    ));
    const more = item.extensions.length > 4 ? <span className="text-[10px] text-gray-400">+{item.extensions.length - 4}</span> : null;

    let stateLabel: React.ReactNode;
    if (isDef) {
      stateLabel = (
        <span className="inline-flex items-center gap-1 text-emerald-600 dark:text-emerald-400 text-xs">
          <CheckCircle2 size={12} /> {t('settings.fileAssociationDefault') || 'Default'}
        </span>
      );
    } else if (item.isAvailable) {
      stateLabel = (
        <span className="text-xs text-gray-500 dark:text-gray-400">{t('settings.fileAssociationAvailable') || 'Available'}</span>
      );
    } else if (item.currentHandler) {
      stateLabel = (
        <span className="text-xs text-amber-600 dark:text-amber-400">{t('settings.fileAssociationOther') || 'Handled by another app'}</span>
      );
    } else {
      stateLabel = (
        <span className="text-xs text-gray-400">{t('settings.fileAssociationUnknown') || 'Unknown'}</span>
      );
    }

    const actionEl = (() => {
      if (isDef) {
        return (
          <span className="text-xs text-emerald-600 dark:text-emerald-400 flex items-center gap-1">
            <CheckCircle2 size={14} /> {t('settings.fileAssociationDefault') || 'Default'}
          </span>
        );
      }
      if (item.action === 'unsupported') {
        return <span className="text-xs text-gray-400">—</span>;
      }
      const busy = applyingKey === item.key;
      if (isLinux && item.action === 'direct') {
        return (
          <button
            onClick={() => makeDefault(item.key)}
            disabled={busy}
            className="text-xs px-2 py-1 rounded border border-gray-300 dark:border-gray-600 hover:bg-gray-50 dark:hover:bg-gray-800 disabled:opacity-60"
          >
            {busy ? '…' : (t('settings.fileAssociationMakeDefault') || 'Make default')}
          </button>
        );
      }
      // Windows + mac best-effort: clicking opens OS UI
      return (
        <button
          onClick={() => makeDefault(item.key)}
          disabled={busy}
          className="text-xs px-2 py-1 rounded border border-gray-300 dark:border-gray-600 hover:bg-gray-50 dark:hover:bg-gray-800 disabled:opacity-60 flex items-center gap-1"
          title={isWindows ? t('settings.fileAssociationWindowsConfirm') : undefined}
        >
          {busy ? '…' : (t('settings.fileAssociationOpenSettings') || 'Open settings')}
          {isWindows && <ExternalLink size={11} />}
        </button>
      );
    })();

    return (
      <div key={item.key} className="flex items-center justify-between py-1.5 border-b border-gray-100 dark:border-gray-800 last:border-b-0 text-sm">
        <div className="min-w-0 flex-1 pr-3">
          <div className="font-medium text-gray-800 dark:text-gray-100 flex items-center gap-2">
            {item.key.includes('zip') || item.key.includes('tar') || item.key === 'rar' || item.key === 'sevenz' || item.key === 'single_stream' ? (
              <Archive size={14} className="text-gray-400 shrink-0" />
            ) : (
              <FileCheck size={14} className="text-gray-400 shrink-0" />
            )}
            <span>{item.label}</span>
          </div>
          <div className="mt-0.5 flex items-center gap-1 flex-wrap">
            {extChips}{more}
          </div>
          {item.currentHandler && !isDef && (
            <div className="text-[10px] text-gray-400 mt-0.5 truncate" title={item.currentHandler}>
              {t('settings.fileAssociationCurrentHandler') || 'Current'}: {item.currentHandler}
            </div>
          )}
        </div>

        <div className="flex items-center gap-3 shrink-0">
          <div className="text-right min-w-[92px]">{stateLabel}</div>
          <div className="min-w-[92px] flex justify-end">{actionEl}</div>
        </div>
      </div>
    );
  };

  return (
    <div className={compact ? 'space-y-3' : 'space-y-4'}>
      <div className="flex items-center justify-between">
        <div>
          <div className="text-sm font-semibold text-gray-700 dark:text-gray-200">{t('settings.fileAssociations') || 'File associations'}</div>
          <p className="text-xs text-gray-500 dark:text-gray-400 mt-0.5">
            {t('settings.fileAssociationsDesc') || 'Control which file types open with AeroFTP by default.'}
          </p>
        </div>
        <div className="flex items-center gap-2">
          {isWindows && (
            <button
              onClick={openOsSettings}
              className="text-xs px-2 py-1 rounded border border-gray-300 dark:border-gray-600 hover:bg-gray-50 dark:hover:bg-gray-800 flex items-center gap-1"
            >
              {t('settings.fileAssociationOpenSettings') || 'Open Default apps'} <ExternalLink size={12} />
            </button>
          )}
          <button
            onClick={refresh}
            disabled={refreshing}
            className="p-1.5 rounded hover:bg-gray-100 dark:hover:bg-gray-800 text-gray-500"
            title={t('settings.fileAssociationRefresh') || 'Refresh'}
            aria-label="Refresh file association status"
          >
            <RefreshCw size={15} className={refreshing ? 'animate-spin' : ''} />
          </button>
        </div>
      </div>

      {error && (
        <div className="flex items-start gap-2 text-xs text-red-600 dark:text-red-400 bg-red-50 dark:bg-red-950/30 p-2 rounded">
          <AlertCircle size={14} className="mt-0.5" />
          <span>{error}</span>
        </div>
      )}
      {note && !error && (
        <div className="text-xs text-amber-600 dark:text-amber-400 bg-amber-50 dark:bg-amber-950/20 p-2 rounded">
          {note}
        </div>
      )}

      {/* AeroFTP formats */}
      <div>
        <div className="text-xs uppercase tracking-wide text-gray-500 dark:text-gray-400 mb-1">
          {t('settings.fileAssociationsAeroFormats') || 'AeroFTP formats'}
        </div>
        <div className="border border-gray-200 dark:border-gray-700 rounded-lg overflow-hidden bg-white dark:bg-gray-900">
          {aeroItems.length > 0 ? aeroItems.map(renderRow) : (
            <div className="p-2 text-xs text-gray-400">—</div>
          )}
        </div>
      </div>

      {/* Compressed archives */}
      <div>
        <div className="text-xs uppercase tracking-wide text-gray-500 dark:text-gray-400 mb-1">
          {t('settings.fileAssociationsArchives') || 'Compressed archives'}
        </div>
        <div className="border border-gray-200 dark:border-gray-700 rounded-lg overflow-hidden bg-white dark:bg-gray-900">
          {archiveItems.length > 0 ? archiveItems.map(renderRow) : (
            <div className="p-2 text-xs text-gray-400">—</div>
          )}
        </div>
      </div>

      <div className="text-[10px] text-gray-400 dark:text-gray-500 pt-1">
        {isLinux && 'Linux: changes apply immediately via xdg-mime.'}
        {isWindows && 'Windows: defaults are protected. AeroFTP opens Default apps — choose AeroFTP there, then Refresh.'}
        {isMac && 'macOS: best-effort only. Confirm in System Settings after attempting.'}
        {!isLinux && !isWindows && !isMac && 'Platform support is limited on this host.'}
      </div>
    </div>
  );
};
