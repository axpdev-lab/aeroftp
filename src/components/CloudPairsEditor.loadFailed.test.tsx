// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
import React from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { CloudPairsEditor } from './CloudPairsEditor';

const mocks = vi.hoisted(() => ({ invoke: vi.fn(), profiles: vi.fn(async () => [] as unknown[]) }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: mocks.invoke }));
vi.mock('../i18n', () => ({ useTranslation: () => (key: string) => key }));
vi.mock('../utils/serverProfileStore', () => ({ loadSavedServerProfiles: () => mocks.profiles() }));

let root: Root | undefined;
let host: HTMLDivElement;
afterEach(async () => { if (root) await React.act(async () => root!.unmount()); host?.remove(); root = undefined; mocks.invoke.mockReset(); mocks.profiles.mockImplementation(async () => []); });

describe('CloudPairsEditor after a failed load', () => {
    it('shows the error instead of an empty list and saves nothing over the file', async () => {
        (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
        mocks.invoke.mockImplementation(async (command: string) => {
            if (command === 'get_cloud_pairs_config') throw new Error('permission denied');
            if (command === 'get_cloud_config') return { enabled: true, local_folder: '/l', remote_folder: '/r' };
            return undefined;
        });
        host = document.createElement('div'); document.body.append(host); root = createRoot(host);
        await React.act(async () => root!.render(<CloudPairsEditor isOpen onClose={vi.fn()} />));

        expect(host.querySelector('[role="alert"]')?.textContent).toBe('common.settingsLoadFailed');
        expect(host.textContent).not.toContain('cloud.pairsEmpty');
        // An unknown pair list is not an empty one: no legacy import offer.
        expect(host.textContent).not.toContain('cloud.importLegacy');
        const save = Array.from(host.querySelectorAll('button')).find(button => button.textContent === 'common.save')!;
        expect(save.disabled).toBe(true);
        await React.act(async () => save.click());
        expect(mocks.invoke.mock.calls.map(([command]) => command)).not.toContain('save_cloud_pairs_config_cmd');
    });

    it('adds no pair while the pairs file could not be read', async () => {
        (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
        mocks.invoke.mockImplementation(async (command: string) => {
            if (command === 'get_cloud_pairs_config' || command === 'add_cloud_pair') throw new Error('permission denied');
            return undefined;
        });
        host = document.createElement('div'); document.body.append(host); root = createRoot(host);
        await React.act(async () => root!.render(<CloudPairsEditor isOpen onClose={vi.fn()} />));
        // A failed add would fall back to a local-only pair that Save may not write.
        const add = Array.from(host.querySelectorAll('button')).find(button => button.textContent?.includes('cloud.pairsAdd'))!;
        expect(add.disabled).toBe(true);
        await React.act(async () => add.click());
        expect(mocks.invoke.mock.calls.map(([command]) => command)).not.toContain('add_cloud_pair');
        expect(host.querySelector('button[title="cloud.disable"]')).toBeNull();
    });

    it('adds no pair while the pairs file is still being read, so a failed add cannot land after a failed read', async () => {
        (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
        let failRead!: () => void;
        let failAdd: (() => void) | null = null;
        mocks.invoke.mockImplementation((command: string) => {
            if (command === 'get_cloud_pairs_config') return new Promise((_, reject) => { failRead = () => reject(new Error('permission denied')); });
            if (command === 'add_cloud_pair') return new Promise((_, reject) => { failAdd = () => reject(new Error('permission denied')); });
            return Promise.resolve(undefined);
        });
        host = document.createElement('div'); document.body.append(host); root = createRoot(host);
        await React.act(async () => root!.render(<CloudPairsEditor isOpen onClose={vi.fn()} />));
        const add = () => Array.from(host.querySelectorAll('button')).find(button => button.textContent?.includes('cloud.pairsAdd'))!;
        const disabledWhileLoading = add().disabled;
        await React.act(async () => add().click());
        // The read fails, then the add's own failure is released.
        await React.act(async () => failRead());
        if (failAdd) await React.act(async () => failAdd!());
        expect(mocks.invoke.mock.calls.map(([command]) => command)).not.toContain('add_cloud_pair');
        expect(host.querySelector('[role="alert"]')?.textContent).toBe('common.settingsLoadFailed');
        // No editable pair from an unconfirmed list.
        expect(host.querySelector('button[title="cloud.disable"]')).toBeNull();
        expect(host.querySelector('input')).toBeNull();
        expect(disabledWhileLoading).toBe(true);
    });

    it('hides an earlier legacy import offer when a reload of the pairs fails', async () => {
        (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
        let readable = true;
        mocks.invoke.mockImplementation(async (command: string) => {
            if (command === 'get_cloud_pairs_config') {
                if (!readable) throw new Error('permission denied');
                return { pairs: [], parallel_pairs: false };
            }
            if (command === 'get_cloud_config') return { enabled: true, local_folder: '/l', remote_folder: '/r' };
            return undefined;
        });
        host = document.createElement('div'); document.body.append(host); root = createRoot(host);
        const render = (isOpen: boolean) => React.act(async () => root!.render(<CloudPairsEditor isOpen={isOpen} onClose={vi.fn()} />));
        await render(true);
        expect(host.textContent).toContain('cloud.importLegacy');

        // Reopen with the pairs unreadable while the later reads still wait.
        await render(false);
        readable = false;
        let release!: () => void;
        mocks.profiles.mockImplementation(() => new Promise(resolve => { release = () => resolve([]); }));
        await render(true);
        expect(host.textContent).not.toContain('cloud.importLegacy');
        await React.act(async () => release());
        expect(host.textContent).not.toContain('cloud.importLegacy');
        expect(mocks.invoke.mock.calls.map(([command]) => command)).not.toContain('add_cloud_pair');
    });

    it('keeps a failed reload when an earlier load finishes after it', async () => {
        (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
        let finishFirst!: (config: unknown) => void;
        let calls = 0;
        mocks.invoke.mockImplementation(async (command: string) => {
            if (command === 'get_cloud_pairs_config') {
                calls += 1;
                if (calls === 1) return new Promise(resolve => { finishFirst = resolve; });
                throw new Error('permission denied');
            }
            return undefined;
        });
        host = document.createElement('div'); document.body.append(host); root = createRoot(host);
        const render = (isOpen: boolean) => React.act(async () => root!.render(<CloudPairsEditor isOpen={isOpen} onClose={vi.fn()} />));
        // The first opening's read is still pending when the editor reopens
        // and the second read fails.
        await render(true);
        await render(false);
        await render(true);
        expect(host.querySelector('[role="alert"]')?.textContent).toBe('common.settingsLoadFailed');

        await React.act(async () => finishFirst({ pairs: [{ id: 'stale', local_folder: '/stale', remote_folder: '/r', server_profile: 'nas', enabled: true }], parallel_pairs: false }));
        expect(host.querySelector('[role="alert"]')?.textContent).toBe('common.settingsLoadFailed');
        expect(host.innerHTML).not.toContain('/stale');
        const save = Array.from(host.querySelectorAll('button')).find(button => button.textContent === 'common.save')!;
        expect(save.disabled).toBe(true);
    });
});

describe('CloudPairsEditor saves', () => {
    // A sync placed `docs` as `/home/u/docs` after the editor read the list:
    // the toggle sends the list as read, so the backend keeps the placed
    // folder, and the editor shows the list it saved; the Save after it reads
    // from that list.
    it('sends the list as read with every save and adopts the list saved', async () => {
        (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
        const read = { id: 'a', name: 'A', local_path: '/l', remote_path: 'docs', enabled: true, server_profile: 'nas', protocol_type: 'sftp', connection_params: {}, sync_direction: 'bidirectional', compress_enabled: false, compress_level: 3 };
        mocks.invoke.mockImplementation(async (command: string, args?: { config: { pairs: Array<Record<string, unknown>> } }) => {
            if (command === 'get_cloud_pairs_config') return { pairs: [read], parallel_pairs: false };
            if (command === 'save_cloud_pairs_config_cmd') return { pairs: args!.config.pairs.map(pair => ({ ...pair, remote_path: '/home/u/docs' })), parallel_pairs: false };
            return undefined;
        });
        host = document.createElement('div'); document.body.append(host); root = createRoot(host);
        await React.act(async () => root!.render(<CloudPairsEditor isOpen onClose={vi.fn()} />));
        const toggle = host.querySelector('button[title="cloud.disable"]') as HTMLButtonElement;
        await React.act(async () => toggle.click());
        const saves = () => mocks.invoke.mock.calls.filter(([command]) => command === 'save_cloud_pairs_config_cmd').map(([, args]) => args as { config: { pairs: typeof read[] }; readAs: { pairs: typeof read[] } });
        expect(saves()).toHaveLength(1);
        expect(saves()[0].readAs.pairs).toEqual([read]);
        expect(saves()[0].config.pairs[0].enabled).toBe(false);
        const remote = Array.from(host.querySelectorAll('input')).map(input => input.value);
        expect(remote).toContain('/home/u/docs');
        expect(remote).not.toContain('docs');
    });
});
