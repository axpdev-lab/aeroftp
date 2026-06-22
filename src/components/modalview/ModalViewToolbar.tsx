// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { List, LayoutGrid, Minus, Plus } from 'lucide-react';
import { useTranslation } from '../../i18n';
import { ModalFileView, MODAL_GRID_SIZES } from './useModalFileView';

interface ModalViewToolbarProps {
  view: ModalFileView;
}

/**
 * Compact list/grid switch + icon-size control shared by the AeroVault,
 * AeroVault Zip, Cryptomator and archive modals. The size stepper only shows in
 * grid mode. Theme-aware (inherits the modal's Tailwind palette).
 */
export const ModalViewToolbar: React.FC<ModalViewToolbarProps> = ({ view }) => {
  const t = useTranslation();
  const { viewMode, setViewMode, gridSize, stepGridSize } = view;
  const sizeIdx = MODAL_GRID_SIZES.indexOf(gridSize);

  return (
    <div className="flex items-center gap-1">
      {viewMode === 'grid' && (
        <div className="flex items-center gap-0.5 mr-1">
          <button
            onClick={() => stepGridSize(-1)}
            disabled={sizeIdx <= 0}
            title={t('modalView.iconSmaller') || 'Smaller icons'}
            aria-label={t('modalView.iconSmaller') || 'Smaller icons'}
            className="p-1 rounded text-gray-500 dark:text-gray-400 hover:bg-gray-200 dark:hover:bg-gray-700 disabled:opacity-40 disabled:cursor-not-allowed"
          >
            <Minus size={13} />
          </button>
          <button
            onClick={() => stepGridSize(1)}
            disabled={sizeIdx >= MODAL_GRID_SIZES.length - 1}
            title={t('modalView.iconLarger') || 'Larger icons'}
            aria-label={t('modalView.iconLarger') || 'Larger icons'}
            className="p-1 rounded text-gray-500 dark:text-gray-400 hover:bg-gray-200 dark:hover:bg-gray-700 disabled:opacity-40 disabled:cursor-not-allowed"
          >
            <Plus size={13} />
          </button>
        </div>
      )}
      <div className="flex items-center rounded border border-gray-300 dark:border-gray-600 overflow-hidden">
        <button
          onClick={() => setViewMode('list')}
          title={t('modalView.listView') || 'List view'}
          aria-label={t('modalView.listView') || 'List view'}
          aria-pressed={viewMode === 'list'}
          className={`p-1.5 ${viewMode === 'list'
            ? 'bg-blue-600 text-white'
            : 'text-gray-500 dark:text-gray-400 hover:bg-gray-200 dark:hover:bg-gray-700'}`}
        >
          <List size={14} />
        </button>
        <button
          onClick={() => setViewMode('grid')}
          title={t('modalView.gridView') || 'Icon view'}
          aria-label={t('modalView.gridView') || 'Icon view'}
          aria-pressed={viewMode === 'grid'}
          className={`p-1.5 ${viewMode === 'grid'
            ? 'bg-blue-600 text-white'
            : 'text-gray-500 dark:text-gray-400 hover:bg-gray-200 dark:hover:bg-gray-700'}`}
        >
          <LayoutGrid size={14} />
        </button>
      </div>
    </div>
  );
};
