// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';

export interface ScanningState {
  active: boolean;
  folderName: string;
  message: string;
  operation: 'delete' | 'download' | 'upload';
}

const INITIAL_STATE: ScanningState = {
  active: false,
  folderName: '',
  message: '',
  operation: 'download',
};

interface ScanningToastProps {
  state: ScanningState;
  t: (key: string, params?: Record<string, string | number>) => string;
}

const operationIcons: Record<string, string> = {
  delete: 'M19 7l-.867 12.142A2 2 0 0116.138 21H7.862a2 2 0 01-1.995-1.858L5 7m5 4v6m4-6v6m1-10V4a1 1 0 00-1-1h-4a1 1 0 00-1 1v3M4 7h16',
  download: 'M4 16v1a3 3 0 003 3h10a3 3 0 003-3v-1m-4-4l-4 4m0 0l-4-4m4 4V4',
  upload: 'M4 16v1a3 3 0 003 3h10a3 3 0 003-3v-1m-4-8l-4-4m0 0L8 8m4-4v12',
};

export const ScanningToast: React.FC<ScanningToastProps> = React.memo(({ state, t }) => {
  if (!state.active) return null;

  const iconPath = operationIcons[state.operation] || operationIcons.download;
  const operationLabel = state.operation === 'delete'
    ? t('scanning.deleting')
    : state.operation === 'upload'
      ? t('scanning.uploading')
      : t('scanning.downloading');

  return (
    <div className="fixed inset-0 z-[60] flex items-start justify-center pointer-events-none pt-16">
      <div
        className="
          pointer-events-auto
          flex items-center gap-3 px-5 py-3.5 rounded-xl
          border shadow-2xl backdrop-blur-md
          bg-white/90 dark:bg-gray-800/90
          border-gray-200 dark:border-gray-700
          text-gray-900 dark:text-gray-100
          animate-scale-in
          max-w-md
        "
        style={{ ['--tw-shadow-color' as string]: 'rgba(0,0,0,0.15)' }}
      >
        {/* Spinner */}
        <div className="relative flex-shrink-0 w-9 h-9">
          <svg className="w-9 h-9 animate-spin text-blue-500 dark:text-blue-400" viewBox="0 0 24 24" fill="none">
            <circle className="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" strokeWidth="3" />
            <path className="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4z" />
          </svg>
          <svg className="absolute inset-0 w-5 h-5 m-auto text-blue-600 dark:text-blue-300" fill="none" stroke="currentColor" viewBox="0 0 24 24" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
            <path d={iconPath} />
          </svg>
        </div>

        {/* Content */}
        <div className="flex-1 min-w-0">
          <p className="text-sm font-semibold truncate">
            {operationLabel}: <span className="font-normal text-gray-600 dark:text-gray-300">{state.folderName}</span>
          </p>
          <p className="text-xs text-gray-500 dark:text-gray-400 mt-0.5 truncate">
            {state.message || t('scanning.preparing')}
          </p>
        </div>
      </div>
    </div>
  );
});

ScanningToast.displayName = 'ScanningToast';

export { INITIAL_STATE as INITIAL_SCANNING_STATE };
