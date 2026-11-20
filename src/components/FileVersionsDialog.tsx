// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { pickSave } from '../utils/pickPath';
import { X, Download, RotateCcw, History, RefreshCw, Trash2, AlertTriangle } from 'lucide-react';
import { useTranslation } from '../i18n';
import { formatBytes } from '../utils/formatters';
import { useDraggableModal } from '../hooks/useDraggableModal';
import { useGuardedClose } from '../hooks/useGuardedClose';
import { useInFlight } from '../hooks/useInFlight';
import { GuardedCloseConfirm } from './GuardedCloseConfirm';
import { versionDownloadName } from '../utils/versionDownloadName';

interface FileVersion {
  id: string;
  modified: string | null;
  size: number;
  modified_by: string | null;
  /** A delete or hide marker (S3, B2): no contents to download or restore. */
  deleted?: boolean;
}

interface Props {
  /** The backend connection generation the dialog was opened on: every
   * command carries it, so nothing runs on a connection switched to since. */
  generation: number;
  /** The remote target the dialog was opened on: an overlay applied,
   * cleared or locked since moves it, and every command is then refused. */
  guiTarget: number;
  /** The provider deletes a single version (`supports_version_delete`):
   * without it no Purge is offered, as the backend would refuse it. */
  canPurge?: boolean;
  /** The provider restores a version (`supports_version_restore`): MEGA
   * lists and downloads versions but refuses their restore. */
  canRestore?: boolean;
  filePath: string;
  fileName: string;
  onClose: () => void;
  onRestore?: () => void;
}

/** The prefix the backend puts on an operation it sent and could not
 * confirm (`APPLIED_UNCONFIRMED` in providers/mod.rs). */
const APPLIED_UNCONFIRMED = 'Applied, not confirmed:';

/** The row of the file as it is now, which a provider lists under its own
 * id (`current`, bound as `<file>:current`): it has no version to purge. */
const isCurrentFileRow = (id: string) => id === 'current' || id.endsWith(':current');

