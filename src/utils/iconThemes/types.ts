// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Icon Theme System: Shared types
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
