// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import { createElement, useEffect } from 'react';
import { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { notifyVaultReady } from '../utils/secureStorage';
import { useAeroShareEnabled } from './useAeroShareEnabled';
import { useAeroShareReceiveSettings, type AeroShareReceiveSettings } from './useAeroShareReceiveSettings';
import { useCardLayout } from './useCardLayout';
import { useDiscoverHealthCheck } from './useDiscoverHealthCheck';
import { useFavoriteMarker } from './useFavoriteMarker';
import { useIntroHubIconSize } from './useIntroHubIconSize';
import { useMyServersDensity } from './useMyServersDensity';
import { useStorageThresholds } from './useStorageThresholds';

const mocks = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: mocks.invoke }));

interface PreferenceCase {
    name: string;
    usePreference: () => unknown;
    select: (value: unknown) => unknown;
    localValue: Record<string, unknown>;
    vaultValue: Record<string, unknown>;
    localExpected: unknown;
    vaultExpected: unknown;
}

const cases: PreferenceCase[] = [
    {
        name: 'useAeroShareEnabled',
        usePreference: useAeroShareEnabled,
        select: value => value,
        localValue: { aeroShareEnabled: true },
        vaultValue: { aeroShareEnabled: false },
        localExpected: true,
        vaultExpected: false,
    },
    {
        name: 'useAeroShareReceiveSettings',
        usePreference: useAeroShareReceiveSettings,
        select: value => value,
        localValue: {
            aeroShareReceiving: true,
            aeroShareAutoAcceptFriends: true,
            aeroShareNotifyOnReceive: true,
        },
        vaultValue: {
            aeroShareReceiving: false,
            aeroShareAutoAcceptFriends: false,
            aeroShareNotifyOnReceive: false,
        },
        localExpected: {
            receiving: true,
            autoAcceptFriends: true,
            notifyOnReceive: true,
        } satisfies AeroShareReceiveSettings,
        vaultExpected: {
            receiving: false,
            autoAcceptFriends: false,
            notifyOnReceive: false,
        } satisfies AeroShareReceiveSettings,
    },
    {
        name: 'useCardLayout',
        usePreference: useCardLayout,
        select: value => value,
        localValue: { cardLayout: 'compact' },
        vaultValue: { cardLayout: 'detailed' },
        localExpected: 'compact',
        vaultExpected: 'detailed',
    },
    {
        name: 'useFavoriteMarker',
        usePreference: useFavoriteMarker,
        select: value => value,
        localValue: { favoriteMarker: 'star' },
        vaultValue: { favoriteMarker: 'heart' },
        localExpected: 'star',
        vaultExpected: 'heart',
    },
    {
        name: 'useIntroHubIconSize',
        usePreference: useIntroHubIconSize,
        select: value => value,
        localValue: { introHubIconSize: 20 },
        vaultValue: { introHubIconSize: 30 },
        localExpected: 20,
        vaultExpected: 30,
    },
    {
        name: 'useDiscoverHealthCheck',
        usePreference: useDiscoverHealthCheck,
        select: value => (value as [boolean, (value: boolean) => void])[0],
        localValue: { discoverHealthCheck: true },
        vaultValue: { discoverHealthCheck: false },
        localExpected: true,
        vaultExpected: false,
    },
    {
        name: 'useStorageThresholds',
        usePreference: useStorageThresholds,
        select: value => (value as { thresholds: unknown }).thresholds,
        localValue: { storage_thresholds: { warn: 70, critical: 90 } },
        vaultValue: { storage_thresholds: { warn: 85, critical: 96 } },
        localExpected: { warn: 70, critical: 90 },
        vaultExpected: { warn: 85, critical: 96 },
    },
    {
        name: 'useMyServersDensity',
        usePreference: useMyServersDensity,
        select: value => (value as { density: unknown }).density,
        localValue: { my_servers_density: 'compact' },
        vaultValue: { my_servers_density: 'comfortable' },
        localExpected: 'compact',
        vaultExpected: 'comfortable',
    },
];

let root: Root;
let host: HTMLDivElement;

