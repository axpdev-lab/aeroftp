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
});
