// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';

interface AeroFtpKeystoreIconProps {
    size?: number;
    className?: string;
}

/**
 * AeroFTP keystore file icon (.aeroftp-keystore): mirrors the OS MIME type
 * icon (application-x-aeroftp-keystore): paper document with an amber
 * badge (key glyph) in the lower-right. Optimised for small list-view
 * sizes (16-24px) by dropping shadows and gradients.
 */
export const AeroFtpKeystoreIcon: React.FC<AeroFtpKeystoreIconProps> = ({ size = 24, className = '' }) => (
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
        {/* Amber key badge */}
        <circle cx="16" cy="17" r="5" fill="#f59e0b" stroke="#b45309" strokeWidth="0.5" />
        <g transform="rotate(-30 16 17)">
            <circle cx="14" cy="17" r="1.5" fill="none" stroke="#ffffff" strokeWidth="0.9" />
            <line x1="15.5" y1="17" x2="19" y2="17" stroke="#ffffff" strokeWidth="0.9" strokeLinecap="round" />
            <line x1="18.3" y1="17" x2="18.3" y2="18.4" stroke="#ffffff" strokeWidth="0.9" strokeLinecap="round" />
            <line x1="17.5" y1="17" x2="17.5" y2="18" stroke="#ffffff" strokeWidth="0.9" strokeLinecap="round" />
        </g>
    </svg>
);

export default AeroFtpKeystoreIcon;
