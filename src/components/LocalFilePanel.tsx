// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * LocalFilePanel: extracted from App.tsx
 *
 * Renders the complete local file panel: header (breadcrumb/address bar),
 * search bar, sidebar, and file views (list/grid/large-icons/trash).
 *
 * All state and business logic remain in App.tsx; this component is
 * a pure rendering extraction for maintainability.
 */

import React from 'react';
import {
  RefreshCw, Search, HardDrive, AlertTriangle, X, ClipboardList, FolderUp, Loader2,
  Copy, ArrowRightLeft,
} from 'lucide-react';
import { BreadcrumbBar } from './BreadcrumbBar';
import { PlacesSidebar } from './PlacesSidebar';
import { SortField, SortOrder } from './SortableHeader';
import { AeroFileTableHeader } from './AeroFileTableHeader';
import type { AeroFileLocalColId, AeroFileLocalTableColumns } from '../hooks/useAeroFileTableColumns';
import { LargeIconsGrid } from './LargeIconsGrid';
import { ImageThumbnail } from './ImageThumbnail';
import { getPreviewCategory, isPreviewable as isMediaPreviewable } from './Preview';
import { isPreviewable } from './DevTools';
import { formatBytes, formatDate } from '../utils';
import { LocalFile } from '../types';
import type { ServerProfile } from '../types';
import type { TrashItem, FileTag, PanelEndpoint } from '../types/aerofile';
import { FileTagBadge } from './FileTagBadge';
import type { PanelKey } from '../hooks/useDragAndDrop';
import { PanelEndpointSelector } from './PanelEndpointSelector';

// ============================================================================
// Types
// ============================================================================

interface IconProvider {
  getFolderIcon: (size: number) => { icon: React.ReactNode; color: string };
  getFileIcon: (name: string, size?: number) => { icon: React.ReactNode; color: string };
  getFolderUpIcon: (size: number) => { icon: React.ReactNode; color: string };
}

export interface LocalFilePanelProps {
  // --- Mode & Layout ---
  isAeroFileMode: boolean;
  isConnected: boolean;
  isDualMode?: boolean;
  /**
   * Identifies this local panel. AeroFile dual-panel mode uses 'local' for the
   * primary panel (left) and 'local2' for the secondary panel (right).
   * Defaults to 'local' for back-compat with single-panel layouts.
   */
  panelKey?: 'local' | 'local2';
  /** True when this local panel has keyboard / drag focus in dual-panel mode. */
  isFocused?: boolean;
  /** Fired when the panel root gets pointer/keyboard focus. */
  onPanelFocus?: () => void;
  /** Inline style applied to the panel root: used by AeroFile dual-panel for resizable flex sizing. */
  style?: React.CSSProperties;
  endpointSelector?: {
    endpoint: PanelEndpoint;
    savedProfiles: ServerProfile[];
    compactLabel?: string;
    onChooseLocalFolder: () => void;
    onChooseRemoteProfile: (profile: ServerProfile) => void;
  };
  /**
   * Cross-panel transfer affordance (Slice B/C, issue #162).
   *
   * When the OPPOSITE local panel has an active selection, the header
   * surfaces an explicit "Copy →" / "Move →" pair of chips that opens
   * the unified transfer plan dialog with this panel as destination. The
   * affordance keeps the F5 / F6 / right-click / drag triggers
   * discoverable without requiring keyboard shortcut knowledge.
   *
   * Pass `null` (or omit) when no selection on the other panel: the
   * chips hide entirely so they never distract from single-panel work.
   */
  crossPanelTransfer?: {
    selectionCount: number;
    onCopyHere: () => void;
    onMoveHere: () => void;
  } | null;

  // --- Navigation ---
  currentPath: string;
  setCurrentPath: (path: string) => void;
  onNavigate: (path: string) => void;
  onRefresh: (path: string) => void;
  isPathCoherent: boolean;
  isSyncPathMismatch: boolean;
  isSyncNavigation: boolean;
  syncBasePaths: { remote: string; local: string } | null;
  /** Spinner overlay during a directory drill-in (issue #178 #2). */
  isLoading?: boolean;

  // --- Files ---
  localFiles: LocalFile[];
  sortedFiles: LocalFile[];

  // --- Selection ---
  selectedFiles: Set<string>;
  setSelectedFiles: React.Dispatch<React.SetStateAction<Set<string>>>;
  lastSelectedIndex: number | null;
  setLastSelectedIndex: (i: number | null) => void;
  setActivePanel: (panel: 'remote' | 'local') => void;
  setPreviewFile: (file: LocalFile | null) => void;

  // --- Sort & columns (Phase 5: lifted to useAeroFileLocalColumns hook) ---
  /**
   * Optional sort metadata. Kept as a back-compat prop so callers that still
   * own their own sort state can pass it through. When `localColumns` is
   * provided, the hook config is the source of truth and these are ignored.
   */
  sortField?: SortField;
  sortOrder?: SortOrder;
  onSort?: (field: SortField) => void;
  /** New unified columns hook result (visibility + order + widths + sort). */
  localColumns: AeroFileLocalTableColumns;

  // --- Search ---
  searchFilter: string;
  setSearchFilter: (f: string) => void;
  showSearchBar: boolean;
  setShowSearchBar: React.Dispatch<React.SetStateAction<boolean>>;
  searchRef: React.RefObject<HTMLInputElement>;
  /** T-FLATTEN-DESCENDANTS: true when `*` / `**` is in the search box. */
  flattenActive?: boolean;
  flattenScanning?: boolean;
  flattenTruncated?: boolean;
  flattenCount?: number;

