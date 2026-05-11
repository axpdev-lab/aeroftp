// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';

interface VaultIconProps {
    size?: number;
    className?: string;
    /**
     * Visual variant:
     * - `filled` (default): emerald shield with white padlock and light green
     *   fill. Matches the OS MIME type icon (application-x-aerovault) and is
     *   used in places where the icon represents an actual `.aerovault` file
     *   (file list, recent vaults card, vault home tile).
     * - `outline`: monochromatic shield+lock that inherits the current text
     *   color via `currentColor`. Matches every other icon in the AeroFile
     *   button bar, the titlebar, and the AeroVault modal header. Use this
     *   when the icon is acting as a UI affordance rather than as a file
     *   stand-in.
     */
    variant?: 'filled' | 'outline';
}

const FILLED_SHIELD = '#10b981';
const FILLED_BG = '#d4fbee';

/**
 * AeroVault shield+lock icon.
 *
 * The geometry of the shield body matches the OS MIME icon
 * (`application-x-aerovault`), the v3 spec figure, and the inline SVG used
 * in the titlebar. Two variants share the same path so the brand identity
 * stays consistent across surfaces, only the rendering style changes.
 */
export const VaultIcon: React.FC<VaultIconProps> = ({ size = 24, className = '', variant = 'filled' }) => {
    if (variant === 'outline') {
        return (
            <svg
                xmlns="http://www.w3.org/2000/svg"
                viewBox="0 0 24 24"
                width={size}
                height={size}
                className={className}
                fill="none"
                stroke="currentColor"
            >
                {/* Shield body */}
                <path
                    d="M12 21l.88-.38a11 11 0 006.63-9.26l.43-5.52a1 1 0 00-.76-1L12 3 4.82 4.8a1 1 0 00-.76 1l.43 5.52a11 11 0 006.63 9.26z"
                    strokeWidth="2"
                    strokeLinecap="round"
                    strokeLinejoin="round"
                />
                {/* Lock body */}
                <rect
                    x="9.25"
                    y="11"
                    width="5.5"
                    height="4"
                    rx="0.75"
                    strokeWidth="1.5"
                    strokeLinecap="round"
                    strokeLinejoin="round"
                />
                {/* Lock shackle */}
                <path
                    d="M10.25 11V9.5a1.75 1.75 0 013.5 0V11"
                    strokeWidth="1.5"
                    strokeLinecap="round"
                    strokeLinejoin="round"
                />
            </svg>
        );
    }
    return (
        <svg
            xmlns="http://www.w3.org/2000/svg"
            viewBox="0 0 24 24"
            width={size}
            height={size}
            className={className}
            fill="none"
        >
            {/* Shield body */}
            <path
                d="M12 21l.88-.38a11 11 0 006.63-9.26l.43-5.52a1 1 0 00-.76-1L12 3 4.82 4.8a1 1 0 00-.76 1l.43 5.52a11 11 0 006.63 9.26z"
                fill={FILLED_BG}
                stroke={FILLED_SHIELD}
                strokeWidth="1.5"
                strokeLinecap="round"
                strokeLinejoin="round"
            />
            {/* Lock body */}
            <rect
                x="9.25"
                y="11"
                width="5.5"
                height="4"
                rx="0.75"
                fill="#fff"
                stroke={FILLED_SHIELD}
                strokeWidth="1.2"
                strokeLinecap="round"
                strokeLinejoin="round"
            />
            {/* Lock shackle */}
            <path
                d="M10.25 11V9.5a1.75 1.75 0 013.5 0V11"
                fill="none"
                stroke={FILLED_SHIELD}
                strokeWidth="1.2"
                strokeLinecap="round"
                strokeLinejoin="round"
            />
        </svg>
    );
};

export default VaultIcon;
