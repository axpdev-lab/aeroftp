// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import appSource from '../App.tsx?raw';
import dialogsSource from '../components/Dialogs/index.tsx?raw';

const notifyCount = (source: string) => source.split('notifyVaultReady();').length - 1;

describe('vault readiness notifications', () => {
    it('signals boot completion, unlock and master-password setup exactly once each', () => {
        const unlock = appSource.slice(appSource.indexOf('<LockScreen'), appSource.indexOf('{/* Account Lock Screen'));
        const setup = appSource.slice(appSource.indexOf('<MasterPasswordSetupDialog'), appSource.indexOf('onClose={() =>', appSource.indexOf('<MasterPasswordSetupDialog')));
        const boot = appSource.slice(appSource.indexOf('const initVault = async'), appSource.indexOf('// Multi-user lock screen'));

        expect(notifyCount(unlock)).toBe(1);
        expect(notifyCount(setup)).toBe(1);
        expect(notifyCount(boot)).toBe(1);
        expect(notifyCount(appSource)).toBe(3);
        expect(notifyCount(dialogsSource)).toBe(0);
    });
});
