/**
 * BatchRenameDialog Component
 * Allows batch renaming of multiple files with pattern-based rules
 * Supports: find/replace, add prefix, add suffix, sequential numbering
 *
 * @since v1.8.0
 */

import * as React from 'react';
import { useState, useMemo, useEffect } from 'react';
import { Replace, Plus, Hash, AlertTriangle, X, Check, Loader2 } from 'lucide-react';
import { useTranslation } from '../i18n';

export type RenameMode = 'findReplace' | 'addPrefix' | 'addSuffix' | 'sequential';

export interface BatchRenameFile {
  name: string;
  path: string;
  isDir: boolean;
}

export interface BatchRenameDialogProps {
  isOpen: boolean;
  files: BatchRenameFile[];
  isRemote: boolean;
  onConfirm: (renames: Map<string, string>) => Promise<void>;
  onClose: () => void;
}

// Separate name and extension
const splitNameExt = (name: string, isDir: boolean): [string, string] => {
  if (isDir) return [name, ''];
  const lastDot = name.lastIndexOf('.');
  if (lastDot === -1 || lastDot === 0) return [name, ''];
  return [name.substring(0, lastDot), name.substring(lastDot)];
};

export const BatchRenameDialog: React.FC<BatchRenameDialogProps> = ({
  isOpen,
  files,
  isRemote,
  onConfirm,
  onClose,
}) => {
  const t = useTranslation();

  // Hide scrollbars when dialog is open (WebKitGTK fix)
  useEffect(() => {
    if (isOpen) {
      document.documentElement.classList.add('modal-open');
      return () => { document.documentElement.classList.remove('modal-open'); };
    }
  }, [isOpen]);

  // State
  const [mode, setMode] = useState<RenameMode>('findReplace');
  const [findText, setFindText] = useState('');
  const [replaceText, setReplaceText] = useState('');
  const [prefix, setPrefix] = useState('');
  const [suffix, setSuffix] = useState('');
  const [baseName, setBaseName] = useState('file');
  const [startNumber, setStartNumber] = useState(1);
  const [padding, setPadding] = useState(2);
  const [caseSensitive, setCaseSensitive] = useState(false);
  const [isProcessing, setIsProcessing] = useState(false);

  // Reset state when dialog opens
  useEffect(() => {
    if (isOpen) {
      setMode('findReplace');
      setFindText('');
      setReplaceText('');
      setPrefix('');
      setSuffix('');
      setBaseName('file');
      setStartNumber(1);
      setPadding(2);
      setCaseSensitive(false);
      setIsProcessing(false);
    }
  }, [isOpen]);

  // Compute preview of new names
  const renames = useMemo(() => {
    const result = new Map<string, string>();

    files.forEach((file, index) => {
      const [nameNoExt, ext] = splitNameExt(file.name, file.isDir);
      let newName = file.name;

      switch (mode) {
        case 'findReplace':
          if (findText) {
            const escapedFind = findText.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
            const regex = caseSensitive
              ? new RegExp(escapedFind, 'g')
              : new RegExp(escapedFind, 'gi');
            newName = file.name.replace(regex, replaceText);
          }
          break;

        case 'addPrefix':
          if (prefix) {
            newName = prefix + file.name;
          }
          break;

        case 'addSuffix':
          if (suffix) {
            newName = nameNoExt + suffix + ext;
          }
          break;

        case 'sequential':
          if (baseName) {
            const num = (startNumber + index).toString().padStart(padding, '0');
            newName = `${baseName}_${num}${ext}`;
          }
          break;
      }

      if (newName !== file.name && newName.trim()) {
        result.set(file.path, newName);
      }
    });

    return result;
  }, [files, mode, findText, replaceText, prefix, suffix, baseName, startNumber, padding, caseSensitive]);

  // Detect naming conflicts
  const conflicts = useMemo(() => {
    const newNames = Array.from(renames.values());
    const duplicates = new Set<string>();
    const seen = new Set<string>();

    newNames.forEach(name => {
      if (seen.has(name)) {
        duplicates.add(name);
      }
      seen.add(name);
    });

    return duplicates;
  }, [renames]);

  const hasConflicts = conflicts.size > 0;
  const hasChanges = renames.size > 0;

  const handleConfirm = async () => {
    if (hasConflicts || !hasChanges) return;
    setIsProcessing(true);
    try {
      await onConfirm(renames);
      onClose();
    } finally {
      setIsProcessing(false);
    }
  };

  if (!isOpen) return null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60" role="dialog" aria-modal="true" aria-label="Batch Rename">
      <div className="bg-white dark:bg-gray-800 rounded-lg shadow-2xl border border-gray-200 dark:border-gray-700 w-[560px] max-h-[80vh] flex flex-col">
        {/* Header */}
        <div className="flex items-center justify-between px-4 py-3 border-b border-gray-200 dark:border-gray-700">
          <div className="flex items-center gap-2">
            <Replace size={18} className="text-blue-500" />
            <span className="font-medium text-gray-900 dark:text-white">
              {t('batchRename.title') || 'Batch Rename'} - {files.length} {t('browser.files') || 'files'}
            </span>
            {isRemote && (
              <span className="px-1.5 py-0.5 text-[10px] bg-green-100 dark:bg-green-900/40 text-green-700 dark:text-green-300 rounded">
                Remote
              </span>
            )}
          </div>
          <button
            onClick={onClose}
            className="p-1 hover:bg-gray-100 dark:hover:bg-gray-700 rounded"
          >
            <X size={18} className="text-gray-500" />
          </button>
        </div>

        <div className="p-4 flex flex-col gap-4 overflow-y-auto">
          {/* Mode selector */}
          <div>
            <label className="text-xs text-gray-500 dark:text-gray-400 block mb-1.5">
              {t('batchRename.mode') || 'Rename Mode'}
            </label>
            <div className="flex gap-1 flex-wrap">
              {[
                { value: 'findReplace', icon: <Replace size={14} />, label: t('batchRename.findReplace') || 'Find & Replace' },
                { value: 'addPrefix', icon: <Plus size={14} />, label: t('batchRename.addPrefix') || 'Add Prefix' },
                { value: 'addSuffix', icon: <Plus size={14} />, label: t('batchRename.addSuffix') || 'Add Suffix' },
                { value: 'sequential', icon: <Hash size={14} />, label: t('batchRename.sequential') || 'Sequential' },
              ].map(opt => (
                <button
                  key={opt.value}
                  onClick={() => setMode(opt.value as RenameMode)}
                  className={`flex items-center gap-1.5 px-3 py-1.5 text-xs rounded border transition-colors ${
                    mode === opt.value
                      ? 'bg-blue-500 border-blue-400 text-white'
                      : 'bg-gray-100 dark:bg-gray-700 border-gray-300 dark:border-gray-600 text-gray-700 dark:text-gray-300 hover:bg-gray-200 dark:hover:bg-gray-600'
                  }`}
                >
                  {opt.icon}
                  {opt.label}
                </button>
              ))}
            </div>
          </div>

          {/* Mode-specific inputs */}
          {mode === 'findReplace' && (
            <div className="space-y-3">
              <div className="grid grid-cols-2 gap-3">
                <div>
                  <label className="text-xs text-gray-500 dark:text-gray-400 block mb-1">
                    {t('batchRename.find') || 'Find'}
                  </label>
                  <input
                    type="text"
                    value={findText}
                    onChange={e => setFindText(e.target.value)}
                    placeholder={t('batchRename.findPlaceholder') || 'Text to find...'}
                    className="w-full px-3 py-1.5 text-sm bg-gray-50 dark:bg-gray-900 border border-gray-300 dark:border-gray-600 rounded focus:ring-2 focus:ring-blue-500 text-gray-900 dark:text-gray-100"
                    autoFocus
                  />
                </div>
                <div>
                  <label className="text-xs text-gray-500 dark:text-gray-400 block mb-1">
                    {t('batchRename.replaceWith') || 'Replace with'}
                  </label>
                  <input
                    type="text"
                    value={replaceText}
                    onChange={e => setReplaceText(e.target.value)}
                    placeholder={t('batchRename.replacePlaceholder') || 'Replacement text...'}
                    className="w-full px-3 py-1.5 text-sm bg-gray-50 dark:bg-gray-900 border border-gray-300 dark:border-gray-600 rounded focus:ring-2 focus:ring-blue-500 text-gray-900 dark:text-gray-100"
                  />
                </div>
              </div>
              <label className="flex items-center gap-2 text-xs text-gray-600 dark:text-gray-400">
                <input
                  type="checkbox"
                  checked={caseSensitive}
                  onChange={e => setCaseSensitive(e.target.checked)}
                  className="w-3.5 h-3.5 rounded"
                />
                {t('batchRename.caseSensitive') || 'Case sensitive'}
              </label>
            </div>
          )}

          {mode === 'addPrefix' && (
            <div>
              <label className="text-xs text-gray-500 dark:text-gray-400 block mb-1">
                {t('batchRename.prefix') || 'Prefix'}
              </label>
              <input
                type="text"
                value={prefix}
                onChange={e => setPrefix(e.target.value)}
                placeholder={t('batchRename.prefixPlaceholder') || 'e.g., backup_'}
                className="w-full px-3 py-1.5 text-sm bg-gray-50 dark:bg-gray-900 border border-gray-300 dark:border-gray-600 rounded focus:ring-2 focus:ring-blue-500 text-gray-900 dark:text-gray-100"
                autoFocus
              />
            </div>
          )}

          {mode === 'addSuffix' && (
            <div>
              <label className="text-xs text-gray-500 dark:text-gray-400 block mb-1">
                {t('batchRename.suffix') || 'Suffix (before extension)'}
              </label>
              <input
                type="text"
                value={suffix}
                onChange={e => setSuffix(e.target.value)}
                placeholder={t('batchRename.suffixPlaceholder') || 'e.g., _v2'}
                className="w-full px-3 py-1.5 text-sm bg-gray-50 dark:bg-gray-900 border border-gray-300 dark:border-gray-600 rounded focus:ring-2 focus:ring-blue-500 text-gray-900 dark:text-gray-100"
                autoFocus
              />
            </div>
          )}

          {mode === 'sequential' && (
            <div className="grid grid-cols-3 gap-3">
              <div className="col-span-3">
                <label className="text-xs text-gray-500 dark:text-gray-400 block mb-1">
                  {t('batchRename.baseName') || 'Base name'}
                </label>
                <input
                  type="text"
                  value={baseName}
                  onChange={e => setBaseName(e.target.value)}
                  placeholder="file"
                  className="w-full px-3 py-1.5 text-sm bg-gray-50 dark:bg-gray-900 border border-gray-300 dark:border-gray-600 rounded focus:ring-2 focus:ring-blue-500 text-gray-900 dark:text-gray-100"
                  autoFocus
                />
              </div>
              <div>
                <label className="text-xs text-gray-500 dark:text-gray-400 block mb-1">
                  {t('batchRename.startNumber') || 'Start at'}
                </label>
                <input
                  type="number"
                  min={0}
                  value={startNumber}
                  onChange={e => setStartNumber(parseInt(e.target.value) || 0)}
                  className="w-full px-3 py-1.5 text-sm bg-gray-50 dark:bg-gray-900 border border-gray-300 dark:border-gray-600 rounded focus:ring-2 focus:ring-blue-500 text-gray-900 dark:text-gray-100"
                />
              </div>
              <div>
                <label className="text-xs text-gray-500 dark:text-gray-400 block mb-1">
                  {t('batchRename.padding') || 'Padding'}
                </label>
                <select
                  value={padding}
                  onChange={e => setPadding(parseInt(e.target.value))}
                  className="w-full px-3 py-1.5 text-sm bg-gray-50 dark:bg-gray-900 border border-gray-300 dark:border-gray-600 rounded text-gray-900 dark:text-gray-100"
                >
                  <option value={1}>1 (1, 2, 3)</option>
                  <option value={2}>2 (01, 02)</option>
                  <option value={3}>3 (001, 002)</option>
                  <option value={4}>4 (0001)</option>
                </select>
              </div>
              <div className="flex items-end pb-1.5">
                <span className="text-xs text-gray-400">
                  {t('batchRename.preview') || 'Preview'}: {baseName}_{startNumber.toString().padStart(padding, '0')}.ext
                </span>
              </div>
            </div>
          )}

          {/* Preview table */}
          <div>
            <label className="text-xs text-gray-500 dark:text-gray-400 block mb-1.5">
              {t('batchRename.previewChanges') || 'Preview Changes'} ({renames.size}/{files.length})
            </label>
            <div className="border border-gray-200 dark:border-gray-700 rounded max-h-48 overflow-y-auto">
              <table className="w-full text-xs">
                <thead className="bg-gray-100 dark:bg-gray-700 sticky top-0">
                  <tr>
                    <th className="px-2 py-1.5 text-left font-medium text-gray-600 dark:text-gray-300">
                      {t('batchRename.oldName') || 'Old Name'}
                    </th>
                    <th className="px-2 py-1.5 text-left font-medium text-gray-600 dark:text-gray-300">
                      {t('batchRename.newName') || 'New Name'}
                    </th>
                  </tr>
                </thead>
                <tbody>
                  {files.map(file => {
                    const newName = renames.get(file.path);
                    const isConflict = newName && conflicts.has(newName);
                    const hasChange = !!newName;

                    return (
                      <tr
                        key={file.path}
                        className={`border-t border-gray-100 dark:border-gray-700 ${
                          isConflict ? 'bg-red-50 dark:bg-red-900/20' : ''
                        }`}
                      >
                        <td className="px-2 py-1 text-gray-700 dark:text-gray-300 truncate max-w-[200px]" title={file.name}>
                          {file.name}
                        </td>
                        <td className={`px-2 py-1 truncate max-w-[200px] ${
                          isConflict
                            ? 'text-red-600 dark:text-red-400'
                            : hasChange
                              ? 'text-green-600 dark:text-green-400'
                              : 'text-gray-400'
                        }`} title={newName || file.name}>
                          <span className="flex items-center gap-1">
                            {isConflict && <AlertTriangle size={12} />}
                            {newName || <span className="italic">{t('batchRename.noChange') || '(no change)'}</span>}
                          </span>
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </div>
          </div>

          {/* Conflict warning */}
          {hasConflicts && (
            <div className="flex items-center gap-2 px-3 py-2 bg-red-50 dark:bg-red-900/30 border border-red-200 dark:border-red-800 rounded text-xs text-red-700 dark:text-red-300">
              <AlertTriangle size={14} />
              {t('batchRename.conflictWarning') || 'Some files would have the same name. Please adjust the pattern.'}
            </div>
          )}
        </div>

        {/* Footer */}
        <div className="flex justify-end gap-2 px-4 py-3 border-t border-gray-200 dark:border-gray-700">
          <button
            onClick={onClose}
            className="px-3 py-1.5 text-sm text-gray-700 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-700 rounded"
          >
            {t('common.cancel') || 'Cancel'}
          </button>
          <button
            onClick={handleConfirm}
            disabled={hasConflicts || !hasChanges || isProcessing}
            className="flex items-center gap-2 px-4 py-1.5 bg-blue-500 hover:bg-blue-600 disabled:opacity-50 disabled:cursor-not-allowed text-white rounded text-sm"
          >
            {isProcessing ? (
              <Loader2 size={14} className="animate-spin" />
            ) : (
              <Check size={14} />
            )}
            {t('batchRename.apply') || 'Apply'} ({renames.size})
          </button>
        </div>
      </div>
    </div>
  );
};

export default BatchRenameDialog;
