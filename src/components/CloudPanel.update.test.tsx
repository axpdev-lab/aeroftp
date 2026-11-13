// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
import React from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { CloudPanel } from './CloudPanel';

const mocks = vi.hoisted(() => ({ invoke: vi.fn(), listeners: new Map<string, (event: { payload: unknown }) => void>(), setupResult: null as unknown }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: mocks.invoke }));
vi.mock('@tauri-apps/api/event', () => ({ emit: vi.fn() }));
vi.mock('../hooks/useTauriListener', () => ({ useTauriListener: (name: string, callback: (event: { payload: unknown }) => void) => { mocks.listeners.set(name, callback); } }));
vi.mock('../hooks/useTraySync', () => ({ useTraySync: () => ({ isRunning: false, startBackgroundSync: async () => {} }) }));
vi.mock('../i18n', () => ({ useTranslation: () => (key: string) => key }));
vi.mock('./AeroCloudProtocols', () => ({ AeroCloudMaturityBadge: () => null, useAeroCloudCaveat: () => () => null }));
vi.mock('./SyncScheduler', () => ({ SyncScheduler: () => null }));
vi.mock('./WatcherStatus', () => ({ WatcherStatus: () => null }));
vi.mock('./Sync/SelectiveSyncTree', () => ({ SelectiveSyncTree: () => null }));
vi.mock('./CloudPairsEditor', () => ({ CloudPairsEditor: () => null }));
vi.mock('./CloudSetupWizard', () => ({
    CloudSetupWizard: ({ onComplete }: { onComplete: (config: unknown) => void }) => (
        <button type="button" data-testid="setup-complete" onClick={() => onComplete(mocks.setupResult)} />
    ),
}));

let root: Root | undefined;
let host: HTMLDivElement;
afterEach(async () => { if (root) await React.act(async () => root!.unmount()); host?.remove(); root = undefined; mocks.listeners.clear(); mocks.invoke.mockReset(); });

