// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { act, createElement } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { TransferToastState } from '../components/Transfer';
import { useTransferEvents } from './useTransferEvents';
import { bucketSpeeds } from '../utils/speedProfile';

const listeners = vi.hoisted(() => new Map<string, (event: { payload: unknown }) => void>());
const toasts = vi.hoisted(() => [] as Array<TransferToastState | null>);
vi.mock('@tauri-apps/api/event', () => ({
    listen: (name: string, handler: (event: { payload: unknown }) => void) => {
        listeners.set(name, handler);
        return Promise.resolve(() => undefined);
    },
}));
vi.mock('../components/Transfer/TransferToastContainer', () => ({
    dispatchTransferToast: (state: TransferToastState | null) => { toasts.push(state); },
}));

type Options = Parameters<typeof useTransferEvents>[0];

let root: Root;
let host: HTMLDivElement;

const mount = async () => {
    const options = {
        t: (key: string) => key,
        activityLog: { updateEntry: () => undefined },
        humanLog: { logStart: () => 'log', logRaw: () => 'log' },
        transferQueue: { items: [], markAsFolder: vi.fn(), startTransfer: vi.fn(), setProgress: vi.fn() },
        notify: { success: () => null, error: () => null, info: () => null, warning: () => null },
        setActiveTransfer: () => undefined,
        loadRemoteFiles: () => undefined,
        loadLocalFiles: () => undefined,
        currentLocalPath: '/home/u/work',
        currentRemotePath: '/srv/data',
        maxChannels: 3,
    } as unknown as Options;
    const Probe = () => {
        useTransferEvents(options);
        return null;
    };
    await act(async () => root.render(createElement(Probe)));
};

const fire = async (channel: string, payload: Record<string, unknown>) => {
    await act(async () => listeners.get(channel)!({ payload }));
};

const laneProgress = (id: string, transferred: number, speed: number) => fire('transfer_event', {
    event_type: 'progress',
    transfer_id: id,
    filename: `${id}.bin`,
    direction: 'upload',
    progress: {
        transfer_id: id, filename: `${id}.bin`, transferred, total: 1000,
        percentage: transferred / 10, speed_bps: speed, eta_seconds: 0, direction: 'upload',
    },
});

const lastToast = (): TransferToastState => {
    const state = toasts[toasts.length - 1];
    if (!state) throw new Error('no toast dispatched');
    return state;
};

beforeEach(() => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    listeners.clear();
    toasts.length = 0;
    host = document.createElement('div');
    document.body.append(host);
    root = createRoot(host);
});
afterEach(async () => {
    await act(async () => root.unmount());
    host.remove();
});

describe('useTransferEvents batch toast', () => {
    it('updates the aggregate speed on every lane update, not only when a file finishes', async () => {
        await mount();
        await fire('transfer_batch_started', { batch_id: 'ul-files-1', display_name: '2 files', direction: 'upload', total: 2 });
        await fire('transfer_batch_progress', {
            batch_id: 'ul-files-1', completed: 0, skipped: 0, failed: 0, active: 2, total: 2,
            bytes_transferred: 0, bytes_total: 2000,
        });

        // No folder-level progress event arrives while the two large files run:
        // the summary must still follow the lanes.
        await laneProgress('ul-files-1-a', 100, 1000);
        expect(lastToast().summary.speed_bps).toBe(1000);
        await laneProgress('ul-files-1-b', 50, 500);
        expect(lastToast().summary.speed_bps).toBe(1500);
        // ETA over the real remaining bytes: (2000 - 150) / 1500.
        expect(lastToast().summary.eta_seconds).toBe(1);
        await laneProgress('ul-files-1-a', 600, 400);
        expect(lastToast().summary.speed_bps).toBe(900);
    });

    it('records the batch on the speed graph by bytes over the whole batch', async () => {
        await mount();
        await fire('transfer_batch_started', { batch_id: 'ul-files-2', display_name: '2 files', direction: 'upload', total: 2 });
        await fire('transfer_batch_progress', {
            batch_id: 'ul-files-2', completed: 0, skipped: 0, failed: 0, active: 1, total: 2,
            bytes_transferred: 0, bytes_total: 2000,
        });
        await laneProgress('ul-files-2-a', 100, 1000);
        await laneProgress('ul-files-2-a', 900, 1000);
        const profile = lastToast().speedProfile!;
        expect(profile.totalBytes).toBe(2000);
        expect(profile.reached).toBeCloseTo(900 / 2000, 5);
        expect(bucketSpeeds(profile).some((speed) => speed !== null)).toBe(true);
    });

    it('starts a fresh graph for the next transfer', async () => {
        await mount();
        await fire('transfer_batch_started', { batch_id: 'ul-files-3', display_name: '2 files', direction: 'upload', total: 2 });
        await fire('transfer_batch_progress', {
            batch_id: 'ul-files-3', completed: 0, skipped: 0, failed: 0, active: 1, total: 2,
            bytes_transferred: 0, bytes_total: 2000,
        });
        await laneProgress('ul-files-3-a', 100, 1000);
        await laneProgress('ul-files-3-a', 900, 1000);
        await fire('transfer_batch_started', { batch_id: 'ul-files-4', display_name: '1 file', direction: 'upload', total: 1 });
        // The new batch's first toast carries no graph from the previous one.
        const fresh = toasts.filter((state) => state?.summary.transfer_id === 'ul-files-4');
        expect(fresh.length).toBeGreaterThan(0);
        for (const state of fresh) {
            expect(state!.speedProfile?.reached ?? 0).toBe(0);
        }
    });
});
