// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';

interface TableColResizerProps {
    /** Current width in pixels (used as the baseline at pointerdown). */
    currentWidth: number;
    /** Minimum allowed width. The resizer never reports a value below this. */
    minWidth?: number;
    /** Streaming updates while the user drags (no persistence). */
    onResize: (nextWidthPx: number) => void;
    /** Final value at pointerup; persistence happens here. */
    onResizeEnd: (finalWidthPx: number) => void;
    title?: string;
}

/**
 * 1px gutter on the right edge of a <th>. Captures the pointer at pointerdown,
 * listens for pointermove on the window to compute a delta from the start
 * width, and clamps to minWidth on the fly. Reports the final pixel value at
 * pointerup so the parent can persist it once.
 */
export function TableColResizer({
    currentWidth,
    minWidth = 60,
    onResize,
    onResizeEnd,
    title,
}: TableColResizerProps) {
    const onPointerDown = React.useCallback((e: React.PointerEvent<HTMLDivElement>) => {
        e.preventDefault();
        e.stopPropagation();
        const startX = e.clientX;
        const startWidth = currentWidth;
        let lastWidth = startWidth;

        const handleMove = (ev: PointerEvent) => {
            const delta = ev.clientX - startX;
            const next = Math.max(minWidth, Math.floor(startWidth + delta));
            if (next !== lastWidth) {
                lastWidth = next;
                onResize(next);
            }
        };

        const handleUp = () => {
            window.removeEventListener('pointermove', handleMove);
            window.removeEventListener('pointerup', handleUp);
            window.removeEventListener('pointercancel', handleUp);
            onResizeEnd(lastWidth);
            document.body.style.userSelect = '';
            document.body.style.cursor = '';
        };

        window.addEventListener('pointermove', handleMove);
        window.addEventListener('pointerup', handleUp);
        window.addEventListener('pointercancel', handleUp);
        document.body.style.userSelect = 'none';
        document.body.style.cursor = 'col-resize';
    }, [currentWidth, minWidth, onResize, onResizeEnd]);

    return (
        <div
            onPointerDown={onPointerDown}
            onClick={(e) => e.stopPropagation()}
            onDoubleClick={(e) => e.stopPropagation()}
            title={title}
            role="separator"
            aria-orientation="vertical"
            className="absolute right-0 top-0 bottom-0 w-1 cursor-col-resize select-none hover:bg-blue-400/60 active:bg-blue-500/80 transition-colors"
        />
    );
}
