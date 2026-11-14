// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// H12 sweep: footer Cancel/Close buttons that bypassed the shared close guard.
import React, { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';

const mocks = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: mocks.invoke }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock('../i18n', () => ({ useTranslation: () => (key: string) => key }));
vi.mock('../hooks/useDraggableModal', () => ({ useDraggableModal: () => ({ panelProps: {}, dragHandleProps: {} }) }));
vi.mock('../hooks/useArchiveProgress', () => ({ useArchiveProgress: () => null }));
vi.mock('../utils/pickPath', () => ({ pickFile: vi.fn(), pickSave: vi.fn() }));
vi.mock('../utils/serverProfileStore', () => ({ loadSavedServerProfiles: vi.fn(async () => []) }));
vi.mock('../utils/logger', () => ({ logger: { debug: vi.fn(), info: vi.fn(), warn: vi.fn(), error: vi.fn() } }));

import { ArchivePasswordDialog, MasterPasswordSetupDialog } from './Dialogs';
import { S3TagsDialog } from './S3TagsDialog';
import { CloudPairsEditor } from './CloudPairsEditor';
import { CompressDialog } from './CompressDialog';

let host: HTMLDivElement;
let root: Root;

beforeEach(() => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    host = document.createElement('div');
    document.body.append(host);
    root = createRoot(host);
    mocks.invoke.mockReset();
    mocks.invoke.mockImplementation(async () => ({}));
});

afterEach(async () => {
    await act(async () => root.unmount());
    host.remove();
});

const click = async (el: Element) => act(async () => (el as HTMLElement).click());
const buttonByText = (text: string) =>
    [...host.querySelectorAll('button')].find(b => b.textContent?.trim() === text);
const typeInto = async (input: HTMLInputElement, value: string) => act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(input, value);
    input.dispatchEvent(new Event('input', { bubbles: true }));
});
const flush = () => act(async () => { await Promise.resolve(); await Promise.resolve(); });

async function expectCancelAsksThenDiscards(onClose: ReturnType<typeof vi.fn>) {
    await click(buttonByText('common.cancel')!);
    expect(host.textContent).toContain('modalGuard.dirtyTitle');
    expect(onClose).not.toHaveBeenCalled();
    await click(buttonByText('modalGuard.keepEditing')!);
    expect(host.textContent).not.toContain('modalGuard.dirtyTitle');
    expect(onClose).not.toHaveBeenCalled();
    await click(buttonByText('modalGuard.discardAndClose')!);
    await click(buttonByText('common.cancel')!);
    expect(onClose).toHaveBeenCalledTimes(1);
}

it('ArchivePasswordDialog Cancel asks before dropping a typed password', async () => {
    const onClose = vi.fn();
    const onSubmit = vi.fn(async () => {});
    await act(async () => root.render(<ArchivePasswordDialog archiveName="a.zip" format="zip" onSubmit={onSubmit} onClose={onClose} />));
    await typeInto(host.querySelector<HTMLInputElement>('input[type="password"]')!, 's3cr3t');
    await click(buttonByText('common.cancel')!);
    expect(host.textContent).toContain('modalGuard.dirtyTitle');
    expect(onClose).not.toHaveBeenCalled();
    await click(buttonByText('modalGuard.discardAndClose')!);
    expect(onClose).toHaveBeenCalledTimes(1);
});

it('MasterPasswordSetupDialog Cancel asks before dropping typed passwords', async () => {
    const onClose = vi.fn();
    const onComplete = vi.fn();
    await act(async () => root.render(<MasterPasswordSetupDialog onComplete={onComplete} onClose={onClose} />));
    const passwords = host.querySelectorAll<HTMLInputElement>('input[type="password"]');
    expect(passwords.length).toBeGreaterThanOrEqual(2);
    await typeInto(passwords[0], 'long-enough-passphrase');
    await typeInto(passwords[1], 'long-enough-passphrase');
    await click(buttonByText('common.cancel')!);
    expect(host.textContent).toContain('modalGuard.dirtyTitle');
    expect(onClose).not.toHaveBeenCalled();
    await click(buttonByText('modalGuard.discardAndClose')!);
    expect(onClose).toHaveBeenCalledTimes(1);
});

