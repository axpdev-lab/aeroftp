/**
 * Minimal Icon Theme — Monochrome Lucide icons with theme-adaptive accent color
 *
 * Uses the same Lucide icon shapes as Outline, but renders all icons
 * in a single accent color that adapts to the current app theme.
 */

import React from 'react';
import {
    Archive, Image, Video, Music, FileCode, Code, Globe,
    FileType, Database, FileSpreadsheet, FileText, Shield, Folder, FolderUp
} from 'lucide-react';
import type { IconThemeProvider, FileIconResult } from './types';

export type EffectiveTheme = 'light' | 'dark' | 'tokyo' | 'cyber';

const THEME_ACCENT: Record<EffectiveTheme, string> = {
    light: 'text-gray-500',
    dark: 'text-gray-400',
    tokyo: 'text-purple-400',
    cyber: 'text-emerald-400',
};

const THEME_FOLDER_ACCENT: Record<EffectiveTheme, string> = {
    light: 'text-gray-400',
    dark: 'text-gray-500',
    tokyo: 'text-purple-300',
    cyber: 'text-emerald-300',
};

const extIconMap: Record<string, React.ElementType> = {
    'aerovault': Shield,
    // Archives
    'zip': Archive, 'rar': Archive, 'tar': Archive, 'gz': Archive, '7z': Archive,
    // Images
    'jpg': Image, 'jpeg': Image, 'png': Image, 'gif': Image,
    'svg': Image, 'webp': Image, 'ico': Image, 'bmp': Image,
    // Video
    'mp4': Video, 'webm': Video, 'avi': Video, 'mkv': Video, 'mov': Video,
    // Audio
    'mp3': Music, 'wav': Music, 'flac': Music, 'ogg': Music, 'm4a': Music, 'aac': Music,
    // Code
    'php': FileCode, 'js': FileCode, 'jsx': FileCode, 'ts': FileCode, 'tsx': FileCode,
    'py': FileCode, 'rb': FileCode, 'go': FileCode, 'rs': FileCode, 'java': FileCode,
    'c': FileCode, 'cpp': FileCode, 'h': FileCode, 'cs': FileCode, 'swift': FileCode,
    'kt': FileCode, 'vue': FileCode, 'svelte': FileCode,
    // CSS/Style
    'css': Code, 'scss': Code, 'sass': Code, 'less': Code,
    // HTML
    'html': Globe, 'htm': Globe,
    // Data
    'json': FileType, 'xml': FileType, 'yaml': FileType, 'yml': FileType,
    'sql': Database,
    // Spreadsheets
    'xls': FileSpreadsheet, 'xlsx': FileSpreadsheet, 'csv': FileSpreadsheet,
};

export const createMinimalTheme = (effectiveTheme: EffectiveTheme): IconThemeProvider => {
    const accent = THEME_ACCENT[effectiveTheme];
    const folderAccent = THEME_FOLDER_ACCENT[effectiveTheme];

    return {
        id: 'minimal',

        getFileIcon: (filename: string, size: number = 16): FileIconResult => {
            const ext = filename.split('.').pop()?.toLowerCase() || '';
            const Icon = extIconMap[ext] || FileText;
            return { icon: <Icon size={size} className={accent} />, color: accent };
        },

        getFolderIcon: (size: number = 16): FileIconResult => {
            return { icon: <Folder size={size} className={folderAccent} />, color: folderAccent };
        },

        getFolderUpIcon: (size: number = 16): FileIconResult => {
            return { icon: <FolderUp size={size} className={folderAccent} />, color: folderAccent };
        },
    };
};
