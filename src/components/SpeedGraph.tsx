// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * SpeedGraph: transfer speed over the whole transfer
 *
 * The chart of the Windows copy dialog: the horizontal axis is the progress of
 * the transfer (0-100 % of the bytes), the area is the speed at each point
 * reached so far, so a drop twenty minutes ago is still there; the dashed line
 * is the current speed, which falls while no progress frame arrives. A
 * transfer of unknown size has no progress axis and shows its recent speeds
 * instead. The data model lives in `utils/speedProfile.ts`.
 * Theme-aware colors for the 8 effective themes.
 */

import React, { useRef, useEffect, useMemo } from 'react';
import { formatSpeed } from '../utils/formatters';
import { useTranslation } from '../i18n';
import {
    SPEED_PROFILE_BUCKETS,
    hasProgressAxis,
    smoothedSpeeds,
    speedStats,
    type SpeedProfile,
} from '../utils/speedProfile';

interface SpeedGraphProps {
    /** Speed over the transfer (see `recordProgress`). */
    profile: SpeedProfile;
    /** Speed to show as current, bytes/sec (already lowered while frames are missing). */
    currentBps: number;
    /** Current theme */
    theme: string;
    /** Graph height in pixels (default: 64) */
    height?: number;
    /** When false, skip the canvas redraw (graph is collapsed but still mounted
     *  for the expand animation). Defaults to true for standalone use. */
    active?: boolean;
}

/** Room above the highest value, so a steady speed does not fill the box. */
export const GRAPH_HEADROOM = 1.2;
/** Lowest top of the vertical scale, bytes/sec. */
const MIN_SCALE_BPS = 1024;

/** Theme colors for the graph */
function getGraphColors(theme: string) {
    switch (theme) {
        case 'tokyo':
            return {
                line: '#a855f7',
                fill: 'rgba(168, 85, 247, 0.15)',
                fillTop: 'rgba(168, 85, 247, 0.35)',
                grid: 'rgba(148, 163, 184, 0.08)',
                text: '#94a3b8',
                bg: 'rgba(30, 25, 50, 0.5)',
                peak: '#ec4899',
                avg: '#c084fc',
            };
        case 'cyber':
            return {
                line: '#22d3ee',
                fill: 'rgba(34, 211, 238, 0.12)',
                fillTop: 'rgba(34, 211, 238, 0.30)',
                grid: 'rgba(34, 211, 238, 0.06)',
                text: '#67e8f9',
                bg: 'rgba(10, 14, 23, 0.5)',
                peak: '#10b981',
                avg: '#06b6d4',
            };
        case 'light':
            return {
                line: '#3b82f6',
                fill: 'rgba(59, 130, 246, 0.08)',
                fillTop: 'rgba(59, 130, 246, 0.20)',
                grid: 'rgba(0, 0, 0, 0.05)',
                text: '#6b7280',
                bg: 'rgba(249, 250, 251, 0.8)',
                peak: '#ef4444',
                avg: '#2563eb',
            };
        case 'ice':
            return {
                line: '#0ea5e9',
                fill: 'rgba(14, 165, 233, 0.08)',
                fillTop: 'rgba(14, 165, 233, 0.22)',
                grid: 'rgba(11, 41, 66, 0.05)',
                text: '#3b6789',
                bg: 'rgba(232, 242, 252, 0.85)',
                peak: '#0284c7',
                avg: '#38bdf8',
            };
        case 'green':
            return {
                line: '#22c55e',
                fill: 'rgba(34, 197, 94, 0.12)',
                fillTop: 'rgba(34, 197, 94, 0.32)',
                grid: 'rgba(148, 163, 184, 0.06)',
                text: '#a7c4b1',
                bg: 'rgba(15, 31, 23, 0.5)',
                peak: '#f59e0b',
                avg: '#4ade80',
            };
        case 'truedark':
            return {
                line: '#58a6ff',
                fill: 'rgba(88, 166, 255, 0.10)',
                fillTop: 'rgba(88, 166, 255, 0.26)',
                grid: 'rgba(139, 148, 158, 0.06)',
                text: '#8b949e',
                bg: 'rgba(1, 4, 9, 0.5)',
                peak: '#f85149',
                avg: '#1f6feb',
            };
        case 'redhorse':
            return {
                line: '#E32636',
                fill: 'rgba(227, 38, 54, 0.12)',
                fillTop: 'rgba(227, 38, 54, 0.32)',
                grid: 'rgba(255, 255, 255, 0.05)',
                text: '#b08488',
                bg: 'rgba(17, 17, 17, 0.55)',
                peak: '#FCE300',
                avg: '#FF2800',
            };
        default: // dark
            return {
                line: '#3b82f6',
                fill: 'rgba(59, 130, 246, 0.10)',
                fillTop: 'rgba(59, 130, 246, 0.25)',
                grid: 'rgba(148, 163, 184, 0.06)',
                text: '#94a3b8',
                bg: 'rgba(15, 23, 42, 0.5)',
                peak: '#ef4444',
                avg: '#60a5fa',
            };
    }
}

