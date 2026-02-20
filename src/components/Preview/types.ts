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

// All editable state — used with useReducer for clean reset
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