  // --- View & Display ---
  viewMode: 'list' | 'grid' | 'large';
  showFileExtensions: boolean;
  debugMode: boolean;
  doubleClickAction: string;
  className?: string;

  // --- Inline Rename ---
  inlineRename: { path: string; name: string; isRemote: boolean } | null;
  inlineRenameValue: string;
  setInlineRenameValue: (v: string) => void;
  inlineRenameRef: React.RefObject<HTMLInputElement>;
  onInlineRenameKeyDown: (e: React.KeyboardEvent) => void;
  onInlineRenameCommit: () => void;
  onInlineRenameStart: (path: string, name: string, isRemote: boolean) => void;
  onInlineRenameCancel: () => void;

  // --- Drag & Drop ---
  onDragStart: (e: React.DragEvent, file: LocalFile, panelKey: PanelKey, selectedFiles: Set<string>, sortedFiles: LocalFile[]) => void;
  onDragEnd: () => void;
  onDragOver: (e: React.DragEvent, path: string, isDir: boolean, panelKey: PanelKey) => void;
  onDragLeave: (e: React.DragEvent) => void;
  onDrop: (e: React.DragEvent, path: string, panelKey: PanelKey) => void;
  dropTargetPath: string | null;
  dragSourcePaths: string[];
  crossPanelTarget: string | null;
  onPanelDragOver: (e: React.DragEvent, panelKey: PanelKey) => void;
  onPanelDrop: (e: React.DragEvent, panelKey: PanelKey) => void;
  onPanelDragLeave: (e: React.DragEvent) => void;

  // --- Context Menu ---
  onContextMenu: (e: React.MouseEvent, file: LocalFile) => void;
  onEmptyContextMenu: (e: React.MouseEvent) => void;

  // --- File Actions ---
  onOpenUniversalPreview: (file: LocalFile, isRemote: boolean) => void;
  onOpenDevToolsPreview: (file: LocalFile, isRemote: boolean) => void;
  onUploadFile: (path: string, name: string, isFolder: boolean) => void;
  onOpenInFileManager: (path: string) => void;

  // --- Trash ---
  isTrashView: boolean;
  trashItems: TrashItem[];
  onEmptyTrash: () => void;
  onRestoreTrashItem: (item: TrashItem) => void;
  onNavigateTrash: () => void;

  // --- Sidebar ---
  showSidebar: boolean;
  sidebarCurrentPath?: string;
  sidebarOnNavigate?: (path: string) => void;
  /** L / R marker rendered in the PlacesSidebar header when dual mode is on,
   * so the user can tell which local panel the sidebar will drive on click. */
  sidebarActivePanelMarker?: 'L' | 'R';
  recentPaths: string[];
  setRecentPaths: React.Dispatch<React.SetStateAction<string[]>>;

  // --- Tags ---
  getTagsForFile: (path: string) => FileTag[];
  labelCounts: import('../types/aerofile').LabelCount[];
  activeTagFilter: number | null;
  onTagFilter: (labelId: number | null) => void;

  // --- Helpers ---
  iconProvider: IconProvider;
  displayName: (name: string, isDir: boolean) => string;
  getSyncBadge: (filePath: string, fileModified: string | undefined, isLocal: boolean) => React.ReactNode;
  t: (key: string, params?: Record<string, string | number>) => string;
  notify: { success: (title: string, message: string) => void };
}

// ============================================================================
// Helpers
// ============================================================================

const isImageFile = (name: string) =>
  /\.(jpg|jpeg|png|gif|svg|webp|bmp|ico)$/i.test(name);

// ============================================================================
// Component
// ============================================================================

