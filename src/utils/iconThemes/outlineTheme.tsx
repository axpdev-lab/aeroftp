/**
 * Outline Icon Theme — Lucide stroke icons with color-coded file types
 * This is the default icon theme, matching the original AeroFTP icon set.
 */

import React from 'react';
import {
    Archive, Image, Video, Music, FileCode, Code, Globe,
    FileType, Database, FileSpreadsheet, FileText, Shield, Folder, FolderUp
} from 'lucide-react';
import type { IconThemeProvider, FileIconResult } from './types';

const iconMap: Record<string, { Icon: React.ElementType; color: string }> = {
    // Vaults
    'aerovault': { Icon: Shield, color: 'text-emerald-400' },
    // Archives
    'zip': { Icon: Archive, color: 'text-yellow-600' },
    'rar': { Icon: Archive, color: 'text-yellow-600' },
    'tar': { Icon: Archive, color: 'text-yellow-600' },
    'gz': { Icon: Archive, color: 'text-yellow-600' },
    '7z': { Icon: Archive, color: 'text-yellow-600' },
    // Images
    'jpg': { Icon: Image, color: 'text-pink-400' },
    'jpeg': { Icon: Image, color: 'text-pink-400' },
    'png': { Icon: Image, color: 'text-pink-400' },
    'gif': { Icon: Image, color: 'text-pink-400' },
    'svg': { Icon: Image, color: 'text-orange-400' },
    'webp': { Icon: Image, color: 'text-pink-400' },
    'ico': { Icon: Image, color: 'text-pink-300' },
    'bmp': { Icon: Image, color: 'text-pink-400' },
    // Video
    'mp4': { Icon: Video, color: 'text-purple-400' },
    'webm': { Icon: Video, color: 'text-purple-400' },
    'avi': { Icon: Video, color: 'text-purple-400' },
    'mkv': { Icon: Video, color: 'text-purple-400' },
    'mov': { Icon: Video, color: 'text-purple-400' },
    // Audio
    'mp3': { Icon: Music, color: 'text-green-400' },
    'wav': { Icon: Music, color: 'text-green-400' },
    'flac': { Icon: Music, color: 'text-green-400' },
    'ogg': { Icon: Music, color: 'text-green-400' },
    'm4a': { Icon: Music, color: 'text-green-400' },
    'aac': { Icon: Music, color: 'text-green-400' },
    // Code
    'php': { Icon: FileCode, color: 'text-purple-500' },
    'js': { Icon: FileCode, color: 'text-yellow-400' },
    'jsx': { Icon: FileCode, color: 'text-yellow-400' },
    'ts': { Icon: FileCode, color: 'text-blue-500' },
    'tsx': { Icon: FileCode, color: 'text-blue-500' },
    'py': { Icon: FileCode, color: 'text-yellow-400' },
    'rb': { Icon: FileCode, color: 'text-red-500' },
    'go': { Icon: FileCode, color: 'text-cyan-400' },
    'rs': { Icon: FileCode, color: 'text-orange-600' },
    'java': { Icon: FileCode, color: 'text-red-400' },
    'c': { Icon: FileCode, color: 'text-blue-400' },
    'cpp': { Icon: FileCode, color: 'text-blue-500' },
    'h': { Icon: FileCode, color: 'text-purple-400' },
    'cs': { Icon: FileCode, color: 'text-green-500' },
    'swift': { Icon: FileCode, color: 'text-orange-500' },
    'kt': { Icon: FileCode, color: 'text-purple-500' },
    'vue': { Icon: FileCode, color: 'text-green-500' },
    'svelte': { Icon: FileCode, color: 'text-orange-600' },
    // CSS/Style
    'css': { Icon: Code, color: 'text-blue-400' },
    'scss': { Icon: Code, color: 'text-pink-400' },
    'sass': { Icon: Code, color: 'text-pink-400' },
    'less': { Icon: Code, color: 'text-blue-300' },
    // HTML
    'html': { Icon: Globe, color: 'text-orange-500' },
    'htm': { Icon: Globe, color: 'text-orange-500' },
    // Data
    'json': { Icon: FileType, color: 'text-yellow-500' },
    'xml': { Icon: FileType, color: 'text-orange-400' },
    'yaml': { Icon: FileType, color: 'text-red-400' },
    'yml': { Icon: FileType, color: 'text-red-400' },
    'sql': { Icon: Database, color: 'text-cyan-500' },
    // Spreadsheets
    'xls': { Icon: FileSpreadsheet, color: 'text-green-600' },
    'xlsx': { Icon: FileSpreadsheet, color: 'text-green-600' },
    'csv': { Icon: FileSpreadsheet, color: 'text-green-500' },
};

const colorMap: Record<string, string> = {
    'md': 'text-blue-300',
    'txt': 'text-gray-400',
    'pdf': 'text-red-500',
    'doc': 'text-blue-600',
    'docx': 'text-blue-600',
    'ppt': 'text-orange-500',
    'pptx': 'text-orange-500',
    'toml': 'text-gray-400',
    'ini': 'text-gray-400',
    'env': 'text-yellow-600',
    'sh': 'text-green-500',
    'bash': 'text-green-500',
    'zsh': 'text-green-500',
    'gitignore': 'text-orange-500',
    'gitattributes': 'text-orange-500',
    'lock': 'text-gray-500',
    'log': 'text-gray-400',
    'htaccess': 'text-green-600',
};

export const outlineTheme: IconThemeProvider = {
    id: 'outline',

    getFileIcon: (filename: string, size: number = 16): FileIconResult => {
        const ext = filename.split('.').pop()?.toLowerCase() || '';
        const entry = iconMap[ext];
        if (entry) {
            const { Icon, color } = entry;
            return { icon: <Icon size={size} className={color} />, color };
        }
        const color = colorMap[ext] || 'text-gray-400';
        return { icon: <FileText size={size} className={color} />, color };
    },

    getFolderIcon: (size: number = 16): FileIconResult => {
        return { icon: <Folder size={size} className="text-yellow-500" />, color: 'text-yellow-500' };
    },

    getFolderUpIcon: (size: number = 16): FileIconResult => {
        return { icon: <FolderUp size={size} className="text-gray-400" />, color: 'text-gray-400' };
    },
};
