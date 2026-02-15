/**
 * AeroSync — Shared constants for sync components
 * Speed presets, UI options, and default policies
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
    retryPolicy: { max_retries: 1, base_delay_ms: 0, max_delay_ms: 0, timeout_ms: 300_000, backoff_multiplier: 1 },
    progressThrottle: 'minimal',
    activityLogLevel: 'errors',
    bandwidthLimit: 0,
    postSyncVerification: true,
};

// --- Speed Mode Labels ---

export const SPEED_MODE_KEYS: Record<SpeedMode, string> = {
    normal: 'syncPanel.speedNormal',
    fast: 'syncPanel.speedFast',
    turbo: 'syncPanel.speedTurbo',
    extreme: 'syncPanel.speedExtreme',
    maniac: 'syncPanel.speedManiac',
};

// Tooltip i18n keys for each speed mode
export const SPEED_MODE_TOOLTIPS: Record<SpeedMode, string> = {
    normal: 'syncPanel.speedNormalTooltip',
    fast: 'syncPanel.speedFastTooltip',
    turbo: 'syncPanel.speedTurboTooltip',
    extreme: 'syncPanel.speedExtremeTooltip',
    maniac: 'syncPanel.speedManiacTooltip',
};

// --- Bandwidth Options ---

export const BANDWIDTH_OPTIONS = [
    { value: 0, label: 'syncPanel.bandwidthUnlimited' },
    { value: 128, label: '128 KB/s' },
    { value: 256, label: '256 KB/s' },
    { value: 512, label: '512 KB/s' },
    { value: 1024, label: '1 MB/s' },
    { value: 2048, label: '2 MB/s' },
    { value: 5120, label: '5 MB/s' },
    { value: 10240, label: '10 MB/s' },
] as const;

// --- Default Policies ---

export const DEFAULT_RETRY_POLICY: RetryPolicy = {
    max_retries: 3,
    base_delay_ms: 500,
    max_delay_ms: 10_000,
    timeout_ms: 120_000,
    backoff_multiplier: 2.0,
};

export const DEFAULT_VERIFY_POLICY: VerifyPolicy = 'size_only';

// --- Virtual Scroll ---

export const VIRTUAL_ROW_HEIGHT = 45;
export const VIRTUAL_OVERSCAN = 10;
export const VIRTUAL_VIEWPORT = 350;

// --- FTP ---

export const FTP_TRANSFER_DELAY_MS = 350;

// --- Theme Detection ---

export function isCyberTheme(): boolean {
    if (typeof document === 'undefined') return false;
    return document.documentElement.classList.contains('cyber');
}
