// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later

import * as React from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { PlanTabContent } from './PlanTabContent';
import { createTabStateStore, TabStateStoreContext } from './tabStateStore';
import { useAeroSyncContextActions, type AeroSyncState } from './useAeroSyncContextActions';
import { compareEntries } from '../../utils/compareEndpoints';

const mocks = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: mocks.invoke }));
vi.mock('../../i18n', () => ({ useTranslation: () => (key: string) => key }));

const result = compareEntries(
    [{ name: 'keep.txt', isDir: false, size: 10 }, { name: '.stfolder', isDir: true, size: 0 }],
    [],
);
const backup = '.aeroftp-versions';
const roots: Root[] = [];
let host: HTMLDivElement;
const render = async (node: React.ReactElement) => {
    const root = createRoot(host); roots.push(root);
    await React.act(async () => root.render(node));
};
const button = (key: string) => Array.from(host.querySelectorAll('button')).find(b => b.textContent?.includes(key));

beforeEach(() => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    host = document.createElement('div'); document.body.append(host);
    mocks.invoke.mockReset();
    mocks.invoke.mockImplementation(async (cmd: string) => {
        if (cmd === 'load_sync_profiles_cmd') return [];
        if (cmd === 'sync_backup_validate') return { status: 'valid', dir: backup, ancestors: [] };
        throw new Error(`Unexpected IPC ${cmd}`);
    });
});
afterEach(async () => {
    await React.act(async () => roots.splice(0).forEach(root => root.unmount()));
    host.remove();
});

describe('the Plan notice for a compare made with other settings', () => {
    const plan = async (field: string, compareExcludes: string[], compareBackupDir: string | undefined, onNarrowComparison = vi.fn(async () => undefined)) => {
        const store = createTabStateStore();
        store.set('sync.exclude', field);
        await render(React.createElement(TabStateStoreContext.Provider, { value: store },
            React.createElement(PlanTabContent, {
                result, pairKind: 'local-local', canExecute: true, onExecute: vi.fn(), onRescan: vi.fn(),
                compareExcludes, compareBackupDir, onNarrowComparison,
            })));
        return onNarrowComparison;
    };

    it('offers to filter when the settings only add an exclusion, with no scan', async () => {
        const narrow = await plan('.stfolder', [], backup);
        expect(host.textContent).toContain('aerosync.narrowAvailable');
        await React.act(async () => button('aerosync.narrowApply')!.click());
        expect(narrow).toHaveBeenCalledWith({ userExcludes: ['.stfolder'], backupDir: backup });
    });

    it('says the scan never read the files when an exclusion goes away', async () => {
        await plan('', ['*.tmp'], backup);
        expect(host.textContent).toContain('aerosync.rescanBringsBack');
        expect(button('aerosync.narrowApply')).toBeUndefined();
    });

    it('says a compare of the panel listings applied nothing', async () => {
        await plan('.stfolder', [], undefined);
        expect(host.textContent).toContain('aerosync.rescanFlat');
        expect(button('aerosync.narrowApply')).toBeUndefined();
    });

    it('shows a refused pattern instead of the offer', async () => {
        await plan('[', [], backup, vi.fn(async () => { throw new Error('invalid exclude pattern'); }));
        await React.act(async () => button('aerosync.narrowApply')!.click());
        expect(host.textContent).toContain('invalid exclude pattern');
    });
});

describe('narrowComparison', () => {
    const context = (): AeroSyncState => ({
        initialTab: 'plan',
        context: { pairKind: 'local-local', compareResult: result, compareLoading: false, leftLabel: 'l', rightLabel: 'r', compareExcludes: [], compareBackupDir: backup },
    });
    let state: AeroSyncState | null;
    let actions: ReturnType<typeof useAeroSyncContextActions>;
    const sequence = { current: 0 };
    const Harness = () => {
        const [current, setCurrent] = React.useState(state);
        state = current;
        actions = useAeroSyncContextActions(current, setCurrent, sequence);
        return null;
    };
    const leftOut = (answer: boolean[]) => mocks.invoke.mockImplementation(async () => answer);

    it('drops the rows and records what the compare now applies', async () => {
        state = context();
        await render(React.createElement(Harness));
        leftOut(result.entries.map((entry) => entry.name === '.stfolder'));
        await React.act(async () => actions.narrowComparison(['.stfolder'], backup));
        expect(mocks.invoke).toHaveBeenCalledWith('compare_paths_left_out', expect.objectContaining({
            paths: expect.arrayContaining([{ local_path: '.stfolder', remote_path: '.stfolder', is_dir: true }]),
        }));
        expect(state!.context.compareResult!.entries.map((entry) => entry.name)).toEqual(['keep.txt']);
        expect(state!.context.compareExcludes).toEqual(['.stfolder']);
        expect(state!.context.compareNarrowed).toBe(true);
    });

    it('lets a scan started meanwhile win', async () => {
        state = context();
        await render(React.createElement(Harness));
        mocks.invoke.mockImplementation(async () => {
            sequence.current += 1;
            return result.entries.map(() => true);
        });
        await React.act(async () => actions.narrowComparison(['.stfolder'], backup));
        expect(state!.context.compareResult).toBe(result);
        expect(state!.context.compareNarrowed).toBeUndefined();
    });

    it('leaves the compare alone when a pattern is refused', async () => {
        state = context();
        await render(React.createElement(Harness));
        mocks.invoke.mockImplementation(async () => { throw new Error('invalid exclude pattern'); });
        await React.act(async () => {
            await expect(actions.narrowComparison(['['], backup)).rejects.toThrow('invalid exclude pattern');
        });
        expect(state!.context.compareResult).toBe(result);
    });
});
