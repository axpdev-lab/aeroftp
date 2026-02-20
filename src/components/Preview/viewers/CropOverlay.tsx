/**
 * CropOverlay — AeroImage crop selection overlay
 *
 * Renders a Photoshop-style crop rectangle with darkened surrounds,
 * 8 resize handles, and a dimension badge. Supports free and
 * fixed-aspect-ratio cropping via mouse interaction.
 */

import React, { useState, useRef, useCallback, useEffect } from 'react';
import { CropRect } from '../types';
import { useI18n } from '../../../i18n';

interface CropOverlayProps {
    imageRef: React.RefObject<HTMLImageElement | null>;
    aspectRatio: number | null;
    onCropChange: (natural: CropRect) => void;
    onCancel: () => void;
}

type HandleId = 'nw' | 'n' | 'ne' | 'e' | 'se' | 's' | 'sw' | 'w';
type DragMode = { kind: 'none' } | { kind: 'create'; sx: number; sy: number } | { kind: 'move'; ox: number; oy: number } | { kind: 'resize'; handle: HandleId };

const HANDLE_CURSORS: Record<HandleId, string> = {
    nw: 'nw-resize', n: 'n-resize', ne: 'ne-resize', e: 'e-resize',
    se: 'se-resize', s: 's-resize', sw: 'sw-resize', w: 'w-resize',
};

const MIN_SIZE = 10;

export const CropOverlay: React.FC<CropOverlayProps> = ({ imageRef, aspectRatio, onCropChange, onCancel }) => {
    useI18n(); // keep hook call even if not used for keys yet

    // Screen-space crop rect (relative to image bounding rect)
    const [crop, setCrop] = useState<{ x: number; y: number; w: number; h: number } | null>(null);
    const dragMode = useRef<DragMode>({ kind: 'none' });
    const cropRef = useRef(crop);
    cropRef.current = crop;

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
        const scaleX = img.naturalWidth / r.width;
        const scaleY = img.naturalHeight / r.height;
        return {
            x: Math.round(sx * scaleX),
            y: Math.round(sy * scaleY),
            width: Math.round(sw * scaleX),
            height: Math.round(sh * scaleY),
        };
    }, [imageRef]);

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

    const onPointerDown = useCallback((e: React.MouseEvent) => {
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
        const onMove = (e: MouseEvent) => {
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
        document.addEventListener('mousemove', onMove);
        document.addEventListener('mouseup', onUp);
        return () => { document.removeEventListener('mousemove', onMove); document.removeEventListener('mouseup', onUp); };
    }, [imgRect, clampScreen, enforce, emitCrop]);

    // Escape key
    useEffect(() => {
        const onKey = (e: KeyboardEvent) => { if (e.key === 'Escape') onCancel(); };
        document.addEventListener('keydown', onKey);
        return () => document.removeEventListener('keydown', onKey);
    }, [onCancel]);

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
    const onMouseMoveLocal = useCallback((e: React.MouseEvent) => {
        if (dragMode.current.kind !== 'none') return;
        setCursorState(getCursor(e));
    }, [getCursor]);

    // --- render helpers -------------------------------------------------------

    const getHandles = (c: { x: number; y: number; w: number; h: number }) => {
        const mx = c.x + c.w / 2, my = c.y + c.h / 2;
        return [
            { id: 'nw' as HandleId, cx: c.x, cy: c.y, size: 10 },
            { id: 'n' as HandleId, cx: mx, cy: c.y, size: 8 },
            { id: 'ne' as HandleId, cx: c.x + c.w, cy: c.y, size: 10 },
            { id: 'e' as HandleId, cx: c.x + c.w, cy: my, size: 8 },
            { id: 'se' as HandleId, cx: c.x + c.w, cy: c.y + c.h, size: 10 },
            { id: 's' as HandleId, cx: mx, cy: c.y + c.h, size: 8 },
            { id: 'sw' as HandleId, cx: c.x, cy: c.y + c.h, size: 10 },
            { id: 'w' as HandleId, cx: c.x, cy: my, size: 8 },
        ];
    };

    const nat = crop && crop.w >= MIN_SIZE && crop.h >= MIN_SIZE ? toNatural(crop.x, crop.y, crop.w, crop.h) : null;
    const r = imgRect();
    const cw = r?.width ?? 0;
    const ch = r?.height ?? 0;

    return (
        <div
            className="absolute inset-0 select-none"
            style={{ cursor }}
            onMouseDown={onPointerDown}
            onMouseMove={onMouseMoveLocal}
        >
            {crop && (
                <>
                    {/* Darkening panels */}
                    <div className="absolute bg-black/50" style={{ top: 0, left: 0, width: cw, height: crop.y }} />
                    <div className="absolute bg-black/50" style={{ top: crop.y, left: 0, width: crop.x, height: crop.h }} />
                    <div className="absolute bg-black/50" style={{ top: crop.y, left: crop.x + crop.w, width: cw - crop.x - crop.w, height: crop.h }} />
                    <div className="absolute bg-black/50" style={{ top: crop.y + crop.h, left: 0, width: cw, height: ch - crop.y - crop.h }} />

                    {/* Crop border */}
                    <div
                        className="absolute border-2 border-dashed border-white pointer-events-none"
                        style={{ left: crop.x, top: crop.y, width: crop.w, height: crop.h }}
                    />

                    {/* Handles */}
                    {getHandles(crop).map(h => (
                        <div
                            key={h.id}
                            className="absolute bg-white rounded-sm pointer-events-none"
                            style={{
                                width: h.size, height: h.size,
                                left: h.cx - h.size / 2,
                                top: h.cy - h.size / 2,
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
                        </div>
                    )}
                </>
            )}
        </div>
    );
};

export default CropOverlay;
