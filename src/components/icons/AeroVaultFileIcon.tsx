// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';

interface AeroVaultFileIconProps {
    size?: number;
    className?: string;
}

/**
 * AeroVault file icon (.aerovault): mirrors the OS MIME type icon
 * (application-x-aerovault) — paper document with an emerald badge
 * (shield + lock glyph) in the lower-right. Used in the file list to
 * mark `.aerovault` archives as a file type.
 *
 * For the AeroVault feature surfaces (titlebar, vault modal, context
 * menus) keep using {@link VaultIcon}, which is the standalone
 * shield+lock symbol of the product itself.
 */
export const AeroVaultFileIcon: React.FC<AeroVaultFileIconProps> = ({ size = 24, className = '' }) => (
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
        {/* Emerald shield+lock badge */}
        <circle cx="16" cy="17" r="5" fill="#10b981" stroke="#047857" strokeWidth="0.5" />
        <path
            d="M16 13.8l2.4 0.7v2.1c0 1.4-1 2.6-2.4 3.2-1.4-0.6-2.4-1.8-2.4-3.2v-2.1z"
            fill="none"
            stroke="#ffffff"
            strokeWidth="0.7"
            strokeLinejoin="round"
        />
        <rect x="14.9" y="16.4" width="2.2" height="1.6" rx="0.2" fill="#ffffff" />
        <path
            d="M15.4 16.4v-0.5a0.6 0.6 0 0 1 1.2 0v0.5"
            fill="none"
            stroke="#ffffff"
            strokeWidth="0.5"
            strokeLinecap="round"
        />
    </svg>
);

export default AeroVaultFileIcon;
