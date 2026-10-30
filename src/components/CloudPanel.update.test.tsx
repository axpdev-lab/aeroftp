// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
import React from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { CloudPanel } from './CloudPanel';

const mocks = vi.hoisted(() => ({ invoke: vi.fn(), listeners: new Map<string, (event: { payload: unknown }) => void>() }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: mocks.invoke }));
vi.mock('@tauri-apps/api/event', () => ({ emit: vi.fn() }));
vi.mock('../hooks/useTauriListener', () => ({ useTauriListener: (name: string, callback: (event: { payload: unknown }) => void) => { mocks.listeners.set(name, callback); } }));
vi.mock('../hooks/useTraySync', () => ({ useTraySync: () => ({ isRunning: false }) }));
vi.mock('../i18n', () => ({ useTranslation: () => (key: string) => key }));
vi.mock('./AeroCloudProtocols', () => ({ AeroCloudMaturityBadge: () => null, useAeroCloudCaveat: () => () => null }));
vi.mock('./SyncScheduler', () => ({ SyncScheduler: () => null }));
vi.mock('./WatcherStatus', () => ({ WatcherStatus: () => null }));
vi.mock('./Sync/SelectiveSyncTree', () => ({ SelectiveSyncTree: () => null }));
vi.mock('./CloudPairsEditor', () => ({ CloudPairsEditor: () => null }));

let root: Root | undefined;
let host: HTMLDivElement;
afterEach(async () => { if (root) await React.act(async () => root!.unmount()); host?.remove(); root = undefined; mocks.listeners.clear(); mocks.invoke.mockReset(); });

async function mount(pairs: unknown[] | null) {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    mocks.invoke.mockImplementation(async (command: string) => {
        if (command === 'get_cloud_config') return { enabled: true, sync_direction: 'local_to_remote', update_only: true, local_folder: '/local', remote_folder: '/remote', exclude_patterns: [], excluded_folders: [] };
        if (command === 'get_cloud_status') return { type: 'has_conflicts', count: 1 };
        if (command === 'get_cloud_pairs_config') { if (pairs === null) throw new Error('unreadable pairs'); return { pairs }; }
        return [];
    });
    host = document.createElement('div'); document.body.append(host); root = createRoot(host);
    await React.act(async () => root!.render(<CloudPanel isOpen onClose={vi.fn()} />));
}

describe('AeroCloud Update settings and cycle reporting', () => {
    it.each([[], [{ id: 'pair' }], null])('exposes the legacy checkbox only for a verified empty pair list (%j)', async pairs => {
        await mount(pairs);
        await React.act(async () => host.querySelector<HTMLButtonElement>('button[title="common.settings"]')!.click());
        const update = Array.from(host.querySelectorAll('label')).find(label => label.textContent?.includes('aerosync.updateOnly'));
        expect(Boolean(update)).toBe(pairs !== null && pairs.length === 0);
    });

    it('keeps a deletion refusal visible alongside conflicts', async () => {
        await mount([]);
        await React.act(async () => mocks.listeners.get('cloud_sync_complete')!({ payload: {
            errors: ['Mass-delete refused: blocked 20 deletes'], uploaded: 0, downloaded: 0, skipped: 0, deleted: 0, conflicts: 1, skipped_details: [],
        } }));
        expect(host.textContent).toContain('cloud.conflictsCount');
        expect(host.querySelector('[role="alert"]')?.textContent).toContain('blocked 20 deletes');
    });
    it('keeps per-pair warnings across partial watcher events and retires only a complete inventory', async () => {
        await mount([]);
        const complete = async (scope: string | undefined, entries: string[], inventory: boolean) => {
            await React.act(async () => mocks.listeners.get('cloud_sync_complete')!({ payload: {
                errors: [], uploaded: 1, downloaded: 0, skipped: entries.length, deleted: 0, conflicts: 0,
                review_scope: scope, review_inventory_complete: inventory,
                skipped_details: entries.map(path => ({ path, needs_review: true, is_dir: false, reason: 'Never synced' })),
            } }));
        };
        await complete('phone', ['photo.jpg'], true);
        await complete('other-pair', ['notes.txt'], true);
        await complete('phone', [], false);
        await complete(undefined, [], false);
        expect(host.textContent).toContain('photo.jpg');
        expect(host.textContent).toContain('notes.txt');
        await complete('phone', [], true);
        expect(host.textContent).not.toContain('photo.jpg');
        expect(host.textContent).toContain('notes.txt');
    });

});
