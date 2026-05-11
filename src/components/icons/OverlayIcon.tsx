// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';

interface OverlayIconProps {
    size?: number;
    className?: string;
}

/**
 * AeroCrypt overlay icon: two offset rectangles with a diagonal connector.
 *
 * Read at a glance as "a layer placed on top of something else", which is
 * exactly what AeroCrypt does (encryption layer applied to an existing
 * remote). Brand-neutral with respect to the backend: today the layer is
 * rclone-crypt; tomorrow AeroVault v4 overlay can plug in under the same
 * button without an icon change.
 *
 * Distinct from VaultIcon: AeroVault is "shield + lock" (a closed container,
 * single .aerovault file), AeroCrypt is "stacked rectangles" (a wrapper on
 * top of a pre-existing remote). Two different metaphors, two different
 * silhouettes — so the two toolbar buttons never get visually confused.
 *
 * Source: docs/dev/img/AeroVault_ico/overlay.svg, inlined as a React
 * component so it ships through bundle (no fetch, CSP-safe) and inherits
 * the current text color via `currentColor`.
 */
export const OverlayIcon: React.FC<OverlayIconProps> = ({ size = 24, className = '' }) => (
    <svg
        xmlns="http://www.w3.org/2000/svg"
        viewBox="0 0 32 32"
        width={size}
        height={size}
        className={className}
        fill="currentColor"
    >
        <path d="M28,8H24V4a2.0023,2.0023,0,0,0-2-2H4A2.0023,2.0023,0,0,0,2,4V22a2.0023,2.0023,0,0,0,2,2H8v4a2.0023,2.0023,0,0,0,2,2H28a2.0023,2.0023,0,0,0,2-2V10A2.0023,2.0023,0,0,0,28,8ZM4,22V4H22V8H10a2.0023,2.0023,0,0,0-2,2V22Zm18,0H19.4141L10,12.586V10h2.5859l9.4153,9.4156ZM10,15.4141,16.5859,22H10ZM22.001,16.587,15.4141,10H22ZM10,28V24H22a2.0023,2.0023,0,0,0,2-2V10h4V28Z" />
    </svg>
);

export default OverlayIcon;
