// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
import React, { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';

const relocateServerProfile = vi.fn();
vi.mock('../utils/userPartitions', () => ({
    listUsers: vi.fn(async () => [
        { id: 2, name: 'Bob', isActive: false, hasPassphrase: false },
    ]),
    relocateServerProfile: (...args: unknown[]) => relocateServerProfile(...args),
}));
vi.mock('../i18n', () => ({ useTranslation: () => (key: string) => key }));
vi.mock('../hooks/useDraggableModal', () => ({
    useDraggableModal: () => ({ panelProps: {}, dragHandleProps: {} }),
}));
vi.mock('./UserAvatar', () => ({ UserAvatar: () => null }));

import { ProfileRelocateDialog } from './ProfileRelocateDialog';
import type { ServerProfile } from '../types';

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

let host: HTMLDivElement;
let root: Root;

beforeEach(() => {
    host = document.createElement('div');
    document.body.append(host);
    root = createRoot(host);
    relocateServerProfile.mockReset();
});
afterEach(async () => {
    await act(async () => root.unmount());
    host.remove();
});

it('reports a Move whose source cleanup failed after the commit as moved, with a warning', async () => {
    relocateServerProfile.mockRejectedValue(
        "RELOCATION_COMMITTED: profile srv_new is saved in target user 2; removal of the source's Proton session failed: refused",
    );
    const toasts: { type: string; message: string }[] = [];
    const onToast = (event: Event) => toasts.push((event as CustomEvent).detail);
    window.addEventListener('aeroftp-toast', onToast);
    const onDone = vi.fn();
    const onClose = vi.fn();
    const profile = { id: 'srv_old', name: 'Drive', host: '', port: 0, username: '' } as ServerProfile;
    await act(async () => {
        root.render(<ProfileRelocateDialog mode="move" profile={profile} onClose={onClose} onDone={onDone} />);
    });
    const submit = [...host.querySelectorAll('button')].find(
        b => b.textContent?.trim() === 'savedServers.relocateSubmitMove',
    )!;
    await act(async () => { submit.click(); });
    window.removeEventListener('aeroftp-toast', onToast);
    expect(onDone).toHaveBeenCalledWith({ moved: true, alreadyPresent: false });
    expect(onClose).toHaveBeenCalledTimes(1);
    expect(toasts).toEqual([
        expect.objectContaining({ type: 'warning', message: 'savedServers.relocateMovedCleanupFailed' }),
    ]);
    expect(host.textContent).not.toContain('savedServers.relocateError');
});

it('keeps any other relocation error in the dialog', async () => {
    relocateServerProfile.mockRejectedValue('TARGET_LOCKED');
    const onDone = vi.fn();
    const profile = { id: 'srv_old', name: 'Drive', host: '', port: 0, username: '' } as ServerProfile;
    await act(async () => {
        root.render(<ProfileRelocateDialog mode="move" profile={profile} onClose={() => undefined} onDone={onDone} />);
    });
    const submit = [...host.querySelectorAll('button')].find(
        b => b.textContent?.trim() === 'savedServers.relocateSubmitMove',
    )!;
    await act(async () => { submit.click(); });
    expect(onDone).not.toHaveBeenCalled();
    expect(host.textContent).toContain('savedServers.relocateError');
});
