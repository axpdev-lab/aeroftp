// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * File icon utilities: delegates to the Outline icon theme
 *
 * This module preserves the original API for backward compatibility.
 * The actual icon mapping lives in iconThemes/outlineTheme.tsx.
 */

import { outlineTheme } from './iconThemes/outlineTheme';

export const getFileIcon = outlineTheme.getFileIcon;

export const getFileIconColor = (filename: string): string => {
    return outlineTheme.getFileIcon(filename).color;
};
