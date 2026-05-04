// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * HealthRadial: small SVG donut indicator for the per-server reachability
 * probe. Sits on the My Servers and Discover cards in detailed layout.
 *
 * Status mapping:
 *   up      → green ring at 100%, latency in ms (tooltip)
 *   slow    → amber ring at 60%
 *   down    → red ring at 100% (filled red)
 *   pending → grey ring with subtle pulse
 *   unknown → grey outlined ring at 0%
 *
 * Sizing: default 22px, kept tight to fit beside the existing star/action
 * buttons without growing the card height.
 */

import * as React from 'react';
import type { HealthStatus } from '../../hooks/useProviderHealth';

interface HealthRadialProps {
    status: HealthStatus;
    latencyMs?: number;
    size?: number;
    title?: string;
    /** When provided the radial becomes a button that re-runs the probe for
     *  this single profile. Used to debug a flaky tab-wide scan: clicking the
     *  ring forces a fresh check and surfaces whether the bad result was the
     *  scan or the server. */
    onRetry?: () => void;
}

const STROKE_WIDTH = 2.5;

const ringColor: Record<HealthStatus, string> = {
    up: '#22c55e',       // green-500
    slow: '#f59e0b',     // amber-500
    down: '#ef4444',     // red-500
    pending: '#9ca3af',  // gray-400
    unknown: '#d1d5db',  // gray-300
};

export const HealthRadial: React.FC<HealthRadialProps> = ({
    status,
    latencyMs,
    size = 22,
    title,
    onRetry,
}) => {
    const r = (size - STROKE_WIDTH) / 2;
    const cx = size / 2;
    const cy = size / 2;
    const circumference = 2 * Math.PI * r;
    // Filled fraction: down is "fully red", slow ~60%, up 100%, pending/unknown empty.
    const fraction =
        status === 'up' ? 1
        : status === 'slow' ? 0.6
        : status === 'down' ? 1
        : 0;
    const dashOffset = circumference * (1 - fraction);
    const ariaLabel = title ?? `${status}${latencyMs ? ` ${latencyMs}ms` : ''}`;
    const interactive = !!onRetry && status !== 'pending';
    const handleClick = interactive ? (e: React.MouseEvent) => {
        // Stop the parent card's click handler (which toggles cross-profile
        // selection) from also firing.
        e.stopPropagation();
        onRetry!();
    } : undefined;
    const Wrapper: React.ElementType = interactive ? 'button' : 'span';

    return (
        <Wrapper
            role={interactive ? 'button' : 'img'}
            aria-label={ariaLabel}
            title={ariaLabel}
            onClick={handleClick}
            type={interactive ? 'button' : undefined}
            className={`inline-flex shrink-0 items-center justify-center ${status === 'pending' ? 'animate-pulse' : ''} ${interactive ? 'cursor-pointer rounded-full hover:bg-gray-100 dark:hover:bg-gray-700/50 transition-colors' : ''}`}
            style={{ width: size, height: size, padding: 0, border: 'none', background: 'transparent' }}
        >
            <svg width={size} height={size} viewBox={`0 0 ${size} ${size}`}>
                {/* Background ring */}
                <circle
                    cx={cx}
                    cy={cy}
                    r={r}
                    fill="none"
                    stroke="currentColor"
                    strokeOpacity="0.18"
                    strokeWidth={STROKE_WIDTH}
                />
                {/* Active arc (skip when status has nothing to show) */}
                {status !== 'unknown' && status !== 'pending' && (
                    <circle
                        cx={cx}
                        cy={cy}
                        r={r}
                        fill="none"
                        stroke={ringColor[status]}
                        strokeWidth={STROKE_WIDTH}
                        strokeDasharray={circumference}
                        strokeDashoffset={dashOffset}
                        strokeLinecap="round"
                        transform={`rotate(-90 ${cx} ${cy})`}
                        style={{ transition: 'stroke-dashoffset 240ms ease-out' }}
                    />
                )}
                {/* Pending: rotating tick to convey activity without popping the ring */}
                {status === 'pending' && (
                    <circle
                        cx={cx}
                        cy={cy}
                        r={r}
                        fill="none"
                        stroke={ringColor.pending}
                        strokeOpacity="0.55"
                        strokeWidth={STROKE_WIDTH}
                        strokeDasharray={`${circumference * 0.18} ${circumference}`}
                        strokeLinecap="round"
                        transform={`rotate(-90 ${cx} ${cy})`}
                    />
                )}
            </svg>
        </Wrapper>
    );
};
