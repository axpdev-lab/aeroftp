// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * UI Tokens Hook
 * Loads user overrides for the published CSS custom properties from
 * `<config_dir>/ui-tokens.json` and applies them to the document root.
 *
 * Contract: docs/UI-TOKENS.md (the published token list and accepted shapes).
 * Validation rules: docs/dev/roadmap/APPENDIX-UI-TOKENS/03-override-loading.md.
 * Validation is reject, not sanitise; every rejection is reported to the
 * activity log with the key and the reason. No file watcher: overrides apply
 * once at startup and on an explicit reload().
 */

import { useCallback, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useActivityLog } from './useActivityLog';

// ============================================================================
// Published tokens (must match docs/UI-TOKENS.md exactly)
// ============================================================================

type UiTokenShape =
    | { kind: 'length'; min: number; max: number }
    | { kind: 'color' };

export const PUBLISHED_UI_TOKENS: Readonly<Record<string, UiTokenShape>> = {
    '--aeroftp-scrollbar-width': { kind: 'length', min: 2, max: 24 },
    '--aeroftp-panel-scrollbar-width': { kind: 'length', min: 2, max: 24 },
    '--aeroftp-scrollbar-radius': { kind: 'length', min: 0, max: 12 },
    '--aeroftp-scrollbar-thumb': { kind: 'color' },
    '--aeroftp-scrollbar-thumb-hover': { kind: 'color' },
    '--color-accent': { kind: 'color' },
    '--color-accent-hover': { kind: 'color' },
    '--color-bg-primary': { kind: 'color' },
    '--color-bg-secondary': { kind: 'color' },
    '--color-bg-tertiary': { kind: 'color' },
    '--color-text-primary': { kind: 'color' },
    '--color-text-secondary': { kind: 'color' },
    '--color-text-tertiary': { kind: 'color' },
    '--color-border': { kind: 'color' },
    '--color-border-strong': { kind: 'color' },
};

export const PUBLISHED_UI_TOKEN_NAMES: readonly string[] = Object.keys(PUBLISHED_UI_TOKENS);

export const UI_TOKENS_FILENAME = 'ui-tokens.json';

// ============================================================================
// Validation (pure, DOM-free, unit tested)
// ============================================================================

export interface UiTokenRejection {
    key: string;
    reason: string;
}

export interface UiTokenValidationResult {
    accepted: Record<string, string>;
    rejected: UiTokenRejection[];
}

const LENGTH_PATTERN = /^(\d+(?:\.\d+)?)px$/;
const HEX_COLOR_PATTERN = /^#(?:[0-9a-fA-F]{3}|[0-9a-fA-F]{6}|[0-9a-fA-F]{8})$/;
const RGB_COLOR_PATTERN = /^rgba?\(\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)\s*(?:,\s*(\d+(?:\.\d+)?)\s*)?\)$/;

function isValidLength(value: string, min: number, max: number): boolean {
    const match = LENGTH_PATTERN.exec(value);
    if (!match) return false;
    const amount = Number.parseFloat(match[1]);
    return amount >= min && amount <= max;
}

function isValidColor(value: string): boolean {
    if (HEX_COLOR_PATTERN.test(value)) return true;
    const match = RGB_COLOR_PATTERN.exec(value);
    if (!match) return false;
    const red = Number.parseInt(match[1], 10);
    const green = Number.parseInt(match[2], 10);
    const blue = Number.parseInt(match[3], 10);
    if (red > 255 || green > 255 || blue > 255) return false;
    const isRgba = value.startsWith('rgba');
    const alpha = match[4];
    // rgb() takes exactly 3 components, rgba() takes 3 components plus alpha.
    if (isRgba && alpha === undefined) return false;
    if (!isRgba && alpha !== undefined) return false;
    if (alpha !== undefined) {
        const alphaValue = Number.parseFloat(alpha);
        if (alphaValue < 0 || alphaValue > 1) return false;
    }
    return true;
}

/**
 * Validate the parsed content of ui-tokens.json. Reject, do not sanitise:
 * unknown keys, out-of-shape values, and anything containing `url(`,
 * `expression`, `;` or `}` are dropped, each with a reason.
 */