async function mount(pairs: unknown[] | null, onClose: () => void = vi.fn()) {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    mocks.invoke.mockImplementation(async (command: string) => {
        if (command === 'get_cloud_config') return { enabled: true, sync_direction: 'local_to_remote', update_only: true, local_folder: '/local', remote_folder: '/remote', exclude_patterns: [], excluded_folders: [] };
        if (command === 'get_cloud_status') return { type: 'has_conflicts', count: 1 };
        if (command === 'get_cloud_pairs_config') { if (pairs === null) throw new Error('unreadable pairs'); return { pairs }; }
        return [];
    });
    host = document.createElement('div'); document.body.append(host); root = createRoot(host);
    await React.act(async () => root!.render(<CloudPanel isOpen onClose={onClose} />));
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

    it('discards an unsaved remote_folder edit on Cancel instead of adopting it as the new baseline', async () => {
        await mount([]);
        const openSettings = () => React.act(async () => host.querySelector<HTMLButtonElement>('button[title="common.settings"]')!.click());
        const remoteFolder = () => host.querySelector<HTMLInputElement>('input[placeholder="/cloud/"]')!;
        await openSettings();
        await React.act(async () => {
            Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(remoteFolder(), '/other');
            remoteFolder().dispatchEvent(new Event('input', { bubbles: true }));
        });
        expect(remoteFolder().value).toBe('/other');
        await React.act(async () => [...host.querySelectorAll('button')].find(b => b.textContent === 'common.cancel')!.click());
        await openSettings();
        expect(remoteFolder().value).toBe('/remote');
    });

    it('ignores Cancel and X while a settings save is in flight, then shows the persisted value', async () => {
        await mount([]);
        const openSettings = () => React.act(async () => host.querySelector<HTMLButtonElement>('button[title="common.settings"]')!.click());
        const remoteFolder = () => host.querySelector<HTMLInputElement>('input[placeholder="/cloud/"]');
        const deferredSave = () => {
            let resolve!: (v: unknown) => void;
            let reject!: (e: unknown) => void;
            const promise = new Promise((res, rej) => { resolve = res; reject = rej; });
            return { promise, resolve, reject };
        };
        const save = deferredSave();
        mocks.invoke.mockImplementation(async (command: string) => {
            if (command === 'save_cloud_config_cmd') return save.promise;
            if (command === 'get_cloud_config') return { enabled: true, sync_direction: 'local_to_remote', update_only: true, local_folder: '/local', remote_folder: '/remote', exclude_patterns: [], excluded_folders: [] };
            if (command === 'get_cloud_status') return { type: 'idle' };
            if (command === 'get_cloud_pairs_config') return { pairs: [] };
            if (command === 'get_sync_schedule_cmd') return null;
            return [];
        });
        await openSettings();
        await React.act(async () => {
            Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(remoteFolder(), '/other');
            remoteFolder()!.dispatchEvent(new Event('input', { bubbles: true }));
        });
        await React.act(async () => [...host.querySelectorAll('button')].find(b => b.textContent?.includes('common.save'))!.click());
        expect(mocks.invoke).toHaveBeenCalledWith('save_cloud_config_cmd', expect.objectContaining({ config: expect.objectContaining({ remote_folder: '/other' }) }));
        // Cancel and X are inert while the save is pending.
        await React.act(async () => [...host.querySelectorAll('button')].find(b => b.textContent === 'common.cancel')!.click());
        expect(remoteFolder()).not.toBeNull();
        await React.act(async () => host.querySelector('button .lucide-x, button svg.lucide-x')!.closest('button')!.click());
        expect(remoteFolder()).not.toBeNull();
        await React.act(async () => save.resolve(null));
        await React.act(async () => { await Promise.resolve(); });
        expect(remoteFolder()).toBeNull();
        expect(host.textContent).toContain('/other');
    });

    it('locks every settings input while a save is in flight, then shows the persisted folder', async () => {
        await mount([]);
        const openSettings = () => React.act(async () => host.querySelector<HTMLButtonElement>('button[title="common.settings"]')!.click());
        const remoteFolder = () => host.querySelector<HTMLInputElement>('input[placeholder="/cloud/"]');
        let resolveSave!: (v: unknown) => void;
        const save = new Promise(res => { resolveSave = res; });
        mocks.invoke.mockImplementation(async (command: string) => {
            if (command === 'save_cloud_config_cmd') return save;
            if (command === 'get_cloud_config') return { enabled: true, sync_direction: 'local_to_remote', update_only: true, local_folder: '/local', remote_folder: '/remote', exclude_patterns: [], excluded_folders: [] };
            if (command === 'get_cloud_status') return { type: 'idle' };
            if (command === 'get_cloud_pairs_config') return { pairs: [] };
            if (command === 'get_sync_schedule_cmd') return null;
            return [];
        });
        await openSettings();
        await React.act(async () => {
            Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(remoteFolder(), '/other');
            remoteFolder()!.dispatchEvent(new Event('input', { bubbles: true }));
        });
        await React.act(async () => [...host.querySelectorAll('button')].find(b => b.textContent?.includes('common.save'))!.click());
        expect(mocks.invoke).toHaveBeenCalledWith('save_cloud_config_cmd', expect.objectContaining({ config: expect.objectContaining({ remote_folder: '/other' }) }));
        // The whole settings form is inert while the save runs: a draft typed
        // now must never become the displayed target.
        expect(remoteFolder()!.matches(':disabled')).toBe(true);
        await React.act(async () => resolveSave(null));
        await React.act(async () => { await Promise.resolve(); });
        expect(remoteFolder()).toBeNull();
        expect(host.textContent).toContain('/other');
    });

    it('keeps the settings panel open on a backdrop click while an unchanged save runs', async () => {
        const onClose = vi.fn();
        await mount([], onClose);
        const openSettings = () => React.act(async () => host.querySelector<HTMLButtonElement>('button[title="common.settings"]')!.click());
        const remoteFolder = () => host.querySelector<HTMLInputElement>('input[placeholder="/cloud/"]');
        let resolveSave!: (v: unknown) => void;
        const save = new Promise(res => { resolveSave = res; });
        mocks.invoke.mockImplementation(async (command: string) => {
            if (command === 'save_cloud_config_cmd') return save;
            if (command === 'get_cloud_config') return { enabled: true, sync_direction: 'local_to_remote', update_only: true, local_folder: '/local', remote_folder: '/remote', exclude_patterns: [], excluded_folders: [] };
            if (command === 'get_cloud_status') return { type: 'idle' };
            if (command === 'get_cloud_pairs_config') return { pairs: [] };
            if (command === 'get_sync_schedule_cmd') return null;
            return [];
        });
        await openSettings();
        // No edits: the dirty guard is off, but a save cannot be stopped.
        await React.act(async () => [...host.querySelectorAll('button')].find(b => b.textContent?.includes('common.save'))!.click());
        const backdrop = host.querySelector('[aria-label="cloud.dialogSettingsLabel"]')!;
        await React.act(async () => backdrop.dispatchEvent(new MouseEvent('click', { bubbles: true })));
        expect(onClose).not.toHaveBeenCalled();
        expect(remoteFolder()).not.toBeNull();
        await React.act(async () => resolveSave(null));
        await React.act(async () => { await Promise.resolve(); });
    });

    it('keeps an open settings draft when the post-sync refresh only moves last_sync', async () => {
        const LAST_SYNC = new Date().toISOString();
        let resolveSync!: (v: unknown) => void;
        const syncPromise = new Promise(res => { resolveSync = res; });
        await mount([]);
        mocks.invoke.mockImplementation(async (command: string) => {
            if (command === 'trigger_cloud_sync') return syncPromise;
            if (command === 'get_cloud_config') return { enabled: true, sync_direction: 'local_to_remote', update_only: true, local_folder: '/local', remote_folder: '/remote', exclude_patterns: [], excluded_folders: [], last_sync: LAST_SYNC };
            if (command === 'get_cloud_status') return { type: 'idle' };
            if (command === 'get_cloud_pairs_config') return { pairs: [] };
            return [];
        });
        // Sync Now, then edit the draft while the sync is still running.
        await React.act(async () => [...host.querySelectorAll('button')].find(b => b.textContent?.includes('cloud.syncNow'))!.click());
        const openSettings = () => React.act(async () => host.querySelector<HTMLButtonElement>('button[title="common.settings"]')!.click());
        await openSettings();
        const remoteFolder = () => host.querySelector<HTMLInputElement>('input[placeholder="/cloud/"]');
        await React.act(async () => {
            Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(remoteFolder(), '/typed');
            remoteFolder()!.dispatchEvent(new Event('input', { bubbles: true }));
        });
        await React.act(async () => resolveSync('done'));
        await React.act(async () => { await Promise.resolve(); await Promise.resolve(); });
        // The refresh merged only last_sync: the draft is still there and the
        // dirty guard still owns the backdrop.
        expect(remoteFolder()?.value).toBe('/typed');
        const backdrop = host.querySelector('[aria-label="cloud.dialogSettingsLabel"]')!;
        await React.act(async () => backdrop.dispatchEvent(new MouseEvent('click', { bubbles: true })));
        expect(remoteFolder()?.value).toBe('/typed');
        // Cancel restores the baseline, which now carries the persisted
        // last_sync, so the dashboard shows it.
        await React.act(async () => [...host.querySelectorAll('button')].find(b => b.textContent?.trim() === 'common.cancel')!.click());
        expect(host.textContent).toContain('cloud.justNow');
        expect(host.textContent).not.toContain('cloud.never');
    });

    it('drops the stale settings baseline when Disable replaces the configuration', async () => {
        mocks.setupResult = { enabled: true, cloud_name: 'fresh', local_folder: '/local2', remote_folder: '/new', sync_interval_secs: 300, sync_direction: 'bidirectional', exclude_patterns: [], excluded_folders: [] };
        const savedConfigs: { remote_folder?: string }[] = [];
        let resolveDisable!: () => void;
        const disablePromise = new Promise<void>(res => { resolveDisable = res; });
        await mount([]);
        mocks.invoke.mockImplementation(async (command: string, args?: { config?: { remote_folder?: string } }) => {
            if (command === 'disable_aerocloud') return disablePromise;
            if (command === 'save_cloud_config_cmd') { savedConfigs.push(args?.config ?? {}); return null; }
            if (command === 'get_cloud_config') return { enabled: true, sync_direction: 'local_to_remote', update_only: true, local_folder: '/local', remote_folder: '/remote', exclude_patterns: [], excluded_folders: [] };
            if (command === 'get_cloud_status') return { type: 'idle' };
            if (command === 'get_cloud_pairs_config') return { pairs: [] };
            if (command === 'get_sync_schedule_cmd') return null;
            return [];
        });
        const openSettings = () => React.act(async () => host.querySelector<HTMLButtonElement>('button[title="common.settings"]')!.click());
        // Disable, then let it resolve while Settings stays closed to new
        // sessions: the open session must not survive the configuration it edited.
        await React.act(async () => [...host.querySelectorAll('button')].find(b => b.textContent?.trim() === 'cloud.disable')!.click());
        const confirmOverlay = host.querySelector('[aria-label="cloud.disableConfirm"]')!;
        await React.act(async () => [...confirmOverlay.querySelectorAll('button')].find(b => b.textContent?.trim() === 'cloud.disable')!.click());
        await React.act(async () => resolveDisable());
        await React.act(async () => { await Promise.resolve(); });
        // Setup wizard is up; completing it installs the NEW configuration.
        await React.act(async () => host.querySelector<HTMLButtonElement>('[data-testid="setup-complete"]')!.click());
        expect(host.textContent).toContain('/new');
        // Cancel on the settings view must restore the NEW baseline, never the old one.
        await openSettings();
        await React.act(async () => [...host.querySelectorAll('button')].find(b => b.textContent === 'common.cancel')!.click());
        expect(host.textContent).toContain('/new');
        expect(host.textContent).not.toContain('/remote');
        // X goes through the same path.
        await openSettings();
        await React.act(async () => host.querySelector('button .lucide-x, button svg.lucide-x')!.closest('button')!.click());
        expect(host.textContent).toContain('/new');
        // And the next Save submits the NEW folder.
        await openSettings();
        await React.act(async () => [...host.querySelectorAll('button')].find(b => b.textContent?.includes('common.save'))!.click());
        await React.act(async () => { await Promise.resolve(); await Promise.resolve(); });
        expect(savedConfigs[savedConfigs.length - 1]?.remote_folder).toBe('/new');
    });

    it('keeps the edit in place when the settings save rejects', async () => {
        await mount([]);
        const openSettings = () => React.act(async () => host.querySelector<HTMLButtonElement>('button[title="common.settings"]')!.click());
        const remoteFolder = () => host.querySelector<HTMLInputElement>('input[placeholder="/cloud/"]');
        mocks.invoke.mockImplementation(async (command: string) => {
            if (command === 'save_cloud_config_cmd') throw new Error('sentinel save failure');
            if (command === 'get_cloud_config') return { enabled: true, sync_direction: 'local_to_remote', update_only: true, local_folder: '/local', remote_folder: '/remote', exclude_patterns: [], excluded_folders: [] };
            if (command === 'get_cloud_status') return { type: 'idle' };
            if (command === 'get_cloud_pairs_config') return { pairs: [] };
            return [];
        });
        await openSettings();
        await React.act(async () => {
            Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(remoteFolder(), '/other');
            remoteFolder()!.dispatchEvent(new Event('input', { bubbles: true }));
        });
        await React.act(async () => [...host.querySelectorAll('button')].find(b => b.textContent?.includes('common.save'))!.click());
        await React.act(async () => { await Promise.resolve(); await Promise.resolve(); });
        expect(remoteFolder()?.value).toBe('/other');
    });

    it('does not clear a full-scan refusal on an unrelated clean watcher completion', async () => {
        await mount([]);
        const completed = async (errors: string[], complete: boolean) => {
            await React.act(async () => mocks.listeners.get('cloud_sync_complete')!({ payload: {
                errors, uploaded: 1, downloaded: 0, skipped: 0, deleted: 0, conflicts: 0,
                review_scope: 'pair', review_inventory_complete: complete, skipped_details: [],
            } }));
        };
        await completed(['Mass-delete refused: blocked 20 deletes'], false);
        await completed([], false);
        expect(host.textContent).toContain('blocked 20 deletes');
        await React.act(async () => mocks.listeners.get('cloud_sync_complete')!({ payload: {
            errors: [], uploaded: 1, downloaded: 0, skipped: 0, deleted: 0, conflicts: 0,
        } }));
        expect(host.textContent).toContain('blocked 20 deletes');
        await completed([], true);
        expect(host.textContent).not.toContain('blocked 20 deletes');
    });

    it('keeps the settings view mounted while the post-sync refresh is in flight', async () => {
        let resolveRefresh!: (v: unknown) => void;
        const refreshPromise = new Promise(res => { resolveRefresh = res; });
        await mount([]);
        mocks.invoke.mockImplementation(async (command: string) => {
            if (command === 'trigger_cloud_sync') return 'done';
            if (command === 'get_cloud_config') return refreshPromise;
            if (command === 'get_cloud_status') return { type: 'idle' };
            if (command === 'get_cloud_pairs_config') return { pairs: [] };
            return [];
        });
        const openSettings = () => React.act(async () => host.querySelector<HTMLButtonElement>('button[title="common.settings"]')!.click());
        // Sync Now starts the deferred refresh, then settings open dirty
        // while it is still in flight.
        await React.act(async () => [...host.querySelectorAll('button')].find(b => b.textContent?.includes('cloud.syncNow'))!.click());
        await React.act(async () => { await Promise.resolve(); });
        await openSettings();
        const remoteFolder = () => host.querySelector<HTMLInputElement>('input[placeholder="/cloud/"]');
        await React.act(async () => {
            Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(remoteFolder(), '/typed');
            remoteFolder()!.dispatchEvent(new Event('input', { bubbles: true }));
        });
        await React.act(async () => { await Promise.resolve(); await Promise.resolve(); });
        // The timestamp refresh must not swap the settings view for the
        // full-panel loading view, whose backdrop would close unguarded.
        expect(remoteFolder()?.value).toBe('/typed');
        expect(host.textContent).not.toContain('common.loading');
        const backdrop = host.querySelector('[aria-label="cloud.dialogSettingsLabel"]')!;
        await React.act(async () => backdrop.dispatchEvent(new MouseEvent('click', { bubbles: true })));
        expect(remoteFolder()?.value).toBe('/typed');
        await React.act(async () => resolveRefresh({ enabled: true, sync_direction: 'local_to_remote', update_only: true, local_folder: '/local', remote_folder: '/remote', exclude_patterns: [], excluded_folders: [], last_sync: new Date().toISOString() }));
        await React.act(async () => { await Promise.resolve(); await Promise.resolve(); });
        expect(remoteFolder()?.value).toBe('/typed');
    });

    it('disables the Settings entry while a Pause request is in flight', async () => {
        let resolvePause!: (v: unknown) => void;
        const pausePromise = new Promise(res => { resolvePause = res; });
        await mount([]);
        mocks.invoke.mockImplementation(async (command: string) => {
            if (command === 'pause_aerocloud') return pausePromise;
            if (command === 'get_cloud_config') return { enabled: true, sync_direction: 'local_to_remote', update_only: true, local_folder: '/local', remote_folder: '/remote', exclude_patterns: [], excluded_folders: [] };
            if (command === 'get_cloud_status') return { type: 'idle' };
            if (command === 'get_cloud_pairs_config') return { pairs: [] };
            return [];
        });
        const settingsButton = () => host.querySelector<HTMLButtonElement>('button[title="common.settings"]')!;
        await React.act(async () => [...host.querySelectorAll('button')].find(b => b.textContent?.includes('cloud.pause'))!.click());
        expect(settingsButton().disabled).toBe(true);
        await React.act(async () => resolvePause({ enabled: true, sync_direction: 'local_to_remote', update_only: true, local_folder: '/local', remote_folder: '/remote', exclude_patterns: [], excluded_folders: [] }));
        await React.act(async () => { await Promise.resolve(); await Promise.resolve(); });
        expect(settingsButton().disabled).toBe(false);
        await React.act(async () => settingsButton().click());
        expect(host.querySelector('input[placeholder="/cloud/"]')).toBeTruthy();
    });

    it('keeps the re-authorize entry from opening Settings while a Pause request is in flight', async () => {
        // The re-authorize banner is the second way into Settings: left open,
        // a Pause resolving later would replace the configuration under a draft.
        let resolvePause!: (v: unknown) => void;
        const pausePromise = new Promise(res => { resolvePause = res; });
        const paused = { enabled: true, sync_direction: 'local_to_remote', update_only: true, local_folder: '/local', remote_folder: '/remote', exclude_patterns: [], excluded_folders: [] };
        await mount([]);
        mocks.invoke.mockImplementation(async (command: string) => {
            if (command === 'pause_aerocloud') return pausePromise;
            if (command === 'get_cloud_config') return paused;
            if (command === 'get_cloud_status') return { type: 'idle' };
            if (command === 'get_cloud_pairs_config') return { pairs: [] };
            return [];
        });
        await React.act(async () => mocks.listeners.get('cloud-reauth-required')!({ payload: { provider: 'gdrive', reason: 'expired', message: 'expired' } }));
        const reauthButton = () => [...host.querySelectorAll<HTMLButtonElement>('button')].find(b => b.textContent?.includes('cloud.reauthorize'))!;
        await React.act(async () => [...host.querySelectorAll('button')].find(b => b.textContent?.includes('cloud.pause'))!.click());
        expect(reauthButton().disabled).toBe(true);
        await React.act(async () => reauthButton().click());
        expect(host.querySelector('input[placeholder="/cloud/"]')).toBeNull();
        await React.act(async () => resolvePause(paused));
        await React.act(async () => { await Promise.resolve(); await Promise.resolve(); });
        expect(reauthButton().disabled).toBe(false);
        await React.act(async () => reauthButton().click());
        expect(host.querySelector('input[placeholder="/cloud/"]')).toBeTruthy();
    });

});
