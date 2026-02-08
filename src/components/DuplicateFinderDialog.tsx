/**
 * DuplicateFinderDialog Component
 * Modal dialog for finding and managing duplicate files within a directory.
 * Scans via Tauri command, displays grouped results with checkboxes,
 * and allows batch deletion of selected duplicates.
 *
 * @since v2.1.0
 */

import * as React from 'react';
import { useState, useEffect, useCallback, useMemo } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Search, X, Trash2, CheckCircle, AlertCircle, Loader2, Copy, FileX } from 'lucide-react';
import { useTranslation } from '../i18n';
import { formatBytes } from '../utils/formatters';
import { DuplicateGroup } from '../types/aerofile';

interface DuplicateFinderDialogProps {
  isOpen: boolean;
  scanPath: string;
  onClose: () => void;
  onDeleteFiles: (paths: string[]) => Promise<void>;
}

/** Extract the filename from a full path */
const getFileName = (path: string): string => {
  const sep = path.includes('\\') ? '\\' : '/';
  const parts = path.split(sep);
  return parts[parts.length - 1] || path;
};

/** Extract the directory portion from a full path */
const getDirectory = (path: string): string => {
  const sep = path.includes('\\') ? '\\' : '/';
  const lastIdx = path.lastIndexOf(sep);
  return lastIdx >= 0 ? path.substring(0, lastIdx) : '';
};

