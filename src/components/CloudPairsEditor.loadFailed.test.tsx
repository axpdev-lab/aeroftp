// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
import React from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { CloudPairsEditor } from './CloudPairsEditor';

const mocks = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: mocks.invoke }));
vi.mock('../i18n', () => ({ useTranslation: () => (key: string) => key }));
vi.mock('../utils/serverProfileStore', () => ({ loadSavedServerProfiles: async () => [] }));

let root: Root | undefined;
let host: HTMLDivElement;
afterEach(async () => { if (root) await React.act(async () => root!.unmount()); host?.remove(); root = undefined; mocks.invoke.mockReset(); });

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

    it('does not let a toggle write a locally added pair over the file it could not read', async () => {
        (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
        mocks.invoke.mockImplementation(async (command: string) => {
            if (command === 'get_cloud_pairs_config' || command === 'add_cloud_pair') throw new Error('permission denied');
            return undefined;
        });
        host = document.createElement('div'); document.body.append(host); root = createRoot(host);
        await React.act(async () => root!.render(<CloudPairsEditor isOpen onClose={vi.fn()} />));
        // A failed add falls back to a local-only pair, which brings a toggle on screen.
        await React.act(async () => Array.from(host.querySelectorAll('button')).find(button => button.textContent?.includes('cloud.pairsAdd'))!.click());
        const toggle = host.querySelector<HTMLButtonElement>('button[title="cloud.disable"]')!;
        await React.act(async () => toggle.click());
        expect(mocks.invoke.mock.calls.map(([command]) => command)).not.toContain('save_cloud_pairs_config_cmd');
    });
});
