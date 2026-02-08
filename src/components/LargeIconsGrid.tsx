/**
 * LargeIconsGrid - Large icon view mode for file browsing
 *
 * Displays files as large icons (64-96px) with thumbnails for images,
 * similar to Finder's icon view or Windows Explorer's Large Icons.
 */

import React, { useCallback, useRef } from 'react';
import { Folder, FolderUp } from 'lucide-react';
import { ImageThumbnail } from './ImageThumbnail';
import type { LocalFile } from '../types';

const IMAGE_EXTENSIONS = /\.(jpg|jpeg|png|gif|svg|webp|bmp|ico)$/i;

interface LargeIconsGridProps {
  files: LocalFile[];
  selectedFiles: Set<string>;
  currentPath: string;
  onFileClick: (file: LocalFile, event: React.MouseEvent) => void;
  onFileDoubleClick: (file: LocalFile) => void;
  onNavigateUp: () => void;
  isAtRoot: boolean;
  getFileIcon: (fileName: string, isDirectory: boolean) => { icon: React.ReactNode; color: string };
  onContextMenu: (event: React.MouseEvent, file?: LocalFile) => void;
  // Drag and drop
  onDragStart?: (e: React.DragEvent, file: LocalFile) => void;
  onDragOver?: (e: React.DragEvent, file: LocalFile) => void;
  onDrop?: (e: React.DragEvent, file: LocalFile) => void;
  onDragLeave?: (e: React.DragEvent) => void;
  onDragEnd?: (e: React.DragEvent) => void;
  dragOverTarget: string | null;
  // Inline rename
  inlineRename: { path: string; name: string; isRemote: boolean } | null;
  onInlineRenameChange: (value: string) => void;
  onInlineRenameCommit: () => void;
  onInlineRenameCancel: () => void;
  // Formatters
  formatBytes: (bytes: number) => string;
}

// --- Individual file card (memoized) ---

interface LargeIconCardProps {
  file: LocalFile;
  isSelected: boolean;
  isDragOver: boolean;
  currentPath: string;
  getFileIcon: LargeIconsGridProps['getFileIcon'];
  onFileClick: LargeIconsGridProps['onFileClick'];
  onFileDoubleClick: LargeIconsGridProps['onFileDoubleClick'];
  onContextMenu: LargeIconsGridProps['onContextMenu'];
  onDragStart?: (e: React.DragEvent, file: LocalFile) => void;
  onDragOver?: (e: React.DragEvent, file: LocalFile) => void;
  onDrop?: (e: React.DragEvent, file: LocalFile) => void;
  onDragLeave?: (e: React.DragEvent) => void;
  onDragEnd?: (e: React.DragEvent) => void;
  inlineRename: LargeIconsGridProps['inlineRename'];
  onInlineRenameChange: LargeIconsGridProps['onInlineRenameChange'];
  onInlineRenameCommit: LargeIconsGridProps['onInlineRenameCommit'];
  onInlineRenameCancel: LargeIconsGridProps['onInlineRenameCancel'];
  formatBytes: (bytes: number) => string;
}

