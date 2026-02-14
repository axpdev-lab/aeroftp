/**
 * Filled Icon Theme — Colored document shapes with type badges
 *
 * Each file type gets a filled document SVG with a colored background
 * and a text badge (e.g. "JS", "PDF", "W" for Word).
 *
 * Architecture: The `badgeContent` prop on FilledDocIcon accepts ReactNode,
 * enabling future replacement of text badges with official file type icons
 * (SVG/PNG) without changing the component structure.
 */

import React from 'react';
import type { IconThemeProvider, FileIconResult } from './types';

// --- SVG Components ---

interface FilledDocIconProps {
    size: number;
    bgColor: string;
    badge: string;
    badgeColor?: string;
    badgeContent?: React.ReactNode;
}

const FilledDocIcon: React.FC<FilledDocIconProps> = ({
    size, bgColor, badge, badgeColor = '#ffffff', badgeContent
}) => (
    <svg width={size} height={size} viewBox="0 0 32 32" fill="none">
        <path d="M6 2h14l8 8v18a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2z" fill={bgColor} />
        <path d="M20 2l8 8h-6a2 2 0 0 1-2-2V2z" fill="rgba(0,0,0,0.15)" />
        {badgeContent || (
            <text
                x="14" y="23"
                textAnchor="middle"
                fill={badgeColor}
                fontSize={badge.length > 3 ? '7' : badge.length > 2 ? '8' : badge.length > 1 ? '10' : '13'}
                fontWeight="bold"
                fontFamily="system-ui,-apple-system,sans-serif"
            >
                {badge}
            </text>
        )}
    </svg>
);

const FilledFolderIcon: React.FC<{ size: number; color?: string }> = ({
    size, color = '#f59e0b'
}) => (
    <svg width={size} height={size} viewBox="0 0 32 32" fill="none">
        <path d="M2 7a2 2 0 0 1 2-2h8l3 3h13a2 2 0 0 1 2 2v16a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V7z" fill={color} />
        <path d="M2 11h28v15a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V11z" fill={color} opacity="0.85" />
    </svg>
);

const FilledFolderUpIcon: React.FC<{ size: number }> = ({ size }) => (
    <svg width={size} height={size} viewBox="0 0 32 32" fill="none">
        <path d="M2 7a2 2 0 0 1 2-2h8l3 3h13a2 2 0 0 1 2 2v16a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V7z" fill="#9ca3af" />
        <path d="M2 11h28v15a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V11z" fill="#9ca3af" opacity="0.85" />
        <path d="M16 15l-5 5h3v4h4v-4h3l-5-5z" fill="#ffffff" opacity="0.7" />
    </svg>
);

// --- Extension → { bgColor, badge } map ---

