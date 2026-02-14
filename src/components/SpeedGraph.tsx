/**
 * SpeedGraph — Real-time transfer speed visualization
 *
 * Canvas-based area chart showing transfer speed over time.
 * Displays: current speed, average, peak, with auto-scaling Y axis.
 * Theme-aware colors (light/dark/tokyo/cyber).
 */

import React, { useRef, useEffect, useMemo } from 'react';
import { formatSpeed } from '../utils/formatters';

interface SpeedGraphProps {
    /** Array of speed samples in bytes/sec (newest last) */
    speedHistory: number[];
    /** Current theme */
    theme: string;
    /** Graph height in pixels (default: 64) */
    height?: number;
    /** Max samples to display (default: 60 = ~30s at 500ms intervals) */
    maxSamples?: number;
}

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
    speedHistory,
    theme,
    height = 64,
    maxSamples = 60,
}) => {
    const canvasRef = useRef<HTMLCanvasElement>(null);
    const containerRef = useRef<HTMLDivElement>(null);

    // Compute stats
    const stats = useMemo(() => {
        if (speedHistory.length === 0) return { current: 0, avg: 0, peak: 0 };
        const current = speedHistory[speedHistory.length - 1];
        const sum = speedHistory.reduce((a, b) => a + b, 0);
        const avg = sum / speedHistory.length;
        const peak = Math.max(...speedHistory);
        return { current, avg, peak };
    }, [speedHistory]);

    useEffect(() => {
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

        // Clear
        ctx.clearRect(0, 0, w, h);

        // Samples (pad left with zeros if needed)
        const samples = speedHistory.slice(-maxSamples);
        if (samples.length < 2) return;

        // Y-axis: auto-scale with padding
        const maxVal = Math.max(...samples, 1024); // minimum 1 KB/s
        const yScale = (h - 8) / maxVal; // 4px padding top+bottom
        const xStep = w / (maxSamples - 1);

        // Draw grid lines (3 horizontal)
        ctx.strokeStyle = colors.grid;
        ctx.lineWidth = 0.5;
        for (let i = 1; i <= 3; i++) {
            const y = h - ((h - 8) * (i / 4)) - 4;
            ctx.beginPath();
            ctx.moveTo(0, y);
            ctx.lineTo(w, y);
            ctx.stroke();
        }

        // Build path
        const startX = w - (samples.length - 1) * xStep;
        const points: [number, number][] = samples.map((val, i) => [
            startX + i * xStep,
            h - 4 - val * yScale,
        ]);

        // Area fill gradient
        const gradient = ctx.createLinearGradient(0, 0, 0, h);
        gradient.addColorStop(0, colors.fillTop);
        gradient.addColorStop(1, colors.fill);

        ctx.beginPath();
        ctx.moveTo(points[0][0], h);
        points.forEach(([x, y]) => ctx.lineTo(x, y));
        ctx.lineTo(points[points.length - 1][0], h);
        ctx.closePath();
        ctx.fillStyle = gradient;
        ctx.fill();

        // Line
        ctx.beginPath();
        ctx.moveTo(points[0][0], points[0][1]);
        for (let i = 1; i < points.length; i++) {
            // Smooth curve using quadratic bezier
            const prev = points[i - 1];
            const curr = points[i];
            const cpx = (prev[0] + curr[0]) / 2;
            ctx.quadraticCurveTo(prev[0], prev[1], cpx, (prev[1] + curr[1]) / 2);
        }
        const last = points[points.length - 1];
        ctx.lineTo(last[0], last[1]);
        ctx.strokeStyle = colors.line;
        ctx.lineWidth = 1.5;
        ctx.stroke();

        // Glow dot at current position
        ctx.beginPath();
        ctx.arc(last[0], last[1], 3, 0, Math.PI * 2);
        ctx.fillStyle = colors.line;
        ctx.fill();
        ctx.beginPath();
        ctx.arc(last[0], last[1], 5, 0, Math.PI * 2);
        ctx.fillStyle = colors.fillTop;
        ctx.fill();

    }, [speedHistory, theme, height, maxSamples]);

    const colors = getGraphColors(theme);

    return (
        <div className="tpb-graph" ref={containerRef}>
            {/* Stats overlay */}
            <div className="tpb-graph-stats">
                <span style={{ color: colors.line }}>
                    {formatSpeed(stats.current)}
                </span>
                <span style={{ color: colors.avg }}>
                    avg {formatSpeed(stats.avg)}
                </span>
                <span style={{ color: colors.peak }}>
                    peak {formatSpeed(stats.peak)}
                </span>
            </div>
            <canvas ref={canvasRef} className="tpb-graph-canvas" />
        </div>
    );
};