export function FileVersionsDialog({ generation, guiTarget, canPurge = false, canRestore = false, filePath, fileName, onClose, onRestore }: Props) {
  const t = useTranslation();
  const modalDrag = useDraggableModal();
  const [versions, setVersions] = useState<FileVersion[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  // The refusal of a Download, Restore or Purge (a file changed since the
  // listing, a connection switched): shown above the list, which stays.
  const [actionError, setActionError] = useState<string | null>(null);
  const ops = useInFlight<string>();
  const [pendingPurge, setPendingPurge] = useState<FileVersion | null>(null);
  // Operations on different versions can overlap: the guard counts every
  // pending request, so the first one to finish cannot drop it (review).
  // A download, restore or purge already sent goes on after a close: the backdrop is inert while it runs (Discussion #1527).
  const guarded = useGuardedClose({ guard: ops.count > 0 ? 'busy' : null, onClose });

  useEffect(() => {
    loadVersions();
  }, [filePath, generation, guiTarget]);

  const loadVersions = async () => {
    setLoading(true);
    setError(null);
    try {
      const result = await invoke<FileVersion[]>('provider_list_versions', { path: filePath, generation, expectedGuiTarget: guiTarget });
      setVersions(result);
    } catch (err) {
      setError(String(err));
    } finally {
      setLoading(false);
    }
  };

  const handleDownload = async (version: FileVersion) => {
    const savePath = await pickSave({ defaultPath: versionDownloadName(fileName, version.id) });
    if (!savePath) return;

    setActionError(null);
    ops.begin(version.id);
    try {
      await invoke('provider_download_version', {
        path: filePath,
        versionId: version.id,
        localPath: savePath,
        generation,
        expectedGuiTarget: guiTarget,
      });
    } catch (err) {
      setActionError(String(err));
    } finally {
      ops.end(version.id);
    }
  };

  const handleRestore = async (version: FileVersion) => {
    setActionError(null);
    ops.begin(version.id);
    try {
      await invoke('provider_restore_version', {
        path: filePath,
        versionId: version.id,
        generation,
        expectedGuiTarget: guiTarget,
      });
      onRestore?.();
    } catch (err) {
      // A restore sent that the server cannot confirm is not a failure to retry: say so in the user's language.
      const message = String(err);
      setActionError(message.includes(APPLIED_UNCONFIRMED)
        ? (t('versions.restoreUnconfirmed') || 'The restore was sent, but it could not be confirmed. Check the versions before restoring again.')
        : message);
      // A refused or unconfirmed restore may still have changed the file: list again before another one.
      await loadVersions();
    } finally {
      ops.end(version.id);
    }
  };

  const handlePurge = async (version: FileVersion) => {
    setPendingPurge(null);
    setActionError(null);
    ops.begin(version.id);
    try {
      await invoke('provider_delete_version', {
        path: filePath,
        versionId: version.id,
        generation,
        expectedGuiTarget: guiTarget,
      });
      await loadVersions();
    } catch (err) {
      setActionError(String(err));
      await loadVersions();
    } finally {
      ops.end(version.id);
    }
  };

  return (
    <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50" onClick={guarded.onBackdropClick}>
      <div
        {...modalDrag.panelProps}
        className="bg-white dark:bg-gray-800 rounded-lg shadow-2xl w-[520px] max-h-[70vh] flex flex-col animate-scale-in"
        onClick={e => e.stopPropagation()}
      >
        {/* Header */}
        <div {...modalDrag.dragHandleProps} className="flex items-center justify-between px-5 py-3 border-b border-gray-200 dark:border-gray-700 cursor-grab active:cursor-grabbing">
          <div className="flex items-center gap-2">
            <History size={18} className="text-blue-500" />
            <h3 className="font-semibold text-sm">{t('versions.title') || 'File Versions'}</h3>
          </div>
          <button onClick={guarded.requestClose} className="text-gray-400 hover:text-gray-600 dark:hover:text-gray-300">
            <X size={16} />
          </button>
        </div>

        {/* File name */}
        <div className="px-5 py-2 text-xs text-gray-500 dark:text-gray-400 truncate border-b border-gray-100 dark:border-gray-700">
          {fileName}
        </div>

        {actionError && (
          <div role="alert" className="px-5 py-2 text-xs text-red-500 border-b border-gray-100 dark:border-gray-700 break-words">
            {actionError}
          </div>
        )}

        {/* Content */}
        <div className="flex-1 overflow-auto">
          {loading ? (
            <div className="flex items-center justify-center py-12">
              <RefreshCw size={20} className="animate-spin text-blue-500" />
            </div>
          ) : error ? (
            <div className="p-5 text-sm text-red-500">{error}</div>
          ) : versions.length === 0 ? (
            <div className="p-5 text-sm text-gray-400 text-center">{t('versions.none') || 'No versions available'}</div>
          ) : (
            <table className="w-full text-sm">
              <thead className="bg-gray-50 dark:bg-gray-700 sticky top-0">
                <tr>
                  <th className="px-4 py-2 text-left text-xs font-medium text-gray-500 uppercase">{t('versions.date') || 'Date'}</th>
                  <th className="px-4 py-2 text-left text-xs font-medium text-gray-500 uppercase">{t('versions.size') || 'Size'}</th>
                  <th className="px-4 py-2 text-left text-xs font-medium text-gray-500 uppercase">{t('versions.author') || 'Author'}</th>
                  <th className="px-4 py-2 text-right text-xs font-medium text-gray-500 uppercase">{t('versions.actions') || 'Actions'}</th>
                </tr>
              </thead>
              <tbody className="divide-y divide-gray-100 dark:divide-gray-700">
                {versions.map((v, i) => (
                  <tr key={v.id} className="hover:bg-gray-50 dark:hover:bg-gray-700/50">
                    <td className="px-4 py-2 text-xs whitespace-nowrap">
                      {v.modified || '-'}
                      {v.deleted
                        ? <span className="ml-1.5 text-[10px] bg-red-100 dark:bg-red-900/40 text-red-600 dark:text-red-400 px-1.5 py-0.5 rounded">{t('contextMenu.s3TrashDeleteMarker') || 'Deleted'}</span>
                        : i === 0 && <span className="ml-1.5 text-[10px] bg-blue-100 dark:bg-blue-900/40 text-blue-600 dark:text-blue-400 px-1.5 py-0.5 rounded">{t('versions.current') || 'Current'}</span>}
                    </td>
                    <td className="px-4 py-2 text-xs text-gray-500">{formatBytes(v.size)}</td>
                    <td className="px-4 py-2 text-xs text-gray-500 truncate max-w-[120px]">{v.modified_by || '-'}</td>
                    <td className="px-4 py-2 text-right">
                      <div className="flex items-center justify-end gap-1">
                        {!v.deleted && (
                        <button
                          onClick={() => handleDownload(v)}
                          disabled={ops.has(v.id)}
                          className="p-1 text-gray-400 hover:text-blue-500 disabled:opacity-50"
                          title={t('versions.download') || 'Download this version'}
                        >
                          {ops.has(v.id) ? <RefreshCw size={13} className="animate-spin" /> : <Download size={13} />}
                        </button>
                        )}
                        {canRestore && i > 0 && !v.deleted && (
                          <button
                            onClick={() => handleRestore(v)}
                            disabled={ops.has(v.id)}
                            className="p-1 text-gray-400 hover:text-orange-500 disabled:opacity-50"
                            title={t('versions.restore') || 'Restore this version'}
                          >
                            <RotateCcw size={13} />
                          </button>
                        )}
                        {canPurge && !isCurrentFileRow(v.id) && (
                        <button
                          onClick={() => setPendingPurge(v)}
                          disabled={ops.has(v.id)}
                          className="p-1 text-gray-400 hover:text-red-500 disabled:opacity-50"
                          title={t('versions.purge') || 'Permanently delete this version'}
                        >
                          <Trash2 size={13} />
                        </button>
                        )}
                      </div>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </div>
      </div>

      {/* Per-version purge confirmation (irreversible) */}
      {pendingPurge && (
        <div className="fixed inset-0 z-[60] bg-black/50 flex items-center justify-center" role="dialog" aria-modal="true" onClick={(e) => { e.stopPropagation(); setPendingPurge(null); }}>
          <div className="bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg p-6 shadow-2xl max-w-sm animate-scale-in" onClick={e => e.stopPropagation()}>
            <div className="flex items-start gap-2 mb-4">
              <AlertTriangle size={18} className="text-red-500 shrink-0 mt-0.5" />
              <p className="text-gray-900 dark:text-gray-100 text-sm">
                {t('versions.purgeConfirm') || 'Permanently delete this version? This cannot be undone.'}
              </p>
            </div>
            <div className="flex justify-end gap-2">
              <button
                onClick={() => setPendingPurge(null)}
                className="px-3 py-1.5 text-sm rounded bg-gray-200 hover:bg-gray-300 dark:bg-gray-700 dark:hover:bg-gray-600 text-gray-900 dark:text-gray-100"
              >
                {t('common.cancel')}
              </button>
              <button
                onClick={() => handlePurge(pendingPurge)}
                className="px-3 py-1.5 text-sm rounded bg-red-600 hover:bg-red-700 text-white"
              >
                {t('versions.purge') || 'Purge'}
              </button>
            </div>
          </div>
        </div>
      )}
      {guarded.confirmOpen && guarded.confirmKind && (
        <GuardedCloseConfirm kind={guarded.confirmKind} canAbort={guarded.canAbort} onKeep={guarded.cancelConfirm} onConfirm={guarded.confirmAndClose} />
      )}
    </div>
  );
}
