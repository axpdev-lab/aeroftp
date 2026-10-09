// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { act, createElement } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { ServerProfile } from '../../types';

const invoke = vi.hoisted(() => vi.fn());
vi.mock('@tauri-apps/api/core', () => ({ invoke }));
vi.mock('../../i18n', () => ({ useTranslation: () => (key: string) => key }));
vi.mock('../../hooks/useActivityLog', () => ({ useActivityLog: () => ({ log: () => 'log-id' }) }));

import { MyServersPanel } from './MyServersPanel';
import { storeSavedServerProfiles } from '../../utils/serverProfileStore';

const profile = (bucket: string): ServerProfile => ({
    id: 'srv_minio',
    name: 'Lab MinIO',
    host: 'https://minio.example.invalid',
    port: 443,
    username: 'access-key',
    protocol: 's3',
    providerId: 'minio',
    hasStoredCredential: true,
    options: { bucket, region: 'us-east-1', endpoint: 'https://minio.example.invalid' },
} as ServerProfile);

type Load = { resolve: (profiles: ServerProfile[]) => void };

let root: Root;
let host: HTMLDivElement;
let loads: Load[];
let onEdit: ReturnType<typeof vi.fn<(profile: ServerProfile) => void>>;

const panel = (lastUpdate: number) => createElement(MyServersPanel, {
    onConnect: () => {},
    onEdit,
    onQuickConnect: () => {},
    lastUpdate,
});

const editButton = () => host.querySelector<HTMLButtonElement>('button[title="common.edit"]');
const editedBucket = () => onEdit.mock.lastCall?.[0]?.options?.bucket;

beforeEach(() => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    vi.stubGlobal('ResizeObserver', class { observe() {} unobserve() {} disconnect() {} });
    localStorage.clear();
    loads = [];
    onEdit = vi.fn<(profile: ServerProfile) => void>();
    // Every profile read waits until the test answers it, like the ~3 s
    // partition read of a debug build; every other command answers at once.
    invoke.mockReset();
    invoke.mockImplementation((command: string) => {
        if (command === 'user_partitions_load_active_server_profiles') {
            return new Promise<ServerProfile[]>((resolve) => { loads.push({ resolve }); });
        }
        if (command === 'user_partitions_save_active_server_profiles') return Promise.resolve();
        return Promise.resolve(null);
    });
    host = document.createElement('div');
    document.body.append(host);
    root = createRoot(host);
});

afterEach(async () => {
    await act(async () => root.unmount());
    host.remove();
    vi.unstubAllGlobals();
});

async function mountWith(saved: ServerProfile) {
    await act(async () => root.render(panel(0)));
    await act(async () => loads.splice(0).forEach((load) => load.resolve([saved])));
    expect(editButton(), 'the saved profile renders as a card').not.toBeNull();
}

describe('My Servers Edit after a profile Save', () => {
    it('opens Edit with the profile just saved, before the list re-read answers', async () => {
        await mountWith(profile('old-bucket'));

        // The edit form saves the new bucket; App then bumps lastUpdate, whose
        // re-read is still in flight when the user opens Edit again.
        await act(async () => storeSavedServerProfiles([profile('new-bucket')]));
        await act(async () => root.render(panel(1)));
        await act(async () => editButton()!.click());

        expect(editedBucket()).toBe('new-bucket');
    });

    it('keeps the saved profile when a read that started before the Save answers after it', async () => {
        await mountWith(profile('old-bucket'));

        // A re-read started before the Save (any earlier lastUpdate bump)...
        await act(async () => root.render(panel(1)));
        const startedBeforeSave = loads.splice(0);
        expect(startedBeforeSave).toHaveLength(1);
        await act(async () => storeSavedServerProfiles([profile('new-bucket')]));
        // ...answers with the list as it was when it was read.
        await act(async () => startedBeforeSave[0].resolve([profile('old-bucket')]));
        await act(async () => editButton()!.click());

        expect(editedBucket()).toBe('new-bucket');
    });
});
