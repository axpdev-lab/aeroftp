// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import { useEffect, useMemo, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import type { ConnectionParams } from '../types';

export type TerminalCwdSource =
    | 'local'
    | 'local2'
    | 'remote-mount'
    | 'remote-unmounted'
    | 'idle';

export interface TerminalCwdResult {
    /**
     * Effective working directory for a freshly spawned local PTY shell.
     * `undefined` means "open the shell in the user's home" (SSHTerminal's
     * default `~` fallback). Existing PTY tabs are NOT retroactively
     * `cd`-ed; the cwd is only consumed by `spawn_shell` at tab creation
     * time. See [APPENDIX-Z Z.3.10](../../docs/dev/roadmap/APPENDIX-Z_AeroRsync-and-AeroFile-Convergence.md).
     */
    cwd: string | undefined;
    /**
     * Provenance of the cwd, exposed mostly for testing and for the future
     * Z.3.10 follow-up that renders the cwd source as a hint in the
     * terminal toolbar.
     */
    source: TerminalCwdSource;
}

export interface TerminalCwdInputs {
    activePanel: 'remote' | 'local';
    activeLocalPanelId: 'local' | 'local2';
    currentLocalPath: string;
    currentLocalPath2: string;
    isDualLocalAeroFileMode: boolean;
    /** Connection identity (savedServerId / server / etc.) so the resolver
     * can branch on "we are at least nominally connected to a remote". */
    isConnectionPresent: boolean;
    /**
     * Mountpoint for the currently-connected server when a running
     * mount is live, `null` otherwise. The synchronous resolver below
     * stays pure: the mount probe is a separate concern handled by the
     * React hook that wraps it.
     */
    mountpoint: string | null;
}

/**
 * Pure resolution of the terminal cwd given the current panel state.
 * Extracted from the hook so it stays unit-testable without spinning up
 * `@testing-library/react`.
 *
 * Resolution order:
 *   1. Active panel is `local` and we're in dual-local AeroFile mode →
 *      the focused panel's path (`local` or `local2`).
 *   2. Active panel is `local` in single-pane mode → `currentLocalPath`.
 *   3. Active panel is `remote` and the connected server has a running
 *      mount → the mountpoint.
 *   4. Active panel is `remote` and the server is not mounted →
 *      `undefined` (no fake cwd; the terminal opens in `~` and the user
 *      can use remote-native commands instead of pretending the remote
 *      tree is a local fs).
 *   5. Not connected → `currentLocalPath` of the focused panel.
 */
export function resolveTerminalCwd(input: TerminalCwdInputs): TerminalCwdResult {
    const {
        activePanel,
        activeLocalPanelId,
        currentLocalPath,
        currentLocalPath2,
        isDualLocalAeroFileMode,
        isConnectionPresent,
        mountpoint,
    } = input;

    if (activePanel === 'local') {
        if (isDualLocalAeroFileMode && activeLocalPanelId === 'local2') {
            return { cwd: currentLocalPath2 || undefined, source: 'local2' };
        }
        return { cwd: currentLocalPath || undefined, source: 'local' };
    }
    // activePanel === 'remote'
    if (!isConnectionPresent) {
        return { cwd: currentLocalPath || undefined, source: 'idle' };
    }
    if (mountpoint) {
        return { cwd: mountpoint, source: 'remote-mount' };
    }
    return { cwd: undefined, source: 'remote-unmounted' };
}

interface UseTerminalCwdArgs {
    activePanel: 'remote' | 'local';
    activeLocalPanelId: 'local' | 'local2';
    currentLocalPath: string;
    currentLocalPath2: string;
    isDualLocalAeroFileMode: boolean;
    isConnected: boolean;
    connectionParams: ConnectionParams;
}

interface MountListResponse {
    storage_mode?: string;
    mounts: Array<{
        config: { id: string; profile: string; mountpoint?: string };
        status: { state: string };
    }>;
}

/**
 * React hook flavour of `resolveTerminalCwd`: probes the FUSE mount
 * registry whenever the active panel turns remote and the connection
 * identity changes, then feeds the result into the pure resolver.
 */
export function useTerminalCwd(args: UseTerminalCwdArgs): TerminalCwdResult {
    const {
        activePanel,
        activeLocalPanelId,
        currentLocalPath,
        currentLocalPath2,
        isDualLocalAeroFileMode,
        isConnected,
        connectionParams,
    } = args;

    const [mountpoint, setMountpoint] = useState<string | null>(null);
    const profileKey = useMemo(() => {
        if (!isConnected) return null;
        return (
            connectionParams.savedServerId
            || connectionParams.providerId
            || connectionParams.server
            || null
        );
    }, [
        isConnected,
        connectionParams.savedServerId,
        connectionParams.providerId,
        connectionParams.server,
    ]);

    useEffect(() => {
        let cancelled = false;
        if (activePanel !== 'remote' || !isConnected || !profileKey) {
            setMountpoint(null);
            return;
        }
        (async () => {
            try {
                const data = await invoke<MountListResponse>('mount_list');
                if (cancelled) return;
                const match = data.mounts?.find(row => {
                    if (row.status?.state !== 'running' && row.status?.state !== 'mounted') {
                        return false;
                    }
                    const profile = row.config?.profile || '';
                    return (
                        profile === connectionParams.savedServerId
                        || profile === connectionParams.providerId
                        || profile === connectionParams.server
                    );
                });
                setMountpoint(match?.config?.mountpoint || null);
            } catch {
                if (!cancelled) setMountpoint(null);
            }
        })();
        return () => { cancelled = true; };
    }, [
        activePanel,
        isConnected,
        profileKey,
        connectionParams.savedServerId,
        connectionParams.providerId,
        connectionParams.server,
    ]);

    return useMemo(
        () =>
            resolveTerminalCwd({
                activePanel,
                activeLocalPanelId,
                currentLocalPath,
                currentLocalPath2,
                isDualLocalAeroFileMode,
                isConnectionPresent: isConnected,
                mountpoint,
            }),
        [
            activePanel,
            activeLocalPanelId,
            currentLocalPath,
            currentLocalPath2,
            isDualLocalAeroFileMode,
            isConnected,
            mountpoint,
        ],
    );
}