export function validateUiTokenOverrides(input: unknown): UiTokenValidationResult {
    const accepted: Record<string, string> = {};
    const rejected: UiTokenRejection[] = [];

    if (typeof input !== 'object' || input === null || Array.isArray(input)) {
        rejected.push({
            key: '(root)',
            reason: 'ui-tokens.json must contain a JSON object mapping token names to values',
        });
        return { accepted, rejected };
    }

    for (const [key, rawValue] of Object.entries(input as Record<string, unknown>)) {
        const shape = PUBLISHED_UI_TOKENS[key];
        if (!shape) {
            rejected.push({ key, reason: 'unknown token: not in the published list (docs/UI-TOKENS.md)' });
            continue;
        }
        if (typeof rawValue !== 'string') {
            rejected.push({ key, reason: 'value must be a string' });
            continue;
        }
        const value = rawValue.trim();
        const lowered = value.toLowerCase();
        if (lowered.includes('url(') || lowered.includes('expression') || value.includes(';') || value.includes('}')) {
            rejected.push({ key, reason: 'value contains forbidden content (url(, expression, ; or })' });
            continue;
        }
        if (shape.kind === 'length') {
            if (!isValidLength(value, shape.min, shape.max)) {
                rejected.push({ key, reason: `expected <number>px within ${shape.min} to ${shape.max}` });
                continue;
            }
        } else if (!isValidColor(value)) {
            rejected.push({ key, reason: 'expected a hex colour (#rgb, #rrggbb, #rrggbbaa) or rgb()/rgba() with numeric components' });
            continue;
        }
        accepted[key] = value;
    }

    return { accepted, rejected };
}

// ============================================================================
// Apply / reset (style target injectable for tests)
// ============================================================================

type UiTokenStyleTarget = Pick<CSSStyleDeclaration, 'setProperty' | 'removeProperty'>;

/** Apply accepted overrides as inline declarations on the given style target. */
export function applyUiTokenOverrides(accepted: Record<string, string>, style: UiTokenStyleTarget): void {
    for (const [name, value] of Object.entries(accepted)) {
        style.setProperty(name, value);
    }
}

/**
 * Remove every published token from the style target, restoring the :root
 * defaults. Properties outside the published list are left untouched.
 */
/**
 * Apply one load result to the document, authoritatively.
 *
 * Extracted from `reload()` so a test can exercise the real code path rather
 * than re-enacting it: a test that repeats the sequence by hand keeps passing
 * when someone deletes the reset from the caller, which makes it decoration
 * rather than a guard.
 *
 * The reset is the whole point. The file is the source of truth, so applying
 * without clearing first is additive: a key the user removed from the file
 * would stay on the document while the panel reported it gone, and deleting
 * the file would leave every override applied until the app restarted.
 */
export function applyLoadResultToDocument(
    result: UiTokenValidationResult | null,
    style: UiTokenStyleTarget,
): void {
    resetUiTokenOverrides(style);
    if (result) applyUiTokenOverrides(result.accepted, style);
}

export function resetUiTokenOverrides(style: UiTokenStyleTarget): void {
    for (const name of PUBLISHED_UI_TOKEN_NAMES) {
        style.removeProperty(name);
    }
}

// ============================================================================
// Loading
// ============================================================================

/**
 * Read and validate `<config_dir>/ui-tokens.json`.
 *
 * The config dir comes from the backend (`get_system_info` -> `config_dir`),
 * NOT from `appConfigDir()`: the fs-plugin `$APPCONFIG` resolves to the legacy
 * identifier-scoped directory that the backend no longer reads.
 *
 * Returns null when the file is missing or unreadable: that is the normal
 * case (no overrides), not an error, and stays silent. A file that exists but
 * is not valid JSON is a user error and is reported as a rejection.
 */
export async function loadUiTokenOverrides(): Promise<UiTokenValidationResult | null> {
    let text: string | null;
    try {
        // Through the backend, not the fs plugin. The data root contains a hidden
        // component on Linux (`~/.config/aeroftp`) and the Tauri fs scope sets
        // `require_literal_leading_dot` on unix, so `$HOME/**` does not match it;
        // widening the scope would also expose `vault.db`, which lives there.
        text = await invoke<string | null>('read_ui_tokens_file');
    } catch (error) {
        // A real failure, not an absent file: the command returns null for that.
        // Reported rather than swallowed, because a silent failure here looks
        // exactly like a file that applied cleanly.
        return {
            accepted: {},
            rejected: [{ key: '(file)', reason: `cannot read ${UI_TOKENS_FILENAME}: ${String(error)}` }],
        };
    }
    if (text === null) return null;
    let parsed: unknown;
    try {
        parsed = JSON.parse(text);
    } catch {
        return {
            accepted: {},
            rejected: [{ key: '(root)', reason: 'ui-tokens.json is not valid JSON' }],
        };
    }
    return validateUiTokenOverrides(parsed);
}

// ============================================================================
// Hook
// ============================================================================

