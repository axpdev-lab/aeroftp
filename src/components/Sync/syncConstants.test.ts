// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
//
// Locks the SPEED_PRESETS / MANIAC_OVERRIDES mapping. The run reads the delta
// flag from this table (App.tsx, `runConnectedRemoteSync`), so a silent drift
// here would silently change which transfers go through the delta path.

import { describe, expect, it } from 'vitest';
import { SPEED_PRESETS, MANIAC_OVERRIDES, type SpeedMode } from './syncConstants';

describe('syncConstants — SPEED_PRESETS', () => {
    it('exposes exactly the five migrated speed modes', () => {
        expect(Object.keys(SPEED_PRESETS).sort()).toEqual(
            ['extreme', 'fast', 'maniac', 'normal', 'turbo'],
        );
    });

    it('carries only the delta flag: no stream count, no compression', () => {
        // The run transfers one file at a time and never compressed, so the
        // table no longer offers knobs the run does not read.
        for (const mode of Object.keys(SPEED_PRESETS) as SpeedMode[]) {
            expect(Object.keys(SPEED_PRESETS[mode])).toEqual(['deltaSyncEnabled']);
        }
    });

    it('enables delta sync from fast upward, as the run always did', () => {
        // The table used to say `fast: false` while the run enabled delta for
        // every mode but normal (`speedMode !== 'normal'`). The run is what
        // users got, so the table now says that and the run reads the table.
        expect(SPEED_PRESETS.normal.deltaSyncEnabled).toBe(false);
        expect(SPEED_PRESETS.fast.deltaSyncEnabled).toBe(true);
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