const filledIconMap: Record<string, { bgColor: string; badge: string }> = {
    // Code — language colors
    'js':     { bgColor: '#eab308', badge: 'JS' },
    'jsx':    { bgColor: '#eab308', badge: 'JSX' },
    'ts':     { bgColor: '#3b82f6', badge: 'TS' },
    'tsx':    { bgColor: '#3b82f6', badge: 'TSX' },
    'py':     { bgColor: '#3b82f6', badge: 'PY' },
    'rs':     { bgColor: '#ea580c', badge: 'RS' },
    'go':     { bgColor: '#06b6d4', badge: 'GO' },
    'java':   { bgColor: '#ef4444', badge: 'J' },
    'php':    { bgColor: '#a855f7', badge: 'PHP' },
    'rb':     { bgColor: '#ef4444', badge: 'RB' },
    'c':      { bgColor: '#60a5fa', badge: 'C' },
    'cpp':    { bgColor: '#3b82f6', badge: 'C++' },
    'h':      { bgColor: '#a855f7', badge: 'H' },
    'cs':     { bgColor: '#22c55e', badge: 'C#' },
    'swift':  { bgColor: '#f97316', badge: 'SW' },
    'kt':     { bgColor: '#a855f7', badge: 'KT' },
    'vue':    { bgColor: '#22c55e', badge: 'VUE' },
    'svelte': { bgColor: '#ea580c', badge: 'SV' },
    // Web
    'html':   { bgColor: '#f97316', badge: 'H' },
    'htm':    { bgColor: '#f97316', badge: 'H' },
    'css':    { bgColor: '#3b82f6', badge: 'CSS' },
    'scss':   { bgColor: '#ec4899', badge: 'SS' },
    'sass':   { bgColor: '#ec4899', badge: 'SA' },
    'less':   { bgColor: '#60a5fa', badge: 'LS' },
    // Data / Config
    'json':   { bgColor: '#eab308', badge: '{ }' },
    'xml':    { bgColor: '#f97316', badge: 'XML' },
    'yaml':   { bgColor: '#ef4444', badge: 'YML' },
    'yml':    { bgColor: '#ef4444', badge: 'YML' },
    'toml':   { bgColor: '#9ca3af', badge: 'TML' },
    'ini':    { bgColor: '#9ca3af', badge: 'INI' },
    'env':    { bgColor: '#ca8a04', badge: 'ENV' },
    'sql':    { bgColor: '#06b6d4', badge: 'SQL' },
    // Documents — Office colors
    'pdf':    { bgColor: '#dc2626', badge: 'PDF' },
    'doc':    { bgColor: '#2563eb', badge: 'W' },
    'docx':   { bgColor: '#2563eb', badge: 'W' },
    'xls':    { bgColor: '#16a34a', badge: 'X' },
    'xlsx':   { bgColor: '#16a34a', badge: 'X' },
    'csv':    { bgColor: '#22c55e', badge: 'CSV' },
    'ppt':    { bgColor: '#ea580c', badge: 'P' },
    'pptx':   { bgColor: '#ea580c', badge: 'P' },
    'odt':    { bgColor: '#2563eb', badge: 'ODT' },
    'ods':    { bgColor: '#16a34a', badge: 'ODS' },
    'odp':    { bgColor: '#ea580c', badge: 'ODP' },
    // Text / Markup
    'md':     { bgColor: '#60a5fa', badge: 'MD' },
    'txt':    { bgColor: '#9ca3af', badge: 'TXT' },
    'rtf':    { bgColor: '#6b7280', badge: 'RTF' },
    'log':    { bgColor: '#6b7280', badge: 'LOG' },
    // Images
    'jpg':    { bgColor: '#ec4899', badge: 'JPG' },
    'jpeg':   { bgColor: '#ec4899', badge: 'JPG' },
    'png':    { bgColor: '#ec4899', badge: 'PNG' },
    'gif':    { bgColor: '#ec4899', badge: 'GIF' },
    'svg':    { bgColor: '#f97316', badge: 'SVG' },
    'webp':   { bgColor: '#ec4899', badge: 'WP' },
    'ico':    { bgColor: '#ec4899', badge: 'ICO' },
    'bmp':    { bgColor: '#ec4899', badge: 'BMP' },
    // Video
    'mp4':    { bgColor: '#8b5cf6', badge: 'MP4' },
    'webm':   { bgColor: '#8b5cf6', badge: 'WBM' },
    'avi':    { bgColor: '#8b5cf6', badge: 'AVI' },
    'mkv':    { bgColor: '#8b5cf6', badge: 'MKV' },
    'mov':    { bgColor: '#8b5cf6', badge: 'MOV' },
    // Audio
    'mp3':    { bgColor: '#22c55e', badge: 'MP3' },
    'wav':    { bgColor: '#22c55e', badge: 'WAV' },
    'flac':   { bgColor: '#22c55e', badge: 'FLC' },
    'ogg':    { bgColor: '#22c55e', badge: 'OGG' },
    'm4a':    { bgColor: '#22c55e', badge: 'M4A' },
    'aac':    { bgColor: '#22c55e', badge: 'AAC' },
    // Archives
    'zip':    { bgColor: '#a16207', badge: 'ZIP' },
    'rar':    { bgColor: '#a16207', badge: 'RAR' },
    'tar':    { bgColor: '#a16207', badge: 'TAR' },
    'gz':     { bgColor: '#a16207', badge: 'GZ' },
    '7z':     { bgColor: '#a16207', badge: '7Z' },
    // Vault
    'aerovault': { bgColor: '#10b981', badge: 'AV' },
    // Shell
    'sh':     { bgColor: '#22c55e', badge: 'SH' },
    'bash':   { bgColor: '#22c55e', badge: 'SH' },
    'zsh':    { bgColor: '#22c55e', badge: 'ZSH' },
    // Git
    'gitignore':     { bgColor: '#f97316', badge: 'GIT' },
    'gitattributes': { bgColor: '#f97316', badge: 'GIT' },
    // Lock
    'lock':   { bgColor: '#6b7280', badge: 'LCK' },
    // Web server
    'htaccess': { bgColor: '#16a34a', badge: 'HTA' },
};

export const filledTheme: IconThemeProvider = {
    id: 'filled',

    getFileIcon: (filename: string, size: number = 16): FileIconResult => {
        const ext = filename.split('.').pop()?.toLowerCase() || '';
        const entry = filledIconMap[ext];
        if (entry) {
            return {
                icon: <FilledDocIcon size={size} bgColor={entry.bgColor} badge={entry.badge} />,
                color: '',
            };
        }
        const badge = ext.toUpperCase().slice(0, 3) || '?';
        return {
            icon: <FilledDocIcon size={size} bgColor="#9ca3af" badge={badge} />,
            color: '',
        };
    },

    getFolderIcon: (size: number = 16): FileIconResult => {
        return { icon: <FilledFolderIcon size={size} />, color: '' };
    },

    getFolderUpIcon: (size: number = 16): FileIconResult => {
        return { icon: <FilledFolderUpIcon size={size} />, color: '' };
    },
};