const LargeIconCard = React.memo<LargeIconCardProps>(({
  file,
  isSelected,
  isDragOver,
  currentPath,
  getFileIcon,
  onFileClick,
  onFileDoubleClick,
  onContextMenu,
  onDragStart,
  onDragOver,
  onDrop,
  onDragLeave,
  onDragEnd,
  inlineRename,
  onInlineRenameChange,
  onInlineRenameCommit,
  onInlineRenameCancel,
  formatBytes,
}) => {
  const renameRef = useRef<HTMLInputElement>(null);
  const isRenaming = inlineRename?.path === file.path;
  const isImage = IMAGE_EXTENSIONS.test(file.name);

  const handleClick = useCallback((e: React.MouseEvent) => {
    onFileClick(file, e);
  }, [file, onFileClick]);

  const handleDoubleClick = useCallback(() => {
    onFileDoubleClick(file);
  }, [file, onFileDoubleClick]);

  const handleContextMenu = useCallback((e: React.MouseEvent) => {
    onContextMenu(e, file);
  }, [file, onContextMenu]);

  const handleDragStart = useCallback((e: React.DragEvent) => {
    onDragStart?.(e, file);
  }, [file, onDragStart]);

  const handleDragOver = useCallback((e: React.DragEvent) => {
    onDragOver?.(e, file);
  }, [file, onDragOver]);

  const handleDrop = useCallback((e: React.DragEvent) => {
    if (file.is_dir) onDrop?.(e, file);
  }, [file, onDrop]);

  const handleRenameKeyDown = useCallback((e: React.KeyboardEvent) => {
    if (e.key === 'Enter') {
      e.preventDefault();
      onInlineRenameCommit();
    } else if (e.key === 'Escape') {
      e.preventDefault();
      onInlineRenameCancel();
    }
  }, [onInlineRenameCommit, onInlineRenameCancel]);

  const tooltip = `${file.name}\n${file.size !== null ? formatBytes(file.size) : ''}\n${file.modified || ''}`.trim();

  const cardClasses = [
    'flex flex-col items-center p-3 rounded-lg cursor-pointer transition-colors select-none',
    isDragOver && file.is_dir
      ? 'bg-green-600/20 ring-1 ring-green-400'
      : isSelected
        ? 'bg-blue-600/20 ring-1 ring-blue-500/50'
        : 'hover:bg-gray-700/30',
  ].join(' ');

  // Render the icon/thumbnail area
  const renderIcon = () => {
    if (file.is_dir) {
      return <Folder size={64} className="text-blue-400" />;
    }
    if (isImage) {
      const imagePath = currentPath === '/'
        ? `/${file.name}`
        : `${currentPath}/${file.name}`;
      return (
        <ImageThumbnail
          path={imagePath}
          name={file.name}
          fallbackIcon={
            <div className="flex items-center justify-center" style={{ width: 96, height: 96 }}>
              {getFileIcon(file.name, false).icon}
            </div>
          }
          isRemote={false}
        />
      );
    }
    const { icon } = getFileIcon(file.name, false);
    return <div className="flex items-center justify-center" style={{ width: 64, height: 64 }}>{icon}</div>;
  };

  return (
    <div
      data-file-card
      className={cardClasses}
      title={tooltip}
      draggable={file.name !== '..'}
      onClick={handleClick}
      onDoubleClick={handleDoubleClick}
      onContextMenu={handleContextMenu}
      onDragStart={handleDragStart}
      onDragOver={handleDragOver}
      onDrop={handleDrop}
      onDragLeave={onDragLeave}
      onDragEnd={onDragEnd}
    >
      {/* Icon / Thumbnail */}
      <div className={`flex items-center justify-center ${isImage && !file.is_dir ? 'w-24 h-24' : 'w-16 h-16'}`}>
        {renderIcon()}
      </div>

      {/* Filename / Inline rename */}
      {isRenaming ? (
        <input
          ref={renameRef}
          autoFocus
          type="text"
          value={inlineRename?.name ?? ''}
          onChange={(e) => onInlineRenameChange(e.target.value)}
          onKeyDown={handleRenameKeyDown}
          onBlur={onInlineRenameCommit}
          onClick={(e) => e.stopPropagation()}
          className="mt-1.5 px-1 w-full text-xs text-center bg-white dark:bg-gray-900 border border-blue-500 rounded outline-none"
        />
      ) : (
        <span className="text-xs text-center leading-tight mt-1.5 line-clamp-2 max-w-full break-all">
          {file.name}
        </span>
      )}

      {/* File size (only for files, not directories) */}
      {!file.is_dir && file.size !== null && (
        <span className="text-[10px] text-gray-500 mt-0.5">
          {formatBytes(file.size)}
        </span>
      )}
    </div>
  );
});

LargeIconCard.displayName = 'LargeIconCard';

// --- Main grid component ---

export function LargeIconsGrid({
  files,
  selectedFiles,
  currentPath,
  onFileClick,
  onFileDoubleClick,
  onNavigateUp,
  isAtRoot,
  getFileIcon,
  onContextMenu,
  onDragStart,
  onDragOver,
  onDrop,
  onDragLeave,
  onDragEnd,
  dragOverTarget,
  inlineRename,
  onInlineRenameChange,
  onInlineRenameCommit,
  onInlineRenameCancel,
  formatBytes,
}: LargeIconsGridProps) {
  const handleNavigateUp = useCallback(() => {
    if (!isAtRoot) onNavigateUp();
  }, [isAtRoot, onNavigateUp]);

  return (
    <div className="grid grid-cols-[repeat(auto-fill,minmax(120px,1fr))] gap-2 p-2">
      {/* Go Up item */}
      <div
        className={`flex flex-col items-center p-3 rounded-lg cursor-pointer transition-colors select-none ${
          isAtRoot ? 'opacity-50 cursor-not-allowed' : 'hover:bg-gray-700/30'
        }`}
        onClick={handleNavigateUp}
        title="Go up"
      >
        <div className="flex items-center justify-center w-16 h-16">
          <FolderUp size={64} className="text-gray-400" />
        </div>
        <span className="text-xs text-center leading-tight mt-1.5 italic text-gray-500">..</span>
      </div>

      {/* File cards */}
      {files.map((file) => (
        <LargeIconCard
          key={file.name}
          file={file}
          isSelected={selectedFiles.has(file.name)}
          isDragOver={dragOverTarget === file.path}
          currentPath={currentPath}
          getFileIcon={getFileIcon}
          onFileClick={onFileClick}
          onFileDoubleClick={onFileDoubleClick}
          onContextMenu={onContextMenu}
          onDragStart={onDragStart}
          onDragOver={onDragOver}
          onDrop={onDrop}
          onDragLeave={onDragLeave}
          onDragEnd={onDragEnd}
          inlineRename={inlineRename}
          onInlineRenameChange={onInlineRenameChange}
          onInlineRenameCommit={onInlineRenameCommit}
          onInlineRenameCancel={onInlineRenameCancel}
          formatBytes={formatBytes}
        />
      ))}
    </div>
  );
}

export default LargeIconsGrid;