export const LocalFilePanel: React.FC<LocalFilePanelProps> = ({
  isAeroFileMode,
  isConnected,
  isDualMode = false,
  panelKey = 'local',
  isFocused = false,
  onPanelFocus,
  style,
  endpointSelector,
  crossPanelTransfer,
  currentPath,
  setCurrentPath,
  onNavigate,
  onRefresh,
  isPathCoherent,
  isSyncPathMismatch,
  isSyncNavigation,
  syncBasePaths,
  isLoading,
  localFiles,
  sortedFiles,
  selectedFiles,
  setSelectedFiles,
  lastSelectedIndex,
  setLastSelectedIndex,
  setActivePanel,
  setPreviewFile,
  localColumns,
  searchFilter,
  setSearchFilter,
  showSearchBar,
  setShowSearchBar,
  searchRef,
  flattenActive,
  flattenScanning,
  flattenTruncated,
  flattenCount,
  viewMode,
  showFileExtensions,
  debugMode,
  doubleClickAction,
  className: extraClassName,
  inlineRename,
  inlineRenameValue,
  setInlineRenameValue,
  inlineRenameRef,
  onInlineRenameKeyDown,
  onInlineRenameCommit,
  onInlineRenameStart,
  onInlineRenameCancel,
  onDragStart,
  onDragEnd,
  onDragOver,
  onDragLeave,
  onDrop,
  dropTargetPath,
  dragSourcePaths,
  crossPanelTarget,
  onPanelDragOver,
  onPanelDrop,
  onPanelDragLeave,
  onContextMenu,
  onEmptyContextMenu,
  onOpenUniversalPreview,
  onOpenDevToolsPreview,
  onUploadFile,
  onOpenInFileManager,
  isTrashView,
  trashItems,
  onEmptyTrash,
  onRestoreTrashItem,
  onNavigateTrash,
  showSidebar,
  sidebarCurrentPath,
  sidebarOnNavigate,
  sidebarActivePanelMarker,
  recentPaths,
  setRecentPaths,
  getTagsForFile,
  labelCounts,
  activeTagFilter,
  onTagFilter,
  iconProvider,
  displayName,
  getSyncBadge,
  t,
  notify,
}) => {
  // Navigate to parent directory
  const navigateUp = () => {
    const parent = currentPath.split(/[\\/]/).slice(0, -1).join('/') || '/';
    onNavigate(parent);
  };

  // Handle file double-click
  const handleDoubleClick = (file: LocalFile) => {
    if (file.is_dir) {
      onNavigate(file.path);
    } else if (doubleClickAction === 'preview') {
      const category = getPreviewCategory(file.name);
      if (['image', 'audio', 'video', 'pdf', 'markdown', 'text'].includes(category)) {
        onOpenUniversalPreview(file, false);
      } else if (isPreviewable(file.name)) {
        onOpenDevToolsPreview(file, false);
      }
    } else {
      if (isConnected) {
        onUploadFile(file.path, file.name, false);
      } else {
        onOpenInFileManager(file.path);
      }
    }
  };

  // Handle file click (selection logic)
  const handleFileClick = (e: React.MouseEvent, file: LocalFile, index: number) => {
    if (file.name === '..') return;
    setActivePanel('local');
    if (e.shiftKey && lastSelectedIndex !== null) {
      const start = Math.min(lastSelectedIndex, index);
      const end = Math.max(lastSelectedIndex, index);
      const rangeNames = sortedFiles.slice(start, end + 1).map(f => f.name);
      setSelectedFiles(new Set(rangeNames));
    } else if (e.ctrlKey || e.metaKey) {
      setSelectedFiles(prev => {
        const next = new Set(prev);
        if (next.has(file.name)) next.delete(file.name);
        else next.add(file.name);
        return next;
      });
      setLastSelectedIndex(index);
    } else {
      if (selectedFiles.size === 1 && selectedFiles.has(file.name)) {
        setSelectedFiles(new Set());
        setPreviewFile(null);
      } else {
        setSelectedFiles(new Set([file.name]));
        setPreviewFile(file);
      }
      setLastSelectedIndex(index);
    }
  };

  // Refresh with spin animation
  const handleRefreshClick = (e: React.MouseEvent<HTMLButtonElement>) => {
    const btn = e.currentTarget;
    btn.querySelector('svg')?.classList.add('animate-spin');
    setTimeout(() => btn.querySelector('svg')?.classList.remove('animate-spin'), 600);
    onRefresh(currentPath);
  };

  // Toggle search bar
  const handleSearchToggle = () => {
    if (searchFilter) {
      setSearchFilter('');
      setShowSearchBar(false);
    } else {
      setShowSearchBar(prev => !prev);
    }
  };

  const isAtRoot = currentPath === '/' || !!(
    isSyncNavigation && syncBasePaths && (
      (currentPath.endsWith('/') && currentPath.length > 1 ? currentPath.slice(0, -1) : currentPath) ===
      (syncBasePaths.local.endsWith('/') && syncBasePaths.local.length > 1 ? syncBasePaths.local.slice(0, -1) : syncBasePaths.local)
    )
  );

  // Focus indication moves to a discrete L/R chip in the header bar plus
  // the StatusBar marker and the PlacesSidebar header, instead of a tinted
  // ring around the whole panel: the ring was getting confused with the
  // drag-over highlight and added visual noise to a panel that already has
  // a path bar and a toolbar above the file list.
  const crossPanelRingClass = crossPanelTarget === panelKey
    ? 'ring-2 ring-inset ring-blue-400 bg-blue-50/30 dark:bg-blue-900/10'
    : '';
  return (
    <div
      role="region"
      aria-label={panelKey === 'local2' ? 'Local files (right panel)' : 'Local files'}
      className={`relative ${isDualMode ? 'min-w-0' : isAeroFileMode ? 'flex-1 min-w-0' : 'w-1/2'} min-h-0 flex flex-col ${crossPanelRingClass}${extraClassName ? ` ${extraClassName}` : ''}`}
      style={style}
      onDragOver={(e) => onPanelDragOver(e, panelKey)}
      onDrop={(e) => onPanelDrop(e, panelKey)}
      onDragLeave={onPanelDragLeave}
      onMouseDown={onPanelFocus}
    >
      {/* Drill-in spinner overlay (issue #178 #2). Debounced via CSS animation
          so fast listings (<250 ms) never reveal the spinner. Soft background,
          no blur, smaller icon to keep the affordance non-invasive. */}
      {isLoading && (
        <div
          className="absolute inset-0 z-20 flex items-center justify-center bg-white/10 dark:bg-gray-900/10 pointer-events-none animate-fade-in-delayed"
          aria-hidden="true"
        >
          <Loader2 size={20} className="animate-spin text-blue-500/80" />
        </div>
      )}
      {/* Header: BreadcrumbBar (AeroFile) or Address Bar (Connected) */}
      <div className="px-3 py-1.5 bg-gray-100 dark:bg-gray-700 border-b border-gray-200 dark:border-gray-600 text-sm font-medium flex items-center gap-2">
        {/* Dual-panel L/R marker. Shown only in dual mode, sits before the
            path bar so the user always sees which panel they are pointing
            at without leaving the toolbar area. Mirrors the StatusBar and
            the PlacesSidebar header markers (same letter, same accent). */}
        {isDualMode && (
          <span
            aria-label={panelKey === 'local2' ? 'Right panel' : 'Left panel'}
            title={panelKey === 'local2' ? 'Right panel' : 'Left panel'}
            className={`flex-shrink-0 inline-flex items-center justify-center w-5 h-5 rounded text-[10px] font-bold tracking-wider transition-opacity ${
              isFocused ? 'opacity-100' : 'opacity-40'
            } ${
              panelKey === 'local2'
                ? 'bg-amber-100 text-amber-700 dark:bg-amber-900/40 dark:text-amber-300'
                : 'bg-blue-100 text-blue-700 dark:bg-blue-900/40 dark:text-blue-300'
            }`}
          >
            {panelKey === 'local2' ? 'R' : 'L'}
          </span>
        )}
        {endpointSelector && (
          <PanelEndpointSelector
            endpoint={endpointSelector.endpoint}
            savedProfiles={endpointSelector.savedProfiles}
            compactLabel={endpointSelector.compactLabel}
            onChooseLocalFolder={endpointSelector.onChooseLocalFolder}
            onChooseRemoteProfile={endpointSelector.onChooseRemoteProfile}
          />
        )}
        {crossPanelTransfer && crossPanelTransfer.selectionCount > 0 && (
          // Discoverable trigger for the unified planner: appears only
          // when the opposite local panel has an active selection, and
          // surfaces "Copy here" / "Move here" as one-click counterparts
          // to F5 / F6. Closes the gap reported on the endpoint selector
          // where remote-profile picks would silently auto-open the plan
          // dialog while local-folder picks would not, leaving the
          // local-to-local path with no menu-driven trigger.
          <div className="flex-shrink-0 inline-flex items-center gap-1">
            <button
              type="button"
              onClick={crossPanelTransfer.onCopyHere}
              className="inline-flex items-center gap-1 px-2 py-0.5 rounded text-xs font-medium bg-blue-100 hover:bg-blue-200 text-blue-700 dark:bg-blue-900/40 dark:hover:bg-blue-900/60 dark:text-blue-300 transition-colors"
              title={t('aerofile.copyHereFromOtherPanelTooltip', { count: crossPanelTransfer.selectionCount })}
            >
              <Copy size={12} />
              <span>{t('aerofile.copyHere', { count: crossPanelTransfer.selectionCount })}</span>
            </button>
            <button
              type="button"
              onClick={crossPanelTransfer.onMoveHere}
              className="inline-flex items-center gap-1 px-2 py-0.5 rounded text-xs font-medium bg-amber-100 hover:bg-amber-200 text-amber-700 dark:bg-amber-900/40 dark:hover:bg-amber-900/60 dark:text-amber-300 transition-colors"
              title={t('aerofile.moveHereFromOtherPanelTooltip', { count: crossPanelTransfer.selectionCount })}
            >
              <ArrowRightLeft size={12} />
              <span>{t('aerofile.moveHere', { count: crossPanelTransfer.selectionCount })}</span>
            </button>
          </div>
        )}
        {isAeroFileMode ? (
          <div className="flex-1 flex items-center gap-1.5 min-w-0">
            <div className="flex-1 min-w-0">
              <BreadcrumbBar
                currentPath={currentPath}
                onNavigate={onNavigate}
                isCoherent={isPathCoherent}
                minPath={isSyncNavigation && syncBasePaths ? syncBasePaths.local : undefined}
                t={t}
              />
            </div>
            {(() => {
              const upDisabled = !currentPath || currentPath === '/' || /^[A-Za-z]:[\\/]?$/.test(currentPath);
              return (
                <button
                  onClick={() => !upDisabled && navigateUp()}
                  disabled={upDisabled}
                  className={`flex-shrink-0 p-1.5 rounded transition-colors ${upDisabled ? 'text-gray-300 dark:text-gray-600 cursor-not-allowed' : 'text-gray-400 hover:text-gray-600 dark:hover:text-gray-300 hover:bg-gray-200 dark:hover:bg-gray-600'}`}
                  title={t('common.up')}
                  aria-label={t('common.up')}
                >
                  <FolderUp size={13} />
                </button>
              );
            })()}
            <button
              onClick={handleRefreshClick}
              className="flex-shrink-0 p-1.5 rounded text-gray-400 hover:text-gray-600 dark:hover:text-gray-300 hover:bg-gray-200 dark:hover:bg-gray-600 transition-colors"
              title={t('common.refresh')}
            >
              <RefreshCw size={13} />
            </button>
            <button
              onClick={handleSearchToggle}
              className={`flex-shrink-0 p-1.5 rounded transition-colors ${searchFilter ? 'text-blue-500' : 'text-gray-400 hover:text-gray-600 dark:hover:text-gray-300 hover:bg-gray-200 dark:hover:bg-gray-600'}`}
              title={searchFilter ? t('search.clear') || 'Clear search' : t('search.search_files') || 'Search files'}
            >
              <Search size={13} />
            </button>
          </div>
        ) : (
          <>
            <div className={`flex-1 flex items-center bg-white dark:bg-gray-800 rounded-lg border ${(!isPathCoherent || isSyncPathMismatch) ? 'border-amber-400 dark:border-amber-500' : 'border-gray-300 dark:border-gray-600 hover:border-blue-400 dark:hover:border-blue-500'} focus-within:border-blue-500 dark:focus-within:border-blue-400 focus-within:ring-2 focus-within:ring-blue-500/20 transition-all overflow-hidden`}>
              <div
                className="flex-shrink-0 pl-2.5 pr-1 flex items-center"
                title={isSyncPathMismatch ? t('browser.syncPathMismatch') : isPathCoherent ? "Local Disk" : "Local path doesn't match the connected server"}
              >
                {(!isPathCoherent || isSyncPathMismatch) ? (
                  <AlertTriangle size={14} className="text-amber-500" />
                ) : (
                  <HardDrive size={14} className={isSyncNavigation ? 'text-purple-500' : 'text-blue-500'} />
                )}
              </div>
              <input
                type="text"
                value={currentPath}
                onChange={(e) => setCurrentPath(e.target.value)}
                onKeyDown={(e) => e.key === 'Enter' && onNavigate((e.target as HTMLInputElement).value)}
                className={`flex-1 pl-1 pr-2 py-1 bg-transparent border-none outline-none text-sm cursor-text selection:bg-blue-200 dark:selection:bg-blue-800 ${(!isPathCoherent || isSyncPathMismatch) ? 'text-amber-600 dark:text-amber-400' : ''}`}
                title={isSyncPathMismatch ? t('browser.syncPathMismatch') : isPathCoherent ? t('browser.editPathHint') : `\u26a0\ufe0f ${t('browser.localPathMismatch')}`}
                placeholder="/path/to/local/directory"
              />
            </div>
            {(() => {
              const upDisabled = !currentPath || currentPath === '/' || /^[A-Za-z]:[\\/]?$/.test(currentPath);
              return (
                <button
                  onClick={() => !upDisabled && navigateUp()}
                  disabled={upDisabled}
                  className={`flex-shrink-0 p-1.5 rounded transition-colors ${upDisabled ? 'text-gray-300 dark:text-gray-600 cursor-not-allowed' : 'text-gray-400 hover:text-gray-600 dark:hover:text-gray-300 hover:bg-gray-200 dark:hover:bg-gray-600'}`}
                  title={t('common.up')}
                  aria-label={t('common.up')}
                >
                  <FolderUp size={13} />
                </button>
              );
            })()}
            <button
              onClick={handleRefreshClick}
              className="flex-shrink-0 p-1.5 rounded text-gray-400 hover:text-gray-600 dark:hover:text-gray-300 hover:bg-gray-200 dark:hover:bg-gray-600 transition-colors"
              title={t('common.refresh')}
            >
              <RefreshCw size={13} />
            </button>
            <button
              onClick={handleSearchToggle}
              className={`flex-shrink-0 p-1.5 rounded transition-colors ${searchFilter ? 'text-blue-500' : 'text-gray-400 hover:text-gray-600 dark:hover:text-gray-300 hover:bg-gray-200 dark:hover:bg-gray-600'}`}
              title={searchFilter ? t('search.clear') || 'Clear search' : t('search.search_files') || 'Search files'}
            >
              <Search size={13} />
            </button>
            {debugMode && (
              <button
                onClick={() => {
                  const lines = sortedFiles.map(f =>
                    `${f.is_dir ? 'd' : '-'}\t${f.size}\t${f.modified || ''}\t${f.name}`
                  );
                  const header = `# Local files: ${currentPath} (${sortedFiles.length} entries)\n# type\tsize\tmodified\tname`;
                  navigator.clipboard.writeText(header + '\n' + lines.join('\n'));
                  notify.success(t('debug.title'), t('debug.filesCopied', { count: sortedFiles.length }));
                }}
                className="flex-shrink-0 p-1.5 rounded text-amber-500 hover:text-amber-600 dark:hover:text-amber-400 hover:bg-gray-200 dark:hover:bg-gray-600 transition-colors"
                title={t('debug.copyFileListToClipboard')}
              >
                <ClipboardList size={13} />
              </button>
            )}
          </>
        )}
      </div>

      {/* Search Bar */}
      {showSearchBar && (
        <div className={`px-3 py-1.5 border-b flex items-center gap-2 ${
          flattenActive
            ? 'bg-purple-50 dark:bg-purple-900/20 border-purple-200 dark:border-purple-800'
            : 'bg-blue-50 dark:bg-blue-900/20 border-blue-200 dark:border-blue-800'
        }`}>
          <Search size={14} className={flattenActive ? 'text-purple-500 flex-shrink-0' : 'text-blue-500 flex-shrink-0'} />
          <input
            autoFocus
            ref={searchRef}
            type="text"
            placeholder={t('search.local_placeholder_flatten') || t('search.local_placeholder') || 'Filter local files... (* for recursive)'}
            value={searchFilter}
            onChange={e => setSearchFilter(e.target.value)}
            onKeyDown={e => {
              if (e.key === 'Escape') {
                setShowSearchBar(false);
                setSearchFilter('');
              }
            }}
            className="flex-1 text-sm bg-transparent border-none outline-none placeholder-gray-400"
          />
          {flattenActive ? (
            <span className="text-xs text-purple-600 dark:text-purple-400 flex-shrink-0 flex items-center gap-1">
              {flattenScanning ? (
                <span className="flex items-center gap-1">
                  <RefreshCw size={11} className="animate-spin" />
                  {t('search.flattenScanning') || 'Scanning subtree...'}
                </span>
              ) : (
                <>
                  {t('search.flattenRecursive') || 'Recursive'}
                  {typeof flattenCount === 'number' && (
                    <span className="opacity-80"> · {t('search.resultsCount', { count: flattenCount })}</span>
                  )}
                  {flattenTruncated && (
                    <span className="ml-1 text-amber-600 dark:text-amber-400" title={t('search.flattenTruncatedHint') || 'Hit the 5,000 entry cap; refine your filter or descend into a subfolder.'}>
                      ({t('search.flattenTruncated') || 'truncated'})
                    </span>
                  )}
                </>
              )}
            </span>
          ) : (
            searchFilter && (
              <span className="text-xs text-blue-600 dark:text-blue-400 flex-shrink-0">
                {t('search.resultsCount', { count: localFiles.filter(f => f.name.toLowerCase().includes(searchFilter.toLowerCase())).length })}
              </span>
            )
          )}
          <button
            onClick={() => { setShowSearchBar(false); setSearchFilter(''); }}
            className="text-gray-400 hover:text-gray-600 dark:hover:text-gray-300 flex-shrink-0"
          >
            <X size={14} />
          </button>
        </div>
      )}

      {/* Sidebar + Content */}
      <div className="flex flex-1 overflow-hidden">
        {/* AeroFile Places Sidebar */}
        {showSidebar && isAeroFileMode && (
          <PlacesSidebar
            currentPath={sidebarCurrentPath ?? currentPath}
            onNavigate={sidebarOnNavigate ?? onNavigate}
            t={t}
            recentPaths={recentPaths}
            onClearRecent={() => setRecentPaths([])}
            onRemoveRecent={(path) => setRecentPaths(prev => prev.filter(p => p !== path))}
            isTrashView={isTrashView}
            onNavigateTrash={onNavigateTrash}
            labelCounts={labelCounts}
            activeTagFilter={activeTagFilter}
            onTagFilter={onTagFilter}
            activePanelMarker={sidebarActivePanelMarker}
          />
        )}
        <div className="flex-1 overflow-auto" onContextMenu={(e) => {
          const target = e.target as HTMLElement;
          const isFileRow = target.closest('tr[data-file-row]') || target.closest('[data-file-card]');
          if (!isFileRow) onEmptyContextMenu(e);
        }}>
        {isTrashView ? (
          /* ===================== TRASH VIEW ===================== */
          <div className="flex-1 overflow-auto">
            <div className="flex items-center gap-2 px-4 py-2 border-b border-gray-200 dark:border-gray-700">
              <span className="text-sm font-medium text-gray-700 dark:text-gray-300">
                {t('trash.title')}: {t('trash.itemCount', { count: trashItems.length })}
              </span>
              <div className="flex-1" />
              {trashItems.length > 0 && (
                <button
                  onClick={onEmptyTrash}
                  className="px-3 py-1 text-xs bg-red-500 text-white rounded hover:bg-red-600 transition-colors"
                >
                  {t('trash.empty')}
                </button>
              )}
            </div>

            {trashItems.length === 0 ? (
              <div className="flex items-center justify-center py-12 text-gray-500 text-sm">
                {t('trash.emptyTrash')}
              </div>
            ) : (
              <table className="w-full text-sm">
                <thead>
                  <tr className="text-xs text-gray-500 dark:text-gray-400 border-b border-gray-200 dark:border-gray-700">
                    <th className="text-left px-4 py-2 font-medium">{t('browser.name')}</th>
                    <th className="text-left px-4 py-2 font-medium">{t('trash.originalPath')}</th>
                    <th className="text-right px-4 py-2 font-medium">{t('browser.size')}</th>
                    <th className="text-left px-4 py-2 font-medium">{t('trash.deletedAt')}</th>
                    <th className="text-center px-4 py-2 font-medium">{t('common.actions')}</th>
                  </tr>
                </thead>
                <tbody>
                  {trashItems.map((item) => (
                    <tr key={item.id} className="border-b border-gray-100 dark:border-gray-800 hover:bg-gray-50 dark:hover:bg-gray-800/50">
                      <td className="px-4 py-2 flex items-center gap-2">
                        {item.is_dir ? iconProvider.getFolderIcon(16).icon : iconProvider.getFileIcon(item.name, 16).icon}
                        <span className="truncate">{item.name}</span>
                      </td>
                      <td className="px-4 py-2 text-gray-500 text-xs truncate max-w-[200px]" title={item.original_path}>
                        {item.original_path}
                      </td>
                      <td className="px-4 py-2 text-right text-gray-500">
                        {item.is_dir ? '\u2014' : formatBytes(item.size)}
                      </td>
                      <td className="px-4 py-2 text-gray-500 text-xs">
                        {item.deleted_at ? new Date(item.deleted_at).toLocaleString() : '\u2014'}
                      </td>
                      <td className="px-4 py-2 text-center">
                        <button
                          onClick={() => onRestoreTrashItem(item)}
                          className="px-2 py-0.5 text-xs bg-blue-500 text-white rounded hover:bg-blue-600 transition-colors"
                        >
                          {t('trash.restore')}
                        </button>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
          </div>
        ) : viewMode === 'list' ? (
          /* ===================== LIST VIEW ===================== */
          (() => {
            const visibility = localColumns.config.visibility;
            const orderedVisible = localColumns.orderedVisibleColumns;
            const orderedExtras = orderedVisible.filter(c => c.id !== 'name');
            const colRefs: Record<string, HTMLTableColElement | null> = {};
            const handleLiveResize = (id: AeroFileLocalColId, w: number) => {
              const el = colRefs[id];
              if (el) el.style.width = `${w}px`;
            };
            const renderEmptyExtra = (id: AeroFileLocalColId) => {
              const cls = id === 'type' ? 'hidden xl:table-cell px-3 py-2 text-sm text-gray-400' : 'px-4 py-2 text-sm text-gray-400';
              return <td key={id} className={cls}>-</td>;
            };
            const renderFileExtra = (id: AeroFileLocalColId, file: LocalFile) => {
              switch (id) {
                case 'size':
                  return <td key="size" className="px-4 py-2 text-sm text-gray-500">{file.size !== null ? (!file.is_dir && file.size === 0 ? <span title={t('toast.zeroByteWarning')}>&#9888; 0 B</span> : formatBytes(file.size)) : '-'}</td>;
                case 'type':
                  return <td key="type" className="hidden xl:table-cell px-3 py-2 text-xs text-gray-500 uppercase">{file.is_dir ? t('browser.folderType') : (file.name.includes('.') ? file.name.split('.').pop() : '-')}</td>;
                case 'modified':
                  return <td key="modified" className="px-4 py-2 text-xs text-gray-500 whitespace-nowrap">{formatDate(file.modified)}</td>;
                default:
                  return null;
              }
            };
            return (
          <table className="w-full text-sm" role="grid" aria-label={t('browser.name')} style={{ tableLayout: 'fixed' }}>
            <colgroup>
              {orderedVisible.map((col) => (
                <col
                  key={col.id}
                  ref={(el) => { colRefs[col.id] = el; }}
                  style={{ width: `${localColumns.config.widths[col.id]}px` }}
                />
              ))}
            </colgroup>
            <AeroFileTableHeader<AeroFileLocalColId>
              columns={localColumns}
              onLiveResize={handleLiveResize}
              columnHeaderClassName={(id) => id === 'type' ? 'hidden xl:table-cell' : undefined}
            />
            <tbody className="divide-y divide-gray-100 dark:divide-gray-700" role="rowgroup">
              {/* Go Up Row */}
              <tr
                role="row"
                className={`${currentPath !== '/' ? 'hover:bg-gray-50 dark:hover:bg-gray-700/50 cursor-pointer' : 'opacity-50 cursor-not-allowed'}`}
                onClick={() => currentPath !== '/' && navigateUp()}
              >
                <td className="px-4 py-2 flex items-center gap-2 text-gray-500">
                  {iconProvider.getFolderUpIcon(16).icon}
                  <span className="italic">{t('browser.parentFolder')}</span>
                </td>
                {orderedExtras.map((c) => visibility[c.id] ? renderEmptyExtra(c.id) : null)}
              </tr>
              {sortedFiles.map((file, i) => (
                <tr
                  key={`${file.name}-${i}`}
                  data-file-row
                  role="row"
                  aria-selected={selectedFiles.has(file.name)}
                  draggable={file.name !== '..' && inlineRename?.path !== file.path}
                  onDragStart={(e) => onDragStart(e, file, panelKey, selectedFiles, sortedFiles)}
                  onDragEnd={onDragEnd}
                  onDragOver={(e) => onDragOver(e, file.path, file.is_dir, panelKey)}
                  onDragLeave={onDragLeave}
                  onDrop={(e) => file.is_dir && onDrop(e, file.path, panelKey)}
                  onClick={(e) => handleFileClick(e, file, i)}
                  onDoubleClick={() => handleDoubleClick(file)}
                  onContextMenu={(e: React.MouseEvent) => onContextMenu(e, file)}
                  className={`cursor-pointer transition-colors ${
                    dropTargetPath === file.path && file.is_dir
                      ? 'bg-green-100 dark:bg-green-900/40 ring-2 ring-green-500'
                      : selectedFiles.has(file.name)
                        ? 'bg-blue-100 dark:bg-blue-900/40'
                        : 'hover:bg-blue-50 dark:hover:bg-gray-700'
                  } ${dragSourcePaths.includes(file.path) ? 'opacity-50' : ''}`}
                >
                  <td className="px-4 py-2 flex items-center gap-2">
                    {file.is_dir ? iconProvider.getFolderIcon(16).icon : iconProvider.getFileIcon(file.name, 16).icon}
                    {inlineRename?.path === file.path && !inlineRename?.isRemote ? (
                      <input
                        ref={inlineRenameRef}
                        type="text"
                        value={inlineRenameValue}
                        onChange={(e) => setInlineRenameValue(e.target.value)}
                        onKeyDown={onInlineRenameKeyDown}
                        onBlur={onInlineRenameCommit}
                        onClick={(e) => e.stopPropagation()}
                        className="px-1 py-0.5 text-sm bg-white dark:bg-gray-900 border border-blue-500 rounded outline-none min-w-[120px]"
                      />
                    ) : (
                      <span
                        className="cursor-text"
                        onClick={(e) => {
                          if (selectedFiles.size === 1 && selectedFiles.has(file.name) && file.name !== '..') {
                            e.stopPropagation();
                            onInlineRenameStart(file.path, file.name, false);
                          }
                        }}
                      >
                        {displayName(file.name, file.is_dir)}
                      </span>
                    )}
                    <FileTagBadge tags={getTagsForFile(file.path)} />
                    {getSyncBadge(file.path, file.modified || undefined, true)}
                  </td>
                  {orderedExtras.map((c) => visibility[c.id] ? renderFileExtra(c.id, file) : null)}
                </tr>
              ))}
            </tbody>
          </table>
            );
          })()
        ) : viewMode === 'grid' ? (
          /* ===================== GRID VIEW ===================== */
          <div className="file-grid" role="grid" aria-label={t('browser.name')}>
            <div
              className={`file-grid-item file-grid-go-up ${currentPath === '/' ? 'opacity-50 cursor-not-allowed' : ''}`}
              role="row"
              onClick={() => currentPath !== '/' && navigateUp()}
            >
              <div className="file-grid-icon">
                {iconProvider.getFolderUpIcon(32).icon}
              </div>
              <span className="file-grid-name italic text-gray-500">{t('browser.goUp')}</span>
            </div>
            {sortedFiles.map((file, i) => (
              <div
                key={`${file.name}-${i}`}
                data-file-card
                role="row"
                aria-selected={selectedFiles.has(file.name)}
                draggable={file.name !== '..' && inlineRename?.path !== file.path}
                onDragStart={(e) => onDragStart(e, file, panelKey, selectedFiles, sortedFiles)}
                onDragEnd={onDragEnd}
                onDragOver={(e) => onDragOver(e, file.path, file.is_dir, panelKey)}
                onDragLeave={onDragLeave}
                onDrop={(e) => file.is_dir && onDrop(e, file.path, panelKey)}
                className={`file-grid-item ${
                  dropTargetPath === file.path && file.is_dir
                    ? 'ring-2 ring-green-500 bg-green-100 dark:bg-green-900/40'
                    : selectedFiles.has(file.name) ? 'selected' : ''
                } ${dragSourcePaths.includes(file.path) ? 'opacity-50' : ''}`}
                onClick={(e) => handleFileClick(e, file, i)}
                onDoubleClick={() => handleDoubleClick(file)}
                onContextMenu={(e: React.MouseEvent) => onContextMenu(e, file)}
              >
                {file.is_dir ? (
                  <div className="file-grid-icon">
                    {iconProvider.getFolderIcon(32).icon}
                  </div>
                ) : isImageFile(file.name) ? (
                  <ImageThumbnail
                    path={file.path}
                    name={file.name}
                    fallbackIcon={iconProvider.getFileIcon(file.name).icon}
                  />
                ) : (
                  <div className="file-grid-icon">
                    {iconProvider.getFileIcon(file.name).icon}
                  </div>
                )}
                {inlineRename?.path === file.path && !inlineRename?.isRemote ? (
                  <input
                    ref={inlineRenameRef}
                    type="text"
                    value={inlineRenameValue}
                    onChange={(e) => setInlineRenameValue(e.target.value)}
                    onKeyDown={onInlineRenameKeyDown}
                    onBlur={onInlineRenameCommit}
                    onClick={(e) => e.stopPropagation()}
                    className="file-grid-name px-1 bg-white dark:bg-gray-900 border border-blue-500 rounded outline-none text-center"
                  />
                ) : (
                  <span
                    className="file-grid-name cursor-text"
                    onClick={(e) => {
                      if (selectedFiles.size === 1 && selectedFiles.has(file.name) && file.name !== '..') {
                        e.stopPropagation();
                        onInlineRenameStart(file.path, file.name, false);
                      }
                    }}
                  >
                    {displayName(file.name, file.is_dir)}
                  </span>
                )}
                <FileTagBadge tags={getTagsForFile(file.path)} />
                {!file.is_dir && file.size !== null && file.size > 0 && (
                  <span className="file-grid-size">{formatBytes(file.size)}</span>
                )}
                {!file.is_dir && file.size === 0 && (
                  <span className="file-grid-size" title={t('toast.zeroByteWarning')}>&#9888; 0 B</span>
                )}
              </div>
            ))}
          </div>
        ) : (
          /* ===================== LARGE ICONS VIEW ===================== */
          <LargeIconsGrid
            files={sortedFiles}
            selectedFiles={selectedFiles}
            currentPath={currentPath}
            onFileClick={(file, e) => {
              setActivePanel('local');
              const idx = sortedFiles.indexOf(file);
              if (e.shiftKey && lastSelectedIndex !== null) {
                const start = Math.min(lastSelectedIndex, idx);
                const end = Math.max(lastSelectedIndex, idx);
                const rangeNames = sortedFiles.slice(start, end + 1).map(f => f.name);
                setSelectedFiles(new Set(rangeNames));
              } else if (e.ctrlKey || e.metaKey) {
                setSelectedFiles(prev => {
                  const next = new Set(prev);
                  if (next.has(file.name)) next.delete(file.name);
                  else next.add(file.name);
                  return next;
                });
                setLastSelectedIndex(idx);
              } else {
                if (selectedFiles.size === 1 && selectedFiles.has(file.name)) {
                  setSelectedFiles(new Set());
                  setPreviewFile(null);
                } else {
                  setSelectedFiles(new Set([file.name]));
                  setPreviewFile(file);
                }
                setLastSelectedIndex(idx);
              }
            }}
            onFileDoubleClick={handleDoubleClick}
            onNavigateUp={navigateUp}
            isAtRoot={isAtRoot}
            getFileIcon={(name, isDir) => {
              if (isDir) return iconProvider.getFolderIcon(64);
              return iconProvider.getFileIcon(name, 48);
            }}
            getFolderUpIcon={() => iconProvider.getFolderUpIcon(64)}
            onContextMenu={(e, file) => file ? onContextMenu(e, file) : onEmptyContextMenu(e)}
            onDragStart={(e, file) => onDragStart(e, file, panelKey, selectedFiles, sortedFiles)}
            onDragOver={(e, file) => onDragOver(e, file.path, file.is_dir, panelKey)}
            onDrop={(e, file) => file.is_dir && onDrop(e, file.path, panelKey)}
            onDragLeave={onDragLeave}
            onDragEnd={onDragEnd}
            dragOverTarget={dropTargetPath}
            inlineRename={inlineRename}
            onInlineRenameChange={setInlineRenameValue}
            onInlineRenameCommit={onInlineRenameCommit}
            onInlineRenameCancel={onInlineRenameCancel}
            formatBytes={formatBytes}
            showFileExtensions={showFileExtensions}
          />
        )}
        </div>
      </div>
    </div>
  );
};

export default LocalFilePanel;