export const SpeedGraph: React.FC<SpeedGraphProps> = ({
    profile,
    currentBps,
    theme,
    height = 64,
    active = true,
}) => {
    const t = useTranslation();
    const canvasRef = useRef<HTMLCanvasElement>(null);
    const containerRef = useRef<HTMLDivElement>(null);
    const stats = useMemo(() => speedStats(profile), [profile]);

    useEffect(() => {
        // Collapsed: stay mounted (the wrapper animates max-height) but skip the
        // full canvas redraw on every speed sample the user cannot see.
        if (!active) return;
        const canvas = canvasRef.current;
        const container = containerRef.current;
        if (!canvas || !container) return;

        const ctx = canvas.getContext('2d');
        if (!ctx) return;

        // High-DPI support
        const dpr = window.devicePixelRatio || 1;
        const rect = container.getBoundingClientRect();
        const w = rect.width;
        const h = height;

        canvas.width = w * dpr;
        canvas.height = h * dpr;
        canvas.style.width = `${w}px`;
        canvas.style.height = `${h}px`;
        ctx.scale(dpr, dpr);

        const colors = getGraphColors(theme);
        ctx.clearRect(0, 0, w, h);

        const progressAxis = hasProgressAxis(profile);
        const speeds = progressAxis ? smoothedSpeeds(profile) : [];
        const recent = progressAxis ? [] : profile.recent.slice();
        const highest = Math.max(
            MIN_SCALE_BPS,
            currentBps,
            ...speeds.map((speed) => speed ?? 0),
            ...recent,
        );
        const top = highest * GRAPH_HEADROOM;
        const yOf = (bps: number) => h - 2 - (Math.max(0, bps) / top) * (h - 4);

        // Grid: 3 horizontal lines; on the progress axis also the quarters.
        ctx.strokeStyle = colors.grid;
        ctx.lineWidth = 0.5;
        for (let i = 1; i <= 3; i++) {
            const y = h - ((h - 4) * (i / 4)) - 2;
            ctx.beginPath();
            ctx.moveTo(0, y);
            ctx.lineTo(w, y);
            ctx.stroke();
        }
        if (progressAxis) {
            for (let i = 1; i <= 3; i++) {
                const x = (w * i) / 4;
                ctx.beginPath();
                ctx.moveTo(x, 0);
                ctx.lineTo(x, h);
                ctx.stroke();
            }
        }

        const gradient = ctx.createLinearGradient(0, 0, 0, h);
        gradient.addColorStop(0, colors.fillTop);
        gradient.addColorStop(1, colors.fill);

        // Contiguous runs of measured points; a gap (bytes before a resumed
        // transfer's first frame) is left blank rather than bridged.
        const runs: Array<Array<[number, number]>> = [];
        if (progressAxis) {
            const slice = w / SPEED_PROFILE_BUCKETS;
            const reachedX = profile.reached * w;
            let run: Array<[number, number]> = [];
            speeds.forEach((speed, i) => {
                if (speed === null) {
                    if (run.length) runs.push(run);
                    run = [];
                    return;
                }
                const x = Math.min(reachedX, (i + 0.5) * slice);
                const y = yOf(speed);
                // A run starts at the left edge of its first slice, so the
                // area covers the slice instead of starting from its middle.
                if (run.length === 0) run.push([i * slice, y]);
                run.push([x, y]);
            });
            if (run.length) {
                // Close the area at the point reached, at the slice's level.
                run.push([reachedX, run[run.length - 1][1]]);
                runs.push(run);
            }
        } else if (recent.length >= 2) {
            const step = w / (recent.length - 1);
            runs.push(recent.map((speed, i) => [i * step, yOf(speed)]));
        }

        for (const run of runs) {
            ctx.beginPath();
            ctx.moveTo(run[0][0], h);
            run.forEach(([x, y]) => ctx.lineTo(x, y));
            ctx.lineTo(run[run.length - 1][0], h);
            ctx.closePath();
            ctx.fillStyle = gradient;
            ctx.fill();

            ctx.beginPath();
            ctx.moveTo(run[0][0], run[0][1]);
            run.forEach(([x, y]) => ctx.lineTo(x, y));
            ctx.strokeStyle = colors.line;
            ctx.lineWidth = 1.5;
            ctx.stroke();
        }

        // Current speed: dashed across the box, with a dot at the point reached.
        // While frames are missing `currentBps` falls, and the dot drops below
        // the edge of the area: the stall shows before it lands in the history.
        const currentY = yOf(currentBps);
        ctx.setLineDash([3, 3]);
        ctx.strokeStyle = colors.avg;
        ctx.lineWidth = 1;
        ctx.beginPath();
        ctx.moveTo(0, currentY);
        ctx.lineTo(w, currentY);
        ctx.stroke();
        ctx.setLineDash([]);

        const dotX = progressAxis ? profile.reached * w : w - 3;
        ctx.beginPath();
        ctx.arc(dotX, currentY, 5, 0, Math.PI * 2);
        ctx.fillStyle = colors.fillTop;
        ctx.fill();
        ctx.beginPath();
        ctx.arc(dotX, currentY, 3, 0, Math.PI * 2);
        ctx.fillStyle = colors.line;
        ctx.fill();
    }, [profile, currentBps, theme, height, active]);

    const colors = getGraphColors(theme);

    return (
        <div className="tpb-graph" ref={containerRef} style={{ background: colors.bg }}>
            {/* Stats overlay */}
            <div className="tpb-graph-stats">
                <span style={{ color: colors.line }}>
                    {formatSpeed(currentBps)}
                </span>
                <span style={{ color: colors.avg }}>
                    {t('transfer.speedAvg', { speed: formatSpeed(stats.avg) })}
                </span>
                <span style={{ color: colors.peak }}>
                    {t('transfer.speedPeak', { speed: formatSpeed(stats.peak) })}
                </span>
            </div>
            <canvas ref={canvasRef} className="tpb-graph-canvas" />
        </div>
    );
};
