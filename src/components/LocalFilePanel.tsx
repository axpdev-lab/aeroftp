/**
 * LocalFilePanel — extracted from App.tsx
 *
 * Renders the complete local file panel: header (breadcrumb/address bar),
 * search bar, sidebar, and file views (list/grid/large-icons/trash).
 *
 * All state and business logic remain in App.tsx; this component is
 * a pure rendering extraction for maintainability.
 */

import React from 'react';
import {
  RefreshCw, Search, HardDrive, AlertTriangle, X, ClipboardList,
} from 'lucide-react';
import { BreadcrumbBar } from './BreadcrumbBar';
import { PlacesSidebar } from './PlacesSidebar';
import { SortableHeader, SortField, SortOrder } from './SortableHeader';
import { LargeIconsGrid } from './LargeIconsGrid';
import { ImageThumbnail } from './ImageThumbnail';
import { getPreviewCategory, isPreviewable as isMediaPreviewable } from './Preview';
import { isPreviewable } from './DevTools';
import { formatBytes, formatDate } from '../utils';
import { LocalFile } from '../types';
import type { TrashItem, FileTag } from '../types/aerofile';
import { FileTagBadge } from './FileTagBadge';

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

  // --- Navigation ---
  currentPath: string;
  setCurrentPath: (path: string) => void;
  onNavigate: (path: string) => void;
  onRefresh: (path: string) => void;
  isPathCoherent: boolean;
  isSyncPathMismatch: boolean;
  isSyncNavigation: boolean;
  syncBasePaths: { remote: string; local: string } | null;

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

  // --- Sort ---
  sortField: SortField;
  sortOrder: SortOrder;
  onSort: (field: SortField) => void;

  // --- Search ---
  searchFilter: string;
  setSearchFilter: (f: string) => void;
  showSearchBar: boolean;
  setShowSearchBar: React.Dispatch<React.SetStateAction<boolean>>;
  searchRef: React.RefObject<HTMLInputElement>;

  // --- View & Display ---
  viewMode: 'list' | 'grid' | 'large';
  visibleColumns: string[];
  showFileExtensions: boolean;
  debugMode: boolean;
  doubleClickAction: string;

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
  onDragStart: (e: React.DragEvent, file: LocalFile, isRemote: boolean, selectedFiles: Set<string>, sortedFiles: LocalFile[]) => void;
  onDragEnd: () => void;
  onDragOver: (e: React.DragEvent, path: string, isDir: boolean, isRemote: boolean) => void;
  onDragLeave: (e: React.DragEvent) => void;
  onDrop: (e: React.DragEvent, path: string, isRemote: boolean) => void;
  dropTargetPath: string | null;
  dragSourcePaths: string[];
  crossPanelTarget: string | null;
  onPanelDragOver: (e: React.DragEvent, isRemote: boolean) => void;
  onPanelDrop: (e: React.DragEvent, isRemote: boolean) => void;
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
  currentPath,
  setCurrentPath,
  onNavigate,
  onRefresh,
  isPathCoherent,
  isSyncPathMismatch,
  isSyncNavigation,
  syncBasePaths,
  localFiles,
  sortedFiles,
  selectedFiles,
  setSelectedFiles,
  lastSelectedIndex,
  setLastSelectedIndex,
  setActivePanel,
  setPreviewFile,
  sortField,
  sortOrder,
  onSort,
  searchFilter,
  setSearchFilter,
  showSearchBar,
  setShowSearchBar,
  searchRef,
  viewMode,
  visibleColumns,
  showFileExtensions,
  debugMode,
  doubleClickAction,
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

  return (
    <div
      role="region"
      aria-label="Local files"
      className={`${isAeroFileMode ? 'flex-1 min-w-0' : 'w-1/2'} flex flex-col transition-all duration-300 ${crossPanelTarget === 'local' ? 'ring-2 ring-inset ring-blue-400 bg-blue-50/30 dark:bg-blue-900/10' : ''}`}
      onDragOver={(e) => onPanelDragOver(e, false)}
      onDrop={(e) => onPanelDrop(e, false)}
      onDragLeave={onPanelDragLeave}
    >
      {/* Header: BreadcrumbBar (AeroFile) or Address Bar (Connected) */}
      <div className="px-3 py-1.5 bg-gray-100 dark:bg-gray-700 border-b border-gray-200 dark:border-gray-600 text-sm font-medium flex items-center gap-2">
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
            <div className={`flex-1 flex items-center bg-white dark:bg-gray-800 rounded-md border ${(!isPathCoherent || isSyncPathMismatch) ? 'border-amber-400 dark:border-amber-500' : 'border-gray-300 dark:border-gray-600 hover:border-blue-400 dark:hover:border-blue-500'} focus-within:border-blue-500 dark:focus-within:border-blue-400 focus-within:ring-2 focus-within:ring-blue-500/20 transition-all overflow-hidden`}>
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
        <div className="px-3 py-1.5 bg-blue-50 dark:bg-blue-900/20 border-b border-blue-200 dark:border-blue-800 flex items-center gap-2">
          <Search size={14} className="text-blue-500 flex-shrink-0" />
          <input
            autoFocus
            ref={searchRef}
            type="text"
            placeholder={t('search.local_placeholder') || 'Filter local files...'}
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
          {searchFilter && (
            <span className="text-xs text-blue-600 dark:text-blue-400 flex-shrink-0">
              {t('search.resultsCount', { count: localFiles.filter(f => f.name.toLowerCase().includes(searchFilter.toLowerCase())).length })}
            </span>
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
            currentPath={currentPath}
            onNavigate={onNavigate}
            t={t}
            recentPaths={recentPaths}
            onClearRecent={() => setRecentPaths([])}
            onRemoveRecent={(path) => setRecentPaths(prev => prev.filter(p => p !== path))}
            isTrashView={isTrashView}
            onNavigateTrash={onNavigateTrash}
            labelCounts={labelCounts}
            activeTagFilter={activeTagFilter}
            onTagFilter={onTagFilter}
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
                {t('trash.title')} — {t('trash.itemCount', { count: trashItems.length })}
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
          <table className="w-full" role="grid" aria-label={t('browser.name')}>
            <thead className="bg-gray-50 dark:bg-gray-700 sticky top-0" role="rowgroup">
              <tr role="row">
                <SortableHeader label={t('browser.name')} field="name" currentField={sortField} order={sortOrder} onClick={onSort} />
                {visibleColumns.includes('size') && <SortableHeader label={t('browser.size')} field="size" currentField={sortField} order={sortOrder} onClick={onSort} />}
                {visibleColumns.includes('type') && <SortableHeader label={t('browser.type')} field="type" currentField={sortField} order={sortOrder} onClick={onSort} className="hidden xl:table-cell" />}
                {visibleColumns.includes('modified') && <SortableHeader label={t('browser.modified')} field="modified" currentField={sortField} order={sortOrder} onClick={onSort} />}
              </tr>
            </thead>
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
                {visibleColumns.includes('size') && <td className="px-4 py-2 text-sm text-gray-400">—</td>}
                {visibleColumns.includes('type') && <td className="hidden xl:table-cell px-3 py-2 text-sm text-gray-400">—</td>}
                {visibleColumns.includes('modified') && <td className="px-4 py-2 text-sm text-gray-400">—</td>}
              </tr>
              {sortedFiles.map((file, i) => (
                <tr
                  key={`${file.name}-${i}`}
                  data-file-row
                  role="row"
                  aria-selected={selectedFiles.has(file.name)}
                  draggable={file.name !== '..'}
                  onDragStart={(e) => onDragStart(e, file, false, selectedFiles, sortedFiles)}
                  onDragEnd={onDragEnd}
                  onDragOver={(e) => onDragOver(e, file.path, file.is_dir, false)}
                  onDragLeave={onDragLeave}
                  onDrop={(e) => file.is_dir && onDrop(e, file.path, false)}
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
                  {visibleColumns.includes('size') && <td className="px-4 py-2 text-sm text-gray-500">{file.size !== null ? formatBytes(file.size) : '-'}</td>}
                  {visibleColumns.includes('type') && <td className="hidden xl:table-cell px-3 py-2 text-xs text-gray-500 uppercase">{file.is_dir ? t('browser.folderType') : (file.name.includes('.') ? file.name.split('.').pop() : '—')}</td>}
                  {visibleColumns.includes('modified') && <td className="px-4 py-2 text-xs text-gray-500 whitespace-nowrap">{formatDate(file.modified)}</td>}
                </tr>
              ))}
            </tbody>
          </table>
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
                draggable={file.name !== '..'}
                onDragStart={(e) => onDragStart(e, file, false, selectedFiles, sortedFiles)}
                onDragEnd={onDragEnd}
                onDragOver={(e) => onDragOver(e, file.path, file.is_dir, false)}
                onDragLeave={onDragLeave}
                onDrop={(e) => file.is_dir && onDrop(e, file.path, false)}
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
                {!file.is_dir && file.size !== null && (
                  <span className="file-grid-size">{formatBytes(file.size)}</span>
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
            onDragStart={(e, file) => onDragStart(e, file, false, selectedFiles, sortedFiles)}
            onDragOver={(e, file) => onDragOver(e, file.path, file.is_dir, false)}
            onDrop={(e, file) => file.is_dir && onDrop(e, file.path, false)}
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
