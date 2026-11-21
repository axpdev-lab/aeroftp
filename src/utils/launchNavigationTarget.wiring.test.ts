// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { transformWithOxc } from 'vite';
import { describe, expect, it, vi } from 'vitest';
import app from '../App.tsx?raw';
import { launchFolderToOpen } from './launchUrlProfile';

// The production launch paths of App.tsx, run with stand-ins for what they
// call: a launch opens its folder only on the tab it made active, never on
// the one the user is still on after a refused host key or a failed switch.

function slice(name: string, end = '\n  };\n'): string {
    const start = app.indexOf(`  const ${name} = async`);
    const stop = app.indexOf(end, start);
    if (start < 0 || stop < 0) throw new Error(`App.tsx has no ${name}`);
    return app.slice(start, stop + end.length);
}

async function load<T>(name: string, context: Record<string, unknown>, end?: string): Promise<T> {
    const { code } = await transformWithOxc(slice(name, end), `${name}.ts`);
    return new Function(...Object.keys(context), `${code}\nreturn ${name};`)(...Object.values(context));
}

const panels = { setShowConnectionScreen: vi.fn(), setShowRemotePanel: vi.fn(), setShowLocalPreview: vi.fn() };
const tabB = { id: 'B', savedServerId: 'profile-b', connectionParams: { protocol: 'sftp', server: 'b.example', port: 22, savedServerId: 'profile-b' } };

describe('a launch navigates only on the tab it made active', () => {
    it.each([[false, []], [true, [['/b/', 'sftp', 'B']]]] as const)(
        'a reused connection tab: switch %s', async (switched, navigations) => {
            const navigateAfterLaunch = vi.fn();
            const open = await load<(req: object) => Promise<void>>('openRemoteUrlSession', {
                ...panels, sessions: [tabB], manifestDefaultPort: () => 22, openSessionMatchesLaunch: () => true,
                switchSession: async () => switched, launchFolderToOpen, navigateAfterLaunch,
            });
            await open({ kind: 'remoteUrl', protocol: 'sftp', server: 'b.example', initialPath: '/b/' });
            expect(navigateAfterLaunch.mock.calls).toEqual(navigations);
        });

    it.each([[false, []], [null, []], [true, [['/b/', 'sftp', 'B']]]] as const)(
        'a sign-in profile with an open tab: activation %s', async (activated, navigations) => {
            const navigateAfterLaunch = vi.fn();
            const openSavedProfile = await load<(profile: object, path?: string) => Promise<void>>('openSavedProfile', {
                ...panels, sessions: [tabB], activateSavedSession: async () => activated,
                profileConnector: { current: null }, launchFolderToOpen, navigateAfterLaunch,
            });
            await openSavedProfile({ id: 'profile-b', protocol: 'sftp' }, '/b/');
            expect(navigateAfterLaunch.mock.calls).toEqual(navigations);
        });

    it('a sign-in profile with no tab connects and is followed by its phase alone', async () => {
        const navigateAfterLaunch = vi.fn();
        const openSavedProfile = await load<(profile: object, path?: string) => Promise<void>>('openSavedProfile', {
            ...panels, sessions: [], activateSavedSession: async () => null, ConnectScope: class {},
            profileConnector: { current: async () => 'connected' }, launchFolderToOpen, navigateAfterLaunch,
        });
        await openSavedProfile({ id: 'profile-c', protocol: 'dropbox' }, '/c/');
        expect(navigateAfterLaunch.mock.calls).toEqual([['/c/', 'dropbox', undefined]]);
    });

    it('switchSession resolves false on a refused host key and true on the tab already active', async () => {
        const setActiveSessionId = vi.fn();
        const context = (active: string) => ({
            sessions: [tabB], activeSessionId: active, showRemotePanel: true, setShowRemotePanel: vi.fn(),
            setActiveSessionId, setIsConnected: vi.fn(), ConnectScope: class {},
            isSshProtocol: (protocol?: string) => protocol === 'sftp',
            normalizeProviderConnectionParams: (params: object) => params,
            checkSftpHostKey: async () => false,
        });
        const scope = { assert: () => undefined, step: <T>(run: () => Promise<T>) => run() };
        const refused = await load<(id: string, scope: object) => Promise<boolean>>('switchSession', context('A'));
        expect(await refused('B', scope)).toBe(false);
        expect(setActiveSessionId).not.toHaveBeenCalled();
        const already = await load<(id: string, scope: object) => Promise<boolean>>('switchSession', context('B'));
        expect(await already('B', scope)).toBe(true);
        expect(await already('missing', scope)).toBe(false);
    });

    // App's navigateAfterLaunch with a commit the test settles by hand, and
    // what the user does while it is pending.
    async function pendingLaunch() {
        const order: string[] = [];
        let commit!: () => void;
        const state = { active: 'B', phase: { current: 7 } };
        const navigateAfterLaunch = await load<(path: string, protocol?: string, sessionId?: string) => Promise<unknown>>('navigateAfterLaunch', {
            afterCommit: () => new Promise<void>(resolve => { commit = () => { order.push('commit'); resolve(); }; }),
            latestChangeRemoteDirectory: (path: string, protocol?: string) => { order.push(`navigate ${path} ${protocol} on ${state.active}`); },
            latestActiveSessionId: () => state.active,
            remoteConnectPhaseVersionRef: state.phase,
            logger: { debug: () => order.push('skipped') },
        });
        return { order, state, navigateAfterLaunch, commit: () => commit() };
    }

    it('navigates only once the state of the opened session has committed', async () => {
        const { order, navigateAfterLaunch, commit } = await pendingLaunch();
        const navigation = navigateAfterLaunch('/b/', 'sftp', 'B');
        await Promise.resolve();
        expect(order).toEqual([]);
        commit();
        await navigation;
        expect(order).toEqual(['commit', 'navigate /b/ sftp on B']);
    });

    it('opens nothing on a tab the user picked while the commit was pending', async () => {
        const { order, state, navigateAfterLaunch, commit } = await pendingLaunch();
        const navigation = navigateAfterLaunch('/b/', 'sftp', 'B');
        state.active = 'C';
        commit();
        expect(await navigation).toBeNull();
        expect(order).toEqual(['commit', 'skipped']);
    });

    it('opens nothing once a newer switch or connect began while the commit was pending', async () => {
        for (const sessionId of ['B', undefined]) {
            const { order, state, navigateAfterLaunch, commit } = await pendingLaunch();
            const navigation = navigateAfterLaunch('/b/', 'sftp', sessionId);
            state.phase.current += 1;
            commit();
            expect(await navigation).toBeNull();
            expect(order).toEqual(['commit', 'skipped']);
        }
    });

    it('a fresh connect with no known tab still navigates when nothing changed', async () => {
        const { order, navigateAfterLaunch, commit } = await pendingLaunch();
        const navigation = navigateAfterLaunch('/c/', 'dropbox');
        commit();
        await navigation;
        expect(order).toEqual(['commit', 'navigate /c/ dropbox on B']);
    });

    it("keeps My Servers' meaning: an open tab handled the request even when its switch was refused", async () => {
        const goToActiveSession = await load<(id: string) => Promise<boolean>>('goToActiveSession', {
            activateSavedSession: async (id: string) => (id === 'profile-b' ? false : null), ConnectScope: class {},
        }, ' !== null;\n');
        expect(await goToActiveSession('profile-b')).toBe(true);
        expect(await goToActiveSession('profile-c')).toBe(false);
    });
});
