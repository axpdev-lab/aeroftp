// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// #1523 CodeRabbit: the profile binding of a scope under the login folder
// came back with no anchor. The re-unlock after a cache miss then never
// matched the session's vault and opened the ad-hoc dialog, and the locked
// prompt opened at the root.

import { expect, it, vi } from 'vitest';
import { transformWithOxc } from 'vite';
import app from '../App.tsx?raw';
import { isLoginRelativeScope, normCryptScope, placeUnderLoginFolder, profileOverlayScope } from './cryptScope';
import { overlayAnchor } from './cryptUnlockTarget';
import { normalizeOverlayAnchor } from './overlayScope';

function block(start: string, end: string): string {
    const a = app.indexOf(start);
    const b = app.indexOf(end, a);
    if (a < 0 || b < 0) throw new Error(`Missing production block: ${start}`);
    return app.slice(a, b);
}

const helpers = (await transformWithOxc([
    block('  const placeLoginRelativeScope =', '  // The login folder of the live session'),
    block('  const getEnabledProfileOverlay =', '  // Cheap, side-effect-free probe'),
].join('\n'), 'profile-overlay.ts')).code;
const prompt = (await transformWithOxc(block('  const openLockedOverlayUnlockPrompt =', '  // Where an ad-hoc unlock dialog opens'), 'locked-prompt.ts')).code;
const reunlock = (await transformWithOxc(block('    if (sess?.savedServerId) {', '    // Ad-hoc overlay (no saved profile)'), 're-unlock.ts')).code;

const sftp = { id: 'S', username: 'u', protocol: 'sftp', initialPath: 'docs', aeroCryptOverlay: { enabled: true, kind: 'rclone-crypt', remoteScope: '' } };

function overlayOf(loginFolder: string | Error) {
    const invoke = vi.fn(async (cmd: string) => {
        if (cmd !== 'provider_login_folder') throw new Error('cache miss');
        if (loginFolder instanceof Error) throw loginFolder;
        return loginFolder;
    });
    const deps = { invoke, loadSavedServerProfiles: async () => [sftp], isLoginRelativeScope, placeUnderLoginFolder, profileOverlayScope, normalizeOverlayAnchor };
    const getEnabledProfileOverlay = new Function(...Object.keys(deps), `${helpers}; return getEnabledProfileOverlay;`)(...Object.values(deps));
    return { invoke, getEnabledProfileOverlay };
}

async function promptScope(loginFolder: string | Error) {
    const { getEnabledProfileOverlay } = overlayOf(loginFolder);
    const setShowRcloneCryptUnlock = vi.fn();
    const deps = { getEnabledProfileOverlay, isLoginRelativeScope, overlayAnchor, setShowAeroCryptUnlock: vi.fn(), setShowRcloneCryptUnlock };
    const open = new Function(...Object.keys(deps), `${prompt}; return openLockedOverlayUnlockPrompt;`)(...Object.values(deps));
    await open({ savedServerId: 'S', kind: 'rclone-crypt' });
    return setShowRcloneCryptUnlock.mock.calls[0]?.[0];
}

it('places the binding of a scope under the login folder through the live session', async () => {
    expect(await overlayOf('/home/u').getEnabledProfileOverlay('S')).toEqual({ kind: 'rclone-crypt', anchor: '/home/u/docs' });
    // Unplaced, it keeps the form SFTP and SCP resolve, never the root.
    expect(await overlayOf(new Error('no session')).getEnabledProfileOverlay('S')).toEqual({ kind: 'rclone-crypt', anchor: '~/docs' });
});

it('opens the locked prompt at the binding folder under the login folder', async () => {
    expect(await promptScope('/home/u')).toEqual({ scope: '/home/u/docs', savedServerId: 'S' });
    expect(await promptScope(new Error('no session'))).toEqual({ scope: '~/docs', savedServerId: 'S' });
});

it('re-derives the binding vault after a cache miss when it is the session one', async () => {
    const run = async (loginFolder: string | Error, sessionScope: string) => {
        const { invoke, getEnabledProfileOverlay } = overlayOf(loginFolder);
        let done!: () => void;
        const finished = new Promise<void>((resolve) => { done = resolve; });
        const openAdHocCryptUnlock = vi.fn(() => done());
        const setPendingOverlayUnlock = vi.fn(() => done());
        const deps = { sess: { savedServerId: 'S', cryptOverlayScope: sessionScope }, kind: 'rclone-crypt', currentRemotePath: '/home/u/docs',
            invoke, getEnabledProfileOverlay, isLoginRelativeScope, normCryptScope, openAdHocCryptUnlock, setPendingOverlayUnlock,
            overlayReloadedVaultRef: { current: 'x' } };
        new Function(...Object.keys(deps), `return (() => { ${reunlock} })();`)(...Object.values(deps));
        await finished;
        return { reDerived: setPendingOverlayUnlock.mock.calls.length === 1, adHoc: openAdHocCryptUnlock.mock.calls[0] };
    };
    expect(await run('/home/u', '/home/u/docs')).toEqual({ reDerived: true, adHoc: undefined });
    // Another folder's vault, or a binding the session cannot place, goes back to the dialog.
    expect(await run('/home/u', '/home/u/other')).toEqual({ reDerived: false, adHoc: ['rclone-crypt', '/home/u/other'] });
    expect(await run(new Error('no session'), '/home/u/docs')).toEqual({ reDerived: false, adHoc: ['rclone-crypt', '/home/u/docs'] });
});
