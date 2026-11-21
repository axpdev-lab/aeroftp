// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { createElement, useState } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, describe, expect, it } from 'vitest';
import { useCommitBarrier } from './useCommitBarrier';
import { useLatestCallback } from './useLatestCallback';

// Outside `act`, as in the app: React renders in a scheduler task, after the
// microtask an `await` resumes in.

type Session = { id: string; overlay: string | null };
type Navigation = { command: string; session: string; path: string };

let root: Root | null = null;
afterEach(() => {
    root?.unmount();
    root = null;
});

const settle = () => new Promise<void>(resolve => setTimeout(resolve, 20));

// Session A has an AeroVault overlay open; a launch opens profile B and then
// its folder, as App's openSavedProfile does through navigateAfterLaunch.
async function launchB(withBarrier: boolean): Promise<Navigation[]> {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = false;
    const navigations: Navigation[] = [];
    let launch!: (path: string) => Promise<void>;
    const App = () => {
        const [active, setActive] = useState<Session>({ id: 'A', overlay: 'vault-a' });
        const afterCommit = useCommitBarrier();
        const changeRemoteDirectory = useLatestCallback((path: string) => {
            navigations.push(active.overlay
                ? { command: 'aerovault_overlay_list', session: active.id, path }
                : { command: 'provider_change_dir', session: active.id, path });
        });
        launch = async (path: string) => {
            await Promise.resolve().then(() => setActive({ id: 'B', overlay: null }));
            if (withBarrier) await afterCommit();
            changeRemoteDirectory(path);
        };
        return null;
    };
    root = createRoot(document.createElement('div'));
    root.render(createElement(App));
    await settle();
    await launch('/b/');
    await settle();
    return navigations;
}

describe('useCommitBarrier', () => {
    it('lets a launch navigate on the session it opened, not on the overlay of the one before', async () => {
        expect(await launchB(true)).toEqual([
            { command: 'provider_change_dir', session: 'B', path: '/b/' },
        ]);
    });

    it('is what the launch needs: without it the navigation reads the session before', async () => {
        expect(await launchB(false)).toEqual([
            { command: 'aerovault_overlay_list', session: 'A', path: '/b/' },
        ]);
    });

    it('settles every waiter, the ones queued in the same tick included', async () => {
        (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = false;
        let afterCommit!: () => Promise<void>;
        const Probe = () => {
            afterCommit = useCommitBarrier();
            return null;
        };
        root = createRoot(document.createElement('div'));
        root.render(createElement(Probe));
        await settle();
        const order: number[] = [];
        await Promise.all([1, 2, 3].map(n => afterCommit().then(() => order.push(n))));
        expect(order).toEqual([1, 2, 3]);
    });
});
