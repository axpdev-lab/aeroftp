// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { GRID_SIZE_METRICS, ModalGridSize } from './useModalFileView';

/** Minimal item shape the grid needs; modals map their entries onto this. */
export interface ModalGridItem {
  /** Unique key (typically the full in-container path). */
  key: string;
  /** Display label (usually the last path segment). */
  label: string;
  isDir: boolean;
  /** Size in bytes (files only). */
  size?: number;
}

interface ModalFileGridProps {
  items: ModalGridItem[];
  gridSize: ModalGridSize;
  /** Icon node for an item at the given pixel size. */
  getIcon: (item: ModalGridItem, px: number) => React.ReactNode;
  /** Double-click / activate (navigate into a folder, open/extract a file). */
  onActivate: (item: ModalGridItem) => void;
  /** Optional hover action buttons (extract, delete...) rendered per tile. */
  renderActions?: (item: ModalGridItem) => React.ReactNode;
  formatSize: (bytes: number) => string;
}

/**
 * Generic icon-grid view for the file-manager-style modals. Presentational
 * only: navigation, extraction and deletion stay in each modal via onActivate /
 * renderActions. Theme-aware via Tailwind dark: classes.
 */
export const ModalFileGrid: React.FC<ModalFileGridProps> = ({
  items,
  gridSize,
  getIcon,
  onActivate,
  renderActions,
  formatSize,
}) => {
  const { tile, icon } = GRID_SIZE_METRICS[gridSize];

  return (
    <div
      className="grid gap-2 p-3"
      style={{ gridTemplateColumns: `repeat(auto-fill, minmax(${tile}px, 1fr))` }}
    >
      {items.map(item => (
        <div
          key={item.key}
          onDoubleClick={() => onActivate(item)}
          title={item.label}
          className="group relative flex flex-col items-center gap-1 p-2 rounded-lg cursor-pointer select-none transition-colors hover:bg-blue-50 dark:hover:bg-gray-700/50"
        >
          {renderActions && (
            <div className="absolute top-1 right-1 flex gap-0.5 opacity-0 group-hover:opacity-100 transition-opacity">
              {renderActions(item)}
            </div>
          )}
          <div className="flex items-center justify-center" style={{ width: icon, height: icon }}>
            {getIcon(item, icon)}
          </div>
          <span className="text-xs text-center leading-tight line-clamp-2 break-all text-gray-700 dark:text-gray-200">
            {item.label}
          </span>
          {!item.isDir && item.size != null && (
            <span className="text-[10px] text-gray-400 dark:text-gray-500">{formatSize(item.size)}</span>
          )}
        </div>
      ))}
    </div>
  );
};
