/**
 * Universal Preview System - File Type Utilities
 * 
 * Functions to detect file types and categories based on extension.
 * Designed for easy extension when adding new supported formats.
 */

import { PreviewCategory } from '../types';

// File extension mappings
const IMAGE_EXTENSIONS = ['jpg', 'jpeg', 'png', 'gif', 'webp', 'svg', 'ico', 'bmp', 'tiff', 'tif'];
const AUDIO_EXTENSIONS = ['mp3', 'wav', 'ogg', 'flac', 'aac', 'm4a', 'wma', 'opus', 'aiff'];
const VIDEO_EXTENSIONS = ['mp4', 'webm', 'mkv', 'avi', 'mov', 'wmv', 'flv', 'm4v', 'ogv'];
const PDF_EXTENSIONS = ['pdf'];
const MARKDOWN_EXTENSIONS = ['md', 'markdown', 'mdown', 'mkd'];
const TEXT_EXTENSIONS = ['txt', 'log', 'ini', 'cfg', 'conf', 'env'];
const CODE_EXTENSIONS = [
    'js', 'jsx', 'ts', 'tsx', 'html', 'htm', 'css', 'scss', 'sass', 'less',
    'json', 'xml', 'yaml', 'yml', 'toml', 'webmanifest',
    'php', 'py', 'rb', 'java', 'c', 'cpp', 'h', 'hpp', 'cs', 'go', 'rs', 'swift',
    'sql', 'sh', 'bash', 'zsh', 'fish', 'ps1', 'bat', 'cmd',
    'vue', 'svelte', 'astro',
    'htaccess', 'config', 'conf'
];

/**
 * Get file extension from filename
 */
export function getFileExtension(filename: string): string {
    const parts = filename.toLowerCase().split('.');
    if (parts.length > 1) return parts.pop() || '';
    // Handle files like .htaccess where parts=['', 'htaccess']
    if (filename.startsWith('.')) return filename.substring(1).toLowerCase();
    return '';
}

/**
 * Determine the preview category for a file
 */
export function getPreviewCategory(filename: string): PreviewCategory {
    const ext = getFileExtension(filename);

    if (IMAGE_EXTENSIONS.includes(ext)) return 'image';
    if (AUDIO_EXTENSIONS.includes(ext)) return 'audio';
    if (VIDEO_EXTENSIONS.includes(ext)) return 'video';
    if (PDF_EXTENSIONS.includes(ext)) return 'pdf';
    if (MARKDOWN_EXTENSIONS.includes(ext)) return 'markdown';
    if (TEXT_EXTENSIONS.includes(ext)) return 'text';
    if (CODE_EXTENSIONS.includes(ext)) return 'code';

    return 'unknown';
}

/**
 * Check if file is previewable in Universal Preview (media, pdf, text, code)
 */
export function isPreviewable(filename: string): boolean {
    const category = getPreviewCategory(filename);
    return ['image', 'audio', 'video', 'pdf', 'text', 'markdown', 'code'].includes(category);
}

/**
 * Check if file can be viewed as source (code, text, markdown)
 */
export function isSourceViewable(filename: string): boolean {
    const category = getPreviewCategory(filename);
    return ['code', 'text', 'markdown'].includes(category);
}

/**
 * Check if file should open in code editor
 */
export function isCodeFile(filename: string): boolean {
    return getPreviewCategory(filename) === 'code';
}

/**
 * Get MIME type for file
 */
export function getMimeType(filename: string): string {
    const ext = getFileExtension(filename);

    const mimeMap: Record<string, string> = {
        // Images
        jpg: 'image/jpeg', jpeg: 'image/jpeg', png: 'image/png',
        gif: 'image/gif', webp: 'image/webp', svg: 'image/svg+xml',
        ico: 'image/x-icon', bmp: 'image/bmp',
        // Audio
        mp3: 'audio/mpeg', wav: 'audio/wav', ogg: 'audio/ogg',
        flac: 'audio/flac', aac: 'audio/aac', m4a: 'audio/mp4',
        // Video
        mp4: 'video/mp4', webm: 'video/webm', mkv: 'video/x-matroska',
        avi: 'video/x-msvideo', mov: 'video/quicktime',
        // Documents
        pdf: 'application/pdf',
        // Text
        txt: 'text/plain', md: 'text/markdown', html: 'text/html',
        css: 'text/css', js: 'text/javascript', json: 'application/json',
    };

    return mimeMap[ext] || 'application/octet-stream';
}

/**
 * Format file size for display
 */
export function formatFileSize(bytes: number): string {
    if (bytes === 0) return '0 B';
    const k = 1024;
    const sizes = ['B', 'KB', 'MB', 'GB', 'TB'];
    const i = Math.floor(Math.log(bytes) / Math.log(k));
    return parseFloat((bytes / Math.pow(k, i)).toFixed(2)) + ' ' + sizes[i];
}

/**
 * Format duration (seconds) for display
 */
export function formatDuration(seconds: number): string {
    if (!seconds || isNaN(seconds)) return '0:00';

    const hours = Math.floor(seconds / 3600);
    const minutes = Math.floor((seconds % 3600) / 60);
    const secs = Math.floor(seconds % 60);

    if (hours > 0) {
        return `${hours}:${minutes.toString().padStart(2, '0')}:${secs.toString().padStart(2, '0')}`;
    }
    return `${minutes}:${secs.toString().padStart(2, '0')}`;
}

/**
 * Get icon name for file category
 */
export function getCategoryIcon(category: PreviewCategory): string {
    const icons: Record<PreviewCategory, string> = {
        image: 'Image',
        audio: 'Music',
        video: 'Video',
        pdf: 'FileText',
        markdown: 'FileText',
        text: 'FileText',
        code: 'Code',
        unknown: 'File',
    };
    return icons[category];
}