it('S3TagsDialog Cancel asks before dropping edited tags', async () => {
    const onClose = vi.fn();
    mocks.invoke.mockImplementation(async (command: string) => {
        if (command === 's3_get_object_tags') return { env: 'prod' };
        return {};
    });
    await act(async () => root.render(<S3TagsDialog path="/a" name="a" onClose={onClose} onSaved={vi.fn()} />));
    await flush();
    const valueInput = host.querySelector<HTMLInputElement>('input[placeholder="s3.tagValue"]')!;
    expect(valueInput).toBeTruthy();
    await typeInto(valueInput, 'dev');
    await click(buttonByText('common.cancel')!);
    expect(host.textContent).toContain('modalGuard.dirtyTitle');
    expect(onClose).not.toHaveBeenCalled();
    await click(buttonByText('modalGuard.discardAndClose')!);
    expect(onClose).toHaveBeenCalledTimes(1);
});

it('CompressDialog Cancel asks before dropping a renamed archive', async () => {
    const onClose = vi.fn();
    const onConfirm = vi.fn();
    await act(async () => root.render(
        <CompressDialog
            files={[{ name: 'a.txt', path: '/a.txt', size: 10, isDir: false }]}
            defaultName="out.zip"
            outputDir="/out"
            onConfirm={onConfirm}
            onClose={onClose}
        />,
    ));
    const nameInput = [...host.querySelectorAll<HTMLInputElement>('input[type="text"]')]
        .find(i => i.value === 'out.zip')!;
    expect(nameInput).toBeTruthy();
    await typeInto(nameInput, 'other.zip');
    await click(buttonByText('common.cancel')!);
    expect(host.textContent).toContain('modalGuard.dirtyTitle');
    expect(onClose).not.toHaveBeenCalled();
    await click(buttonByText('modalGuard.discardAndClose')!);
    expect(onClose).toHaveBeenCalledTimes(1);
});

it('CloudPairsEditor Cancel asks before dropping an edited pair', async () => {
    const onClose = vi.fn();
    const pair = {
        id: 'p1', name: 'Pair 1', local_path: '/l', remote_path: '/r', enabled: true,
        server_profile: '', protocol_type: 'ftp', connection_params: {},
        sync_direction: 'bidirectional', preserve_remote_deletes: true, update_only: false,
        compress_enabled: false, compress_level: 3, conflict_strategy: 'ask_user',
        versioning_strategy: { type: 'trash_can', max_age_days: 30 },
        excluded_folders: [], exclude_patterns: [], last_sync: null,
    };
    mocks.invoke.mockImplementation(async (command: string) => {
        if (command === 'get_cloud_pairs_config') return { pairs: [pair], parallel_pairs: false };
        if (command === 'get_cloud_config') throw new Error('none');
        return {};
    });
    await act(async () => root.render(<CloudPairsEditor isOpen onClose={onClose} />));
    await flush();
    await flush();
    const nameInput = host.querySelector<HTMLInputElement>('input[placeholder="cloud.pairNamePlaceholder"]')!;
    expect(nameInput).toBeTruthy();
    await typeInto(nameInput, 'Renamed pair');
    await click(buttonByText('common.cancel')!);
    expect(host.textContent).toContain('modalGuard.dirtyTitle');
    expect(onClose).not.toHaveBeenCalled();
    await click(buttonByText('modalGuard.discardAndClose')!);
    expect(onClose).toHaveBeenCalledTimes(1);
});

