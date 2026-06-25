// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import {
    computeCompressionRatio,
    formatCompressDetails,
    formatExtractDetails,
} from './archiveSizeReport';

describe('computeCompressionRatio', () => {
    it('reports a positive saving when the archive shrinks', () => {
        const r = computeCompressionRatio(1000, 400);
        expect(r.savedBytes).toBe(600);
        expect(r.savedPercent).toBeCloseTo(60);
    });

    it('reports a negative saving when the archive grows', () => {
        const r = computeCompressionRatio(1000, 1200);
        expect(r.savedBytes).toBe(-200);
        expect(r.savedPercent).toBeCloseTo(-20);
    });

    it('never divides by zero on unknown input size', () => {
        const r = computeCompressionRatio(0, 0);
        expect(r.savedPercent).toBe(0);
        expect(Number.isFinite(r.savedPercent)).toBe(true);
    });

    it('treats a fully redundant payload as 100% saved', () => {
        const r = computeCompressionRatio(1000, 0);
        expect(r.savedPercent).toBe(100);
    });
});

describe('formatCompressDetails', () => {
    it('formats input -> output with a saved percentage', () => {
        // 1 MiB -> 256 KiB == 75% saved
        expect(formatCompressDetails(1024 * 1024, 256 * 1024)).toContain('saved 75%');
    });

    it('labels an expanded archive with grew, not a negative saved', () => {
        const out = formatCompressDetails(1000, 1500);
        expect(out).toContain('grew 50%');
        expect(out).not.toContain('-');
    });

    it('reports input only when the output size is unknown', () => {
        const out = formatCompressDetails(2048, 0);
        expect(out).not.toContain('→');
        expect(out).not.toContain('%');
    });
});

describe('formatExtractDetails', () => {
    it('labels a known extracted total as expanded', () => {
        expect(formatExtractDetails(1000, 4000)).toContain('expanded');
    });

    it('reports archive size only when the extracted total is unknown', () => {
        const out = formatExtractDetails(2048, 0);
        expect(out).not.toContain('→');
        expect(out).not.toContain('expanded');
    });
});
