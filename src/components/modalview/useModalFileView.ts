// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { useCallback, useEffect, useState } from 'react';

/**
 * Shared list/grid view preference for the in-app "file manager" modals
 * (AeroVault, AeroVault Zip, Cryptomator, archive browsers). One localStorage
 * key keeps the choice consistent across every modal. Default is a medium icon
 * grid, so opening any container feels like a file-manager window rather than a
 * bare table.
 */
export type ModalViewMode = 'list' | 'grid';
export type ModalGridSize = 'sm' | 'md' | 'lg';

export const MODAL_GRID_SIZES: ModalGridSize[] = ['sm', 'md', 'lg'];

/** Tile min-width and icon px for each grid size (small / medium / large). */
export const GRID_SIZE_METRICS: Record<ModalGridSize, { tile: number; icon: number }> = {
  sm: { tile: 92, icon: 32 },
  md: { tile: 124, icon: 48 },
  lg: { tile: 164, icon: 72 },
};

const STORAGE_KEY = 'aeroModalFileView';

interface PersistedView {
  mode: ModalViewMode;
  size: ModalGridSize;
}

const DEFAULT_VIEW: PersistedView = { mode: 'grid', size: 'md' };

function loadView(): PersistedView {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return DEFAULT_VIEW;
    const parsed = JSON.parse(raw) as Partial<PersistedView>;
    const mode: ModalViewMode = parsed.mode === 'list' ? 'list' : 'grid';
    const size: ModalGridSize = MODAL_GRID_SIZES.includes(parsed.size as ModalGridSize)
      ? (parsed.size as ModalGridSize)
      : 'md';
    return { mode, size };
  } catch {
    return DEFAULT_VIEW;
  }
}

export interface ModalFileView {
  viewMode: ModalViewMode;
  setViewMode: (mode: ModalViewMode) => void;
  gridSize: ModalGridSize;
  setGridSize: (size: ModalGridSize) => void;
}

export function useModalFileView(): ModalFileView {
  const [view, setView] = useState<PersistedView>(loadView);

  useEffect(() => {
    try {
      localStorage.setItem(STORAGE_KEY, JSON.stringify(view));
    } catch {
      // best-effort persistence
    }
  }, [view]);

  const setViewMode = useCallback((mode: ModalViewMode) => setView(v => ({ ...v, mode })), []);
  const setGridSize = useCallback((size: ModalGridSize) => setView(v => ({ ...v, size })), []);

  return {
    viewMode: view.mode,
    setViewMode,
    gridSize: view.size,
    setGridSize,
  };
}
