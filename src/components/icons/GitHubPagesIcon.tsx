// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';

interface GitHubPagesIconProps {
    size?: number;
    className?: string;
    style?: React.CSSProperties;
}

/**
 * GitHub Pages icon — official octicon-browser from GitHub's UI.
 * Browser window shape, used in Pages deployment displays.
 */
export const GitHubPagesIcon: React.FC<GitHubPagesIconProps> = ({ size = 16, className = '', style }) => (
    <svg
        xmlns="http://www.w3.org/2000/svg"
        viewBox="0 0 16 16"
        width={size}
        height={size}
        fill="currentColor"
        className={className}
        style={style}
        aria-hidden="true"
    >
        <path d="M0 2.75C0 1.784.784 1 1.75 1h12.5c.966 0 1.75.784 1.75 1.75v10.5A1.75 1.75 0 0 1 14.25 15H1.75A1.75 1.75 0 0 1 0 13.25ZM14.5 6h-13v7.25c0 .138.112.25.25.25h12.5a.25.25 0 0 0 .25-.25Zm-6-3.5v2h6V2.75a.25.25 0 0 0-.25-.25ZM5 2.5v2h2v-2Zm-3.25 0a.25.25 0 0 0-.25.25V4.5h2v-2Z" />
    </svg>
);
