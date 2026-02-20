/**
 * Image Viewer Component — AeroImage
 *
 * Advanced image viewer with:
 * - Zoom in/out (scroll wheel or buttons)
 * - Pan (drag when zoomed)
 * - Rotate 90° clockwise
 * - Fit to screen / Actual size toggle
 * - Color picker
 * - AeroImage editor (crop, resize, rotate, flip, adjustments, effects, save as)
 */

import React, { useRef, useState, useCallback, useEffect, useMemo } from 'react';
import { ZoomIn, ZoomOut, RotateCw, Maximize2, Minimize2, Move, Pipette, Pencil, X } from 'lucide-react';
import { ViewerBaseProps, ImageMetadata, EditState, INITIAL_EDIT_STATE, CropRect } from '../types';
import type { ImageResult } from '../types';
import { useI18n } from '../../../i18n';
import ImageEditor from './ImageEditor';
import { CropOverlay } from './CropOverlay';
import { ImageSaveDialog } from './ImageSaveDialog';

interface ImageViewerProps extends ViewerBaseProps {
    className?: string;
}

// Zoom limits
const MIN_ZOOM = 0.1;
const MAX_ZOOM = 5;
const ZOOM_STEP = 0.25;

export const ImageViewer: React.FC<ImageViewerProps> = ({
    file,
    onError,
    className = '',
}) => {
    const { t } = useI18n();
    const containerRef = useRef<HTMLDivElement>(null);
    const imageRef = useRef<HTMLImageElement>(null);

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

    // AeroImage editor state
    const [editMode, setEditMode] = useState(false);
    const [editState, setEditState] = useState<EditState>(INITIAL_EDIT_STATE);
    const [cropMode, setCropMode] = useState(false);
    const [saveDialogOpen, setSaveDialogOpen] = useState(false);

    // Image source URL
    const imageSrc = file.blobUrl || file.content as string || '';

    // Track previous src to avoid resetting on initial load
    const prevSrcRef = React.useRef<string>(imageSrc);

    // Reset state only when switching to a DIFFERENT image (not on initial load)
    useEffect(() => {
        if (prevSrcRef.current && prevSrcRef.current !== imageSrc && imageSrc) {
            setZoom(1);
            setRotation(0);
            setPosition({ x: 0, y: 0 });
            setIsFitToScreen(true);
            setImageLoaded(false);
            setImageError(false);
            setMetadata(null);
            setEditMode(false);
            setEditState(INITIAL_EDIT_STATE);
            setCropMode(false);
            setSaveDialogOpen(false);
        }
        prevSrcRef.current = imageSrc;
    }, [imageSrc]);

    // Handle image load
    const handleImageLoad = useCallback(() => {
        setImageLoaded(true);
        if (imageRef.current) {
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

    // Zoom controls
    const zoomIn = useCallback(() => {
        setZoom(prev => Math.min(MAX_ZOOM, prev + ZOOM_STEP));
        setIsFitToScreen(false);
    }, []);

    const zoomOut = useCallback(() => {
        setZoom(prev => Math.max(MIN_ZOOM, prev - ZOOM_STEP));
        setIsFitToScreen(false);
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

    // Mouse wheel zoom
    const handleWheel = useCallback((e: React.WheelEvent) => {
        if (cropMode) return;
        e.preventDefault();
        const delta = e.deltaY > 0 ? -ZOOM_STEP : ZOOM_STEP;
        setZoom(prev => Math.max(MIN_ZOOM, Math.min(MAX_ZOOM, prev + delta)));
        setIsFitToScreen(false);
    }, [cropMode]);

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
        navigator.clipboard.writeText(hex).catch(() => {});
    }, [colorPickMode]);

    // ─── AeroImage Edit Handlers ─────────────────────────────────────

    const toggleEditMode = useCallback(() => {
        if (editMode) {
            setEditMode(false);
            setEditState(INITIAL_EDIT_STATE);
            setCropMode(false);
        } else {
            setEditMode(true);
            setColorPickMode(false);
            setPickedColor(null);
        }
    }, [editMode]);

    const handleEditStateChange = useCallback((state: EditState) => {
        setEditState(state);
    }, []);

    const handleCropModeToggle = useCallback((active: boolean) => {
        setCropMode(active);
        if (active) {
            setZoom(1);
            setPosition({ x: 0, y: 0 });
            setIsFitToScreen(true);
        }
    }, []);

    const handleCropChange = useCallback((natural: CropRect) => {
        setEditState(prev => ({ ...prev, crop: natural }));
    }, []);

    const handleSaveResult = useCallback((result: ImageResult) => {
        setSaveDialogOpen(false);
        window.dispatchEvent(new CustomEvent('file-changed', {
            detail: { path: result.path },
        }));
        // If replaced original, force image reload
        if (result.path === file.path) {
            const img = imageRef.current;
            if (img) {
                const src = img.src;
                img.src = '';
                img.src = src + (src.includes('?') ? '&' : '?') + `t=${Date.now()}`;
            }
        }
    }, [file.path]);

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

    // Edit transforms (rotation + flip) — not applied during crop mode
    const editTransform = useMemo(() => {
        if (!editMode || cropMode) return '';
        const parts: string[] = [];
        if (editState.flipH) parts.push('scaleX(-1)');
        if (editState.flipV) parts.push('scaleY(-1)');
        if (editState.rotation !== 0) parts.push(`rotate(${editState.rotation}deg)`);
        return parts.join(' ');
    }, [editMode, cropMode, editState.flipH, editState.flipV, editState.rotation]);

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
        if (!editMode && effectiveRotation !== 0) {
            parts.push(`rotate(${effectiveRotation}deg)`);
        }
        if (editTransform) {
            parts.push(editTransform);
        }
        return parts.join(' ');
    }, [effectivePosition, effectiveZoom, effectiveRotation, editMode, editTransform]);

    // Render loading state
    if (!imageSrc) {
        return (
            <div className={`flex items-center justify-center h-full bg-gray-900 ${className}`}>
                <div className="text-gray-500">{t('preview.common.noData')}</div>
            </div>
        );
    }

    return (
        <div className={`relative flex flex-col h-full bg-gray-900 ${className}`}>
            {/* Toolbar */}
            <div className="flex items-center justify-between px-4 py-2 bg-gray-800/80 border-b border-gray-700">
                <div className="flex items-center gap-2">
                    {/* Zoom controls */}
                    <button
                        onClick={zoomOut}
                        disabled={cropMode}
                        className="p-2 hover:bg-gray-700 rounded-lg transition-colors disabled:opacity-40"
                        title={t('preview.image.zoomOut')}
                    >
                        <ZoomOut size={18} className="text-gray-400" />
                    </button>
                    <span className="text-sm text-gray-400 w-16 text-center font-mono">
                        {Math.round(effectiveZoom * 100)}%
                    </span>
                    <button
                        onClick={zoomIn}
                        disabled={cropMode}
                        className="p-2 hover:bg-gray-700 rounded-lg transition-colors disabled:opacity-40"
                        title={t('preview.image.zoomIn')}
                    >
                        <ZoomIn size={18} className="text-gray-400" />
                    </button>

                    <div className="w-px h-6 bg-gray-700 mx-2" />

                    {/* Rotate (view rotation — disabled in edit mode) */}
                    <button
                        onClick={rotate}
                        disabled={editMode}
                        className="p-2 hover:bg-gray-700 rounded-lg transition-colors disabled:opacity-40"
                        title={t('preview.image.rotate')}
                    >
                        <RotateCw size={18} className="text-gray-400" />
                    </button>

                    {/* Fit toggle */}
                    <button
                        onClick={toggleFit}
                        disabled={cropMode}
                        className="p-2 hover:bg-gray-700 rounded-lg transition-colors disabled:opacity-40"
                        title={isFitToScreen ? t('preview.image.actualSize') : t('preview.image.fit')}
                    >
                        {isFitToScreen ? (
                            <Maximize2 size={18} className="text-gray-400" />
                        ) : (
                            <Minimize2 size={18} className="text-gray-400" />
                        )}
                    </button>

                    <div className="w-px h-6 bg-gray-700 mx-2" />

                    {/* Color Picker (disabled in edit mode) */}
                    <button
                        onClick={() => { setColorPickMode(p => !p); setPickedColor(null); }}
                        disabled={editMode}
                        className={`p-2 rounded-lg transition-colors flex items-center gap-1.5 disabled:opacity-40 ${colorPickMode ? 'bg-purple-500/20 text-purple-400' : 'hover:bg-gray-700 text-gray-400'}`}
                        title={colorPickMode ? t('preview.image.cancelPick') : t('preview.image.pickColor')}
                    >
                        <Pipette size={18} />
                        {pickedColor && (
                            <span className="flex items-center gap-1 text-xs font-mono">
                                <span className="w-4 h-4 rounded border border-gray-600 inline-block" style={{ backgroundColor: pickedColor }} />
                                {pickedColor}
                            </span>
                        )}
                    </button>

                    {/* Edit button (local files only) */}
                    {canEdit && (
                        <>
                            <div className="w-px h-6 bg-gray-700 mx-2" />
                            <button
                                onClick={toggleEditMode}
                                className={`p-2 rounded-lg transition-colors flex items-center gap-1.5 ${editMode ? 'bg-blue-500/20 text-blue-400' : 'hover:bg-gray-700 text-gray-400'}`}
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

                {/* Image info */}
                {metadata && (
                    <div className="text-xs text-gray-500 font-mono">
                        {metadata.width} × {metadata.height} • {metadata.format}
                    </div>
                )}
            </div>

            {/* Main content: image + optional editor sidebar */}
            <div className="flex-1 flex overflow-hidden">
                {/* Image container */}
                <div
                    ref={containerRef}
                    className={`flex-1 overflow-hidden flex items-center justify-center relative ${
                        colorPickMode ? 'cursor-crosshair' :
                        cropMode ? 'cursor-default' :
                        isDragging ? 'cursor-grabbing' :
                        zoom > 1 ? 'cursor-grab' : 'cursor-default'
                    }`}
                    onWheel={handleWheel}
                    onMouseDown={handleMouseDown}
                    onMouseMove={handleMouseMove}
                    onMouseUp={handleMouseUp}
                    onMouseLeave={handleMouseUp}
                >
                    {/* Loading indicator */}
                    {!imageLoaded && !imageError && (
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
                        src={imageSrc}
                        alt={file.name}
                        onClick={handleColorPick}
                        className={`max-w-full max-h-full transition-opacity duration-300 select-none ${imageLoaded ? 'opacity-100' : 'opacity-0'}`}
                        style={{
                            transform: imageTransform,
                            transformOrigin: 'center center',
                            objectFit: (isFitToScreen || cropMode) ? 'contain' : 'none',
                            filter: cssFilter,
                        }}
                        onLoad={handleImageLoad}
                        onError={handleImageError}
                        draggable={false}
                    />

                    {/* Crop overlay */}
                    {cropMode && imageLoaded && (
                        <CropOverlay
                            imageRef={imageRef}
                            aspectRatio={null}
                            onCropChange={handleCropChange}
                            onCancel={() => setCropMode(false)}
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
                {editMode && metadata && (
                    <ImageEditor
                        file={file}
                        metadata={metadata}
                        editState={editState}
                        onEditStateChange={handleEditStateChange}
                        onCropModeToggle={handleCropModeToggle}
                        cropMode={cropMode}
                        onSaveRequest={() => setSaveDialogOpen(true)}
                    />
                )}
            </div>

            {/* Save dialog */}
            <ImageSaveDialog
                isOpen={saveDialogOpen}
                filePath={file.path}
                fileName={file.name}
                editState={editState}
                originalDimensions={{
                    width: metadata?.width ?? 0,
                    height: metadata?.height ?? 0,
                }}
                onSaved={handleSaveResult}
                onClose={() => setSaveDialogOpen(false)}
            />
        </div>
    );
};

export default ImageViewer;
