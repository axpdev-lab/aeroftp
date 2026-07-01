// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
//
// FINDING-4: the simple ring spinner shown on the toolbar Stop button while a
// connected-remote AeroSync cancellation is pending (the in-flight transfer is
// being aborted and the run is unwinding). A faint ring with a single orbiting
// dot in the app accent blue.

import * as React from 'react';

export const StopCancelSpinner: React.FC<{ size?: number }> = ({ size = 16 }) => (
    <svg
        width={size}
        height={size}
        viewBox="0 0 24 24"
        fill="hsl(228, 97%, 42%)"
        xmlns="http://www.w3.org/2000/svg"
        aria-hidden="true"
    >
        <path
            d="M12,1A11,11,0,1,0,23,12,11,11,0,0,0,12,1Zm0,19a8,8,0,1,1,8-8A8,8,0,0,1,12,20Z"
            opacity=".25"
        />
        <circle cx="12" cy="2.5" r="1.5">
            <animateTransform
                attributeName="transform"
                type="rotate"
                dur="0.75s"
                values="0 12 12;360 12 12"
                repeatCount="indefinite"
            />
        </circle>
    </svg>
);

export default StopCancelSpinner;
