// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
//
// GAP-9b — locks the SPEED_PRESETS / MANIAC_OVERRIDES mapping. The unified
// Plan tab seeds its parallel-streams + compression controls from these
// presets and threads them into RemoteSyncConfig, so a silent drift here
// would silently change the migrated SyncPanel behaviour.

import { describe, expect, it } from 'vitest';
import { SPEED_PRESETS, MANIAC_OVERRIDES, type SpeedMode } from './syncConstants';

describe('syncConstants — SPEED_PRESETS', () => {
    it('exposes exactly the five migrated speed modes', () => {
        expect(Object.keys(SPEED_PRESETS).sort()).toEqual(
            ['extreme', 'fast', 'maniac', 'normal', 'turbo'],
        );
    });

    it('maps each mode to the legacy parallel-stream count', () => {
        const streams: Record<SpeedMode, number> = {
            normal: 1,
            fast: 3,
            turbo: 6,
            extreme: 8,
            maniac: 8,
        };
        for (const [mode, count] of Object.entries(streams)) {
            expect(SPEED_PRESETS[mode as SpeedMode].parallelStreams).toBe(count);
        }
    });

    it('maps each mode to the legacy compression mode', () => {
        expect(SPEED_PRESETS.normal.compressionMode).toBe('off');
        expect(SPEED_PRESETS.fast.compressionMode).toBe('auto');
        expect(SPEED_PRESETS.turbo.compressionMode).toBe('on');
        expect(SPEED_PRESETS.extreme.compressionMode).toBe('on');
        expect(SPEED_PRESETS.maniac.compressionMode).toBe('on');
    });

    it('enables delta sync only from turbo upward', () => {
        expect(SPEED_PRESETS.normal.deltaSyncEnabled).toBe(false);
        expect(SPEED_PRESETS.fast.deltaSyncEnabled).toBe(false);
        expect(SPEED_PRESETS.turbo.deltaSyncEnabled).toBe(true);
        expect(SPEED_PRESETS.extreme.deltaSyncEnabled).toBe(true);
        expect(SPEED_PRESETS.maniac.deltaSyncEnabled).toBe(true);
    });
});

describe('syncConstants — MANIAC_OVERRIDES', () => {
    it('disables the journal and forces verification off during the run', () => {
        expect(MANIAC_OVERRIDES.journalEnabled).toBe(false);
        expect(MANIAC_OVERRIDES.verifyPolicy).toBe('none');
    });

    it('drops the bandwidth cap and mandates a post-sync verification pass', () => {
        expect(MANIAC_OVERRIDES.bandwidthLimit).toBe(0);
        expect(MANIAC_OVERRIDES.postSyncVerification).toBe(true);
    });

    it('keeps a shallow retry policy with a long per-file timeout', () => {
        expect(MANIAC_OVERRIDES.retryPolicy.max_retries).toBe(2);
        expect(MANIAC_OVERRIDES.retryPolicy.timeout_ms).toBe(300_000);
    });
});
