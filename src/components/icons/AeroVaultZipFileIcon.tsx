// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';

interface AeroVaultZipFileIconProps {
    size?: number;
    className?: string;
}

/**
 * AeroVault Zip file icon (.aerozip): mirrors the OS MIME type icon
 * (application-x-aerozip) — paper document with an amber badge (archive-box
 * glyph) in the lower-right. Used in the file list to mark `.aerozip`
 * archives as a file type.
 *
 * The amber badge deliberately contrasts with the emerald
 * {@link AeroVaultFileIcon} (`.aerovault`): the Zip lane is the plaintext,
 * "not confidential" sibling of the encrypted AeroVault container, so the
 * colour keeps the two visually distinct at a glance.
 */
export const AeroVaultZipFileIcon: React.FC<AeroVaultZipFileIconProps> = ({ size = 24, className = '' }) => (
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
        {/* Amber archive-box badge */}
        <circle cx="16" cy="17" r="5" fill="#d97706" stroke="#92400e" strokeWidth="0.5" />
        <rect x="13.6" y="14.9" width="4.8" height="1.5" rx="0.3" fill="#ffffff" />
        <path
            d="M13.9 16.4h4.2v3.1a0.5 0.5 0 0 1-0.5 0.5h-3.2a0.5 0.5 0 0 1-0.5-0.5z"
            fill="none"
            stroke="#ffffff"
            strokeWidth="0.8"
            strokeLinejoin="round"
        />
        <rect x="15.2" y="17.2" width="1.6" height="0.9" rx="0.2" fill="#ffffff" />
    </svg>
);

export default AeroVaultZipFileIcon;
