// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import {
    reduceScanProgress,
    scanProgressTotals,
    EMPTY_SCAN_PROGRESS,
    formatElapsed,
} from './useScanProgress';

const fold = (...events: Parameters<typeof reduceScanProgress>[1][]) =>
    events.reduce(reduceScanProgress, EMPTY_SCAN_PROGRESS);

describe('reduceScanProgress', () => {
    it('keeps the latest tick per side, not the latest tick overall', () => {
        // Local and remote scan concurrently, so ticks interleave.
        const state = fold(
            { phase: 'local', files_found: 100, dirs_found: 10, bytes_found: 1000 },
            { phase: 'remote', files_found: 150, dirs_found: 5, bytes_found: 500 },
            { phase: 'local', files_found: 200, dirs_found: 20, bytes_found: 2000 },
        );
        expect(state.local).toEqual({ files: 200, dirs: 20, bytes: 2000 });
        expect(state.remote).toEqual({ files: 150, dirs: 5, bytes: 500 });
    });

    it('keeps the last known dirs/bytes when a tick omits them', () => {
        // The DAG observer emitter only knows how many entries it has scanned.
        const state = fold(
            { phase: 'remote', files_found: 10, dirs_found: 3, bytes_found: 300 },
            { phase: 'remote', files_found: 40 },
        );
        expect(state.remote).toEqual({ files: 40, dirs: 3, bytes: 300 });
    });

    it('records the comparing phase without disturbing the counters', () => {
        const state = fold(
            { phase: 'local', files_found: 7, dirs_found: 1, bytes_found: 70 },
            { phase: 'comparing', files_found: 999 },
        );
        expect(state.comparing).toBe(true);
        expect(state.local.files).toBe(7);
    });

    it('does not double-count the file total the remote side reports cumulatively', () => {
        // The remote emitter sends `local + remote so far`; adding the two sides
        // would show 300 files for a 200-file pair.
        const totals = scanProgressTotals(fold(
            { phase: 'local', files_found: 100, dirs_found: 10, bytes_found: 1000 },
            { phase: 'remote', files_found: 200, dirs_found: 4, bytes_found: 500 },
        ));
        expect(totals.files).toBe(200);
        expect(totals.dirs).toBe(14);
        expect(totals.bytes).toBe(1500);
    });

    it('starts at zero rather than undefined', () => {
        expect(scanProgressTotals(EMPTY_SCAN_PROGRESS)).toEqual({ files: 0, dirs: 0, bytes: 0 });
    });
});

describe('formatElapsed', () => {
    it('counts in m:ss and grows an hours field only when needed', () => {
        expect(formatElapsed(0)).toBe('0:00');
        expect(formatElapsed(9_000)).toBe('0:09');
        expect(formatElapsed(65_000)).toBe('1:05');
        expect(formatElapsed(3_600_000)).toBe('1:00:00');
        expect(formatElapsed(3_725_000)).toBe('1:02:05');
    });
});