export const DuplicateFinderDialog: React.FC<DuplicateFinderDialogProps> = ({
  isOpen,
  scanPath,
  onClose,
  onDeleteFiles,
}) => {
  const t = useTranslation();

  const [groups, setGroups] = useState<DuplicateGroup[]>([]);
  const [isScanning, setIsScanning] = useState(false);
  const [isDeleting, setIsDeleting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // Set of file paths selected for deletion
  const [selectedPaths, setSelectedPaths] = useState<Set<string>>(new Set());

  // Scan for duplicates when the dialog opens
  const scan = useCallback(async () => {
    setIsScanning(true);
    setError(null);
    setGroups([]);
    setSelectedPaths(new Set());

    try {
      const result = await invoke<DuplicateGroup[]>('find_duplicate_files', { path: scanPath });
      setGroups(result);

      // Auto-select all non-first files (duplicates) by default
      const autoSelected = new Set<string>();
      for (const group of result) {
        for (let i = 1; i < group.files.length; i++) {
          autoSelected.add(group.files[i]);
        }
      }
      setSelectedPaths(autoSelected);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setIsScanning(false);
    }
  }, [scanPath]);

  useEffect(() => {
    if (isOpen) {
      scan();
    } else {
      // Reset state when dialog closes
      setGroups([]);
      setSelectedPaths(new Set());
      setError(null);
      setIsScanning(false);
      setIsDeleting(false);
    }
  }, [isOpen, scan]);

  // Keyboard handler: Escape to close
  useEffect(() => {
    if (!isOpen) return;

    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.preventDefault();
        e.stopPropagation();
        onClose();
      }
    };

    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, [isOpen, onClose]);

  // Toggle a single file's selection
  const toggleFile = useCallback((path: string) => {
    setSelectedPaths(prev => {
      const next = new Set(prev);
      if (next.has(path)) {
        next.delete(path);
      } else {
        next.add(path);
      }
      return next;
    });
  }, []);

  // Select all duplicates (all non-first files)
  const selectAllDuplicates = useCallback(() => {
    const all = new Set<string>();
    for (const group of groups) {
      for (let i = 1; i < group.files.length; i++) {
        all.add(group.files[i]);
      }
    }
    setSelectedPaths(all);
  }, [groups]);

  // Deselect everything
  const deselectAll = useCallback(() => {
    setSelectedPaths(new Set());
  }, []);

  // Delete selected files
  const handleDelete = useCallback(async () => {
    const paths = Array.from(selectedPaths);
    if (paths.length === 0) return;

    const confirmed = window.confirm(
      t('duplicates.confirmDelete', { count: paths.length }) ||
      `Are you sure you want to delete ${paths.length} files?`
    );
    if (!confirmed) return;

    setIsDeleting(true);
    try {
      await onDeleteFiles(paths);
      // Remove deleted files from groups and update state
      const updatedGroups: DuplicateGroup[] = [];
      for (const group of groups) {
        const remaining = group.files.filter(f => !selectedPaths.has(f));
        if (remaining.length > 1) {
          updatedGroups.push({ ...group, files: remaining });
        }
      }
      setGroups(updatedGroups);
      setSelectedPaths(new Set());
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setIsDeleting(false);
    }
  }, [selectedPaths, groups, onDeleteFiles]);

  // Summary calculations
  const summary = useMemo(() => {
    const totalGroups = groups.length;
    let totalDuplicates = 0;
    let wastedBytes = 0;

    for (const group of groups) {
      const dupeCount = group.files.length - 1;
      totalDuplicates += dupeCount;
      wastedBytes += group.size * dupeCount;
    }

    return { totalGroups, totalDuplicates, wastedBytes };
  }, [groups]);

  const selectedCount = selectedPaths.size;

  if (!isOpen) return null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
      <div
        className="bg-white dark:bg-gray-800 rounded-lg shadow-xl w-[700px] max-h-[80vh] flex flex-col"
        role="dialog"
        aria-label={t('duplicates.title')}
        aria-modal="true"
      >
        {/* Header */}
        <div className="flex items-center justify-between px-4 py-3 border-b border-gray-200 dark:border-gray-700">
          <div className="flex items-center gap-2 min-w-0">
            <Search size={18} className="text-blue-500 shrink-0" />
            <span className="font-medium text-gray-900 dark:text-white truncate">
              {t('duplicates.title')}
            </span>
            <span
              className="text-xs text-gray-500 dark:text-gray-400 truncate max-w-[300px]"
              title={scanPath}
            >
              {scanPath}
            </span>
          </div>
          <button
            onClick={onClose}
            className="p-1 hover:bg-gray-100 dark:hover:bg-gray-700 rounded shrink-0"
          >
            <X size={18} className="text-gray-500" />
          </button>
        </div>

        {/* Scanning state */}
        {isScanning && (
          <div className="flex flex-col items-center justify-center py-16 gap-3">
            <Loader2 size={32} className="animate-spin text-blue-500" />
            <span className="text-sm text-gray-600 dark:text-gray-400">
              {t('duplicates.scanning')}
            </span>
          </div>
        )}

        {/* Error state */}
        {!isScanning && error && (
          <div className="flex flex-col items-center justify-center py-16 gap-3">
            <AlertCircle size={32} className="text-red-500" />
            <span className="text-sm text-red-600 dark:text-red-400 text-center px-8">
              {error}
            </span>
            <button
              onClick={scan}
              className="mt-2 px-4 py-1.5 text-sm bg-blue-500 hover:bg-blue-600 text-white rounded"
            >
              {t('duplicates.retry')}
            </button>
          </div>
        )}

        {/* Empty state */}
        {!isScanning && !error && groups.length === 0 && (
          <div className="flex flex-col items-center justify-center py-16 gap-3">
            <CheckCircle size={32} className="text-green-500" />
            <span className="text-sm text-gray-600 dark:text-gray-400">
              {t('duplicates.noDuplicates')}
            </span>
          </div>
        )}

        {/* Results */}
        {!isScanning && !error && groups.length > 0 && (
          <>
            {/* Summary bar */}
            <div className="flex items-center gap-4 px-4 py-2.5 bg-gray-50 dark:bg-gray-900/50 border-b border-gray-200 dark:border-gray-700 text-xs text-gray-600 dark:text-gray-400">
              <span className="flex items-center gap-1.5">
                <Copy size={13} className="text-blue-400" />
                {summary.totalGroups} {t('duplicates.groups')}
              </span>
              <span className="flex items-center gap-1.5">
                <FileX size={13} className="text-orange-400" />
                {summary.totalDuplicates} {t('duplicates.duplicateFiles')}
              </span>
              <span className="flex items-center gap-1.5">
                <Trash2 size={13} className="text-red-400" />
                {formatBytes(summary.wastedBytes)} {t('duplicates.wasted')}
              </span>
            </div>

            {/* Groups list (scrollable) */}
            <div className="flex-1 overflow-y-auto px-4 py-3 space-y-4 min-h-0">
              {groups.map((group, groupIdx) => (
                <div
                  key={group.hash}
                  className="border border-gray-200 dark:border-gray-700 rounded-lg overflow-hidden"
                >
                  {/* Group header */}
                  <div className="flex items-center gap-2 px-3 py-2 bg-gray-100 dark:bg-gray-700/60 text-xs">
                    <Copy size={13} className="text-blue-400 shrink-0" />
                    <span className="font-medium text-gray-800 dark:text-gray-200">
                      {t('duplicates.group')} {groupIdx + 1}
                    </span>
                    <span className="text-gray-500 dark:text-gray-400">
                      &mdash; {getFileName(group.files[0])}
                    </span>
                    <span className="text-gray-400 dark:text-gray-500 ml-auto shrink-0">
                      {formatBytes(group.size)} &times; {group.files.length} {t('duplicates.copies')}
                    </span>
                  </div>

                  {/* File entries */}
                  <div className="divide-y divide-gray-100 dark:divide-gray-700/50">
                    {group.files.map((filePath, fileIdx) => {
                      const isFirst = fileIdx === 0;
                      const isChecked = selectedPaths.has(filePath);
                      const fileName = getFileName(filePath);
                      const dirPath = getDirectory(filePath);

                      return (
                        <label
                          key={filePath}
                          className={`flex items-start gap-3 px-3 py-2 cursor-pointer transition-colors ${
                            isFirst
                              ? 'bg-green-50/50 dark:bg-green-900/10'
                              : isChecked
                                ? 'bg-red-50/50 dark:bg-red-900/10 hover:bg-red-50 dark:hover:bg-red-900/20'
                                : 'hover:bg-gray-50 dark:hover:bg-gray-700/30'
                          }`}
                        >
                          {/* Checkbox — disabled for the first (kept) file */}
                          <input
                            type="checkbox"
                            checked={isChecked}
                            disabled={isFirst}
                            onChange={() => toggleFile(filePath)}
                            className="mt-1 w-3.5 h-3.5 rounded shrink-0 accent-red-500 disabled:opacity-30"
                          />

                          {/* File info */}
                          <div className="flex-1 min-w-0">
                            <div className="text-sm font-medium text-gray-900 dark:text-gray-100 truncate">
                              {fileName}
                            </div>
                            <div className="text-xs text-gray-500 dark:text-gray-400 truncate" title={dirPath}>
                              {dirPath}
                            </div>
                          </div>

                          {/* Keep / delete badge */}
                          <span
                            className={`shrink-0 mt-0.5 px-2 py-0.5 text-[10px] font-medium rounded ${
                              isFirst
                                ? 'bg-green-100 dark:bg-green-900/40 text-green-700 dark:text-green-300'
                                : isChecked
                                  ? 'bg-red-100 dark:bg-red-900/40 text-red-700 dark:text-red-300'
                                  : 'bg-gray-100 dark:bg-gray-700 text-gray-500 dark:text-gray-400'
                            }`}
                          >
                            {isFirst
                              ? t('duplicates.keep')
                              : isChecked
                                ? t('duplicates.delete')
                                : t('duplicates.skip')}
                          </span>
                        </label>
                      );
                    })}
                  </div>
                </div>
              ))}
            </div>

            {/* Footer actions */}
            <div className="flex items-center justify-between px-4 py-3 border-t border-gray-200 dark:border-gray-700">
              <div className="flex items-center gap-2">
                <button
                  onClick={selectAllDuplicates}
                  className="px-3 py-1.5 text-xs text-gray-700 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-700 rounded border border-gray-300 dark:border-gray-600"
                >
                  {t('duplicates.selectAll')}
                </button>
                <button
                  onClick={deselectAll}
                  className="px-3 py-1.5 text-xs text-gray-700 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-700 rounded border border-gray-300 dark:border-gray-600"
                >
                  {t('duplicates.deselectAll')}
                </button>
              </div>

              <button
                onClick={handleDelete}
                disabled={selectedCount === 0 || isDeleting}
                className="flex items-center gap-2 px-4 py-1.5 bg-red-500 hover:bg-red-600 disabled:opacity-50 disabled:cursor-not-allowed text-white rounded text-sm"
              >
                {isDeleting ? (
                  <Loader2 size={14} className="animate-spin" />
                ) : (
                  <Trash2 size={14} />
                )}
                {t('duplicates.deleteSelected')} ({selectedCount})
              </button>
            </div>
          </>
        )}

        {/* Footer for scanning/empty/error states (close only) */}
        {(isScanning || error || groups.length === 0) && (
          <div className="flex justify-end px-4 py-3 border-t border-gray-200 dark:border-gray-700">
            <button
              onClick={onClose}
              className="px-3 py-1.5 text-sm text-gray-700 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-700 rounded"
            >
              {t('common.close')}
            </button>
          </div>
        )}
      </div>
    </div>
  );
};

export default DuplicateFinderDialog;
