// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
import React from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { CloudPanel } from './CloudPanel';
import en from '../i18n/locales/en.json';

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

async function mount(pairs: unknown[] | null, onClose: () => void = vi.fn(), configReadable: () => boolean = () => true) {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    mocks.invoke.mockImplementation(async (command: string) => {
        if (command === 'get_cloud_config' && !configReadable()) throw new Error('permission denied');
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

    it('shows an unreadable configuration as an error, not as the setup wizard, until a retry reads it', async () => {
        let readable = false;
        await mount([], vi.fn(), () => readable);
        expect(host.querySelector('[role="alert"]')?.textContent).toBe('common.settingsLoadFailed');
        expect(host.querySelector('[aria-label="cloud.dialogSetupLabel"]')).toBeNull();
        readable = true;
        await React.act(async () => Array.from(host.querySelectorAll('button')).find(button => button.textContent === 'common.retry')!.click());
        expect(host.querySelector('[role="alert"]')).toBeNull();
        expect(host.querySelector('button[title="common.settings"]')).not.toBeNull();
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

    it('keeps an open settings draft when the post-sync refresh cannot read the configuration', async () => {
        let resolveSync!: (v: unknown) => void;
        const syncPromise = new Promise(res => { resolveSync = res; });
        await mount([]);
        mocks.invoke.mockImplementation(async (command: string) => {
            if (command === 'trigger_cloud_sync') return syncPromise;
            if (command === 'get_cloud_config') throw new Error('permission denied');
            return [];
        });
        await React.act(async () => [...host.querySelectorAll('button')].find(b => b.textContent?.includes('cloud.syncNow'))!.click());
        await React.act(async () => host.querySelector<HTMLButtonElement>('button[title="common.settings"]')!.click());
        const remoteFolder = () => host.querySelector<HTMLInputElement>('input[placeholder="/cloud/"]');
        await React.act(async () => {
            Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(remoteFolder(), '/typed');
            remoteFolder()!.dispatchEvent(new Event('input', { bubbles: true }));
        });
        await React.act(async () => resolveSync('done'));
        await React.act(async () => { await Promise.resolve(); await Promise.resolve(); });
        // Only a full load replaces the configuration: the failed refresh
        // leaves the draft open and says the file could not be read again.
        // Save stays available, so the warning is not the one that promises
        // nothing will be saved.
        expect(remoteFolder()?.value).toBe('/typed');
        expect(host.querySelector('[role="alert"]')?.textContent).toContain('common.settingsRefreshFailed');
        expect(host.querySelector('[role="alert"]')?.textContent).not.toContain('common.settingsLoadFailed');

        // A save the backend refuses keeps the settings open and says so.
        mocks.invoke.mockImplementation(async (command: string) => {
            if (command === 'save_cloud_config_cmd') throw new Error('refusing to overwrite');
            if (command === 'get_cloud_config') throw new Error('permission denied');
            return [];
        });
        await React.act(async () => [...host.querySelectorAll('button')].find(b => b.textContent === 'common.save common.settings')!.click());
        expect(remoteFolder()?.value).toBe('/typed');
        expect(host.querySelector('[role="alert"]')?.textContent).toContain('common.saveFailed');

        // Retry reads the file again: once it is readable the error clears.
        mocks.invoke.mockImplementation(async (command: string) => {
            if (command === 'get_cloud_config') return { enabled: true, sync_direction: 'local_to_remote', update_only: true, local_folder: '/local', remote_folder: '/remote', exclude_patterns: [], excluded_folders: [] };
            return [];
        });
        await React.act(async () => [...host.querySelectorAll('button')].find(b => b.textContent === 'common.retry')!.click());
        expect(host.querySelector('[role="alert"]')?.textContent ?? '').not.toContain('common.settingsRefreshFailed');
        expect(remoteFolder()?.value).toBe('/typed');
    });

    it('words the refresh warning without promising that nothing will be saved', () => {
        const common = en.translations.common;
        expect(common.settingsLoadFailed).toMatch(/nothing will be saved/);
        expect(common.settingsRefreshFailed).toContain('{error}');
        expect(common.settingsRefreshFailed).not.toMatch(/sav/i);
    });

    it('keeps an open settings draft when the post-sync refresh only moves last_sync', async () => {
        const LAST_SYNC = new Date().toISOString();
        let resolveSync!: (v: unknown) => void;
        const syncPromise = new Promise(res => { resolveSync = res; });
        const onClose = vi.fn();
        await mount([], onClose);
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
        expect(onClose).not.toHaveBeenCalled();
        // Cancel restores the baseline, which now carries the persisted
        // last_sync, so the dashboard shows it.
        await React.act(async () => [...host.querySelectorAll('button')].find(b => b.textContent?.trim() === 'common.cancel')!.click());
        expect(host.textContent).toContain('cloud.justNow');
        expect(host.textContent).not.toContain('cloud.never');
    });

    it('saves the folder a sync placed, not the `~` form it loaded, when another setting changes', async () => {
        (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
        let folder = '~/docs';
        mocks.invoke.mockImplementation(async (command: string) => {
            if (command === 'get_cloud_config') return { enabled: true, sync_direction: 'local_to_remote', update_only: true, local_folder: '/local', remote_folder: folder, exclude_patterns: [], excluded_folders: [] };
            if (command === 'trigger_cloud_sync') { folder = '/home/u/docs'; return 'done'; }
            if (command === 'get_cloud_status') return { type: 'idle' };
            if (command === 'get_cloud_pairs_config') return { pairs: [] };
            return [];
        });
        host = document.createElement('div'); document.body.append(host); root = createRoot(host);
        await React.act(async () => root!.render(<CloudPanel isOpen onClose={vi.fn()} />));
        await React.act(async () => host.querySelector<HTMLButtonElement>('button[title="common.settings"]')!.click());
        const remoteFolder = () => host.querySelector<HTMLInputElement>('input[placeholder="/cloud/"]');
        expect(remoteFolder()?.value).toBe('~/docs');
        await React.act(async () => [...host.querySelectorAll('button')].find(b => b.textContent?.trim() === 'common.cancel')!.click());

        // The sync places the folder and saves it; the refresh brings it in.
        await React.act(async () => [...host.querySelectorAll('button')].find(b => b.textContent?.includes('cloud.syncNow'))!.click());
        await React.act(async () => { await Promise.resolve(); await Promise.resolve(); });
        await React.act(async () => host.querySelector<HTMLButtonElement>('button[title="common.settings"]')!.click());
        expect(remoteFolder()?.value).toBe('/home/u/docs');

        // Another setting changes and is saved: the placed folder goes with it.
        const update = Array.from(host.querySelectorAll('label')).find(label => label.textContent?.includes('aerosync.updateOnly'))!;
        await React.act(async () => update.querySelector('input')!.click());
        await React.act(async () => [...host.querySelectorAll('button')].find(b => b.textContent === 'common.save common.settings')!.click());
        expect(mocks.invoke).toHaveBeenCalledWith('save_cloud_config_cmd', expect.objectContaining({ config: expect.objectContaining({ remote_folder: '/home/u/docs', update_only: false }) }));
    });

    it.each([
        ['the panel', true],
        ['the tray, the watcher or the scheduler', false],
    ])('keeps the folder a sync placed after a saved `~` edit, when the sync runs from %s', async (_from, fromPanel) => {
        (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
        const saved: { remote_folder?: string; update_only?: boolean }[] = [];
        // The file as the backend keeps it: a save replaces it, a sync places
        // a `~/...` folder under the login folder and saves the absolute path.
        let disk: { remote_folder?: string } = { enabled: true, sync_direction: 'local_to_remote', update_only: true, local_folder: '/local', remote_folder: '/remote', exclude_patterns: [], excluded_folders: [] } as { remote_folder?: string };
        const place = () => { if (disk.remote_folder === '~/docs') disk = { ...disk, remote_folder: '/home/u/docs' }; };
        mocks.invoke.mockImplementation(async (command: string, args?: { config?: { remote_folder?: string } }) => {
            if (command === 'get_cloud_config') return disk;
            if (command === 'trigger_cloud_sync') { place(); return 'done'; }
            if (command === 'save_cloud_config_cmd') { saved.push(args?.config ?? {}); disk = args?.config ?? {}; return null; }
            if (command === 'get_cloud_status') return { type: 'idle' };
            if (command === 'get_cloud_pairs_config') return { pairs: [] };
            if (command === 'get_sync_schedule_cmd') return null;
            return [];
        });
        host = document.createElement('div'); document.body.append(host); root = createRoot(host);
        await React.act(async () => root!.render(<CloudPanel isOpen onClose={vi.fn()} />));
        const openSettings = () => React.act(async () => host.querySelector<HTMLButtonElement>('button[title="common.settings"]')!.click());
        const remoteFolder = () => host.querySelector<HTMLInputElement>('input[placeholder="/cloud/"]');
        const save = () => React.act(async () => [...host.querySelectorAll('button')].find(b => b.textContent === 'common.save common.settings')!.click());
        // The user changes /remote to ~/docs and saves: Settings closes.
        await openSettings();
        await React.act(async () => {
            Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(remoteFolder(), '~/docs');
            remoteFolder()!.dispatchEvent(new Event('input', { bubbles: true }));
        });
        await save();
        await React.act(async () => { await Promise.resolve(); await Promise.resolve(); });
        expect(remoteFolder()).toBeNull();
        // A sync places the folder after Settings closed.
        if (fromPanel) {
            await React.act(async () => [...host.querySelectorAll('button')].find(b => b.textContent?.includes('cloud.syncNow'))!.click());
            await React.act(async () => { await Promise.resolve(); await Promise.resolve(); });
        } else {
            place();
        }
        expect(disk.remote_folder).toBe('/home/u/docs');
        // Settings opens on the placed folder, and an unrelated save keeps it.
        await openSettings();
        await React.act(async () => { await Promise.resolve(); await Promise.resolve(); });
        expect(remoteFolder()?.value).toBe('/home/u/docs');
        const update = Array.from(host.querySelectorAll('label')).find(label => label.textContent?.includes('aerosync.updateOnly'))!;
        await React.act(async () => update.querySelector('input')!.click());
        await save();
        expect(saved.map(config => config.remote_folder)).toEqual(['~/docs', '/home/u/docs']);
        expect(saved[1].update_only).toBe(false);
        expect(disk.remote_folder).toBe('/home/u/docs');
    });

    it('drops a post-sync refresh that read the file before a settings save', async () => {
        (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
        let releaseRefresh!: (config: unknown) => void;
        let reads = 0;
        const saved: { remote_folder?: string }[] = [];
        // The file as the backend keeps it: a save replaces it.
        let disk: unknown = { enabled: true, sync_direction: 'local_to_remote', update_only: true, local_folder: '/local', remote_folder: '/remote', exclude_patterns: [], excluded_folders: [] };
        mocks.invoke.mockImplementation(async (command: string, args?: { config?: { remote_folder?: string } }) => {
            if (command === 'get_cloud_config') {
                reads += 1;
                if (reads === 2) return new Promise(resolve => { releaseRefresh = resolve; });
                return disk;
            }
            if (command === 'trigger_cloud_sync') return 'done';
            if (command === 'save_cloud_config_cmd') { saved.push(args?.config ?? {}); disk = args?.config; return null; }
            if (command === 'get_cloud_status') return { type: 'idle' };
            if (command === 'get_cloud_pairs_config') return { pairs: [] };
            if (command === 'get_sync_schedule_cmd') return null;
            return [];
        });
        host = document.createElement('div'); document.body.append(host); root = createRoot(host);
        await React.act(async () => root!.render(<CloudPanel isOpen onClose={vi.fn()} />));
        // The sync ends and its refresh read waits.
        await React.act(async () => [...host.querySelectorAll('button')].find(b => b.textContent?.includes('cloud.syncNow'))!.click());
        await React.act(async () => { await Promise.resolve(); await Promise.resolve(); });
        expect(reads).toBe(2);
        // Meanwhile the user saves another folder and reopens Settings.
        const openSettings = () => React.act(async () => host.querySelector<HTMLButtonElement>('button[title="common.settings"]')!.click());
        const remoteFolder = () => host.querySelector<HTMLInputElement>('input[placeholder="/cloud/"]');
        await openSettings();
        await React.act(async () => {
            Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(remoteFolder(), '/new');
            remoteFolder()!.dispatchEvent(new Event('input', { bubbles: true }));
        });
        await React.act(async () => [...host.querySelectorAll('button')].find(b => b.textContent === 'common.save common.settings')!.click());
        await React.act(async () => { await Promise.resolve(); await Promise.resolve(); });
        await openSettings();
        // The old read lands now: it must not bring /remote back.
        await React.act(async () => releaseRefresh({ enabled: true, sync_direction: 'local_to_remote', update_only: true, local_folder: '/local', remote_folder: '/remote', exclude_patterns: [], excluded_folders: [] }));
        expect(remoteFolder()?.value).toBe('/new');
        await React.act(async () => [...host.querySelectorAll('button')].find(b => b.textContent === 'common.save common.settings')!.click());
        expect(saved.map(config => config.remote_folder)).toEqual(['/new', '/new']);
    });

    it('applies only the latest read: an older refresh released after a newer one keeps the placed folder', async () => {
        (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
        const base = { enabled: true, sync_direction: 'local_to_remote', update_only: true, local_folder: '/local', server_profile: 'nas', protocol_type: 'sftp', exclude_patterns: [], excluded_folders: [] };
        let disk: { remote_folder?: string } = { ...base, remote_folder: '~/docs' };
        const saved: { remote_folder?: string; update_only?: boolean }[] = [];
        let holdNext = false;
        let releaseA!: (config: unknown) => void;
        mocks.invoke.mockImplementation(async (command: string, args?: { config?: { remote_folder?: string } }) => {
            if (command === 'get_cloud_config') {
                if (holdNext) { holdNext = false; return new Promise(resolve => { releaseA = resolve; }); }
                return disk;
            }
            if (command === 'trigger_cloud_sync') return 'done';
            if (command === 'save_cloud_config_cmd') { saved.push(args?.config ?? {}); disk = args?.config ?? {}; return null; }
            if (command === 'get_cloud_status') return { type: 'idle' };
            if (command === 'get_cloud_pairs_config') return { pairs: [] };
            if (command === 'get_sync_schedule_cmd') return null;
            return [];
        });
        host = document.createElement('div'); document.body.append(host); root = createRoot(host);
        await React.act(async () => root!.render(<CloudPanel isOpen onClose={vi.fn()} />));
        // A: the refresh after a sync read the file before the placement and waits.
        holdNext = true;
        await React.act(async () => [...host.querySelectorAll('button')].find(b => b.textContent?.includes('cloud.syncNow'))!.click());
        await React.act(async () => { await Promise.resolve(); await Promise.resolve(); });
        // A background cycle places the folder; B: opening Settings reads it.
        disk = { ...disk, remote_folder: '/home/u/docs' };
        const remoteFolder = () => host.querySelector<HTMLInputElement>('input[placeholder="/cloud/"]');
        await React.act(async () => host.querySelector<HTMLButtonElement>('button[title="common.settings"]')!.click());
        await React.act(async () => { await Promise.resolve(); await Promise.resolve(); });
        expect(remoteFolder()?.value).toBe('/home/u/docs');
        // A lands now with the file as it read it.
        await React.act(async () => releaseA({ ...base, remote_folder: '~/docs' }));
        expect(remoteFolder()?.value).toBe('/home/u/docs');
        // An unrelated save keeps the placed folder.
        const update = Array.from(host.querySelectorAll('label')).find(label => label.textContent?.includes('aerosync.updateOnly'))!;
        await React.act(async () => update.querySelector('input')!.click());
        await React.act(async () => [...host.querySelectorAll('button')].find(b => b.textContent === 'common.save common.settings')!.click());
        expect(saved.map(config => config.remote_folder)).toEqual(['/home/u/docs']);
        expect(saved[0].update_only).toBe(false);
    });

    it('drops a read still in flight when the panel closes, and reads the file again on reopen', async () => {
        (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
        const base = { enabled: true, sync_direction: 'local_to_remote', update_only: true, local_folder: '/local', server_profile: 'nas', protocol_type: 'sftp', exclude_patterns: [], excluded_folders: [] };
        let disk: unknown = { ...base, remote_folder: '/remote' };
        let holdNext = false;
        let release!: (config: unknown) => void;
        mocks.invoke.mockImplementation(async (command: string) => {
            if (command === 'get_cloud_config') {
                if (holdNext) { holdNext = false; return new Promise(resolve => { release = resolve; }); }
                return disk;
            }
            if (command === 'trigger_cloud_sync') return 'done';
            if (command === 'get_cloud_status') return { type: 'idle' };
            if (command === 'get_cloud_pairs_config') return { pairs: [] };
            if (command === 'get_sync_schedule_cmd') return null;
            return [];
        });
        host = document.createElement('div'); document.body.append(host); root = createRoot(host);
        const render = (isOpen: boolean) => React.act(async () => root!.render(<CloudPanel isOpen={isOpen} onClose={vi.fn()} />));
        await render(true);
        holdNext = true;
        await React.act(async () => [...host.querySelectorAll('button')].find(b => b.textContent?.includes('cloud.syncNow'))!.click());
        await React.act(async () => { await Promise.resolve(); await Promise.resolve(); });
        await render(false);
        // The file changes while the panel is closed; the old read lands late.
        disk = { ...base, remote_folder: '/new' };
        await React.act(async () => release({ ...base, remote_folder: '/old' }));
        // The reopen reads the file again; until that read lands the panel
        // shows what it had, never the dropped reply.
        holdNext = true;
        await render(true);
        expect(host.textContent).toContain('/remote');
        expect(host.textContent).not.toContain('/old');
        await React.act(async () => release(disk));
        expect(host.textContent).toContain('/new');
        expect(host.textContent).not.toContain('/old');
    });

    it('loads the configuration on reopen when the first load was dropped by a close', async () => {
        (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
        const disk = { enabled: true, sync_direction: 'local_to_remote', update_only: true, local_folder: '/local', server_profile: 'nas', protocol_type: 'sftp', remote_folder: '/remote', exclude_patterns: [], excluded_folders: [] };
        let reads = 0;
        let releaseFirst!: (config: unknown) => void;
        mocks.invoke.mockImplementation(async (command: string) => {
            if (command === 'get_cloud_config') {
                reads += 1;
                if (reads === 1) return new Promise(resolve => { releaseFirst = resolve; });
                return disk;
            }
            if (command === 'get_cloud_status') return { type: 'idle' };
            if (command === 'get_cloud_pairs_config') return { pairs: [] };
            if (command === 'get_sync_schedule_cmd') return null;
            return [];
        });
        host = document.createElement('div'); document.body.append(host); root = createRoot(host);
        const render = (isOpen: boolean) => React.act(async () => root!.render(<CloudPanel isOpen={isOpen} onClose={vi.fn()} />));
        await render(true);
        await render(false);
        await render(true);
        await React.act(async () => releaseFirst({ ...disk, remote_folder: '/old' }));
        expect(reads).toBe(2);
        expect(host.textContent).toContain('/remote');
        expect(host.querySelector('button[title="common.settings"]')).not.toBeNull();
    });

    it('replaces the panel with a refreshed file that names another account instead of merging its folder', async () => {
        (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
        const base = { enabled: true, sync_direction: 'local_to_remote', update_only: true, local_folder: '/local', protocol_type: 'sftp', exclude_patterns: [], excluded_folders: [] };
        let disk: unknown = { ...base, server_profile: 'nas', remote_folder: '/remote' };
        const saved: unknown[] = [];
        mocks.invoke.mockImplementation(async (command: string, args?: { config?: unknown }) => {
            if (command === 'get_cloud_config') return disk;
            if (command === 'save_cloud_config_cmd') { saved.push(args?.config); return null; }
            if (command === 'get_cloud_status') return { type: 'idle' };
            if (command === 'get_cloud_pairs_config') return { pairs: [] };
            if (command === 'get_sync_schedule_cmd') return null;
            return [];
        });
        host = document.createElement('div'); document.body.append(host); root = createRoot(host);
        await React.act(async () => root!.render(<CloudPanel isOpen onClose={vi.fn()} />));
        expect(host.textContent).toContain('nas');
        // Another window set AeroCloud up on another profile meanwhile.
        disk = { ...base, server_profile: 'office', remote_folder: '/home/o/docs' };
        await React.act(async () => host.querySelector<HTMLButtonElement>('button[title="common.settings"]')!.click());
        await React.act(async () => { await Promise.resolve(); await Promise.resolve(); });
        // The settings of the old account close: nothing of theirs is saved over the new one.
        expect(host.querySelector('input[placeholder="/cloud/"]')).toBeNull();
        expect(host.textContent).toContain('office');
        expect(saved).toEqual([]);
    });

    it('drops a post-sync refresh that read the file before Disable and a new setup', async () => {
        mocks.setupResult = { enabled: true, cloud_name: 'fresh', local_folder: '/local2', remote_folder: '/new', sync_interval_secs: 300, sync_direction: 'bidirectional', exclude_patterns: [], excluded_folders: [] };
        (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
        let releaseRefresh!: (config: unknown) => void;
        let reads = 0;
        mocks.invoke.mockImplementation(async (command: string) => {
            if (command === 'get_cloud_config') {
                reads += 1;
                if (reads === 2) return new Promise(resolve => { releaseRefresh = resolve; });
                return { enabled: true, sync_direction: 'local_to_remote', update_only: true, local_folder: '/local', remote_folder: '/remote', exclude_patterns: [], excluded_folders: [] };
            }
            if (command === 'trigger_cloud_sync') return 'done';
            if (command === 'disable_aerocloud') return null;
            if (command === 'get_cloud_status') return { type: 'idle' };
            if (command === 'get_cloud_pairs_config') return { pairs: [] };
            return [];
        });
        host = document.createElement('div'); document.body.append(host); root = createRoot(host);
        await React.act(async () => root!.render(<CloudPanel isOpen onClose={vi.fn()} />));
        await React.act(async () => [...host.querySelectorAll('button')].find(b => b.textContent?.includes('cloud.syncNow'))!.click());
        await React.act(async () => { await Promise.resolve(); await Promise.resolve(); });
        expect(reads).toBe(2);
        await React.act(async () => [...host.querySelectorAll('button')].find(b => b.textContent?.trim() === 'cloud.disable')!.click());
        const confirmOverlay = host.querySelector('[aria-label="cloud.disableConfirm"]')!;
        await React.act(async () => [...confirmOverlay.querySelectorAll('button')].find(b => b.textContent?.trim() === 'cloud.disable')!.click());
        await React.act(async () => { await Promise.resolve(); });
        await React.act(async () => host.querySelector<HTMLButtonElement>('[data-testid="setup-complete"]')!.click());
        expect(host.textContent).toContain('/new');
        await React.act(async () => releaseRefresh({ enabled: true, sync_direction: 'local_to_remote', update_only: true, local_folder: '/local', remote_folder: '/remote', exclude_patterns: [], excluded_folders: [], last_sync: new Date().toISOString() }));
        expect(host.textContent).toContain('/new');
        expect(host.textContent).not.toContain('/remote');
        expect(host.textContent).not.toContain('cloud.justNow');
    });

    it('drops the stale settings baseline when Disable replaces the configuration', async () => {
        mocks.setupResult = { enabled: true, cloud_name: 'fresh', local_folder: '/local2', remote_folder: '/new', sync_interval_secs: 300, sync_direction: 'bidirectional', exclude_patterns: [], excluded_folders: [] };
        const savedConfigs: { remote_folder?: string }[] = [];
        let resolveDisable!: () => void;
        const disablePromise = new Promise<void>(res => { resolveDisable = res; });
        await mount([]);
        // The file as the backend keeps it: the setup and a save replace it.
        let disk: unknown = { enabled: true, sync_direction: 'local_to_remote', update_only: true, local_folder: '/local', remote_folder: '/remote', exclude_patterns: [], excluded_folders: [] };
        mocks.invoke.mockImplementation(async (command: string, args?: { config?: { remote_folder?: string } }) => {
            if (command === 'disable_aerocloud') return disablePromise;
            if (command === 'save_cloud_config_cmd') { savedConfigs.push(args?.config ?? {}); disk = args?.config; return null; }
            if (command === 'get_cloud_config') return disk;
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
        disk = mocks.setupResult;
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

    it('keeps a post-sync refresh that lands after Disable from restoring the configuration', async () => {
        // The refresh read the configuration before Disable cleared it; its
        // late answer must not bring the disabled setup back to the dashboard.
        const before = { enabled: true, sync_direction: 'local_to_remote', update_only: true, local_folder: '/local', remote_folder: '/remote', exclude_patterns: [], excluded_folders: [] };
        let resolveRefresh!: (v: unknown) => void;
        await mount([]);
        mocks.invoke.mockImplementation(async (command: string) => {
            if (command === 'trigger_cloud_sync') return 'done';
            if (command === 'get_cloud_config') return new Promise(res => { resolveRefresh = res; });
            if (command === 'disable_aerocloud') return null;
            if (command === 'get_cloud_pairs_config') return { pairs: [] };
            return [];
        });
        await React.act(async () => [...host.querySelectorAll('button')].find(b => b.textContent?.includes('cloud.syncNow'))!.click());
        await React.act(async () => { await Promise.resolve(); await Promise.resolve(); });
        await React.act(async () => [...host.querySelectorAll('button')].find(b => b.textContent?.includes('cloud.disable'))!.click());
        const confirm = host.querySelector('[aria-label="cloud.disableConfirm"]')!;
        await React.act(async () => [...confirm.querySelectorAll('button')].find(b => b.textContent?.trim() === 'cloud.disable')!.click());
        await React.act(async () => { await Promise.resolve(); await Promise.resolve(); });
        expect(host.querySelector('[data-testid="setup-complete"]')).toBeTruthy();
        await React.act(async () => resolveRefresh(before));
        await React.act(async () => { await Promise.resolve(); await Promise.resolve(); });
        expect(host.querySelector('[data-testid="setup-complete"]')).toBeTruthy();
        expect([...host.querySelectorAll('button')].some(b => b.textContent?.includes('cloud.syncNow'))).toBe(false);
    });

});
