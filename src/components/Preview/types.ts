// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Universal Preview System - Type Definitions
 * 
 * Centralized types for the preview system to ensure consistency
 * and enable easy refactoring.
 */

// Supported file categories
export type PreviewCategory = 'image' | 'audio' | 'video' | 'pdf' | 'markdown' | 'text' | 'code' | 'unknown';

// File metadata for preview
export interface PreviewFileData {
    name: string;
    path: string;
    size: number;
    isRemote: boolean;
    mimeType?: string;
    content?: string | ArrayBuffer;
    blobUrl?: string;
    modified?: string;
    /** True while the bytes are still being fetched. The modal opens
     *  immediately and shows a pulsing skeleton until the content is ready,
     *  so a click feels responsive instead of "nothing happening" (#128). */
    loading?: boolean;
    /** Preview-load failure (too large / fetch error) rendered inside the
     *  modal, rather than only a transient toast that lands in the hidden
     *  activity log (#128). */
    error?: string;
}

// Media metadata (audio/video)
export interface MediaMetadata {
    title?: string;
    artist?: string;
    album?: string;
    year?: string;
    genre?: string;
    duration?: number;
    bitrate?: number;
    sampleRate?: number;
    channels?: number;
    codec?: string;
    coverArt?: string; // Base64 or URL
}

// Image metadata (EXIF)
export interface ImageMetadata {
    width: number;
    height: number;
    format: string;
    colorSpace?: string;
    camera?: string;
    dateTaken?: string;
    gps?: { lat: number; lng: number };
}

// PDF metadata
export interface PDFMetadata {
    title?: string;
    author?: string;
    pages: number;
    createdDate?: string;
}

// Playback state for audio/video
export interface PlaybackState {
    isPlaying: boolean;
    currentTime: number;
    duration: number;
    volume: number;
    isMuted: boolean;
    playbackRate: number;
    isLooping: boolean;
    bufferedPercent: number;
}

// Equalizer preset
export interface EQPreset {
    name: string;
    bands: number[]; // 10 bands: 32Hz to 16kHz
}

// Equalizer state
export interface EqualizerState {
    enabled: boolean;
    bands: number[]; // -12 to +12 dB for each band
    balance: number; // -1 (left) to +1 (right)
    presetName: string;
}

// Stream progress for remote files
export interface StreamProgress {
    loaded: number;
    total: number;
    percent: number;
    isComplete: boolean;
}

// Preview modal props
export interface UniversalPreviewProps {
    isOpen: boolean;
    file: PreviewFileData | null;
    onClose: () => void;
    onDownload?: () => void;
    /** Open the current file in the AeroTools editor. Shown only for
     *  source-viewable files (text, markdown, code). */
    onEdit?: () => void;
    onNext?: () => void;
    onPrevious?: () => void;
    hasNext?: boolean;
    hasPrevious?: boolean;
}

// Viewer component base props
export interface ViewerBaseProps {
    file: PreviewFileData;
    onError?: (error: string) => void;
}

// ─── AeroImage Editor Types ─────────────────────────────────────────────────

// Crop rectangle in natural image pixels
export interface CropRect {
    x: number;
    y: number;
    width: number;
    height: number;
}

// All editable state: used with useReducer for clean reset
export interface EditState {
    crop: CropRect | null;
    resize: { width: number; height: number } | null;
    rotation: 0 | 90 | 180 | 270;
    flipH: boolean;
    flipV: boolean;
    brightness: number;   // -100 to +100
    contrast: number;     // -100 to +100
    hue: number;          // -180 to +180
    blur: number;         // 0 to 10
    sharpen: number;      // 0 to 10
    grayscale: boolean;
    invert: boolean;
}

// Image operation sent to Rust backend
export interface ImageOperation {
    type: string;
    [key: string]: unknown;
}

// Result from process_image command
export interface ImageResult {
    path: string;
    width: number;
    height: number;
    size: number;
    format: string;
}

// Initial edit state (all defaults)
export const INITIAL_EDIT_STATE: EditState = {
    crop: null,
    resize: null,
    rotation: 0,
    flipH: false,
    flipV: false,
    brightness: 0,
    contrast: 0,
    hue: 0,
    blur: 0,
    sharpen: 0,
    grayscale: false,
    invert: false,
};

// Supported output formats for Save As
export const OUTPUT_FORMATS = [
    { value: 'png', label: 'PNG' },
    { value: 'jpg', label: 'JPEG' },
    { value: 'webp', label: 'WebP' },
    { value: 'bmp', label: 'BMP' },
    { value: 'tiff', label: 'TIFF' },
    { value: 'gif', label: 'GIF' },
] as const;

