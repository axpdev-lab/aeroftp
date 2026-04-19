// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

import { useCallback, useEffect, useRef } from 'react';

/**
 * Pointer-based drag handler with guaranteed cleanup on unmount.
 *
 * Pattern replaced: manually attaching `mousemove`/`mouseup` to `document`
 * inside `onMouseDown` and forgetting to remove them on unmount while a drag
 * is still in progress. Pointer events also capture across iframes and
 * respect touch/stylus without extra code.
 *
 * The returned `onPointerDown` sets pointer capture on the triggering element,
 * so a single listener per drag gesture replaces the global mousemove pair.
 */
export interface PointerDragHandlers {
    onPointerMove?: (event: PointerEvent) => void;
    onPointerUp?: (event: PointerEvent) => void;
    onPointerCancel?: (event: PointerEvent) => void;
}

export function usePointerDrag(handlers: PointerDragHandlers) {
    const handlersRef = useRef(handlers);
    handlersRef.current = handlers;

    // Tracks the active capture so unmount during drag can release it.
    const captureRef = useRef<{ element: HTMLElement; pointerId: number } | null>(null);

    const release = useCallback((element: HTMLElement | null, pointerId: number) => {
        if (!element) return;
        try {
            element.releasePointerCapture(pointerId);
        } catch {
            // The element may have been detached before we got here — ignore.
        }
    }, []);

    const onPointerDown = useCallback(
        (event: React.PointerEvent<HTMLElement>) => {
            const element = event.currentTarget;
            element.setPointerCapture(event.pointerId);
            captureRef.current = { element, pointerId: event.pointerId };

            const handleMove = (e: PointerEvent) => {
                handlersRef.current.onPointerMove?.(e);
            };
            const handleEnd = (e: PointerEvent) => {
                handlersRef.current.onPointerUp?.(e);
                detach();
            };
            const handleCancel = (e: PointerEvent) => {
                handlersRef.current.onPointerCancel?.(e);
                detach();
            };

            const detach = () => {
                element.removeEventListener('pointermove', handleMove);
                element.removeEventListener('pointerup', handleEnd);
                element.removeEventListener('pointercancel', handleCancel);
                release(element, event.pointerId);
                captureRef.current = null;
            };

            element.addEventListener('pointermove', handleMove);
            element.addEventListener('pointerup', handleEnd);
            element.addEventListener('pointercancel', handleCancel);
        },
        [release],
    );

    // If the component unmounts mid-drag, release the capture so the element
    // doesn't retain pointer events after removal.
    useEffect(() => {
        return () => {
            const capture = captureRef.current;
            if (capture) {
                release(capture.element, capture.pointerId);
                captureRef.current = null;
            }
        };
    }, [release]);

    return { onPointerDown };
}
