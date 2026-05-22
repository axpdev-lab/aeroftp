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

import { RetryPolicy, VerifyPolicy, CompressionMode } from '../../types';

// --- Speed Mode Types ---

export type SpeedMode = 'normal' | 'fast' | 'turbo' | 'extreme' | 'maniac';

export interface SpeedPreset {
    parallelStreams: number;
    compressionMode: CompressionMode;
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
    normal:  { parallelStreams: 1, compressionMode: 'off',  deltaSyncEnabled: false },
    fast:    { parallelStreams: 3, compressionMode: 'auto', deltaSyncEnabled: false },
    turbo:   { parallelStreams: 6, compressionMode: 'on',   deltaSyncEnabled: true  },
    extreme: { parallelStreams: 8, compressionMode: 'on',   deltaSyncEnabled: true  },
    maniac:  { parallelStreams: 8, compressionMode: 'on',   deltaSyncEnabled: true  },
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