function credentialReads(): number {
    return mocks.invoke.mock.calls.filter(([command]) => command === 'get_credential').length;
}

beforeEach(() => {
    Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
    localStorage.clear();
    mocks.invoke.mockReset().mockImplementation(async (command: string) => {
        if (command === 'get_credential') throw new Error('STORE_NOT_READY');
        return null;
    });
    host = document.createElement('div');
    document.body.append(host);
    root = createRoot(host);
});

afterEach(async () => {
    await act(async () => root.unmount());
    host.remove();
});

describe('vault-ready lightweight preferences', () => {
    it.each(cases)('$name applies the vault value after readiness without remounting', async testCase => {
        localStorage.setItem('aeroftp_settings', JSON.stringify(testCase.localValue));
        let latest: unknown;
        let mounts = 0;
        function Harness() {
            useEffect(() => {
                mounts++;
            }, []);
            latest = testCase.usePreference();
            return null;
        }

        await act(async () => root.render(createElement(Harness)));
        expect(testCase.select(latest)).toEqual(testCase.localExpected);
        const mounted = mounts;

        mocks.invoke.mockImplementation(async (command: string) => {
            if (command === 'get_credential') return JSON.stringify(testCase.vaultValue);
            return null;
        });
        await act(async () => notifyVaultReady());

        expect(testCase.select(latest)).toEqual(testCase.vaultExpected);
        expect(mounts).toBe(mounted);
        expect(credentialReads()).toBe(2);
        expect(mocks.invoke.mock.calls.filter(([command]) => command === 'store_credential')).toHaveLength(0);
    });

    it.each(cases)('$name does not let an older mount read overwrite an inline settings event', async testCase => {
        let resolveMount!: (value: string) => void;
        mocks.invoke.mockImplementation((command: string) => {
            if (command === 'get_credential') {
                return new Promise<string>(resolve => { resolveMount = resolve; });
            }
            return Promise.resolve(null);
        });
        let latest: unknown;
        function Harness() {
            latest = testCase.usePreference();
            return null;
        }
        await act(async () => root.render(createElement(Harness)));

        await act(async () => {
            window.dispatchEvent(new CustomEvent('aeroftp-settings-changed', { detail: testCase.vaultValue }));
        });
        expect(testCase.select(latest)).toEqual(testCase.vaultExpected);

        await act(async () => resolveMount(JSON.stringify(testCase.localValue)));
        expect(testCase.select(latest)).toEqual(testCase.vaultExpected);
    });

    it('does not let an older mount read overwrite a newer readiness read', async () => {
        let resolveMount!: (value: string) => void;
        let resolveReady!: (value: string) => void;
        let reads = 0;
        mocks.invoke.mockImplementation((command: string) => {
            if (command !== 'get_credential') return Promise.resolve(null);
            reads++;
            return new Promise<string>(resolve => {
                if (reads === 1) resolveMount = resolve;
                else resolveReady = resolve;
            });
        });
        let latest: ReturnType<typeof useCardLayout> | undefined;
        function Harness() {
            latest = useCardLayout();
            return null;
        }
        await act(async () => root.render(createElement(Harness)));
        await act(async () => notifyVaultReady());
        await act(async () => resolveReady(JSON.stringify({ cardLayout: 'detailed' })));
        expect(latest).toBe('detailed');

        await act(async () => resolveMount(JSON.stringify({ cardLayout: 'compact' })));
        expect(latest).toBe('detailed');
        expect(credentialReads()).toBe(2);
    });

    it('preserves the synchronous My Servers density localStorage seed', async () => {
        localStorage.setItem('aeroftp-my-servers-density', 'comfortable');
        mocks.invoke.mockImplementation((command: string) => {
            if (command === 'get_credential') return new Promise<string>(() => undefined);
            return Promise.resolve(null);
        });
        let latest: ReturnType<typeof useMyServersDensity> | undefined;
        function Harness() {
            latest = useMyServersDensity();
            return null;
        }

        act(() => root.render(createElement(Harness)));
        expect(latest?.density).toBe('comfortable');
    });
});