export interface UseUiTokensResult {
    /** Re-read the file and re-apply the overrides. */
    reload: () => Promise<void>;
    /** Remove every published token override, restoring the defaults. */
    reset: () => void;
    /** Overrides applied by the last load. */
    appliedCount: number;
    /** Entries rejected by the last load. */
    rejectedCount: number;
}

export interface UseUiTokensOptions {
    /**
     * Set to false to skip the initial file read on mount. The instance still
     * shares the counts of the last load (whoever ran it) and can reload() or
     * reset() explicitly. Used by the Settings panel, which must not re-read
     * the file (and re-log the same rejections) every time it opens.
     */
    loadOnMount?: boolean;
}

interface UiTokenCounts {
    appliedCount: number;
    rejectedCount: number;
}

// Shared snapshot of the last load's counts, so every useUiTokens() instance
// reports the same numbers regardless of which instance performed the load.
let uiTokenCounts: UiTokenCounts = { appliedCount: 0, rejectedCount: 0 };
const uiTokenListeners = new Set<(counts: UiTokenCounts) => void>();

function setUiTokenCounts(next: UiTokenCounts): void {
    uiTokenCounts = next;
    for (const listener of uiTokenListeners) {
        listener(next);
    }
}

/**
 * The startup load happens once per process, not once per mount.
 *
 * The overrides belong to the document, not to a component: two components
 * calling `useUiTokens()` must not each read the file, and remounting must not
 * re-read it. Applying twice is harmless because `setProperty` is idempotent,
 * but LOGGING twice is not: a user reading eight rejections for four bad values
 * reasonably concludes something ran twice, and in a panel whose whole job is to
 * tell the truth about what was dropped, that is a defect.
 *
 * This is not a suppression of React StrictMode's double invoke. StrictMode
 * double-mounts precisely to surface effects that are not idempotent, and the
 * correct answer to that signal is to make the effect idempotent rather than to
 * hide it. The flag is set BEFORE the first await so the second synchronous
 * mount cannot slip past it.
 *
 * `reload()` is never guarded: an explicit reload always re-reads the file.
 */
let startupLoadDone = false;

/**
 * True exactly once per process, for the first caller. Every later caller gets
 * false. Named as a question rather than exposed as a flag so the invariant is
 * readable at the call site and cannot be half-applied.
 */
export function shouldRunStartupLoad(): boolean {
    if (startupLoadDone) return false;
    startupLoadDone = true;
    return true;
}

/** Record that a load has happened, so a later mount does not repeat it. */
function markStartupLoadDone(): void {
    startupLoadDone = true;
}

/** Test-only: the guard is process-wide, so a test suite must be able to clear it. */
export function resetStartupLoadForTests(): void {
    startupLoadDone = false;
}

/**
 * Load ui-tokens.json once at startup and apply the surviving overrides to
 * the document root. Rejections are reported to the activity log with the
 * key and the reason. Not driven by a file watcher: re-application happens
 * only through reload().
 */
export function useUiTokens(options?: UseUiTokensOptions): UseUiTokensResult {
    const { log } = useActivityLog();
    const [counts, setCounts] = useState<UiTokenCounts>(uiTokenCounts);

    useEffect(() => {
        const listener = (next: UiTokenCounts) => setCounts(next);
        uiTokenListeners.add(listener);
        // Catch up with a load that landed between render and subscribe.
        setCounts(uiTokenCounts);
        return () => {
            uiTokenListeners.delete(listener);
        };
    }, []);

    const reload = useCallback(async () => {
        // Always re-reads: `reload` is the explicit path and is never guarded.
        // It also marks the startup load as done, so a component mounting later
        // does not read the file a second time on top of a manual reload.
        markStartupLoadDone();
        const result = await loadUiTokenOverrides();
        applyLoadResultToDocument(result, document.documentElement.style);
        if (!result) {
            setUiTokenCounts({ appliedCount: 0, rejectedCount: 0 });
            return;
        }
        for (const rejection of result.rejected) {
            log('ERROR', `UI token override rejected: ${rejection.key}`, 'error', rejection.reason);
        }
        setUiTokenCounts({
            appliedCount: Object.keys(result.accepted).length,
            rejectedCount: result.rejected.length,
        });
    }, [log]);

    const reset = useCallback(() => {
        resetUiTokenOverrides(document.documentElement.style);
        setUiTokenCounts({ appliedCount: 0, rejectedCount: 0 });
    }, []);

    const loadOnMount = options?.loadOnMount !== false;
    useEffect(() => {
        if (!loadOnMount || !shouldRunStartupLoad()) return;
        void reload();
    }, [reload, loadOnMount]);

    return { reload, reset, appliedCount: counts.appliedCount, rejectedCount: counts.rejectedCount };
}

export default useUiTokens;
