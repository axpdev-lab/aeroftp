// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { useCallback, useLayoutEffect, useRef } from 'react';
import type { FtpSession } from '../types';

type Session = Pick<FtpSession, 'id' | 'status' | 'savedServerId' | 'cryptOverlay'>
    & { connectionParams?: Pick<FtpSession['connectionParams'], 'savedServerId'> };
type Waiter = { profileId: string; requireOverlay: boolean; finish: (live: boolean) => void };

/** Resume may write only through the committed active session of its profile.
 * A successful async connect can finish just before React commits its new tab.
 * Wait for that commit, not an arbitrary sleep, with a bounded failure path.
 */
export function useResumeProfileGuard(connected: boolean, sessions: readonly Session[], activeId: string | null) {
    const state = useRef({ connected, sessions, activeId });
    const mounted = useRef(false);
    const pending = useRef(new Set<Waiter>());
    const isLiveOnProfile = useCallback((profileId: string, requireOverlay = false): boolean => {
        const current = state.current;
        const active = current.sessions.find(session => session.id === current.activeId);
        return mounted.current && current.connected && active?.status === 'connected'
            && (active.savedServerId === profileId || active.connectionParams?.savedServerId === profileId)
            && (!requireOverlay || !!active.cryptOverlay?.vaultId);
    }, []);

    useLayoutEffect(() => {
        mounted.current = true;
        state.current = { connected, sessions, activeId };
        for (const waiter of pending.current) {
            if (isLiveOnProfile(waiter.profileId, waiter.requireOverlay)) waiter.finish(true);
        }
    });
    useLayoutEffect(() => () => {
        mounted.current = false;
        for (const waiter of pending.current) waiter.finish(false);
    }, []);

    const waitForLiveProfile = useCallback((profileId: string, requireOverlay = false): Promise<boolean> => {
        if (isLiveOnProfile(profileId, requireOverlay)) return Promise.resolve(true);
        if (!mounted.current) return Promise.resolve(false);
        return new Promise(resolve => {
            const waiter: Waiter = { profileId, requireOverlay, finish: live => {
                clearTimeout(timer);
                pending.current.delete(waiter);
                resolve(live);
            } };
            const timer = setTimeout(() => waiter.finish(false), requireOverlay ? 30_000 : 1000);
            pending.current.add(waiter);
        });
    }, [isLiveOnProfile]);

    return { isLiveOnProfile, waitForLiveProfile };
}
