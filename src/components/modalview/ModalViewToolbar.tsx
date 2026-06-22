// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { List, LayoutGrid } from 'lucide-react';
import { useTranslation } from '../../i18n';
import { ModalFileView, ModalGridSize } from './useModalFileView';

interface ModalViewToolbarProps {
  view: ModalFileView;
}

const SIZE_OPTIONS: { id: ModalGridSize; letter: string; labelKey: string; fallback: string }[] = [
  { id: 'sm', letter: 'S', labelKey: 'modalView.iconSmall', fallback: 'Small icons' },
  { id: 'md', letter: 'M', labelKey: 'modalView.iconMedium', fallback: 'Medium icons' },
  { id: 'lg', letter: 'L', labelKey: 'modalView.iconLarge', fallback: 'Large icons' },
];

/**
 * Compact list/grid switch + a 3-way small/medium/large icon-size toggle, shared
 * by the AeroVault, AeroVault Zip, Cryptomator and archive modals. The size
 * toggle only shows in grid mode. Theme-aware (inherits the modal palette).
 */
export const ModalViewToolbar: React.FC<ModalViewToolbarProps> = ({ view }) => {
  const t = useTranslation();
  const { viewMode, setViewMode, gridSize, setGridSize } = view;

  return (
    <div className="flex items-center gap-1.5">
      {viewMode === 'grid' && (
        <div className="flex items-center rounded border border-gray-300 dark:border-gray-600 overflow-hidden">
          {SIZE_OPTIONS.map(opt => (
            <button
              key={opt.id}
              onClick={() => setGridSize(opt.id)}
              title={t(opt.labelKey) || opt.fallback}
              aria-label={t(opt.labelKey) || opt.fallback}
              aria-pressed={gridSize === opt.id}
              className={`w-6 py-1 text-[11px] font-semibold leading-none ${gridSize === opt.id
                ? 'bg-blue-600 text-white'
                : 'text-gray-500 dark:text-gray-400 hover:bg-gray-200 dark:hover:bg-gray-700'}`}
            >
              {opt.letter}
            </button>
          ))}
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
