// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { KeystoreMigrationWizard } from './KeystoreMigrationWizard';

const mocks = vi.hoisted(() => ({
    read: vi.fn(),
    add: vi.fn(),
}));

vi.mock('../utils/serverProfileStore', () => ({
    readSavedServerProfilesForWrite: mocks.read,
    addMigratedServerProfiles: mocks.add,
}));
vi.mock('../utils/secureStorage', () => ({ secureStore: vi.fn() }));
vi.mock('../i18n', () => ({ useTranslation: () => (key: string) => key }));
vi.mock('../hooks/useDraggableModal', () => ({
    useDraggableModal: () => ({ panelProps: {}, dragHandleProps: {} }),
}));

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const profiles = [{ id: 'srv_old', name: 'Old', host: 'h', port: 21, username: 'u' }];

describe('KeystoreMigrationWizard account binding', () => {
    let host: HTMLDivElement;
    let root: Root;

    /** Open the wizard and press through to the migration, whose start is on a timer. */
    const startMigration = async () => {
        await act(async () => {
            root.render(React.createElement(KeystoreMigrationWizard, { isOpen: true, onComplete: vi.fn(), onSkip: vi.fn() }));
        });
        const forward = () => [...host.querySelectorAll('button')].find(b => b.className.includes('bg-blue-600')) as HTMLButtonElement;
        await act(async () => { forward().click(); });
        await act(async () => { forward().click(); });
        await act(async () => { await vi.advanceTimersByTimeAsync(200); });
    };

    beforeEach(() => {
        vi.useFakeTimers();
        mocks.read.mockReset();
        mocks.add.mockReset();
        mocks.add.mockResolvedValue([]);
        localStorage.setItem('aeroftp-saved-servers', JSON.stringify(profiles));
        host = document.createElement('div');
        document.body.appendChild(host);
        root = createRoot(host);
    });

    afterEach(async () => {
        await act(async () => { root.unmount(); });
        host.remove();
        localStorage.clear();
        vi.useRealTimers();
    });

    it('waits for the account read at detection, and writes bound to that account', async () => {
        let answer: (read: { userId: number; profiles: unknown[] }) => void = () => {};
        mocks.read.mockReturnValue(new Promise(resolve => { answer = resolve; }));
        await startMigration();
        expect(mocks.add).not.toHaveBeenCalled();
        await act(async () => { answer({ userId: 4, profiles: [] }); });
        expect(mocks.add).toHaveBeenCalledTimes(1);
        expect(mocks.add.mock.calls[0][1]).toBe(4);
    });

    it('reads the legacy store as no account, and goes on without one after a failed read', async () => {
        mocks.read.mockResolvedValue({ profiles: [] });
        await startMigration();
        expect(mocks.add.mock.calls[0][1]).toBeNull();
        await act(async () => { root.unmount(); });
        root = createRoot(host);
        mocks.add.mockClear();
        mocks.read.mockRejectedValue(new Error('STORE_NOT_READY'));
        await startMigration();
        expect(mocks.add.mock.calls[0][1]).toBeUndefined();
    });
});
