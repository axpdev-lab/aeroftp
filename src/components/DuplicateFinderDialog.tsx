// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

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
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { Search, X, Trash2, CheckCircle, AlertCircle, Loader2, Copy, FileX, Check, ExternalLink, FolderOpen } from 'lucide-react';
import { useTranslation } from '../i18n';
import { formatBytes } from '../utils/formatters';
import { DuplicateGroup } from '../types/aerofile';
import { Checkbox } from './ui/Checkbox';
import { useDraggableModal } from '../hooks/useDraggableModal';
import { useClipboardCopy } from '../hooks/useClipboardCopy';
import { MODAL_Z } from '../utils/modalLayers';
import { ImageThumbnail } from './ImageThumbnail';
import { signatureOf } from '../utils/thumbnailCache';

/** Extensions ImageThumbnail can render, i.e. what is worth a preview square. */
const PREVIEWABLE_IMAGE = /\.(jpe?g|png|gif|svg|webp|bmp|ico)$/i;
const isPreviewableImage = (name: string): boolean => PREVIEWABLE_IMAGE.test(name);

/** Payload of the backend's `duplicate-scan-progress` event (filesystem.rs). */
interface DuplicateScanProgress {
  phase: 'walk' | 'analyze';
  files_scanned: number;
  dirs_scanned: number;
  bytes_scanned: number;
  max_depth: number;
  files_processed: number;
  files_total: number;
  current_path: string;
}

/** m:ss, or h:mm:ss once a scan has been running that long. */
const formatElapsed = (ms: number): string => {
  const total = Math.floor(ms / 1000);
  const s = total % 60;
  const m = Math.floor(total / 60) % 60;
  const h = Math.floor(total / 3600);
  const mm = String(m).padStart(2, '0');
  const ss = String(s).padStart(2, '0');
  return h > 0 ? `${h}:${mm}:${ss}` : `${m}:${ss}`;
};

