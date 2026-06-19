// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * App-aware status banner for the local helper-app bridges (Filen Desktop /
 * MEGAcmd) behind the "Local WebDAV" / "Local S3" / "WebDAV (MEGAcmd)" Quick
 * Connect modes (#215 follow-up).
 *
 * Replaces the static "Requires …" amber warning with a live 🔴/🟠/🟢 status
 * dot (same small dot used by the My Servers / Discovery health check) and a
 * message that adapts to whether the helper app is installed and running:
 *   - 🔴 not installed       -> install prompt + Save disabled (reported upward)
 *   - 🟠 installed, not active -> start-and-sign-in prompt
 *   - 🟢 active               -> green confirmation, no warning
 */
import React, { useEffect } from 'react';
import { RefreshCw } from 'lucide-react';
import { useTranslation } from '../i18n';
import { useBridgeStatus, type BridgeUiState } from '../hooks/useBridgeStatus';
import type { BridgeKind } from './providerModeGroups';

interface BridgeStatusBannerProps {
    kind: BridgeKind;
    /** Optional custom loopback port (e.g. a non-default MEGAcmd WebDAV port). */
    port?: number;
    /** Reports the confidently-not-installed state so the parent can disable Save. */
    onSaveBlockedChange?: (blocked: boolean) => void;
    /** Reports the current 🔴/🟠/🟢 state so the parent can collapse the setup
     *  box once the bridge is active (#215 follow-up, idea D). */
    onUiStateChange?: (state: BridgeUiState) => void;
}

/** Friendly helper-app name per bridge kind. */
function appNameFor(kind: BridgeKind): string {
    return kind === 'megacmd-webdav' ? 'MEGAcmd' : 'Filen Desktop';
}

const DOT_BG: Record<BridgeUiState, string> = {
    green: 'bg-emerald-500',
    amber: 'bg-amber-500',
    red: 'bg-red-500',
};

const BOX: Record<BridgeUiState, string> = {
    green:
        'text-emerald-700 dark:text-emerald-300 bg-emerald-50 dark:bg-emerald-900/20 border-emerald-200 dark:border-emerald-800/40',
    amber:
        'text-amber-700 dark:text-amber-300 bg-amber-50 dark:bg-amber-900/20 border-amber-200 dark:border-amber-800/40',
    red: 'text-red-700 dark:text-red-300 bg-red-50 dark:bg-red-900/20 border-red-200 dark:border-red-800/40',
};

/** Small status dot matching the health-check style (green pulses). */
const StatusDot: React.FC<{ state: BridgeUiState }> = ({ state }) => (
    <span className="relative flex items-center justify-center shrink-0" style={{ width: 10, height: 10 }}>
        {state === 'green' && (
            <span className={`absolute inset-0 rounded-full ${DOT_BG.green} opacity-30 animate-ping`} />
        )}
        <span className={`relative rounded-full ${DOT_BG[state]}`} style={{ width: 8, height: 8 }} />
    </span>
);

export const BridgeStatusBanner: React.FC<BridgeStatusBannerProps> = ({
    kind,
    port,
    onSaveBlockedChange,
    onUiStateChange,
}) => {
    const t = useTranslation();
    const { status, uiState, saveBlocked, loading, refresh } = useBridgeStatus(kind, port);
    const probedPort = status?.port ?? port;
    const app = appNameFor(kind);

    // Report Save-gating upward; clear it when this banner unmounts (tab switch).
    useEffect(() => {
        onSaveBlockedChange?.(saveBlocked);
    }, [saveBlocked, onSaveBlockedChange]);
    useEffect(() => () => onSaveBlockedChange?.(false), [onSaveBlockedChange]);

    // Report the dot state upward (drives the collapsible setup box, idea D).
    useEffect(() => {
        onUiStateChange?.(uiState);
    }, [uiState, onUiStateChange]);

    // translate-or-fallback so the banner works before i18n keys exist.
    const tr = (key: string, fallback: string, vars?: Record<string, string | number>) => {
        let s = t(key);
        if (!s || s === key) s = fallback;
        if (vars) for (const [k, v] of Object.entries(vars)) s = s.replace(`{${k}}`, String(v));
        return s;
    };

    const message =
        loading
            ? tr('bridge.checking', 'Checking {app}…', { app })
            : uiState === 'green'
              ? tr('bridge.active', '{app} is running, the local bridge is active.', { app })
              : uiState === 'red'
                ? tr(
                      'bridge.notInstalled',
                      '{app} was not found on this machine. Install it to use this mode.',
                      { app },
                  )
                : tr(
                      'bridge.inactive',
                      '{app} is installed but the local bridge is not responding on 127.0.0.1:{port}. Start it and sign in.',
                      { app, port: probedPort ?? '' },
                  );

    return (
        <div className={`mt-2 text-[11px] border rounded px-2 py-1.5 leading-snug ${BOX[loading ? 'amber' : uiState]}`}>
            <div className="flex items-start gap-2">
                <span className="mt-0.5">
                    <StatusDot state={loading ? 'amber' : uiState} />
                </span>
                <div className="flex-1 min-w-0">
                    <span>{message}</span>
                    {uiState === 'red' && !loading && (
                        <span className="block mt-0.5 font-medium">
                            {tr('bridge.saveDisabled', 'Save is disabled until {app} is installed.', { app })}
                        </span>
                    )}
                </div>
                <button
                    type="button"
                    onClick={refresh}
                    title={tr('bridge.recheck', 'Re-check')}
                    className="shrink-0 inline-flex items-center text-current opacity-60 hover:opacity-100"
                >
                    <RefreshCw size={12} className={loading ? 'animate-spin' : ''} />
                </button>
            </div>
        </div>
    );
};

export default BridgeStatusBanner;