it('CloudPairsEditor stays open while a pair removal is in flight', async () => {
    // Add, remove, toggle and the legacy import persist on their own: an
    // unchanged editor had no guard while one ran, so a stray click closed it
    // before its result or error showed.
    const onClose = vi.fn();
    const pair = {
        id: 'p1', name: 'Pair 1', local_path: '/l', remote_path: '/r', enabled: true,
        server_profile: '', protocol_type: 'ftp', connection_params: {},
        sync_direction: 'bidirectional', preserve_remote_deletes: true, update_only: false,
        compress_enabled: false, compress_level: 3, conflict_strategy: 'ask_user',
        versioning_strategy: { type: 'trash_can', max_age_days: 30 },
        excluded_folders: [], exclude_patterns: [], last_sync: null,
    };
    let resolveRemove!: (v: unknown) => void;
    mocks.invoke.mockImplementation(async (command: string) => {
        if (command === 'get_cloud_pairs_config') return { pairs: [pair], parallel_pairs: false };
        if (command === 'get_cloud_config') throw new Error('none');
        if (command === 'remove_cloud_pair') return new Promise(res => { resolveRemove = res; });
        return {};
    });
    await act(async () => root.render(<CloudPairsEditor isOpen onClose={onClose} />));
    await flush();
    await flush();
    await click(host.querySelector('button[title="common.delete"]')!);
    const backdrop = host.querySelector('[aria-label="cloud.pairsEditorLabel"]')!;
    await act(async () => backdrop.dispatchEvent(new MouseEvent('click', { bubbles: true })));
    expect(onClose).not.toHaveBeenCalled();
    await click(buttonByText('common.cancel')!);
    expect(host.textContent).toContain('modalGuard.busyTitle');
    expect(onClose).not.toHaveBeenCalled();
    // Save writes the whole list: it waits until the removal settles.
    const save = buttonByText('common.save')!;
    expect(save.disabled).toBe(true);
    await act(async () => resolveRemove({ pairs: [] }));
    await flush();
    expect(buttonByText('common.save')!.disabled).toBe(false);
});

it('CloudPairsEditor stays busy until an add and a removal running together both settle', async () => {
    // A pending add guards like a removal, and with both in flight the first
    // to settle does not release the editor or its Save.
    const onClose = vi.fn();
    const pair = {
        id: 'p1', name: 'Pair 1', local_path: '/l', remote_path: '/r', enabled: true,
        server_profile: '', protocol_type: 'ftp', connection_params: {},
        sync_direction: 'bidirectional', preserve_remote_deletes: true, update_only: false,
        compress_enabled: false, compress_level: 3, conflict_strategy: 'ask_user',
        versioning_strategy: { type: 'trash_can', max_age_days: 30 },
        excluded_folders: [], exclude_patterns: [], last_sync: null,
    };
    let resolveAdd!: (v: unknown) => void;
    let resolveRemove!: (v: unknown) => void;
    mocks.invoke.mockImplementation(async (command: string) => {
        if (command === 'get_cloud_pairs_config') return { pairs: [pair], parallel_pairs: false };
        if (command === 'get_cloud_config') throw new Error('none');
        if (command === 'add_cloud_pair') return new Promise(res => { resolveAdd = res; });
        if (command === 'remove_cloud_pair') return new Promise(res => { resolveRemove = res; });
        return {};
    });
    await act(async () => root.render(<CloudPairsEditor isOpen onClose={onClose} />));
    await flush();
    await flush();
    const backdrop = () => host.querySelector('[aria-label="cloud.pairsEditorLabel"]')!;
    const clickBackdrop = () => act(async () => backdrop().dispatchEvent(new MouseEvent('click', { bubbles: true })));
    await click(buttonByText('cloud.pairsAdd')!);
    await clickBackdrop();
    expect(onClose).not.toHaveBeenCalled();
    expect(buttonByText('common.save')!.disabled).toBe(true);
    await click(host.querySelector('button[title="common.delete"]')!);
    await act(async () => resolveAdd({ pairs: [pair, { ...pair, id: 'p2', name: 'Pair 2' }] }));
    await flush();
    // The removal still runs.
    await clickBackdrop();
    expect(onClose).not.toHaveBeenCalled();
    expect(buttonByText('common.save')!.disabled).toBe(true);
    await act(async () => resolveRemove({ pairs: [{ ...pair, id: 'p2', name: 'Pair 2' }] }));
    await flush();
    expect(buttonByText('common.save')!.disabled).toBe(false);
    await clickBackdrop();
    expect(onClose).toHaveBeenCalledTimes(1);
});