interface DuplicateFinderDialogProps {
  isOpen: boolean;
  scanPath: string;
  onClose: () => void;
  /**
   * Deletes the given paths. It must NOT ask the user anything: this dialog owns
   * the confirmation, and a second prompt raised by the caller is exactly the
   * deadlock of #537 — it renders behind this modal, unreachable behind its
   * backdrop, while the promise it gates never settles.
   */
  onDeleteFiles: (paths: string[]) => Promise<void>;
  /**
   * The app's "confirm before delete" setting. It governs *this* dialog's own
   * confirmation; when it is off the delete runs straight away.
   */
  confirmBeforeDelete?: boolean;
  /**
   * Opens one file in the app's preview, where it can also be edited. Given, the
   * rows grow a clickable thumbnail; omitted, they do not.
   */
  onPreviewFile?: (path: string) => void;
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

/**
 * Copy affordance for one row value. One hook instance per button, so the tick
 * lands on the button that was pressed and not on every copy in the list.
 *
 * Ehud asked for these because reading a duplicate's folder off the screen with
 * an OCR app was the only way to go compare the two files outside AeroFTP.
 */
const RowCopyButton: React.FC<{ value: string; label: string }> = ({ value, label }) => {
  const { copied, copy } = useClipboardCopy();
  return (
    <button
      type="button"
      onClick={(e) => { e.stopPropagation(); void copy(value); }}
      title={label}
      aria-label={label}
      className="shrink-0 p-1 rounded text-gray-400 hover:text-blue-500 hover:bg-gray-100 dark:hover:bg-gray-700 transition-colors"
    >
      {copied ? <Check size={12} className="text-green-500" /> : <Copy size={12} />}
    </button>
  );
};

type KeepPolicy = 'shortestName' | 'oldest' | 'newest' | 'firstFound';

/**
 * Which member of a group to keep, by policy.
 *
 * `oldest`/`newest` fall back to the scan order when the backend did not report
 * sizes to order by: the engine returns members in walk order, which is stable
 * but not meaningful, so the honest answer there is "the first one".
 */
export function keeperOf(group: DuplicateGroup, policy: KeepPolicy): string | undefined {
  const files = group.files;
  if (files.length === 0) return undefined;
  const nameOf = (p: string) => getFileName(p);
  switch (policy) {
    case 'shortestName':
      // " (copy)", " (1)", " - Copy" — the derived file is the longer name
      // almost every time, so the shortest is the likeliest original.
      return files.reduce((best, cur) =>
        nameOf(cur).length < nameOf(best).length ? cur : best);
    case 'oldest':
    case 'newest': {
      const sizes = group.file_sizes;
      if (!sizes || sizes.length !== files.length) return files[0];
      // Without mtimes from this command, size stands in for "the fuller copy":
      // largest for `newest`, smallest for `oldest`.
      let bestIdx = 0;
      for (let i = 1; i < files.length; i++) {
        const better = policy === 'newest' ? sizes[i] > sizes[bestIdx] : sizes[i] < sizes[bestIdx];
        if (better) bestIdx = i;
      }
      return files[bestIdx];
    }
    case 'firstFound':
    default:
      return files[0];
  }
}

/** Every copy except each group's keeper. */
export function selectionForPolicy(groups: DuplicateGroup[], policy: KeepPolicy): Set<string> {
  const selected = new Set<string>();
  for (const group of groups) {
    const keeper = keeperOf(group, policy);
    for (const file of group.files) if (file !== keeper) selected.add(file);
  }
  return selected;
}

/**
 * A hash for reading, not for comparing byte-for-byte.
 *
 * Ehud's point on #347: 128 bits is past any collision anyone will meet, and a
 * 64-hex-digit string on every row is a wall. The full value stays one hover or
 * one copy away — and, importantly, stays the grouping key in the engine: there
 * is no reason to weaken the key of an operation that deletes files when the
 * digest has already been computed in full.
 */
export const SHORT_HASH_HEX = 32;
export function shortHash(hash: string): string {
  return /^[0-9a-f]+$/i.test(hash) && hash.length > SHORT_HASH_HEX
    ? `${hash.slice(0, SHORT_HASH_HEX)}…`
    : hash;
}

export const DuplicateFinderDialog: React.FC<DuplicateFinderDialogProps> = ({
  isOpen,
  scanPath,
  onClose,
  onDeleteFiles,
  confirmBeforeDelete = true,
  onPreviewFile,
}) => {
  const t = useTranslation();
  const modalDrag = useDraggableModal();

  const [groups, setGroups] = useState<DuplicateGroup[]>([]);
  const [isScanning, setIsScanning] = useState(false);
  const [isDeleting, setIsDeleting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // Set of file paths selected for deletion
  const [selectedPaths, setSelectedPaths] = useState<Set<string>>(new Set());
  // Mode: 'exact' (default, byte-identical) or 'non-identical' (consumes shared engine)
  const [mode, setMode] = useState<'exact' | 'non-identical'>('exact');
  // What the backend is chewing through, and for how long. A scan over a photo
  // library is long enough that a bare spinner cannot be told apart from a hang.
  const [progress, setProgress] = useState<DuplicateScanProgress | null>(null);
  const [elapsedMs, setElapsedMs] = useState(0);
  // Show the hash behind each row. Off by default: it is diagnostic, and it is
  // the reason a group exists rather than something to act on.
  const [showHashes, setShowHashes] = useState(false);
  // 'waste' is the historical order (biggest reclaimable space first).
  // 'similarity' answers "which of these are really the same file?": exact
  // groups first, then fuzzy groups from the closest signature to the loosest.
  const [sortBy, setSortBy] = useState<'waste' | 'similarity' | 'sizeSpread'>('waste');
  // Which copy a group starts out keeping. The dialog used to decide this and
  // then refuse to let it be changed; it is now only a starting point, and every
  // row can be ticked. 'shortestName' is the default because " (copy)" and
  // " (1)" make the derived file the longer name almost every time (#347).
  const [keepPolicy, setKeepPolicy] = useState<'shortestName' | 'oldest' | 'newest' | 'firstFound'>('shortestName');
  // The fuzzy cutoff, applied only in non-identical mode. null = engine defaults
  // (raster <=10, text <=3, other <=100), which is what every scan used before.
  const [threshold, setThreshold] = useState<number | null>(null);
  // Committed separately from the input so typing does not restart the scan on
  // every keystroke; the scan re-runs when this changes.
  const [appliedThreshold, setAppliedThreshold] = useState<number | null>(null);

  // Scan for duplicates when the dialog opens
  const scan = useCallback(async () => {
    setIsScanning(true);
    setError(null);
    setGroups([]);
    setSelectedPaths(new Set());
    setProgress(null);
    setElapsedMs(0);

    try {
      const result = await invoke<DuplicateGroup[]>('find_duplicate_files', {
        path: scanPath,
        mode: mode,
        // Only meaningful for the fuzzy engine; null keeps its per-modality defaults.
        distance: mode === 'non-identical' ? appliedThreshold : null,
      });
      setGroups(result);

      if (mode === 'exact') {
        // Exact mode ticks every copy but the one the keep policy picks. The
        // copies are byte-identical, so which one survives is a question about
        // names and dates rather than about content.
        setSelectedPaths(selectionForPolicy(result, keepPolicy));
      } else {
        // Non-identical mode: NEVER auto-select. The members are not the same
        // file, so nothing here may be pre-ticked for deletion.
        setSelectedPaths(new Set());
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setIsScanning(false);
    }
  }, [scanPath, mode, appliedThreshold]);

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
    // Note: `scan` depends on `mode` and on the applied fuzzy threshold, so
    // changing either re-runs this effect and re-scans. No separate effect.
  }, [isOpen, scan]);

  // Backend progress. Subscribed for the dialog's whole open lifetime rather
  // than per scan, so a tick that arrives between two scans cannot be missed.
  useEffect(() => {
    if (!isOpen) return;
    let unlisten: UnlistenFn | null = null;
    let cancelled = false;
    listen<DuplicateScanProgress>('duplicate-scan-progress', (e) => {
      setProgress(e.payload);
    }).then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    });
    return () => {
      cancelled = true;
      if (unlisten) unlisten();
    };
  }, [isOpen]);

  // The clock. Ticking here rather than off the backend events keeps it moving
  // during a long single file, which is exactly when the user starts to wonder
  // whether the scan is stuck.
  useEffect(() => {
    if (!isScanning) return;
    const startedAt = Date.now();
    setElapsedMs(0);
    const id = setInterval(() => setElapsedMs(Date.now() - startedAt), 250);
    return () => clearInterval(id);
  }, [isScanning]);

  // Hide scrollbars when dialog is open (WebKitGTK fix)
  useEffect(() => {
    if (isOpen) {
      document.documentElement.classList.add('modal-open');
      return () => { document.documentElement.classList.remove('modal-open'); };
    }
  }, [isOpen]);

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

  /**
   * Tick every copy except the one the keep policy picks — what the button
   * labelled "Select All Duplicates" actually did. It is now labelled that way.
   */
  const selectAllButOne = useCallback(() => {
    setSelectedPaths(selectionForPolicy(groups, keepPolicy));
  }, [groups, keepPolicy]);

  /**
   * Tick everything, including the copy the policy would have kept. Allowed:
   * "delete every copy of this" is a thing a user can mean, and the group guard
   * below makes them say so on purpose rather than by not noticing.
   */
  const selectAll = useCallback(() => {
    setSelectedPaths(new Set(groups.flatMap((g) => g.files)));
  }, [groups]);

  // Deselect everything
  const deselectAll = useCallback(() => {
    setSelectedPaths(new Set());
  }, []);

  /**
   * The groups where every single copy is ticked.
   *
   * This is the invariant the disabled first checkbox was protecting, and it was
   * protecting it by removing the choice: one file per group could not be ticked
   * at all, so a copy in the wrong folder, or the one actually named " (copy)",
   * was the one you could not delete (#347). Watching the invariant instead of
   * amputating the control gives the choice back and still refuses the accident.
   */
  const fullyTickedGroups = useMemo(
    () => groups.filter((g) => g.files.length > 0 && g.files.every((f) => selectedPaths.has(f))),
    [groups, selectedPaths],
  );

  // Inline confirmation dialog state (replaces window.confirm for styled UX)
  const [pendingDeleteConfirm, setPendingDeleteConfirm] = useState(false);

  const runDelete = useCallback(async () => {
    setPendingDeleteConfirm(false);
    const paths = Array.from(selectedPaths);
    if (paths.length === 0) return;

    setIsDeleting(true);
    try {
      await onDeleteFiles(paths);
      // Remove deleted files from groups and update state
      const updatedGroups: DuplicateGroup[] = [];
      for (const group of groups) {
        // `file_hashes` and `file_sizes` are parallel to `files`; filtering one
        // and not the others would silently shift every row's hash and size by
        // the number of copies deleted above it.
        const kept = group.files
          .map((f, i) => ({ f, i }))
          .filter(({ f }) => !selectedPaths.has(f));
        if (kept.length > 1) {
          updatedGroups.push({
            ...group,
            files: kept.map(({ f }) => f),
            file_hashes: group.file_hashes ? kept.map(({ i }) => group.file_hashes![i]) : undefined,
            file_sizes: group.file_sizes ? kept.map(({ i }) => group.file_sizes![i]) : undefined,
          });
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

  /**
   * The one confirmation in this flow. `confirmBeforeDelete` decides whether it
   * is raised at all; `onDeleteFiles` must not raise a second one (#537).
   */
  const handleDelete = useCallback(() => {
    if (selectedPaths.size === 0) return;
    if (!confirmBeforeDelete) {
      void runDelete();
      return;
    }
    setPendingDeleteConfirm(true);
  }, [selectedPaths, confirmBeforeDelete, runDelete]);

  // Summary calculations
  const summary = useMemo(() => {
    const totalGroups = groups.length;
    let totalDuplicates = 0;
    let wastedBytes = 0;

    if (mode === 'non-identical') {
      // Non-identical: "potential" waste only from user-selected files (nothing auto-selected)
      for (const group of groups) {
        for (const f of group.files) {
          if (selectedPaths.has(f)) {
            // count as potential dupe (size is group size, approximate)
            totalDuplicates += 1;
            wastedBytes += group.size;
          }
        }
      }
    } else {
      for (const group of groups) {
        const dupeCount = group.files.length - 1;
        totalDuplicates += dupeCount;
        wastedBytes += group.size * dupeCount;
      }
    }

    return { totalGroups, totalDuplicates, wastedBytes };
  }, [groups, mode, selectedPaths]);

  // The order the groups are shown in. The backend returns them by reclaimable
  // space; 'similarity' re-sorts by how alike the group's members actually are,
  // which is the question when deciding what is a real duplicate: byte-identical
  // groups (no fuzzy distance at all) first, then the closest signatures, then
  // the ones the threshold only just let in.
  /** Largest minus smallest member, in bytes. 0 when sizes are unknown. */
  const sizeSpreadOf = (group: DuplicateGroup): number => {
    const sizes = group.file_sizes;
    if (!sizes || sizes.length < 2) return 0;
    return Math.max(...sizes) - Math.min(...sizes);
  };

  const orderedGroups = useMemo(() => {
    if (sortBy === 'sizeSpread') {
      // Ehud's observation on #347: the pair that looked identical differed by a
      // few bytes, the pair that was clearly different differed by a third. The
      // spread does not replace the Hamming distance as the clustering metric —
      // a re-encode moves bytes a lot and the perceptual hash barely at all,
      // which is exactly the case worth catching — but as an ordering it puts
      // the "same picture, different encoder" groups first and the "same subject,
      // different picture" groups last.
      return [...groups].sort((a, b) => sizeSpreadOf(a) - sizeSpreadOf(b));
    }
    if (sortBy !== 'similarity') return groups;
    return [...groups].sort((a, b) => {
      const da = a.distance ?? -1;
      const db = b.distance ?? -1;
      if (da !== db) return da - db;
      // Same closeness: fall back to the reclaimable-space order.
      return b.size * (b.files.length - 1) - a.size * (a.files.length - 1);
    });
  }, [groups, sortBy]);

  const selectedCount = selectedPaths.size;

  if (!isOpen) return null;

  return (
    <div className={`fixed inset-0 ${MODAL_Z.modal} flex items-center justify-center bg-black/50`}>
      <div
        {...modalDrag.panelProps}
        className="bg-white dark:bg-gray-800 rounded-lg shadow-xl w-[700px] max-h-[80vh] flex flex-col animate-scale-in"
        role="dialog"
        aria-label={t('duplicates.title')}
        aria-modal="true"
      >
        {/* Header */}
        <div {...modalDrag.dragHandleProps} className="flex items-center justify-between px-4 py-3 border-b border-gray-200 dark:border-gray-700 cursor-grab active:cursor-grabbing">
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

        {/* Mode toggle: Exact (byte-identical, default, auto-selects dupes) vs Non-identical (uses shared engine, no auto-select) */}
        <div className="flex items-center gap-2 px-4 py-2 border-b border-gray-200 dark:border-gray-700 bg-gray-50 dark:bg-gray-800/50">
          <span className="text-xs text-gray-500 dark:text-gray-400 mr-1">Mode:</span>
          <button
            disabled={isScanning}
            onClick={() => setMode('exact')}
            className={`px-3 py-1 text-xs rounded border transition-colors disabled:opacity-50 ${
              mode === 'exact'
                ? 'bg-blue-500 text-white border-blue-500'
                : 'bg-white dark:bg-gray-700 text-gray-700 dark:text-gray-300 border-gray-300 dark:border-gray-600 hover:bg-gray-100 dark:hover:bg-gray-600'
            }`}
          >
            Exact
          </button>
          <button
            disabled={isScanning}
            onClick={() => setMode('non-identical')}
            className={`px-3 py-1 text-xs rounded border transition-colors disabled:opacity-50 ${
              mode === 'non-identical'
                ? 'bg-purple-500 text-white border-purple-500'
                : 'bg-white dark:bg-gray-700 text-gray-700 dark:text-gray-300 border-gray-300 dark:border-gray-600 hover:bg-gray-100 dark:hover:bg-gray-600'
            }`}
          >
            Non-identical
          </button>
          <span className="ml-2 text-[10px] text-gray-400 dark:text-gray-500">
            {mode === 'non-identical' ? 'Perceptual / text similarity — no auto-delete' : 'Byte-identical'}
          </span>
        </div>

        {/* View controls: what order the groups come in, whether the hashes are
            visible, and — in fuzzy mode — how far apart two signatures may be
            and still be called duplicates. */}
        <div className="flex flex-wrap items-center gap-x-3 gap-y-2 px-4 py-2 border-b border-gray-200 dark:border-gray-700 text-xs">
          <label className="flex items-center gap-1.5 text-gray-500 dark:text-gray-400">
            {t('browser.sortBy')}
            <select
              value={sortBy}
              onChange={(e) => setSortBy(e.target.value as 'waste' | 'similarity' | 'sizeSpread')}
              className="px-1.5 py-0.5 rounded border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-700 text-gray-700 dark:text-gray-200"
            >
              <option value="waste">{t('duplicates.sortWasted')}</option>
              <option value="similarity">{t('duplicates.sortSimilarity')}</option>
              <option value="sizeSpread">{t('duplicates.sortSizeSpread')}</option>
            </select>
          </label>

          <label className="flex items-center gap-1.5 text-gray-500 dark:text-gray-400">
            {t('duplicates.keepByDefault')}
            <select
              value={keepPolicy}
              onChange={(e) => setKeepPolicy(e.target.value as typeof keepPolicy)}
              className="px-1.5 py-0.5 rounded border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-700 text-gray-700 dark:text-gray-200"
            >
              <option value="shortestName">{t('duplicates.keepShortestName')}</option>
              <option value="oldest">{t('duplicates.keepSmallest')}</option>
              <option value="newest">{t('duplicates.keepLargest')}</option>
              <option value="firstFound">{t('duplicates.keepFirstFound')}</option>
            </select>
          </label>

          {/* The label rides on the Checkbox itself: a bare <label> around it
              has no form control to point at, so clicking the text did nothing
              while the cursor promised otherwise. */}
          <Checkbox
            checked={showHashes}
            onChange={setShowHashes}
            label={t('duplicates.showHashes')}
            labelClassName="text-gray-500 dark:text-gray-400"
          />

          {mode === 'non-identical' && (
            <label className="flex items-center gap-1.5 text-gray-500 dark:text-gray-400">
              {t('duplicates.fuzzyCutoff')}
              <input
                type="number"
                min={0}
                max={200}
                value={threshold ?? ''}
                placeholder={t('duplicates.fuzzyCutoffPlaceholder') || 'auto'}
                disabled={isScanning}
                onChange={(e) => {
                  const raw = e.target.value.trim();
                  setThreshold(raw === '' ? null : Math.max(0, Math.min(200, Number(raw))));
                }}
                // Committed on blur or Enter, never per keystroke: each commit
                // re-runs the whole scan.
                onBlur={() => setAppliedThreshold(threshold)}
                onKeyDown={(e) => { if (e.key === 'Enter') setAppliedThreshold(threshold); }}
                className="w-16 px-1.5 py-0.5 rounded border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-700 text-gray-700 dark:text-gray-200 disabled:opacity-50"
              />
              <span className="text-[10px] text-gray-400 dark:text-gray-500">
                {t('duplicates.fuzzyCutoffHint')}
              </span>
            </label>
          )}
        </div>

        {/* Scanning state: the counters the summary would have shown anyway,
            shown while they are still the only thing the user can act on. */}
        {isScanning && (
          <div className="flex flex-col items-center justify-center py-12 gap-3 px-8">
            <Loader2 size={32} className="animate-spin text-blue-500" />
            <span className="text-sm text-gray-600 dark:text-gray-400">
              {t('duplicates.scanning')}
            </span>

            {/* Determinate bar for the analysis pass, whose total is known;
                the walk has no denominator yet, so it stays indeterminate. */}
            {progress?.phase === 'analyze' && progress.files_total > 0 && (
              <div className="w-full max-w-sm h-1.5 rounded-full bg-gray-200 dark:bg-gray-700 overflow-hidden">
                <div
                  className="h-full bg-blue-500 transition-[width] duration-200"
                  style={{ width: `${Math.min(100, Math.round((progress.files_processed / progress.files_total) * 100))}%` }}
                />
              </div>
            )}

            <div className="flex flex-wrap items-center justify-center gap-x-4 gap-y-1 text-xs text-gray-500 dark:text-gray-400">
              <span className="tabular-nums">⏱ {formatElapsed(elapsedMs)}</span>
              <span className="tabular-nums">
                📄 {(progress?.files_scanned ?? 0).toLocaleString()}
              </span>
              <span className="tabular-nums">
                📁 {(progress?.dirs_scanned ?? 0).toLocaleString()}
              </span>
              <span className="tabular-nums">{formatBytes(progress?.bytes_scanned ?? 0)}</span>
              {(progress?.max_depth ?? 0) > 0 && (
                <span className="tabular-nums">↳ {progress?.max_depth}</span>
              )}
              {progress?.phase === 'analyze' && progress.files_total > 0 && (
                <span className="tabular-nums">
                  {progress.files_processed.toLocaleString()} / {progress.files_total.toLocaleString()}
                </span>
              )}
            </div>

            {progress?.phase === 'walk' && progress.current_path && (
              <span className="max-w-full truncate text-[10px] text-gray-400 dark:text-gray-500" title={progress.current_path}>
                {progress.current_path}
              </span>
            )}
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
                {formatBytes(summary.wastedBytes)} {mode === 'non-identical' ? 'potential waste (selected)' : t('duplicates.wasted')}
              </span>
            </div>

            {/* Groups list (scrollable) */}
            <div className="modal-scroll flex-1 overflow-y-auto px-4 py-3 space-y-4 min-h-0">
              {orderedGroups.map((group, groupIdx) => (
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
                    {group.files.every((f) => selectedPaths.has(f)) && (
                      <span className="ml-2 px-1.5 py-0.5 text-[10px] rounded bg-amber-100 dark:bg-amber-900/40 text-amber-800 dark:text-amber-300 shrink-0">
                        {t('duplicates.everyCopyTicked')}
                      </span>
                    )}
                    {group.similarity && (
                      <span className="ml-2 px-1.5 py-0.5 text-[10px] rounded bg-purple-100 dark:bg-purple-900/40 text-purple-700 dark:text-purple-300 shrink-0">
                        {group.similarity}{group.distance != null ? `, dist ${group.distance}` : ''}
                      </span>
                    )}
                  </div>

                  {/* File entries */}
                  <div className="divide-y divide-gray-100 dark:divide-gray-700/50">
                    {group.files.map((filePath, fileIdx) => {
                      const isChecked = selectedPaths.has(filePath);
                      const isKept = !isChecked;
                      const fileName = getFileName(filePath);
                      const dirPath = getDirectory(filePath);
                      const fileSize = group.file_sizes?.[fileIdx];
                      // How much smaller this copy is than the largest in the
                      // group. The pair Ehud described as "clearly different"
                      // differed by a third; the pair that looked identical, by
                      // a rounding error.
                      const largest = group.file_sizes?.length ? Math.max(...group.file_sizes) : undefined;
                      const delta = fileSize != null && largest != null && largest !== fileSize
                        ? fileSize - largest
                        : null;
                      // Exact mode: one BLAKE3 for the whole group. Fuzzy mode:
                      // this file's own signature, which is what explains why it
                      // was clustered with the others.
                      const rowHash = group.file_hashes?.[fileIdx] ?? (group.similarity ? null : group.hash);

                      return (
                        <div
                          key={filePath}
                          className={`flex items-start gap-3 px-3 py-2 cursor-pointer transition-colors ${
                            isChecked
                              ? 'bg-red-50/50 dark:bg-red-900/10 hover:bg-red-50 dark:hover:bg-red-900/20'
                              : 'bg-green-50/50 dark:bg-green-900/10 hover:bg-green-50 dark:hover:bg-green-900/20'
                          }`}
                        >
                          {/* Every copy can be ticked, including the one the
                              policy kept. Which one survives is the user's
                              call; the guard below refuses only the case where
                              none of them does. */}
                          <div className="mt-1 shrink-0">
                            <Checkbox
                              checked={isChecked}
                              onChange={() => toggleFile(filePath)}
                            />
                          </div>

                          {/* A square of the file itself. Deciding which of two
                              near-identical pictures to keep from their names
                              and byte counts alone is guesswork (#347). Click to
                              open it full size in the preview, where AeroImage
                              can edit it. */}
                          {onPreviewFile && isPreviewableImage(fileName) ? (
                            <button
                              type="button"
                              onClick={(e) => { e.stopPropagation(); onPreviewFile(filePath); }}
                              title={t('contextMenu.preview')}
                              className="shrink-0 mt-0.5 rounded overflow-hidden border border-gray-200 dark:border-gray-600 hover:border-blue-400 transition-colors"
                            >
                              <ImageThumbnail
                                path={filePath}
                                name={fileName}
                                signature={fileSize != null ? signatureOf(fileSize, null) : null}
                                cacheScope="dedupe"
                                fallbackIcon={<div className="w-10 h-10" />}
                                className="w-10 h-10 object-contain bg-gray-50 dark:bg-gray-900"
                              />
                            </button>
                          ) : null}

                          {/* File info, each line with its own copy button */}
                          <div className="flex-1 min-w-0">
                            <div className="flex items-center gap-1 min-w-0">
                              <span className="text-sm font-medium text-gray-900 dark:text-gray-100 truncate" title={fileName}>
                                {fileName}
                              </span>
                              <RowCopyButton value={fileName} label={t('contextMenu.copyName') || 'Copy Name'} />
                              {fileSize != null && (
                                <span className="shrink-0 ml-auto text-[11px] tabular-nums text-gray-500 dark:text-gray-400">
                                  {formatBytes(fileSize)}
                                  {delta != null && (
                                    <span className="ml-1 text-gray-400 dark:text-gray-500">
                                      ({formatBytes(delta)})
                                    </span>
                                  )}
                                </span>
                              )}
                            </div>
                            <div className="flex items-center gap-1 min-w-0">
                              <span className="text-xs text-gray-500 dark:text-gray-400 truncate" title={dirPath}>
                                {dirPath}
                              </span>
                              <RowCopyButton value={dirPath} label={t('contextMenu.copyPath') || 'Copy Path'} />
                            </div>
                            {showHashes && rowHash && (
                              <div className="flex items-center gap-1 min-w-0">
                                <span
                                  className="font-mono text-[10px] text-gray-400 dark:text-gray-500 truncate"
                                  title={rowHash}
                                >
                                  {group.similarity ? `${group.similarity}: ` : ''}{shortHash(rowHash)}
                                </span>
                                <RowCopyButton value={rowHash} label={t('common.copy') || 'Copy'} />
                              </div>
                            )}
                          </div>

                          {/* Open the file, or its folder, in the desktop's own
                              apps: comparing two candidates is what the user is
                              here to do, and it cannot be done in this dialog. */}
                          <button
                            type="button"
                            onClick={(e) => {
                              e.stopPropagation();
                              invoke('open_local_file', { path: filePath }).catch((err) => {
                                setError(err instanceof Error ? err.message : String(err));
                              });
                            }}
                            title={t('contextMenu.open') || 'Open'}
                            aria-label={t('contextMenu.open') || 'Open'}
                            className="shrink-0 mt-0.5 p-1 rounded text-gray-400 hover:text-blue-500 hover:bg-gray-100 dark:hover:bg-gray-700 transition-colors"
                          >
                            <ExternalLink size={12} />
                          </button>
                          <button
                            type="button"
                            onClick={(e) => {
                              e.stopPropagation();
                              // the file, not its folder: the command reveals and
                              // selects it, which is the useful thing here.
                              invoke('open_in_file_manager', { path: filePath }).catch((err) => {
                                setError(err instanceof Error ? err.message : String(err));
                              });
                            }}
                            title={t('aeroShare.inbox.revealFile') || 'Show in folder'}
                            aria-label={t('aeroShare.inbox.revealFile') || 'Show in folder'}
                            className="shrink-0 mt-0.5 p-1 rounded text-gray-400 hover:text-blue-500 hover:bg-gray-100 dark:hover:bg-gray-700 transition-colors"
                          >
                            <FolderOpen size={12} />
                          </button>

                          {/* Keep / delete badge */}
                          <span
                            className={`shrink-0 mt-0.5 px-2 py-0.5 text-[10px] font-medium rounded ${
                              isKept
                                ? 'bg-green-100 dark:bg-green-900/40 text-green-700 dark:text-green-300'
                                : 'bg-red-100 dark:bg-red-900/40 text-red-700 dark:text-red-300'
                            }`}
                          >
                            {isChecked ? t('duplicates.delete') : t('duplicates.keep')}
                          </span>
                        </div>
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
                  onClick={selectAllButOne}
                  className="px-3 py-1.5 text-xs text-gray-700 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-700 rounded border border-gray-300 dark:border-gray-600"
                >
                  {t('duplicates.selectAllButOne')}
                </button>
                <button
                  onClick={selectAll}
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
                title={fullyTickedGroups.length > 0 ? t('duplicates.everyCopyTickedHint') : undefined}
                disabled={selectedCount === 0 || isDeleting || fullyTickedGroups.length > 0}
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

      {/* Styled confirmation dialog (replaces window.confirm) */}
      {pendingDeleteConfirm && (
        <div className={`fixed inset-0 ${MODAL_Z.modalConfirm} bg-black/50 flex items-center justify-center`} role="dialog" aria-modal="true">
          <div className="bg-white dark:bg-gray-800 rounded-lg p-6 shadow-2xl max-w-sm animate-scale-in">
            <p className="text-gray-900 dark:text-gray-100 mb-4">
              {/* `deleteConfirm`, the key that exists. `duplicates.confirmDelete`
                  was in none of the 47 locales, so this line printed its own key
                  name at the user instead of a question (#537). */}
              {t('duplicates.deleteConfirm', { count: selectedPaths.size })}
            </p>
            <div className="flex justify-end gap-2">
              <button
                onClick={() => setPendingDeleteConfirm(false)}
                className="px-4 py-2 text-gray-600 dark:text-gray-400 hover:bg-gray-100 dark:hover:bg-gray-700 rounded-lg"
              >
                {t('common.cancel')}
              </button>
              <button
                onClick={runDelete}
                className="px-4 py-2 text-white rounded-lg bg-red-500 hover:bg-red-600"
              >
                {t('common.delete')}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
};

export default DuplicateFinderDialog;
