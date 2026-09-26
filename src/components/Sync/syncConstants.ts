// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * AeroSync: shared sync constants.
 *
 * Speed-mode presets and the Maniac override bundle, consumed by the unified
 * AeroSync Plan tab. Trimmed in GAP-9e: the legacy `SyncPanel` /
 * `SyncAdvancedConfig` / `SyncQuickMode` consumers were retired, so the
 * speed-mode i18n key maps, bandwidth options, default policy bundles and
 * virtual-scroll constants they relied on were removed with them.
 */

import { RetryPolicy, VerifyPolicy } from '../../types';

// --- Speed Mode Types ---

export type SpeedMode = 'normal' | 'fast' | 'turbo' | 'extreme' | 'maniac';

/**
 * What a speed mode changes in an AeroSync run: whether transfers go through
 * the delta path (SFTP with native rsync; other protocols ignore it). The retry
 * policy per mode is `retryPolicyForSpeed`; Maniac also applies
 * `MANIAC_OVERRIDES`. There is no parallelism or compression here: the run
 * transfers one file at a time.
 */
export interface SpeedPreset {
    deltaSyncEnabled: boolean;
}

export interface ManiacOverrides {
    journalEnabled: boolean;
    verifyPolicy: VerifyPolicy;
    retryPolicy: RetryPolicy;
    progressThrottle: 'normal' | 'minimal';
    activityLogLevel: 'all' | 'errors';
    bandwidthLimit: number;
    postSyncVerification: boolean;
}

// --- Speed Presets ---

export const SPEED_PRESETS: Record<SpeedMode, SpeedPreset> = {
    // Delta from Fast up: what runs have done since the Plan tab took over
    // (the executor used `speedMode !== 'normal'`); this table is now the one
    // place that says so.
    normal:  { deltaSyncEnabled: false },
    fast:    { deltaSyncEnabled: true  },
    turbo:   { deltaSyncEnabled: true  },
    extreme: { deltaSyncEnabled: true  },
    maniac:  { deltaSyncEnabled: true  },
};

export const MANIAC_OVERRIDES: ManiacOverrides = {
    journalEnabled: false,
    verifyPolicy: 'none',
    retryPolicy: { max_retries: 2, base_delay_ms: 250, max_delay_ms: 2_000, timeout_ms: 300_000, backoff_multiplier: 1.5 },
    progressThrottle: 'minimal',
    activityLogLevel: 'errors',
    bandwidthLimit: 0,
    postSyncVerification: true,
};

// --- Theme Detection ---

export function isCyberTheme(): boolean {
    if (typeof document === 'undefined') return false;
    return document.documentElement.classList.contains('cyber');
}
