// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
import React from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { MultiPathEditor } from './MultiPathEditor';

const mocks = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: mocks.invoke }));
vi.mock('../../i18n', () => ({ useTranslation: () => (key: string) => key }));

let root: Root | undefined;
let host: HTMLDivElement;
afterEach(async () => { if (root) await React.act(async () => root!.unmount()); host?.remove(); root = undefined; mocks.invoke.mockReset(); });

const pair = { id: 'p1', name: 'Docs', local_path: '/l', remote_path: '/r', enabled: true, exclude_overrides: [] };

async function render(isOpen: boolean, onClose = vi.fn()) {
    await React.act(async () => root!.render(<MultiPathEditor isOpen={isOpen} embedded onClose={onClose} localPath="/l" remotePath="/r" />));
}

function button(label: string) {
    return Array.from(host.querySelectorAll('button')).find(candidate => candidate.textContent === label)!;
}
const saveButton = () => button('common.save');

describe('MultiPathEditor after a failed load', () => {
    it('writes nothing over a file it could not read, even after an earlier load succeeded', async () => {
        (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
        let readable = true;
        mocks.invoke.mockImplementation(async (command: string) => {
            if (command === 'get_multi_path_config') {
                if (!readable) throw new Error('permission denied');
                return { pairs: [pair], parallel_pairs: false };
            }
            return undefined;
        });
        host = document.createElement('div'); document.body.append(host); root = createRoot(host);
        await render(true);
        expect(saveButton().matches(':disabled')).toBe(false);

        await render(false);
        readable = false;
        await render(true);

        expect(host.querySelector('[role="alert"]')?.textContent).toBe('common.settingsLoadFailed');
        expect(saveButton().matches(':disabled')).toBe(true);
        await React.act(async () => saveButton().click());
        expect(mocks.invoke.mock.calls.map(([command]) => command)).not.toContain('save_multi_path_config_cmd');
    });

    it('keeps Cancel working in the embedded editor after a failed reload', async () => {
        (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
        let readable = true;
        mocks.invoke.mockImplementation(async (command: string) => {
            if (command === 'get_multi_path_config') {
                if (!readable) throw new Error('permission denied');
                return { pairs: [pair], parallel_pairs: false };
            }
            return undefined;
        });
        host = document.createElement('div'); document.body.append(host); root = createRoot(host);
        const onClose = vi.fn();
        await render(true, onClose);
        await render(false, onClose);
        readable = false;
        await render(true, onClose);

        expect(saveButton().matches(':disabled')).toBe(true);
        expect(button('common.cancel').matches(':disabled')).toBe(false);
        await React.act(async () => button('common.cancel').click());
        expect(onClose).toHaveBeenCalledTimes(1);
    });
});
