// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { useState, useEffect, useRef } from 'react';
import { X, Layers, Loader2, Snowflake } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';
import { useTranslation } from '../i18n';
import { useDraggableModal } from '../hooks/useDraggableModal';
import { useModalFocusTrap } from '../hooks/useModalFocusTrap';
import {
  AZURE_ACCESS_TIERS,
  S3_RESTORE_TIERS,
  S3_STORAGE_CLASSES,
  changeS3StorageClass,
  restoreFromGlacier,
  setAzureTier,
  type AzureAccessTier,
  type S3RestoreTier,
  type S3StorageClass,
} from '../utils/cloudTiers';

export type ObjectTierMode = 's3-class' | 's3-restore' | 'azure-tier';

interface ObjectTierDialogProps {
  mode: ObjectTierMode;
  path: string;
  name: string;
  /** The current S3 storage class, from the listing. */
  current?: string;
  onClose: () => void;
  onDone: (message: string) => void;
}

/**
 * Move an S3 object to another storage class, start a Glacier / Deep Archive
 * restore, or set the access tier of an Azure blob. The choices are the ones
 * the backend accepts.
 */
export function ObjectTierDialog({ mode, path, name, current, onClose, onDone }: ObjectTierDialogProps) {
  const t = useTranslation();
  const modalDrag = useDraggableModal();
  const panelRef = useRef<HTMLDivElement>(null);
  useModalFocusTrap(panelRef);
  const [choice, setChoice] = useState<string>(
    mode === 's3-class' ? (current && (S3_STORAGE_CLASSES as readonly string[]).includes(current) ? current : 'STANDARD')
      : mode === 's3-restore' ? 'Standard' : 'Cool',
  );
  const [days, setDays] = useState(7);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const handleKey = (e: KeyboardEvent) => { if (e.key === 'Escape') onClose(); };
    window.addEventListener('keydown', handleKey);
    return () => window.removeEventListener('keydown', handleKey);
  }, [onClose]);

  const title = mode === 's3-class' ? t('s3.storageClass') : mode === 's3-restore' ? t('s3.restoreFromGlacier') : t('azure.accessTier');
  const options: readonly string[] = mode === 's3-class' ? S3_STORAGE_CLASSES : mode === 's3-restore' ? S3_RESTORE_TIERS : AZURE_ACCESS_TIERS;
  const unchanged = mode === 's3-class' && choice === current;

  const apply = async () => {
    setBusy(true);
    setError(null);
    try {
      if (mode === 's3-class') {
        await changeS3StorageClass(invoke, path, choice as S3StorageClass);
        onDone(t('s3.storageClassChanged', { storageClass: choice }));
      } else if (mode === 's3-restore') {
        await restoreFromGlacier(invoke, path, days, choice as S3RestoreTier);
        onDone(t('s3.restoreStarted', { days: String(days) }));
      } else {
        await setAzureTier(invoke, path, choice as AzureAccessTier);
        onDone(t('azure.accessTierChanged', { tier: choice }));
      }
      onClose();
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="fixed inset-0 z-50 flex items-start justify-center pt-[5vh]">
      <div className="absolute inset-0 bg-black/50" onClick={onClose} />
      <div
        {...modalDrag.panelProps}
        ref={panelRef}
        className="relative bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg shadow-2xl w-full max-w-sm overflow-hidden animate-scale-in"
        role="dialog"
        aria-modal="true"
        aria-label={title}
      >
        <div {...modalDrag.dragHandleProps} className="flex items-center justify-between px-5 py-3 border-b border-gray-200 dark:border-gray-700 cursor-grab active:cursor-grabbing">
          <div className="flex items-center gap-2">
            {mode === 's3-restore' ? <Snowflake size={16} className="text-cyan-500" /> : <Layers size={16} className="text-cyan-500" />}
            <h2 className="text-sm font-semibold text-gray-900 dark:text-gray-100">{title}</h2>
          </div>
          <button onClick={onClose} className="p-1 rounded hover:bg-gray-200 dark:hover:bg-gray-700" aria-label={t('common.close')}>
            <X size={16} className="text-gray-500" />
          </button>
        </div>
        <div className="px-5 py-2 border-b border-gray-200 dark:border-gray-700/50">
          <p className="text-xs text-gray-500 dark:text-gray-400 truncate" title={path}>{name}</p>
        </div>
        <div className="px-5 py-4 space-y-3">
          <label className="block text-xs text-gray-600 dark:text-gray-300">
            {mode === 's3-restore' ? t('s3.restoreTier') : title}
            <select
              value={choice}
              onChange={(e) => setChoice(e.target.value)}
              className="mt-1 w-full text-sm bg-transparent border border-gray-300 dark:border-gray-600 rounded px-2 py-1.5 dark:bg-gray-800"
            >
              {options.map((o) => <option key={o} value={o}>{o.replace(/_/g, ' ')}</option>)}
            </select>
          </label>
          {mode === 's3-restore' && (
            <label className="block text-xs text-gray-600 dark:text-gray-300">
              {t('s3.restoreDays')}
              <input
                type="number"
                min={1}
                max={365}
                value={days}
                onChange={(e) => setDays(Math.max(1, Math.min(365, Math.trunc(Number(e.target.value)) || 1)))}
                className="mt-1 w-full text-sm bg-transparent border border-gray-300 dark:border-gray-600 rounded px-2 py-1.5 dark:bg-gray-800"
              />
            </label>
          )}
          {error && <p className="text-xs text-red-500">{error}</p>}
        </div>
        <div className="flex justify-end gap-2 px-5 py-3 border-t border-gray-200 dark:border-gray-700">
          <button onClick={onClose} className="px-3 py-1.5 text-xs rounded-lg border border-gray-300 dark:border-gray-600 text-gray-700 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-800">
            {t('common.cancel')}
          </button>
          <button
            onClick={() => void apply()}
            disabled={busy || unchanged}
            className="flex items-center gap-1.5 px-3 py-1.5 text-xs rounded-lg bg-blue-500 text-white hover:bg-blue-600 disabled:opacity-50 disabled:cursor-not-allowed"
          >
            {busy && <Loader2 size={12} className="animate-spin" />}
            {t('common.apply')}
          </button>
        </div>
      </div>
    </div>
  );
}
