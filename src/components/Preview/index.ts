/**
 * Universal Preview System - Public API
 * 
 * Single entry point for importing preview components.
 * This allows easy refactoring without changing import paths.
 */

// Main component
export { UniversalPreview } from './UniversalPreview';
export type { UniversalPreviewProps } from './types';

// Types
export type {
    PreviewFileData,
    PreviewCategory,
    MediaMetadata,
    ImageMetadata,
    PlaybackState,
    EqualizerState,
    EQPreset,
    StreamProgress,
    ViewerBaseProps,
} from './types';

// Utilities
export {
    getPreviewCategory,
    isPreviewable,
    isCodeFile,
    getFileExtension,
    getMimeType,
    formatFileSize,
    formatDuration,
    getCategoryIcon,
} from './utils/fileTypes';

// Viewers (export individually as they're implemented)
export { ImageViewer } from './viewers/ImageViewer';
export { AudioPlayer } from './viewers/AudioPlayer';
export { VideoPlayer } from './viewers/VideoPlayer';
export { PDFViewer } from './viewers/PDFViewer';
export { TextViewer } from './viewers/TextViewer';

// Audio controls
export { AudioVisualizer } from './controls/AudioVisualizer';
export { AudioMixer, EQ_BANDS, EQ_PRESETS } from './controls/AudioMixer';
