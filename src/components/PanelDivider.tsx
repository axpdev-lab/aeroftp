// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * PanelDivider: the vertical separator between two side-by-side panels,
 * shared by the connected Local/Remote layout and the AeroFile dual-local
 * layout. Purely presentational; all resize state lives in usePanelSplit.
 *
 * Operable from keyboard as well: tabIndex=0 so it can take focus,
 * Arrow Left/Right shift the split by 5 %, Home / End jump to the live
 * extremes, Enter resets to 50/50. Required because the dual-panel feature
 * is targeted at Linux keyboard-only setups, where mouse-only resize would
 * silently lock users out of one panel.
 *
 * The width is fixed (w-1) on purpose: a hover-grow width would shift the
 * panels under the cursor and make the usable-width math ambiguous. Hover
 * and focus feedback is color-only, so the separator never jumps around.
 */

import React from 'react';
import type { PanelSplitDividerHandlers } from '../hooks/usePanelSplit';

export interface PanelDividerProps {
    /** Accessible label, e.g. t('aerofile.resizePanels'). */
    label: string;
    handlers: PanelSplitDividerHandlers;
    /** Extra classes, e.g. flex order for swapped panels. */
    className?: string;
}

export const PanelDivider: React.FC<PanelDividerProps> = ({ label, handlers, className }) => (
    <div
        role="separator"
        aria-orientation="vertical"
        aria-label={label}
        aria-valuemin={handlers.ariaValueMin}
        aria-valuemax={handlers.ariaValueMax}
        aria-valuenow={handlers.ariaValueNow}
        tabIndex={0}
        onMouseDown={handlers.onMouseDown}
        onDoubleClick={handlers.onDoubleClick}
        onKeyDown={handlers.onKeyDown}
        className={`w-1 cursor-col-resize bg-gray-200 dark:bg-gray-700 hover:bg-blue-400 dark:hover:bg-blue-500 focus:bg-blue-500 focus:outline-none transition-colors flex-shrink-0${className ? ` ${className}` : ''}`}
        style={{ touchAction: 'none' }}
    />
);

export default PanelDivider;