// Build the operations pipeline from EditState
export function buildOperations(state: EditState): ImageOperation[] {
    const ops: ImageOperation[] = [];
    if (state.crop) ops.push({ type: 'Crop', ...state.crop });
    if (state.resize) ops.push({ type: 'Resize', ...state.resize });
    if (state.rotation === 90) ops.push({ type: 'Rotate90' });
    if (state.rotation === 180) ops.push({ type: 'Rotate180' });
    if (state.rotation === 270) ops.push({ type: 'Rotate270' });
    if (state.flipH) ops.push({ type: 'FlipH' });
    if (state.flipV) ops.push({ type: 'FlipV' });
    if (state.brightness !== 0) ops.push({ type: 'Brightness', value: state.brightness });
    if (state.contrast !== 0) ops.push({ type: 'Contrast', value: state.contrast });
    if (state.blur > 0) ops.push({ type: 'Blur', sigma: state.blur });
    if (state.sharpen > 0) ops.push({ type: 'Sharpen', sigma: state.sharpen });
    if (state.grayscale) ops.push({ type: 'Grayscale' });
    if (state.invert) ops.push({ type: 'Invert' });
    if (state.hue !== 0) ops.push({ type: 'HueRotate', degrees: state.hue });
    return ops;
}

// ─── Lossless / lossy classification (#270, requested by Ehud) ─────────────
// Two independent dimensions decide whether an edit keeps the picture intact:
//   1. the operation itself (is it reversible?), and
//   2. the output container the result is re-encoded into.
// A "lossless" operation only stays lossless when written to a lossless format;
// any save to a lossy format re-encodes every pixel and erases that guarantee.

export type LossKind = 'lossless' | 'lossy';

// Output containers, classified for the AeroImage backend (image_edit.rs):
// JPEG uses a quality-based encoder and GIF quantises truecolour down to a
// 256-colour palette (both lossy); every other format round-trips pixels
// exactly through image::save.
const FORMAT_LOSS: Record<string, LossKind> = {
    png: 'lossless',
    webp: 'lossless',
    bmp: 'lossless',
    tiff: 'lossless',
    jpg: 'lossy',
    jpeg: 'lossy',
    gif: 'lossy',
};

/** Lossless/lossy nature of an output container (defaults to lossy when unknown). */
export function formatLossKind(format: string): LossKind {
    return FORMAT_LOSS[format.toLowerCase()] ?? 'lossy';
}

/** True when saving a lossy source as a DIFFERENT lossy format stacks a
 * second generation of loss (the guarded lossy->lossy re-encode). Pass-through
 * (same format) and any lossless target add no new loss; a lossless source only
 * ever earns an informational note, never this guard. */
export function isLossyReencode(sourceFormat: string, targetFormat: string): boolean {
    const src = sourceFormat.toLowerCase();
    const tgt = targetFormat.toLowerCase();
    if (src === tgt) return false; // pass-through, no conversion
    if (formatLossKind(tgt) === 'lossless') return false; // lossless target: no NEW loss
    return formatLossKind(src) === 'lossy'; // lossy -> lossy re-encode
}

// Individual operations. Geometry permutations (rotate by a right angle, flip)
// and a colour inversion are bijective, so they preserve every pixel value;
// resampling (resize) and colour math (brightness/contrast/hue/blur/sharpen/
// grayscale) clip and round, and cannot be undone exactly. Crop keeps the
// retained region bit-for-bit and only drops the area the user removed.
const OPERATION_LOSS: Record<string, LossKind> = {
    Crop: 'lossless',
    Rotate90: 'lossless',
    Rotate180: 'lossless',
    Rotate270: 'lossless',
    FlipH: 'lossless',
    FlipV: 'lossless',
    Invert: 'lossless',
    Resize: 'lossy',
    Brightness: 'lossy',
    Contrast: 'lossy',
    HueRotate: 'lossy',
    Blur: 'lossy',
    Sharpen: 'lossy',
    Grayscale: 'lossy',
};

/** Lossless/lossy nature of a single operation (defaults to lossy when unknown). */
export function operationLossKind(type: string): LossKind {
    return OPERATION_LOSS[type] ?? 'lossy';
}

/** Tally the active lossy/lossless operations for the current edit state. */
export function activeEditLoss(state: EditState): { lossless: number; lossy: number } {
    let lossless = 0;
    let lossy = 0;
    for (const op of buildOperations(state)) {
        if (operationLossKind(op.type) === 'lossy') lossy++;
        else lossless++;
    }
    return { lossless, lossy };
}
