// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * CropOverlay: AeroImage crop selection overlay
 *
 * Renders a Photoshop-style crop rectangle with darkened surrounds,
 * 8 resize handles, and a dimension badge. Supports free and
 * fixed-aspect-ratio cropping via mouse interaction.
 *
 * The selection starts from the crop already chosen, or else as the whole
 * image with its handles on the image edges, as in other image editors. Every coordinate is relative to the image's own
 * box, and the drawing sits in a frame placed over that box: the overlay
 * itself covers the whole viewer, where the image is centred with margins,
 * and drawing in the overlay's frame shifted the selection by the margin, so
 * part of the image could not be reached.
 *
 * A double-click on the selection, or Enter, applies it; Escape leaves crop
 * mode without applying (#1075).
 */

import React, { useState, useRef, useCallback, useEffect } from 'react';
import { CropRect } from '../types';
import { useI18n } from '../../../i18n';

interface CropOverlayProps {
    imageRef: React.RefObject<HTMLImageElement | null>;
    aspectRatio: number | null;
    /** The picture's size in the file's pixels, when the image on screen is a
     *  smaller preview of it; none: the image's own size. */
    naturalSize?: { width: number; height: number } | null;
    /** The crop already chosen (natural pixels), to start from; none: the whole image. */
    initialCrop?: CropRect | null;
    onCropChange: (natural: CropRect) => void;
    /** Apply the selection (double-click on it, or Enter). */
    onApply?: () => void;
    onCancel: () => void;
}

type HandleId = 'nw' | 'n' | 'ne' | 'e' | 'se' | 's' | 'sw' | 'w';
type DragMode = { kind: 'none' } | { kind: 'create'; sx: number; sy: number } | { kind: 'move'; ox: number; oy: number } | { kind: 'resize'; handle: HandleId };

const HANDLE_CURSORS: Record<HandleId, string> = {
    nw: 'nw-resize', n: 'n-resize', ne: 'ne-resize', e: 'e-resize',
    se: 'se-resize', s: 's-resize', sw: 'sw-resize', w: 'w-resize',
};

const MIN_SIZE = 10;

/**
 * A selection on screen, in the pixels of the file. The two edges are rounded,
 * not the corner and the size: rounding a corner and a size separately can
 * add up to one pixel past the picture, which the save refuses as "exceeds
 * image bounds" (#1075). The result never leaves the picture.
 */
export function screenToNatural(
    sel: { x: number; y: number; w: number; h: number },
    box: { width: number; height: number },
    natural: { width: number; height: number },
): CropRect {
    if (box.width <= 0 || box.height <= 0) return { x: 0, y: 0, width: 0, height: 0 };
    const kx = natural.width / box.width;
    const ky = natural.height / box.height;
    const clamp = (v: number, max: number) => Math.min(max, Math.max(0, Math.round(v)));
    const x0 = clamp(sel.x * kx, natural.width);
    const y0 = clamp(sel.y * ky, natural.height);
    const x1 = Math.max(x0, clamp((sel.x + sel.w) * kx, natural.width));
    const y1 = Math.max(y0, clamp((sel.y + sel.h) * ky, natural.height));
    return { x: x0, y: y0, width: x1 - x0, height: y1 - y0 };
}

/** A selection of the whole image, which is no crop at all. */
export function cropCoversWholeImage(c: CropRect, naturalWidth: number, naturalHeight: number): boolean {
    return c.x <= 0 && c.y <= 0 && c.x + c.width >= naturalWidth && c.y + c.height >= naturalHeight;
}

export const CropOverlay: React.FC<CropOverlayProps> = ({ imageRef, aspectRatio, naturalSize, initialCrop, onCropChange, onApply, onCancel }) => {
    const { t } = useI18n();

    // Screen-space crop rect (relative to image bounding rect)
    const [crop, setCrop] = useState<{ x: number; y: number; w: number; h: number } | null>(null);
    const dragMode = useRef<DragMode>({ kind: 'none' });
    const cropRef = useRef(crop);
    cropRef.current = crop;
    const overlayRef = useRef<HTMLDivElement>(null);
    // Bumped when the image box changes size, so the frame is placed again.
    const [, setLayout] = useState(0);

    // --- coordinate helpers ---------------------------------------------------

    const imgRect = useCallback(() => imageRef.current?.getBoundingClientRect() ?? null, [imageRef]);

    const clampScreen = useCallback((x: number, y: number): [number, number] => {
        const r = imgRect();
        if (!r) return [x, y];
        return [Math.max(0, Math.min(x, r.width)), Math.max(0, Math.min(y, r.height))];
    }, [imgRect]);

    const toNatural = useCallback((sx: number, sy: number, sw: number, sh: number): CropRect => {
        const img = imageRef.current;
        if (!img) return { x: 0, y: 0, width: 0, height: 0 };
        const r = img.getBoundingClientRect();
        return screenToNatural({ x: sx, y: sy, w: sw, h: sh }, r, naturalSize ?? { width: img.naturalWidth, height: img.naturalHeight });
    }, [imageRef, naturalSize]);

    const emitCrop = useCallback((c: { x: number; y: number; w: number; h: number } | null) => {
        if (c && c.w >= MIN_SIZE && c.h >= MIN_SIZE) {
            onCropChange(toNatural(c.x, c.y, c.w, c.h));
        }
    }, [onCropChange, toNatural]);

    // --- aspect ratio enforcement ---------------------------------------------

    const enforce = useCallback((x: number, y: number, w: number, h: number, anchor: 'w' | 'h' = 'w'): [number, number, number, number] => {
        if (!aspectRatio) return [x, y, w, h];
        if (anchor === 'w') {
            h = w / aspectRatio;
        } else {
            w = h * aspectRatio;
        }
        const r = imgRect();
        if (r) {
            if (x + w > r.width) w = r.width - x;
            if (y + h > r.height) h = r.height - y;
            if (anchor === 'w') h = w / aspectRatio;
            else w = h * aspectRatio;
        }
        return [x, y, Math.max(MIN_SIZE, w), Math.max(MIN_SIZE, h)];
    }, [aspectRatio, imgRect]);

    // --- mouse handlers -------------------------------------------------------

    // Pointer events: mouse, touchpad, touch screen and pen draw the same way.
    const onPointerDown = useCallback((e: React.PointerEvent) => {
        if (e.button !== 0) return;
        e.preventDefault();
        const r = imgRect();
        if (!r) return;
        const mx = e.clientX - r.left;
        const my = e.clientY - r.top;
        const c = cropRef.current;

        // Check handles first
        if (c) {
            const hit = hitHandle(c, mx, my);
            if (hit) {
                dragMode.current = { kind: 'resize', handle: hit };
                return;
            }
            // Inside crop -> move
            if (mx >= c.x && mx <= c.x + c.w && my >= c.y && my <= c.y + c.h) {
                dragMode.current = { kind: 'move', ox: mx - c.x, oy: my - c.y };
                return;
            }
        }
        // Create new
        dragMode.current = { kind: 'create', sx: mx, sy: my };
    }, [imgRect]);

    useEffect(() => {
        const onMove = (e: PointerEvent) => {
            const mode = dragMode.current;
            if (mode.kind === 'none') return;
            const r = imgRect();
            if (!r) return;
            const [mx, my] = clampScreen(e.clientX - r.left, e.clientY - r.top);

            if (mode.kind === 'create') {
                let x = Math.min(mode.sx, mx);
                let y = Math.min(mode.sy, my);
                let w = Math.abs(mx - mode.sx);
                let h = Math.abs(my - mode.sy);
                [x, y, w, h] = enforce(x, y, w, h);
                const nc = { x, y, w, h };
                setCrop(nc);
                emitCrop(nc);
            } else if (mode.kind === 'move') {
                const c = cropRef.current;
                if (!c) return;
                let nx = mx - mode.ox;
                let ny = my - mode.oy;
                nx = Math.max(0, Math.min(nx, r.width - c.w));
                ny = Math.max(0, Math.min(ny, r.height - c.h));
                const nc = { ...c, x: nx, y: ny };
                setCrop(nc);
                emitCrop(nc);
            } else if (mode.kind === 'resize') {
                const c = cropRef.current;
                if (!c) return;
                let { x, y, w, h } = c;
                const hid: string = mode.handle;
                if (hid.includes('e')) w = Math.max(MIN_SIZE, mx - x);
                if (hid.includes('w')) { const nx = Math.min(mx, x + w - MIN_SIZE); w = w + (x - nx); x = nx; }
                if (hid.includes('s')) h = Math.max(MIN_SIZE, my - y);
                if (hid.includes('n')) { const ny = Math.min(my, y + h - MIN_SIZE); h = h + (y - ny); y = ny; }
                const anchor = (hid === 'n' || hid === 's') ? 'h' : 'w';
                [x, y, w, h] = enforce(x, y, w, h, anchor);
                const nc = { x, y, w, h };
                setCrop(nc);
                emitCrop(nc);
            }
        };
        const onUp = () => { dragMode.current = { kind: 'none' }; };
        document.addEventListener('pointermove', onMove);
        document.addEventListener('pointerup', onUp);
        document.addEventListener('pointercancel', onUp);
        return () => {
            document.removeEventListener('pointermove', onMove);
            document.removeEventListener('pointerup', onUp);
            document.removeEventListener('pointercancel', onUp);
        };
    }, [imgRect, clampScreen, enforce, emitCrop]);

    // Start from the crop already chosen, or else the whole image, and keep
    // the selection on the same part of the image when the viewer changes.
    useEffect(() => {
        const img = imageRef.current;
        if (!img) return undefined;
        let last = img.getBoundingClientRect();
        let started = false;
        // On the first box with a size: the image can still be laid out at
        // zero size when the overlay mounts, and then the first observation
        // is where the selection starts.
        const start = (box: DOMRect) => {
            if (started || box.width <= 0 || box.height <= 0) return;
            started = true;
            const natW = naturalSize?.width ?? img.naturalWidth;
            const natH = naturalSize?.height ?? img.naturalHeight;
            const kx = natW > 0 ? box.width / natW : 1;
            const ky = natH > 0 ? box.height / natH : 1;
            const first = initialCrop
                ? { x: initialCrop.x * kx, y: initialCrop.y * ky, w: initialCrop.width * kx, h: initialCrop.height * ky }
                : { x: 0, y: 0, w: box.width, h: box.height };
            setCrop(first);
            emitCrop(first);
        };
        start(last);
        if (typeof ResizeObserver === 'undefined') return undefined;
        // The image is centred in the viewer: the viewer can change width while
        // the image, limited by its height, keeps its size and only moves. So
        // the overlay (which covers the viewer) is watched too, and every
        // change places the frame again.
        const observer = new ResizeObserver(() => {
            const now = img.getBoundingClientRect();
            if (now.width <= 0 || now.height <= 0) return;
            if (!started) {
                start(now);
                last = now;
                setLayout((n) => n + 1);
                return;
            }
            const sx = last.width > 0 ? now.width / last.width : 1;
            const sy = last.height > 0 ? now.height / last.height : 1;
            last = now;
            if (sx !== 1 || sy !== 1) {
                setCrop((c) => (c ? { x: c.x * sx, y: c.y * sy, w: c.w * sx, h: c.h * sy } : c));
            }
            setLayout((n) => n + 1);
        });
        observer.observe(img);
        if (overlayRef.current) observer.observe(overlayRef.current);
        return () => observer.disconnect();
        // Once per crop session: emitCrop changes with its parent's callback.
        // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [imageRef]);

    // A new proportion reshapes the selection: the largest of that shape,
    // centred on the picture, as other editors do.
    const firstRatio = useRef(true);
    useEffect(() => {
        if (firstRatio.current) { firstRatio.current = false; return; }
        if (!aspectRatio) return;
        const r = imgRect();
        if (!r || r.width <= 0 || r.height <= 0) return;
        let w = r.width;
        let h = w / aspectRatio;
        if (h > r.height) { h = r.height; w = h * aspectRatio; }
        const nc = { x: (r.width - w) / 2, y: (r.height - h) / 2, w, h };
        setCrop(nc);
        emitCrop(nc);
        // Only when the proportion changes.
        // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [aspectRatio]);

    // Escape leaves crop mode, Enter applies. Caught before the preview window
    // sees them: there Escape closes the whole preview.
    useEffect(() => {
        const onKey = (e: KeyboardEvent) => {
            if (e.key === 'Escape') {
                e.preventDefault();
                e.stopPropagation();
                onCancel();
            } else if (e.key === 'Enter' && onApply) {
                e.preventDefault();
                e.stopPropagation();
                onApply();
            }
        };
        window.addEventListener('keydown', onKey, true);
        return () => window.removeEventListener('keydown', onKey, true);
    }, [onCancel, onApply]);

    const onDoubleClick = useCallback((e: React.MouseEvent) => {
        const r = imgRect();
        const c = cropRef.current;
        if (!r || !c || !onApply) return;
        const mx = e.clientX - r.left;
        const my = e.clientY - r.top;
        if (mx >= c.x && mx <= c.x + c.w && my >= c.y && my <= c.y + c.h) onApply();
    }, [imgRect, onApply]);

    // --- hit testing ----------------------------------------------------------

    const hitHandle = (c: { x: number; y: number; w: number; h: number }, mx: number, my: number): HandleId | null => {
        const handles = getHandles(c);
        for (const h of handles) {
            const half = h.size / 2 + 4; // generous hit area
            if (Math.abs(mx - h.cx) <= half && Math.abs(my - h.cy) <= half) return h.id;
        }
        return null;
    };

    // --- cursor ---------------------------------------------------------------

    const getCursor = useCallback((e: React.MouseEvent): string => {
        const r = imgRect();
        if (!r) return 'crosshair';
        const mx = e.clientX - r.left;
        const my = e.clientY - r.top;
        const c = cropRef.current;
        if (c) {
            const h = hitHandle(c, mx, my);
            if (h) return HANDLE_CURSORS[h];
            if (mx >= c.x && mx <= c.x + c.w && my >= c.y && my <= c.y + c.h) return 'move';
        }
        return 'crosshair';
    }, [imgRect]);

    const [cursor, setCursorState] = useState('crosshair');
    const onMouseMoveLocal = useCallback((e: React.PointerEvent) => {
        if (dragMode.current.kind !== 'none') return;
        setCursorState(getCursor(e));
    }, [getCursor]);

    // --- render helpers -------------------------------------------------------

    const getHandles = (c: { x: number; y: number; w: number; h: number }) => {
        const mx = c.x + c.w / 2, my = c.y + c.h / 2;
        return [
            { id: 'nw' as HandleId, cx: c.x, cy: c.y, size: 12 },
            { id: 'n' as HandleId, cx: mx, cy: c.y, size: 10 },
            { id: 'ne' as HandleId, cx: c.x + c.w, cy: c.y, size: 12 },
            { id: 'e' as HandleId, cx: c.x + c.w, cy: my, size: 10 },
            { id: 'se' as HandleId, cx: c.x + c.w, cy: c.y + c.h, size: 12 },
            { id: 's' as HandleId, cx: mx, cy: c.y + c.h, size: 10 },
            { id: 'sw' as HandleId, cx: c.x, cy: c.y + c.h, size: 12 },
            { id: 'w' as HandleId, cx: c.x, cy: my, size: 10 },
        ];
    };

    const nat = crop && crop.w >= MIN_SIZE && crop.h >= MIN_SIZE ? toNatural(crop.x, crop.y, crop.w, crop.h) : null;
    const r = imgRect();
    const cw = r?.width ?? 0;
    const ch = r?.height ?? 0;
    // Where the image box sits inside the overlay: the drawing frame.
    const o = overlayRef.current?.getBoundingClientRect();
    const frame = r && o ? { left: r.left - o.left, top: r.top - o.top } : { left: 0, top: 0 };

    return (
        <div
            ref={overlayRef}
            className="absolute inset-0 select-none"
            style={{ cursor, touchAction: 'none' }}
            onPointerDown={onPointerDown}
            onPointerMove={onMouseMoveLocal}
            onDoubleClick={onDoubleClick}
        >
            {crop && (
                <div
                    data-crop-frame
                    className="absolute pointer-events-none"
                    style={{ left: frame.left, top: frame.top, width: cw, height: ch }}
                >
                    {/* Darkening panels */}
                    <div className="absolute bg-black/50" style={{ top: 0, left: 0, width: cw, height: crop.y }} />
                    <div className="absolute bg-black/50" style={{ top: crop.y, left: 0, width: crop.x, height: crop.h }} />
                    <div className="absolute bg-black/50" style={{ top: crop.y, left: crop.x + crop.w, width: cw - crop.x - crop.w, height: crop.h }} />
                    <div className="absolute bg-black/50" style={{ top: crop.y + crop.h, left: 0, width: cw, height: ch - crop.y - crop.h }} />

                    {/* Crop border: white dashes on a dark outline, so it shows
                        on a light picture as well as on a dark one (#1075). */}
                    <div
                        data-crop-border
                        className="absolute border-2 border-dashed border-white pointer-events-none"
                        style={{
                            left: crop.x, top: crop.y, width: crop.w, height: crop.h,
                            boxShadow: '0 0 0 1px rgba(0,0,0,0.75), inset 0 0 0 1px rgba(0,0,0,0.75)',
                        }}
                    />

                    {/* Rule-of-thirds guides inside the selection */}
                    {[1, 2].map(i => (
                        <React.Fragment key={i}>
                            <div data-crop-third className="absolute bg-white/40 pointer-events-none" style={{ left: crop.x + (crop.w * i) / 3, top: crop.y, width: 1, height: crop.h }} />
                            <div data-crop-third className="absolute bg-white/40 pointer-events-none" style={{ left: crop.x, top: crop.y + (crop.h * i) / 3, width: crop.w, height: 1 }} />
                        </React.Fragment>
                    ))}

                    {/* Handles: white with a dark border and shadow, visible on
                        any background. */}
                    {getHandles(crop).map(h => (
                        <div
                            key={h.id}
                            data-crop-handle={h.id}
                            className="absolute bg-white rounded-sm pointer-events-none border border-black/80"
                            style={{
                                width: h.size, height: h.size,
                                left: h.cx - h.size / 2,
                                top: h.cy - h.size / 2,
                                boxShadow: '0 0 0 1px rgba(255,255,255,0.6), 0 1px 4px rgba(0,0,0,0.8)',
                            }}
                        />
                    ))}

                    {/* Dimension badge */}
                    {nat && (
                        <div
                            className="absolute flex justify-center pointer-events-none"
                            style={{ left: crop.x, top: crop.y + crop.h + 6, width: crop.w }}
                        >
                            <span className="bg-gray-800/90 text-gray-300 text-xs font-mono px-2 py-0.5 rounded-full whitespace-nowrap">
                                {nat.width} × {nat.height} px
                            </span>
                            {onApply && (
                                <span data-crop-hint className="ml-2 bg-gray-800/90 text-gray-300 text-xs px-2 py-0.5 rounded-full whitespace-nowrap">
                                    {t('preview.image.edit.cropHint')}
                                </span>
                            )}
                        </div>
                    )}
                </div>
            )}
        </div>
    );
};

export default CropOverlay;
