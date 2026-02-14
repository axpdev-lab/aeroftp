/**
 * Icon Theme System — Shared types
 */

import type React from 'react';

export interface FileIconResult {
    icon: React.ReactNode;
    color: string;
}

export interface IconThemeProvider {
    id: string;
    getFileIcon: (filename: string, size?: number) => FileIconResult;
    getFolderIcon: (size?: number) => FileIconResult;
    getFolderUpIcon: (size?: number) => FileIconResult;
}
