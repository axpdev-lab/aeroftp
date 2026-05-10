// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';

interface AeroFtpFileIconProps {
    size?: number;
    className?: string;
}

/**
 * AeroFTP backup file icon (.aeroftp): mirrors the OS MIME type icon
 * (application-x-aeroftp): paper document with a blue badge (list lines)
 * in the lower-right. Optimised for small list-view sizes (16-24px) by
 * dropping shadows and gradients.
 */
export const AeroFtpFileIcon: React.FC<AeroFtpFileIconProps> = ({ size = 24, className = '' }) => (
    <svg
        xmlns="http://www.w3.org/2000/svg"
        viewBox="0 0 24 24"
        width={size}
        height={size}
        className={className}
        fill="none"
    >
        {/* Paper body with folded corner */}
        <path
            d="M5 2h10l4 4v15a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1V3a1 1 0 0 1 1-1z"
            fill="#f4f5f7"
            stroke="#bec3ca"
            strokeWidth="1"
            strokeLinejoin="round"
        />
        <path
            d="M15 2v4h4"
            fill="#d8dce1"
            stroke="#a9afb6"
            strokeWidth="1"
            strokeLinejoin="round"
        />
        {/* Blue list badge */}
        <circle cx="16" cy="17" r="5" fill="#3b82f6" stroke="#1e40af" strokeWidth="0.5" />
        <g stroke="#ffffff" strokeWidth="1" strokeLinecap="round">
            <line x1="13.5" y1="15.5" x2="18.5" y2="15.5" />
            <line x1="13.5" y1="17" x2="18.5" y2="17" />
            <line x1="13.5" y1="18.5" x2="18.5" y2="18.5" />
        </g>
    </svg>
);

export default AeroFtpFileIcon;
