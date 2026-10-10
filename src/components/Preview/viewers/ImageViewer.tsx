// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Image Viewer Component: AeroImage
 *
 * Advanced image viewer with:
 * - Zoom in/out (scroll wheel or buttons)
 * - Pan (drag when zoomed)
 * - Rotate 90° clockwise
 * - Fit to screen / Actual size toggle
 * - Color picker
 * - AeroImage editor (crop, resize, rotate, flip, adjustments, effects, save as)
 *
 * An applied crop is shown at once: the preview is the cropped picture, and
 * the format is chosen only when saving. Every edit is a step Undo (Ctrl+Z)
 * and Redo (Ctrl+Y, Ctrl+Shift+Z) walk through; Ctrl+S saves (#1075).
 */

import React, { useRef, useState, useCallback, useEffect, useMemo } from 'react';
import { ZoomIn, ZoomOut, RotateCw, Maximize2, Minimize2, Move, Pipette, Pencil, X, SquareDashedBottom, ChevronLeft, ChevronRight, Undo2, Redo2, Save } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { ViewerBaseProps, ImageMetadata, EditState, INITIAL_EDIT_STATE, CropRect } from '../types';
import type { ImageResult } from '../types';
import { useI18n } from '../../../i18n';
import { useImagePreviewBg, writeImagePreviewBg, IMAGE_PREVIEW_BG_PRESETS } from '../../../utils/imagePreviewBg';
import ImageEditor from './ImageEditor';
import { CropOverlay, cropCoversWholeImage } from './CropOverlay';
import { ImageSaveDialog } from './ImageSaveDialog';
import { EditHistory, applyCrop, commit, createHistory, orientedSize, redo, sameEdits, undo } from './imageEditHistory';
import { copyText } from '../../../utils/clipboard';
import { useGuardedClose } from '../../../hooks/useGuardedClose';
import { GuardedCloseConfirm } from '../../GuardedCloseConfirm';

interface ImageViewerProps extends ViewerBaseProps {
    className?: string;
    /** Reports unsaved AeroImage edits so the host can guard accidental close. */
    onDirtyChange?: (dirty: boolean) => void;
    /** Gallery paging (#128): toolbar prev/next buttons that mirror the modal's
     *  hover arrows and the ← → keys. Disabled when there is nothing to page. */
    onNext?: () => void;
    onPrevious?: () => void;
    hasNext?: boolean;
    hasPrevious?: boolean;
}

// Zoom limits
const MIN_ZOOM = 0.1;
const MAX_ZOOM = 5;
const ZOOM_STEP = 0.25;

const IMAGE_MIME: Record<string, string> = {
    jpg: 'image/jpeg', jpeg: 'image/jpeg', png: 'image/png', webp: 'image/webp',
    gif: 'image/gif', bmp: 'image/bmp', tif: 'image/tiff', tiff: 'image/tiff',
};

/** True for a key typed into a field, where Ctrl+Z belongs to the field. */
function isTextField(target: EventTarget | null): boolean {
    if (!(target instanceof HTMLElement)) return false;
    if (target.isContentEditable || target.tagName === 'TEXTAREA') return true;
    if (target.tagName !== 'INPUT') return false;
    const type = (target as HTMLInputElement).type;
    return type !== 'range' && type !== 'checkbox' && type !== 'button';
}

interface Geometry {
    crop: CropRect | null;
    rotation: EditState['rotation'];
    flipH: boolean;
    flipV: boolean;
}

/** The longest side of the edit preview: the screen, not the file. */
function previewMaxSide(): number {
    const dpr = typeof window !== 'undefined' ? window.devicePixelRatio || 1 : 1;
    const screenSide = typeof window !== 'undefined' ? Math.max(window.screen?.width ?? 0, window.screen?.height ?? 0) : 0;
    return Math.max(1024, Math.round((screenSide || 1920) * dpr));
}

/**
 * The picture as the edit shapes it (cropped, then turned clockwise, then
 * mirrored, the order of the save), for the preview. Crop mode draws on this
 * picture, so a selection is made on what the user sees. It is drawn at the
 * size of the screen, so a 24-megapixel photo turns and crops at once; the
 * selection is still measured in the file's pixels (CropOverlay naturalSize).
 */
async function renderGeometry(img: HTMLImageElement, g: Geometry, mime: string): Promise<string> {
    const sx = g.crop?.x ?? 0;
    const sy = g.crop?.y ?? 0;
    const sw = g.crop?.width ?? img.naturalWidth;
    const sh = g.crop?.height ?? img.naturalHeight;
    const quarter = g.rotation === 90 || g.rotation === 270;
    const k = Math.min(1, previewMaxSide() / Math.max(sw, sh));
    const dw = Math.max(1, Math.round(sw * k));
    const dh = Math.max(1, Math.round(sh * k));
    const canvas = document.createElement('canvas');
    canvas.width = quarter ? dh : dw;
    canvas.height = quarter ? dw : dh;
    const ctx = canvas.getContext('2d');
    if (!ctx) throw new Error('no 2d context');
    ctx.imageSmoothingQuality = 'high';
    // The last transform set acts first on the drawing: turn, then mirror.
    ctx.translate(canvas.width / 2, canvas.height / 2);
    ctx.scale(g.flipH ? -1 : 1, g.flipV ? -1 : 1);
    ctx.rotate((g.rotation * Math.PI) / 180);
    ctx.drawImage(img, sx, sy, sw, sh, -dw / 2, -dh / 2, dw, dh);
    const type = mime === 'image/jpeg' ? 'image/jpeg' : 'image/png';
    const blob = await new Promise<Blob | null>((resolve) => canvas.toBlob(resolve, type, 0.92));
    if (!blob) throw new Error('crop preview failed');
    return URL.createObjectURL(blob);
}

export const ImageViewer: React.FC<ImageViewerProps> = ({
    file,
    onError,
    className = '',
    onDirtyChange,
    onNext,
    onPrevious,
    hasNext,
    hasPrevious,
}) => {
    const { t } = useI18n();
    const containerRef = useRef<HTMLDivElement>(null);
    const imageRef = useRef<HTMLImageElement>(null);

    // Transparency background behind the image (discussion #270). Synced with
    // Settings > Appearance; the toolbar button cycles the presets in place.
    const { value: previewBgValue, style: previewBgStyle } = useImagePreviewBg();
    const cyclePreviewBg = useCallback(() => {
        const ids = IMAGE_PREVIEW_BG_PRESETS.map((p) => p.id) as string[];
        const idx = ids.indexOf(previewBgValue);
        writeImagePreviewBg(ids[(idx + 1) % ids.length] ?? ids[0]);
    }, [previewBgValue]);

    // View state
    const [zoom, setZoom] = useState(1);
    const [rotation, setRotation] = useState(0);
    const [position, setPosition] = useState({ x: 0, y: 0 });
    const [isFitToScreen, setIsFitToScreen] = useState(true);
    const [isDragging, setIsDragging] = useState(false);
    const [dragStart, setDragStart] = useState({ x: 0, y: 0 });
    const [imageLoaded, setImageLoaded] = useState(false);
    const [imageError, setImageError] = useState(false);
    const [metadata, setMetadata] = useState<ImageMetadata | null>(null);
    const [colorPickMode, setColorPickMode] = useState(false);
    const [pickedColor, setPickedColor] = useState<string | null>(null);

    // AeroImage editor state. `history.present` is the edit the save applies;
    // `savedEdit` is what is already on disk, for the unsaved marker.
    const [editMode, setEditMode] = useState(false);
    const [history, setHistory] = useState<EditHistory>(() => createHistory(INITIAL_EDIT_STATE));
    const editState = history.present;
    const [savedEdit, setSavedEdit] = useState<EditState>(INITIAL_EDIT_STATE);
    const [cropMode, setCropMode] = useState(false);
    // The selection while crop mode is open, in the pixels of the picture on
    // screen (the cropped one, once a crop is applied); null: all of it.
    const [cropDraft, setCropDraft] = useState<CropRect | null>(null);
    // Fixed proportions for the selection (width / height); null: free.
    const [cropAspect, setCropAspect] = useState<number | null>(null);
    const [saveDialogOpen, setSaveDialogOpen] = useState(false);

    // Image source URL
    const imageSrc = file.blobUrl || file.content as string || '';
    // The picture being edited: the file as opened, or as re-read from disk
    // after Replace Original.
    const [reloadedSrc, setReloadedSrc] = useState<string | null>(null);
    const baseSrc = reloadedSrc ?? imageSrc;
    const baseSrcRef = useRef(baseSrc);
    baseSrcRef.current = baseSrc;
    const fileExt = (file.name.split('.').pop() ?? '').toLowerCase();
    // The preview of the edited geometry (crop, turn, mirror).
    const [shapedSrc, setShapedSrc] = useState<string | null>(null);

    const setEditState = useCallback((next: EditState) => {
        setHistory((h) => commit(h, next, Date.now()));
    }, []);
    const resetEdits = useCallback(() => {
        setHistory(createHistory(INITIAL_EDIT_STATE));
        setSavedEdit(INITIAL_EDIT_STATE);
        setCropDraft(null);
    }, []);

    // Track previous src to avoid resetting on initial load
    const prevSrcRef = React.useRef<string>(imageSrc);
    // Paging in the gallery: the picture leaving stays under the new one until
    // that one is drawn, then the new one fades in over it. No empty frame.
    const [outgoingSrc, setOutgoingSrc] = useState<string | null>(null);

    // Reset state only when switching to a DIFFERENT image (not on initial load)
    useEffect(() => {
        if (prevSrcRef.current && prevSrcRef.current !== imageSrc && imageSrc) {
            if (!imageError) setOutgoingSrc(prevSrcRef.current);
            setZoom(1);
            setRotation(0);
            setPosition({ x: 0, y: 0 });
            setIsFitToScreen(true);
            setImageLoaded(false);
            setImageError(false);
            setMetadata(null);
            setEditMode(false);
            resetEdits();
            setCropMode(false);
            setSaveDialogOpen(false);
            setReloadedSrc(null);
        }
        prevSrcRef.current = imageSrc;
        // Only on a change of picture.
        // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [imageSrc, resetEdits]);

    // A URL this viewer made is released when it is replaced or unmounted.
    useEffect(() => () => { if (reloadedSrc) URL.revokeObjectURL(reloadedSrc); }, [reloadedSrc]);
    useEffect(() => () => { if (shapedSrc) URL.revokeObjectURL(shapedSrc); }, [shapedSrc]);

    const decodedBase = useRef<{ src: string; ready: Promise<HTMLImageElement> } | null>(null);
    // Build the preview of the edited geometry.
    const geometry = useMemo<Geometry | null>(() => {
        if (!editMode) return null;
        const { crop, rotation, flipH, flipV } = editState;
        return crop || rotation !== 0 || flipH || flipV ? { crop, rotation, flipH, flipV } : null;
    }, [editMode, editState]);
    const geometryKey = geometry ? JSON.stringify(geometry) : '';
    const [shapedKey, setShapedKey] = useState('');
    useEffect(() => {
        if (!geometry) {
            setShapedSrc(null);
            setShapedKey('');
            return undefined;
        }
        let cancelled = false;
        // The file is decoded once per picture, not once per edit.
        const cached = decodedBase.current;
        const decoded = cached && cached.src === baseSrc
            ? cached.ready
            : (() => {
                const img = new Image();
                img.src = baseSrc;
                const ready = img.decode().then(() => img);
                decodedBase.current = { src: baseSrc, ready };
                return ready;
            })();
        decoded
            .then((img) => renderGeometry(img, geometry, IMAGE_MIME[fileExt] ?? ''))
            .then((url) => {
                if (cancelled) URL.revokeObjectURL(url);
                else { setShapedSrc(url); setShapedKey(geometryKey); }
            })
            .catch(() => { if (!cancelled) { setShapedSrc(null); setShapedKey(''); } });
        return () => { cancelled = true; };
        // geometryKey stands for geometry.
        // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [baseSrc, geometryKey, fileExt]);
    // Until the new preview is ready the last one stays; with no geometry, the file.
    const displaySrc = (geometry && shapedSrc) || baseSrc;
    // Crop mode waits for the preview of the current geometry, so the
    // selection is drawn on the picture the edit describes.
    const previewCurrent = !geometry || shapedKey === geometryKey;
    // The picture on screen in the file's pixels: the preview may be smaller.
    const shownSize = useMemo(() => {
        if (!metadata) return null;
        const base = editState.crop ? { width: editState.crop.width, height: editState.crop.height } : { width: metadata.width, height: metadata.height };
        return editMode ? orientedSize(base, editState.rotation) : base;
    }, [metadata, editMode, editState.crop, editState.rotation]);

    // Report unsaved AeroImage edits up so the preview modal can guard against
    // an accidental click-outside while the user is mid-edit (#270).
    const exitEditMode = useCallback(() => {
        setEditMode(false);
        resetEdits();
        setCropMode(false);
    }, [resetEdits]);

    // Unsaved: an open selection, or edits that differ from what was saved.
    const dirty = editMode && (cropDraft !== null || !sameEdits(editState, savedEdit));
    useEffect(() => {
        onDirtyChange?.(dirty);
    }, [dirty, onDirtyChange]);

    // Leaving the editor with unsaved edits asks first, as closing the
    // preview does: Exit Edit used to drop them without a word.
    const exitGuard = useGuardedClose({ guard: dirty ? 'dirty' : null, onClose: exitEditMode });
    const toggleEditMode = useCallback(() => {
        if (editMode) {
            exitGuard.requestClose();
        } else {
            setEditMode(true);
            setColorPickMode(false);
            setPickedColor(null);
        }
    }, [editMode, exitGuard]);

    // Handle image load
    const outgoingTimer = useRef<number | undefined>(undefined);
    useEffect(() => () => window.clearTimeout(outgoingTimer.current), []);
    const handleImageLoad = useCallback(() => {
        setImageLoaded(true);
        window.clearTimeout(outgoingTimer.current);
        outgoingTimer.current = window.setTimeout(() => setOutgoingSrc(null), 160);
        // The size of the file, not of the cropped preview.
        if (imageRef.current && imageRef.current.getAttribute('src') === baseSrcRef.current) {
            setMetadata({
                width: imageRef.current.naturalWidth,
                height: imageRef.current.naturalHeight,
                format: file.name.split('.').pop()?.toUpperCase() || 'Unknown',
            });
        }
    }, [file.name]);

    // Handle image error
    const handleImageError = useCallback(() => {
        setImageError(true);
        onError?.(t('preview.image.loadFailed'));
    }, [onError, t]);

    // Zoom controls.
    //
    // Wheel and toolbar zoom act as a multiplier on top of the current
    // object-fit mode. They do not silently flip the user out of
    // fit-to-screen: that switch is reserved for the explicit Fit /
    // Actual-size button. When the new zoom drops to 1 or below the pan
    // offset is reset, otherwise a pan applied during a zoom-in session
    // would leave the image off-centre after scrolling back out. See
    // issue #239.
    const zoomIn = useCallback(() => {
        setZoom(prev => {
            const next = Math.min(MAX_ZOOM, prev + ZOOM_STEP);
            if (next <= 1) setPosition({ x: 0, y: 0 });
            return next;
        });
    }, []);

    const zoomOut = useCallback(() => {
        setZoom(prev => {
            const next = Math.max(MIN_ZOOM, prev - ZOOM_STEP);
            if (next <= 1) setPosition({ x: 0, y: 0 });
            return next;
        });
    }, []);

    // Rotate 90° clockwise (view rotation)
    const rotate = useCallback(() => {
        setRotation(prev => (prev + 90) % 360);
    }, []);

    // Toggle fit to screen
    const toggleFit = useCallback(() => {
        if (isFitToScreen) {
            setZoom(1);
            setPosition({ x: 0, y: 0 });
        } else {
            setZoom(1);
            setPosition({ x: 0, y: 0 });
        }
        setIsFitToScreen(!isFitToScreen);
    }, [isFitToScreen]);

    // Wheel, touchpad and pinch. Same contract as zoomIn / zoomOut: never
    // flips the Fit / Actual-size mode, and resets the pan offset when zoom
    // returns to fit (issue #239). Zoom keeps the point under the pointer in
    // place, as image viewers do; the step follows the size of the scroll, so
    // a touchpad zooms smoothly and a mouse wheel notch is about 20 %.
    const zoomRef = useRef(zoom);
    zoomRef.current = zoom;
    const positionRef = useRef(position);
    positionRef.current = position;
    const navRef = useRef({ onNext, onPrevious });
    navRef.current = { onNext, onPrevious };
    const wheelState = useRef({ cropMode, editMode, isFitToScreen });
    wheelState.current = { cropMode, editMode, isFitToScreen };

    const zoomAt = useCallback((clientX: number, clientY: number, factor: number) => {
        const el = containerRef.current;
        if (!el || !Number.isFinite(factor) || factor <= 0) return;
        const rect = el.getBoundingClientRect();
        const cx = clientX - (rect.left + rect.width / 2);
        const cy = clientY - (rect.top + rect.height / 2);
        const z = zoomRef.current;
        const nz = Math.max(MIN_ZOOM, Math.min(MAX_ZOOM, z * factor));
        if (nz === z) return;
        const p = positionRef.current;
        const k = nz / z;
        const np = nz <= 1 ? { x: 0, y: 0 } : { x: cx - k * (cx - p.x), y: cy - k * (cy - p.y) };
        zoomRef.current = nz;
        positionRef.current = np;
        setZoom(nz);
        setPosition(np);
    }, []);

    // Zoom from a button moves smoothly; a gesture follows the fingers, so the
    // transition is off while one runs and while dragging.
    const [gestureActive, setGestureActive] = useState(false);
    const gestureTimer = useRef<number | undefined>(undefined);
    const markGesture = useCallback(() => {
        setGestureActive(true);
        window.clearTimeout(gestureTimer.current);
        gestureTimer.current = window.setTimeout(() => setGestureActive(false), 200);
    }, []);
    useEffect(() => () => window.clearTimeout(gestureTimer.current), []);

    // Linux: WebKitGTK keeps a touchpad pinch for itself, so the backend takes
    // it and sends it here (webview_pinch.rs).
    useEffect(() => {
        let base = 1;
        let unlisten: (() => void) | undefined;
        let disposed = false;
        listen<{ phase: string; scale: number; x: number; y: number }>('touchpad-pinch', ({ payload }) => {
            if (wheelState.current.cropMode) return;
            if (payload.phase === 'begin') {
                base = zoomRef.current;
                return;
            }
            if (payload.phase !== 'update') return;
            markGesture();
            zoomAt(payload.x, payload.y, (base * payload.scale) / zoomRef.current);
        }).then((fn) => {
            if (disposed) fn();
            else unlisten = fn;
        }).catch(() => undefined);
        return () => { disposed = true; unlisten?.(); };
    }, [zoomAt, markGesture]);

    const swipe = useRef<{ dx: number; locked: boolean; timer: number | undefined }>({ dx: 0, locked: false, timer: undefined });
    useEffect(() => {
        const el = containerRef.current;
        if (!el) return undefined;
        const pan = (dx: number, dy: number) => {
            const p = positionRef.current;
            const np = { x: p.x - dx, y: p.y - dy };
            positionRef.current = np;
            setPosition(np);
        };
        const onWheel = (e: WheelEvent) => {
            const st = wheelState.current;
            // A pinch arrives as a wheel with Ctrl: never let it zoom the page.
            if (st.cropMode) {
                if (e.ctrlKey) e.preventDefault();
                return;
            }
            e.preventDefault();
            markGesture();
            if (e.ctrlKey) {
                zoomAt(e.clientX, e.clientY, Math.exp(-e.deltaY * 0.01));
                return;
            }
            const zoomed = zoomRef.current > 1 || !st.isFitToScreen;
            if (Math.abs(e.deltaX) > Math.abs(e.deltaY)) {
                if (zoomed) {
                    pan(e.deltaX, e.deltaY);
                    return;
                }
                // Two-finger swipe: one picture per gesture, left for the
                // next one, as in photo viewers. Not while editing.
                if (st.editMode) return;
                const sw = swipe.current;
                window.clearTimeout(sw.timer);
                sw.timer = window.setTimeout(() => { sw.dx = 0; sw.locked = false; }, 250);
                if (sw.locked) return;
                sw.dx += e.deltaX;
                if (Math.abs(sw.dx) > 120) {
                    sw.locked = true;
                    const go = sw.dx > 0 ? navRef.current.onNext : navRef.current.onPrevious;
                    sw.dx = 0;
                    go?.();
                }
                return;
            }
            // Vertical: a touchpad scroll moves a zoomed picture, a mouse
            // wheel (whole steps) zooms.
            const touchpad = e.deltaMode === 0 && Math.abs(e.deltaY) < 50 && e.deltaX !== 0;
            if (zoomed && touchpad) {
                pan(e.deltaX, e.deltaY);
                return;
            }
            const px = e.deltaMode === 1 ? e.deltaY * 33 : e.deltaY;
            zoomAt(e.clientX, e.clientY, Math.exp(-px * 0.0018));
        };
        // Safari / WKWebView pinch.
        let gestureStart = 1;
        const onGestureStart = (e: Event) => { e.preventDefault(); gestureStart = zoomRef.current; };
        const onGestureChange = (e: Event) => {
            e.preventDefault();
            markGesture();
            if (wheelState.current.cropMode) return;
            const g = e as Event & { scale?: number; clientX?: number; clientY?: number };
            if (!g.scale) return;
            zoomAt(g.clientX ?? 0, g.clientY ?? 0, (gestureStart * g.scale) / zoomRef.current);
        };
        el.addEventListener('wheel', onWheel, { passive: false });
        el.addEventListener('gesturestart', onGestureStart);
        el.addEventListener('gesturechange', onGestureChange);
        return () => {
            el.removeEventListener('wheel', onWheel);
            el.removeEventListener('gesturestart', onGestureStart);
            el.removeEventListener('gesturechange', onGestureChange);
            window.clearTimeout(swipe.current.timer);
        };
        // The container exists once there is a picture to show.
    }, [zoomAt, markGesture, !!imageSrc]);

    // Drag handlers for panning
    const handleMouseDown = useCallback((e: React.MouseEvent) => {
        if (cropMode || colorPickMode) return;
        if (zoom > 1 || !isFitToScreen) {
            setIsDragging(true);
            setDragStart({ x: e.clientX - position.x, y: e.clientY - position.y });
        }
    }, [zoom, isFitToScreen, position, cropMode, colorPickMode]);

    const handleMouseMove = useCallback((e: React.MouseEvent) => {
        if (isDragging) {
            setPosition({
                x: e.clientX - dragStart.x,
                y: e.clientY - dragStart.y,
            });
        }
    }, [isDragging, dragStart]);

    const handleMouseUp = useCallback(() => {
        setIsDragging(false);
    }, []);

    // Color picker: draw image on canvas and read pixel at click position
    const handleColorPick = useCallback((e: React.MouseEvent) => {
        if (!colorPickMode || !imageRef.current) return;
        e.stopPropagation();
        const img = imageRef.current;
        const rect = img.getBoundingClientRect();
        const scaleX = img.naturalWidth / rect.width;
        const scaleY = img.naturalHeight / rect.height;
        const x = Math.floor((e.clientX - rect.left) * scaleX);
        const y = Math.floor((e.clientY - rect.top) * scaleY);
        const canvas = document.createElement('canvas');
        canvas.width = img.naturalWidth;
        canvas.height = img.naturalHeight;
        const ctx = canvas.getContext('2d');
        if (!ctx) return;
        ctx.drawImage(img, 0, 0);
        const pixel = ctx.getImageData(x, y, 1, 1).data;
        const hex = `#${pixel[0].toString(16).padStart(2, '0')}${pixel[1].toString(16).padStart(2, '0')}${pixel[2].toString(16).padStart(2, '0')}`;
        setPickedColor(hex);
        setColorPickMode(false);
        void copyText(hex).catch(() => {}).catch(() => undefined);
    }, [colorPickMode]);

    // ─── AeroImage Edit Handlers ─────────────────────────────────────


    const handleEditStateChange = useCallback((state: EditState) => {
        setEditState(state);
    }, [setEditState]);

    const original = useMemo(
        () => ({ width: metadata?.width ?? 0, height: metadata?.height ?? 0 }),
        [metadata],
    );

    // The selection becomes part of the edit: the preview shows it cropped.
    const applyCropDraft = useCallback(() => {
        if (cropDraft && original.width > 0) {
            setHistory((h) => commit(h, applyCrop(h.present, cropDraft, original), Date.now()));
        }
        setCropDraft(null);
        setCropMode(false);
    }, [cropDraft, original]);

    const cancelCrop = useCallback(() => {
        setCropDraft(null);
        setCropMode(false);
    }, []);

    const handleCropModeToggle = useCallback((active: boolean) => {
        if (!active) {
            // Leaving crop mode with the button keeps the selection.
            applyCropDraft();
            return;
        }
        setCropDraft(null);
        setCropMode(true);
        setZoom(1);
        setPosition({ x: 0, y: 0 });
        setIsFitToScreen(true);
    }, [applyCropDraft]);

    const handleCropChange = useCallback((natural: CropRect) => {
        // The selection starts as the whole picture: that is no crop at all.
        const whole = !!shownSize && cropCoversWholeImage(natural, shownSize.width, shownSize.height);
        setCropDraft(whole ? null : natural);
    }, [shownSize]);

    const canUndo = history.past.length > 0;
    const canRedo = history.future.length > 0;
    const handleUndo = useCallback(() => {
        setCropDraft(null);
        setCropMode(false);
        setHistory(undo);
    }, []);
    const handleRedo = useCallback(() => {
        setCropDraft(null);
        setCropMode(false);
        setHistory(redo);
    }, []);

    // Save: a selection still open is applied first, as it is on screen.
    const requestSave = useCallback(() => {
        if (cropMode) applyCropDraft();
        setSaveDialogOpen(true);
    }, [cropMode, applyCropDraft]);

    const handleSaveResult = useCallback(async (result: ImageResult) => {
        setSaveDialogOpen(false);
        window.dispatchEvent(new CustomEvent('file-changed', {
            detail: { path: result.path },
        }));
        if (result.path !== file.path) {
            // Saved as a copy: these edits are on disk, in the copy.
            setSavedEdit(history.present);
            return;
        }
        // Replaced the original: edit the new file from here, read again
        // from disk. Reusing the old picture would apply the same edits a
        // second time to a file that already has them.
        try {
            const base64 = await invoke<string>('read_local_file_base64', { path: file.path });
            const bytes = Uint8Array.from(atob(base64), (c) => c.charCodeAt(0));
            const url = URL.createObjectURL(new Blob([bytes], { type: IMAGE_MIME[fileExt] ?? 'application/octet-stream' }));
            setImageLoaded(false);
            setMetadata(null);
            setReloadedSrc(url);
            resetEdits();
            setCropMode(false);
        } catch (err) {
            onError?.(String(err));
        }
    }, [file.path, fileExt, history.present, resetEdits, onError]);

    // Editor keys: Ctrl+Z, Ctrl+Y / Ctrl+Shift+Z, Ctrl+S. Caught before the
    // rest of the app, which has its own Ctrl+S.
    useEffect(() => {
        if (!editMode || saveDialogOpen) return undefined;
        const onKey = (e: KeyboardEvent) => {
            // The arrows page to the next picture: not while editing this one.
            if ((e.key === 'ArrowLeft' || e.key === 'ArrowRight') && !isTextField(e.target)) {
                e.stopPropagation();
                return;
            }
            if (!(e.ctrlKey || e.metaKey) || e.altKey) return;
            const key = e.key.toLowerCase();
            if (key === 's') {
                e.preventDefault();
                e.stopPropagation();
                requestSave();
                return;
            }
            if (isTextField(e.target)) return;
            if (key === 'z' && !e.shiftKey) {
                e.preventDefault();
                e.stopPropagation();
                handleUndo();
            } else if (key === 'y' || (key === 'z' && e.shiftKey)) {
                e.preventDefault();
                e.stopPropagation();
                handleRedo();
            }
        };
        window.addEventListener('keydown', onKey, true);
        return () => window.removeEventListener('keydown', onKey, true);
    }, [editMode, saveDialogOpen, requestSave, handleUndo, handleRedo]);

    // The editor works on the picture as cropped: sizes and their presets
    // refer to it.
    const editorMetadata = useMemo(
        () => (metadata && editState.crop
            ? { ...metadata, width: editState.crop.width, height: editState.crop.height }
            : metadata),
        [metadata, editState.crop],
    );

    // ─── CSS Filters (live preview) ──────────────────────────────────

    const cssFilter = useMemo(() => {
        if (!editMode) return undefined;
        const parts: string[] = [];
        if (editState.brightness !== 0) parts.push(`brightness(${1 + editState.brightness / 100})`);
        if (editState.contrast !== 0) parts.push(`contrast(${1 + editState.contrast / 100})`);
        if (editState.hue !== 0) parts.push(`hue-rotate(${editState.hue}deg)`);
        if (editState.blur > 0) parts.push(`blur(${editState.blur}px)`);
        if (editState.grayscale) parts.push('grayscale(1)');
        if (editState.invert) parts.push('invert(1)');
        return parts.length > 0 ? parts.join(' ') : undefined;
    }, [editMode, editState.brightness, editState.contrast, editState.hue, editState.blur, editState.grayscale, editState.invert]);

    // Local file check (edit only for local files)
    const canEdit = !file.isRemote;

    // In crop mode: force zoom=1, no view rotation, fit to screen
    const effectiveZoom = cropMode ? 1 : zoom;
    const effectiveRotation = cropMode ? 0 : rotation;
    const effectivePosition = cropMode ? { x: 0, y: 0 } : position;

    // Combined image transform
    const imageTransform = useMemo(() => {
        const parts = [
            `translate(${effectivePosition.x}px, ${effectivePosition.y}px)`,
            `scale(${effectiveZoom})`,
        ];
        // The edit's turn and mirror are in the preview picture itself.
        if (!editMode && effectiveRotation !== 0) {
            parts.push(`rotate(${effectiveRotation}deg)`);
        }
        return parts.join(' ');
    }, [effectivePosition, effectiveZoom, effectiveRotation, editMode]);

    // Render loading state
    if (!imageSrc) {
        return (
            <div className={`flex items-center justify-center h-full bg-black ${className}`}>
                <div className="text-gray-500">{t('preview.common.noData')}</div>
            </div>
        );
    }

    return (
        <div className={`relative flex flex-col h-full bg-black ${className}`}>
            {/* Toolbar: theme-aware chrome (the image viewport below stays black) */}
            <div className="flex items-center justify-between px-4 py-2 bg-[var(--color-bg-secondary)] border-b border-[var(--color-border)]">
                <div className="flex items-center gap-2">
                    {/* Gallery paging (#128): prev/next mirror the modal hover
                        arrows and the ← → keys; disabled when there is no other
                        same-kind image in the folder. */}
                    {(onPrevious || onNext) && (
                        <>
                            <button
                                onClick={onPrevious}
                                disabled={!hasPrevious || editMode}
                                className="p-2 hover:bg-[var(--color-bg-tertiary)] rounded-lg transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
                                title={t('preview.common.previous')}
                            >
                                <ChevronLeft size={18} className="text-[var(--color-text-secondary)]" />
                            </button>
                            <button
                                onClick={onNext}
                                disabled={!hasNext || editMode}
                                className="p-2 hover:bg-[var(--color-bg-tertiary)] rounded-lg transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
                                title={t('preview.common.next')}
                            >
                                <ChevronRight size={18} className="text-[var(--color-text-secondary)]" />
                            </button>
                            <div className="w-px h-6 bg-[var(--color-border)] mx-2" />
                        </>
                    )}
                    {/* Zoom controls */}
                    <button
                        onClick={zoomOut}
                        disabled={cropMode}
                        className="p-2 hover:bg-[var(--color-bg-tertiary)] rounded-lg transition-colors disabled:opacity-40"
                        title={t('preview.image.zoomOut')}
                    >
                        <ZoomOut size={18} className="text-[var(--color-text-secondary)]" />
                    </button>
                    <span className="text-sm text-[var(--color-text-secondary)] w-16 text-center font-mono">
                        {Math.round(effectiveZoom * 100)}%
                    </span>
                    <button
                        onClick={zoomIn}
                        disabled={cropMode}
                        className="p-2 hover:bg-[var(--color-bg-tertiary)] rounded-lg transition-colors disabled:opacity-40"
                        title={t('preview.image.zoomIn')}
                    >
                        <ZoomIn size={18} className="text-[var(--color-text-secondary)]" />
                    </button>

                    <div className="w-px h-6 bg-[var(--color-border)] mx-2" />

                    {/* Rotate (view rotation: disabled in edit mode) */}
                    <button
                        onClick={rotate}
                        disabled={editMode}
                        className="p-2 hover:bg-[var(--color-bg-tertiary)] rounded-lg transition-colors disabled:opacity-40"
                        title={t('preview.image.rotate')}
                    >
                        <RotateCw size={18} className="text-[var(--color-text-secondary)]" />
                    </button>

                    {/* Fit toggle */}
                    <button
                        onClick={toggleFit}
                        disabled={cropMode}
                        className="p-2 hover:bg-[var(--color-bg-tertiary)] rounded-lg transition-colors disabled:opacity-40"
                        title={isFitToScreen ? t('preview.image.actualSize') : t('preview.image.fit')}
                    >
                        {isFitToScreen ? (
                            <Maximize2 size={18} className="text-[var(--color-text-secondary)]" />
                        ) : (
                            <Minimize2 size={18} className="text-[var(--color-text-secondary)]" />
                        )}
                    </button>

                    <div className="w-px h-6 bg-[var(--color-border)] mx-2" />

                    {/* Color Picker (disabled in edit mode) */}
                    <button
                        onClick={() => { setColorPickMode(p => !p); setPickedColor(null); }}
                        disabled={editMode}
                        className={`p-2 rounded-lg transition-colors flex items-center gap-1.5 disabled:opacity-40 ${colorPickMode ? 'bg-purple-500/20 text-purple-400' : 'hover:bg-[var(--color-bg-tertiary)] text-[var(--color-text-secondary)]'}`}
                        title={colorPickMode ? t('preview.image.cancelPick') : t('preview.image.pickColor')}
                    >
                        <Pipette size={18} />
                        {pickedColor && (
                            <span className="flex items-center gap-1 text-xs font-mono">
                                <span className="w-4 h-4 rounded border border-[var(--color-border-strong)] inline-block" style={{ backgroundColor: pickedColor }} />
                                {pickedColor}
                            </span>
                        )}
                    </button>

                    {/* Transparency background cycle (discussion #270): toggles
                        the colour shown behind a transparent image. Persisted and
                        synced with Settings > Appearance. */}
                    <button
                        onClick={cyclePreviewBg}
                        className="p-2 rounded-lg transition-colors hover:bg-[var(--color-bg-tertiary)] text-[var(--color-text-secondary)]"
                        title={t('preview.image.toggleTransparencyBg')}
                    >
                        <SquareDashedBottom size={18} />
                    </button>

                    {/* Undo / Redo / Save, while editing */}
                    {editMode && (
                        <>
                            <div className="w-px h-6 bg-[var(--color-border)] mx-2" />
                            <button
                                onClick={handleUndo}
                                disabled={!canUndo}
                                data-image-undo
                                className="p-2 hover:bg-[var(--color-bg-tertiary)] rounded-lg transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
                                title={`${t('preview.image.edit.undo')} (Ctrl+Z)`}
                            >
                                <Undo2 size={18} className="text-[var(--color-text-secondary)]" />
                            </button>
                            <button
                                onClick={handleRedo}
                                disabled={!canRedo}
                                data-image-redo
                                className="p-2 hover:bg-[var(--color-bg-tertiary)] rounded-lg transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
                                title={`${t('preview.image.edit.redo')} (Ctrl+Y)`}
                            >
                                <Redo2 size={18} className="text-[var(--color-text-secondary)]" />
                            </button>
                            <button
                                onClick={requestSave}
                                data-image-save
                                className={`ml-1 px-3 py-1.5 rounded-lg transition-colors flex items-center gap-1.5 text-xs font-medium ${dirty ? 'bg-green-600 hover:bg-green-500 text-white' : 'bg-[var(--color-bg-tertiary)] hover:bg-[var(--color-surface-hover)] text-[var(--color-text-secondary)]'}`}
                                title={`${t('preview.image.edit.saveTitle')} (Ctrl+S)`}
                            >
                                <Save size={16} />
                                <span>{t('preview.image.edit.saveTitle')}{dirty ? ' *' : ''}</span>
                            </button>
                        </>
                    )}

                    {/* Edit button (local files only) */}
                    {canEdit && (
                        <>
                            <div className="w-px h-6 bg-[var(--color-border)] mx-2" />
                            <button
                                onClick={toggleEditMode}
                                className={`p-2 rounded-lg transition-colors flex items-center gap-1.5 ${editMode ? 'bg-blue-500/20 text-blue-400' : 'hover:bg-[var(--color-bg-tertiary)] text-[var(--color-text-secondary)]'}`}
                                title={t('preview.image.edit.editImage') || 'Edit Image'}
                            >
                                {editMode ? <X size={18} /> : <Pencil size={18} />}
                                <span className="text-xs">
                                    {editMode
                                        ? (t('preview.image.edit.exitEdit') || 'Exit')
                                        : (t('preview.image.edit.editImage') || 'Edit')}
                                </span>
                            </button>
                        </>
                    )}
                </div>

                {/* Image info: the size being edited, after an applied crop */}
                {editorMetadata && (
                    <div className="text-xs text-[var(--color-text-tertiary)] font-mono">
                        {editorMetadata.width} × {editorMetadata.height} • {editorMetadata.format}
                    </div>
                )}
            </div>

            {/* Main content: image + optional editor sidebar */}
            <div className="flex-1 flex overflow-hidden">
                {/* Image container */}
                <div
                    ref={containerRef}
                    style={previewBgStyle}
                    className={`flex-1 overflow-hidden flex items-center justify-center relative ${
                        colorPickMode ? 'cursor-crosshair' :
                        cropMode ? 'cursor-default' :
                        isDragging ? 'cursor-grabbing' :
                        zoom > 1 ? 'cursor-grab' : 'cursor-default'
                    }`}
                    onMouseDown={handleMouseDown}
                    onMouseMove={handleMouseMove}
                    onMouseUp={handleMouseUp}
                    onMouseLeave={handleMouseUp}
                >
                    {/* Loading indicator */}
                    {/* The picture leaving, under the one arriving */}
                    {outgoingSrc && !imageError && (
                        <img
                            src={outgoingSrc}
                            alt=""
                            aria-hidden
                            data-image-outgoing
                            className="absolute max-w-full max-h-full select-none pointer-events-none"
                            style={{ objectFit: 'contain' }}
                            draggable={false}
                        />
                    )}

                    {!imageLoaded && !imageError && !outgoingSrc && (
                        <div className="absolute inset-0 flex items-center justify-center">
                            <div className="w-10 h-10 border-2 border-blue-500 border-t-transparent rounded-full animate-spin" />
                        </div>
                    )}

                    {/* Error state */}
                    {imageError && (
                        <div className="text-red-400 text-center">
                            <div className="text-4xl mb-2">!</div>
                            <div>{t('preview.image.loadFailed')}</div>
                        </div>
                    )}

                    {/* Image */}
                    <img
                        ref={imageRef}
                        src={displaySrc}
                        alt={file.name}
                        onClick={handleColorPick}
                        className={`relative max-w-full max-h-full select-none ${imageLoaded ? 'opacity-100' : 'opacity-0'}`}
                        style={{
                            transform: imageTransform,
                            // Transform only: the compositor animates it without layout.
                            transition: isDragging || gestureActive || cropMode
                                ? 'opacity 150ms ease-out'
                                : 'opacity 150ms ease-out, transform 180ms cubic-bezier(0.2, 0.7, 0.2, 1)',
                            willChange: zoom !== 1 || isDragging ? 'transform' : undefined,
                            transformOrigin: 'center center',
                            objectFit: (isFitToScreen || cropMode) ? 'contain' : 'none',
                            filter: cssFilter,
                        }}
                        onLoad={handleImageLoad}
                        onError={handleImageError}
                        draggable={false}
                    />

                    {/* Crop overlay */}
                    {cropMode && imageLoaded && previewCurrent && (
                        <CropOverlay
                            imageRef={imageRef}
                            aspectRatio={cropAspect}
                            naturalSize={shownSize}
                            initialCrop={null}
                            onCropChange={handleCropChange}
                            onApply={applyCropDraft}
                            onCancel={cancelCrop}
                        />
                    )}

                    {/* Pan indicator when zoomed (hidden during crop/edit) */}
                    {zoom > 1 && !cropMode && !editMode && (
                        <div className="absolute bottom-4 left-1/2 -translate-x-1/2 flex items-center gap-2 px-3 py-1.5 bg-gray-800/90 rounded-full text-xs text-gray-400">
                            <Move size={14} />
                            <span>{t('preview.image.dragToPan')}</span>
                        </div>
                    )}
                </div>

                {/* Editor sidebar */}
                {editMode && editorMetadata && (
                    <ImageEditor
                        file={file}
                        metadata={editorMetadata}
                        editState={editState}
                        onEditStateChange={handleEditStateChange}
                        onCropModeToggle={handleCropModeToggle}
                        onCropApply={applyCropDraft}
                        onCropCancel={cancelCrop}
                        cropAspect={cropAspect}
                        onCropAspectChange={setCropAspect}
                        shownSize={shownSize}
                        cropMode={cropMode}
                        dirty={dirty}
                        onSaveRequest={requestSave}
                    />
                )}
            </div>

            {/* Save dialog */}
            <ImageSaveDialog
                isOpen={saveDialogOpen}
                filePath={file.path}
                fileName={file.name}
                editState={editState}
                originalDimensions={original}
                onSaved={handleSaveResult}
                onClose={() => setSaveDialogOpen(false)}
            />

            {exitGuard.confirmOpen && exitGuard.confirmKind && (
                <GuardedCloseConfirm
                    kind={exitGuard.confirmKind}
                    onKeep={exitGuard.cancelConfirm}
                    onConfirm={exitGuard.confirmAndClose}
                />
            )}
        </div>
    );
};

export default ImageViewer;
