// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { transformWithOxc } from 'vite';
import { expect, it, vi } from 'vitest';
import usersPanel from './UsersManagePanel.tsx?raw';
import userDropdown from './UserDropdown.tsx?raw';

// Run each component's own notifier against a recording window: accountChanged
// resets the GUI controller's sessions, so only a real account switch may send it.
async function notifiedDetail(source: string) {
    const start = source.indexOf('const notifyProfilesChanged =');
    const end = source.indexOf('\n};\n', start);
    if (start < 0 || end < 0) throw new Error('Missing notifyProfilesChanged');
    const { code } = await transformWithOxc(source.slice(start, end + 3), 'notify.ts');
    const dispatchEvent = vi.fn();
    const window = { dispatchEvent };
    const notify = new Function('window', 'PROFILES_CHANGED_EVENT', 'CustomEvent', `${code}\nreturn notifyProfilesChanged;`)(window, 'profiles-changed', CustomEvent);
    const onChanged = vi.fn();
    notify(onChanged);
    expect(onChanged).toHaveBeenCalledTimes(1);
    return (dispatchEvent.mock.calls[0][0] as CustomEvent).detail;
}

it('user management edits are profile changes, not an account switch', async () => {
    expect((await notifiedDetail(usersPanel))?.accountChanged).toBeUndefined();
});

it('locking or unlocking from the user menu is an account switch', async () => {
    expect(await notifiedDetail(userDropdown)).toEqual({ accountChanged: true });
});
