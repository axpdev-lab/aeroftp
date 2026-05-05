// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import { useCallback, useRef, useState } from 'react';
import type { PointerEvent as ReactPointerEvent } from 'react';

const MODAL_MARGIN = 12;

type Point = { x: number; y: number };

interface DragStart {
    pointer: Point;
    offset: Point;
    rect: DOMRect;
}

const clamp = (value: number, min: number, max: number) =>
    Math.min(Math.max(value, min), max);

// We test against `Element` (not `HTMLElement`) so SVG icons inside <button>
// also resolve via `closest()`. Otherwise WebKit fires pointerdown with
// target = SVGElement, drag captures the pointer, and the button's click
// never fires: this caused #129 ("X close needs many clicks to register").
const isInteractiveTarget = (target: EventTarget | null): boolean =>
    target instanceof Element
        && !!target.closest('button, input, select, textarea, a, [role="button"], [data-modal-drag-ignore]');

/**
 * Moves a modal panel inside the app window without invoking Tauri's native
 * window drag region. Use the returned handlers on the modal header.
 */
export function useDraggableModal() {
    const [offset, setOffset] = useState<Point>({ x: 0, y: 0 });
    const startRef = useRef<DragStart | null>(null);

    const onPointerDown = useCallback((event: ReactPointerEvent<HTMLElement>) => {
        if (event.button !== 0 || isInteractiveTarget(event.target)) return;

        const panel = event.currentTarget.closest<HTMLElement>('[data-draggable-modal-panel]');
        if (!panel) return;

        event.preventDefault();
        event.stopPropagation();
        event.currentTarget.setPointerCapture(event.pointerId);
        startRef.current = {
            pointer: { x: event.clientX, y: event.clientY },
            offset,
            rect: panel.getBoundingClientRect(),
        };
    }, [offset]);

    const onPointerMove = useCallback((event: ReactPointerEvent<HTMLElement>) => {
        const start = startRef.current;
        if (!start) return;

        event.preventDefault();
        const dx = event.clientX - start.pointer.x;
        const dy = event.clientY - start.pointer.y;

        const clampedDx = clamp(
            dx,
            MODAL_MARGIN - start.rect.left,
            window.innerWidth - MODAL_MARGIN - start.rect.right,
        );
        const clampedDy = clamp(
            dy,
            MODAL_MARGIN - start.rect.top,
            window.innerHeight - MODAL_MARGIN - start.rect.bottom,
        );

        setOffset({
            x: Math.round(start.offset.x + clampedDx),
            y: Math.round(start.offset.y + clampedDy),
        });
    }, []);

    const endDrag = useCallback((event: ReactPointerEvent<HTMLElement>) => {
        if (!startRef.current) return;
        try {
            event.currentTarget.releasePointerCapture(event.pointerId);
        } catch {
            // Capture may already be gone if the pointer was cancelled.
        }
        startRef.current = null;
    }, []);

    return {
        panelProps: {
            'data-draggable-modal-panel': true,
            style: { transform: `translate3d(${offset.x}px, ${offset.y}px, 0)` },
        } as const,
        dragHandleProps: {
            onPointerDown,
            onPointerMove,
            onPointerUp: endDrag,
            onPointerCancel: endDrag,
        },
    };
}
