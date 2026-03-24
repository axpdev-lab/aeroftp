// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';

interface GitHubActionsIconProps {
    size?: number;
    className?: string;
    style?: React.CSSProperties;
}

/**
 * GitHub Actions icon — official octicon-play from GitHub's UI.
 * Circle with play triangle, used in workflow run displays.
 */
export const GitHubActionsIcon: React.FC<GitHubActionsIconProps> = ({ size = 16, className = '', style }) => (
    <svg
        xmlns="http://www.w3.org/2000/svg"
        viewBox="0 0 16 16"
        width={size}
        height={size}
        fill="currentColor"
        style={style}
        className={className}
        aria-hidden="true"
    >
        <path d="M8 0a8 8 0 1 1 0 16A8 8 0 0 1 8 0ZM1.5 8a6.5 6.5 0 1 0 13 0 6.5 6.5 0 0 0-13 0Zm4.879-2.773 4.264 2.559a.25.25 0 0 1 0 .428l-4.264 2.559A.25.25 0 0 1 6 10.559V5.442a.25.25 0 0 1 .379-.215Z" />
    </svg>
);
