import { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { save } from '@tauri-apps/plugin-dialog';
import { X, Download, RotateCcw, History, RefreshCw } from 'lucide-react';
import { useTranslation } from '../i18n';
import { formatBytes } from '../utils/formatters';

interface FileVersion {
  id: string;
  modified: string | null;
  size: number;
  modified_by: string | null;
}

interface Props {
  filePath: string;
  fileName: string;
  onClose: () => void;
  onRestore?: () => void;
}

export function FileVersionsDialog({ filePath, fileName, onClose, onRestore }: Props) {
  const t = useTranslation();
  const [versions, setVersions] = useState<FileVersion[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [actionInProgress, setActionInProgress] = useState<string | null>(null);

  useEffect(() => {
    loadVersions();
  }, [filePath]);

  const loadVersions = async () => {
    setLoading(true);
    setError(null);
    try {
      const result = await invoke<FileVersion[]>('provider_list_versions', { path: filePath });
      setVersions(result);
    } catch (err) {
      setError(String(err));
    } finally {
      setLoading(false);
    }
  };

  const handleDownload = async (version: FileVersion) => {
    const ext = fileName.includes('.') ? '.' + fileName.split('.').pop() : '';
    const defaultName = `${fileName.replace(ext, '')}_v${version.id.slice(0, 8)}${ext}`;
    const savePath = await save({ defaultPath: defaultName });
    if (!savePath) return;

    setActionInProgress(version.id);
    try {
      await invoke('provider_download_version', {
        path: filePath,
        versionId: version.id,
        localPath: savePath,
      });
    } catch (err) {
      console.error('Download version failed:', err);
    } finally {
      setActionInProgress(null);
    }
  };

  const handleRestore = async (version: FileVersion) => {
    setActionInProgress(version.id);
    try {
      await invoke('provider_restore_version', {
        path: filePath,
        versionId: version.id,
      });
      onRestore?.();
    } catch (err) {
      console.error('Restore version failed:', err);
    } finally {
      setActionInProgress(null);
    }
  };

  return (
    <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50" onClick={onClose}>
      <div
        className="bg-white dark:bg-gray-800 rounded-xl shadow-2xl w-[520px] max-h-[70vh] flex flex-col animate-scale-in"
        onClick={e => e.stopPropagation()}
      >
        {/* Header */}
        <div className="flex items-center justify-between px-5 py-3 border-b border-gray-200 dark:border-gray-700">
          <div className="flex items-center gap-2">
            <History size={18} className="text-blue-500" />
            <h3 className="font-semibold text-sm">{t('versions.title') || 'File Versions'}</h3>
          </div>
          <button onClick={onClose} className="text-gray-400 hover:text-gray-600 dark:hover:text-gray-300">
            <X size={16} />
          </button>
        </div>

        {/* File name */}
        <div className="px-5 py-2 text-xs text-gray-500 dark:text-gray-400 truncate border-b border-gray-100 dark:border-gray-700">
          {fileName}
        </div>

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
                      {i === 0 && <span className="ml-1.5 text-[10px] bg-blue-100 dark:bg-blue-900/40 text-blue-600 dark:text-blue-400 px-1.5 py-0.5 rounded">{t('versions.current') || 'Current'}</span>}
                    </td>
                    <td className="px-4 py-2 text-xs text-gray-500">{formatBytes(v.size)}</td>
                    <td className="px-4 py-2 text-xs text-gray-500 truncate max-w-[120px]">{v.modified_by || '-'}</td>
                    <td className="px-4 py-2 text-right">
                      <div className="flex items-center justify-end gap-1">
                        <button
                          onClick={() => handleDownload(v)}
                          disabled={actionInProgress === v.id}
                          className="p-1 text-gray-400 hover:text-blue-500 disabled:opacity-50"
                          title={t('versions.download') || 'Download this version'}
                        >
                          {actionInProgress === v.id ? <RefreshCw size={13} className="animate-spin" /> : <Download size={13} />}
                        </button>
                        {i > 0 && (
                          <button
                            onClick={() => handleRestore(v)}
                            disabled={actionInProgress === v.id}
                            className="p-1 text-gray-400 hover:text-orange-500 disabled:opacity-50"
                            title={t('versions.restore') || 'Restore this version'}
                          >
                            <RotateCcw size={13} />
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
    </div>
  );
}
