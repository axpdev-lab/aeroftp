// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import React, { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const tauri = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: tauri.invoke }));
vi.mock('../i18n', () => ({ useTranslation: () => (key: string) => key }));
vi.mock('../hooks/useHumanizedLog', () => ({
    useHumanizedLog: () => ({ logRaw: () => 'log-1', updateEntry: () => undefined }),
}));

import { AzureTrashManager } from './AzureTrashManager';
import { BoxTrashManager } from './BoxTrashManager';
import { DropboxTrashManager } from './DropboxTrashManager';
import { FileLuTrashManager } from './FileLuTrashManager';
import { GoogleDriveTrashManager } from './GoogleDriveTrashManager';
import { JottacloudTrashManager } from './JottacloudTrashManager';
import { KDriveTrashManager } from './KDriveTrashManager';
import { KoofrTrashManager } from './KoofrTrashManager';
import { MegaTrashManager } from './MegaTrashManager';
import { NextcloudTrashManager } from './NextcloudTrashManager';
import { OneDriveTrashManager } from './OneDriveTrashManager';
import { OpenDriveTrashManager } from './OpenDriveTrashManager';
import { PCloudTrashManager } from './PCloudTrashManager';
import { S3TrashManager } from './S3TrashManager';
import { YandexTrashManager } from './YandexTrashManager';
import { ZohoTrashManager } from './ZohoTrashManager';
import { B2HiddenManager } from './B2HiddenManager';

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

// One artificial entry carrying the fields of every manager's listing shape.
const ENTRY = {
    id: 'sentinel-id',
    name: 'sentinel-a.txt',
    path: '/sentinel/sentinel-a.txt',
    original_path: '/sentinel/sentinel-a.txt',
    is_dir: false,
    size: 1,
    modified: null,
    deleted: null,
    deleted_at: 0,
    deleted_ago_sec: null,
    content_type: 'text/plain',
    mount_id: 'sentinel-mount',
    file_code: 'sentinel01',
    key: 'sentinel/sentinel-a.txt',
    display_key: 'sentinel/sentinel-a.txt',
    version_id: 'sentinel-v1',
    is_delete_marker: true,
    is_latest: true,
    last_modified: null,
    metadata: { id: 'sentinel-id', file_id: '1', fileid: '1', rev: 'r1', trash_kind: 'file', item_type: 'file' },
};

type Props = { onClose: () => void; onRefreshFiles?: () => void };
interface Case {
    name: string;
    Component: React.ComponentType<Props>;
    /** The command a restore sends; held in flight by the test. */
    restore: string;
    /** Button that selects the rows before a bulk restore, if any. */
    selectAll?: string;
    /** Button that starts the restore. */
    restoreButton: string;
    /** FileLu's backdrop and ESC never closed it: only its X does. */
    xOnly?: boolean;
}

/** The generation App read when it opened the manager: every trash command must carry it. */
const GEN = 7;
/** Commands a manager sends that are not about the provider's trash (settings reads). */
const NOT_TRASH = new Set(['get_credential']);
const atGen = <P,>(C: React.ComponentType<P & Props & { generation: number }>, extra?: P) =>
    (props: Props) => <C {...(extra as P)} generation={GEN} {...props} />;
const bulk = { selectAll: 'contextMenu.trashSelectAll', restoreButton: 'contextMenu.restoreFromTrash' };

// InternxtTrashManager is read-only (no restore or delete call), so it has
// nothing in flight to guard and is not listed here.
const CASES: Case[] = [
    { name: 'Azure', Component: atGen(AzureTrashManager), restore: 'azure_undelete_blob', ...bulk },
    { name: 'Box', Component: atGen(BoxTrashManager), restore: 'box_restore_from_trash', ...bulk },
    { name: 'Dropbox', Component: atGen(DropboxTrashManager, { currentPath: '/' }), restore: 'dropbox_restore_from_trash', ...bulk },
    { name: 'FileLu', Component: atGen(FileLuTrashManager), restore: 'filelu_restore_file', selectAll: 'common.selectAll', restoreButton: 'filelu.restoreSelected', xOnly: true },
    { name: 'Google Drive', Component: atGen(GoogleDriveTrashManager), restore: 'google_drive_restore_from_trash', ...bulk },
    { name: 'Jottacloud', Component: atGen(JottacloudTrashManager), restore: 'jottacloud_restore_from_trash', ...bulk },
    { name: 'kDrive', Component: atGen(KDriveTrashManager), restore: 'kdrive_restore_from_trash', ...bulk },
    { name: 'Koofr', Component: atGen(KoofrTrashManager), restore: 'koofr_restore_trash', ...bulk },
    { name: 'MEGA', Component: atGen(MegaTrashManager), restore: 'mega_restore_from_trash', ...bulk },
    { name: 'Nextcloud', Component: NextcloudTrashManager, restore: 'webdav_restore_trash', ...bulk },
    { name: 'OneDrive', Component: atGen(OneDriveTrashManager), restore: 'onedrive_restore_from_trash', ...bulk },
    { name: 'OpenDrive', Component: atGen(OpenDriveTrashManager), restore: 'opendrive_restore_from_trash', ...bulk },
    { name: 'pCloud', Component: atGen(PCloudTrashManager), restore: 'pcloud_restore_from_trash', ...bulk },
    { name: 'S3', Component: atGen(S3TrashManager), restore: 's3_restore_from_trash', restoreButton: 'contextMenu.s3TrashUndelete' },
    { name: 'Yandex', Component: atGen(YandexTrashManager), restore: 'yandex_restore_from_trash', ...bulk },
    { name: 'Zoho', Component: atGen(ZohoTrashManager), restore: 'zoho_restore_from_trash', ...bulk },
    { name: 'B2 hidden files', Component: atGen(B2HiddenManager, { currentPath: '/' }), restore: 'b2_restore_hidden', ...bulk },
];

/**
 * Discussion #1527, class A: a stray click on a trash manager's backdrop
 * closed it while a restore or a delete was running. The request goes on at
 * the provider either way, so the backdrop is now inert while it runs, and
 * the X (or ESC) asks first, saying the operation keeps running.
 */
describe.each(CASES)('the $name trash manager', ({ name, Component, restore, selectAll, restoreButton, xOnly }) => {
    let host: HTMLDivElement;
    let root: Root;
    let release: (() => void) | null;
    beforeEach(() => {
        host = document.createElement('div');
        document.body.appendChild(host);
        root = createRoot(host);
        release = null;
        tauri.invoke.mockImplementation((cmd: string) => {
            if (cmd === restore) {
                return new Promise(resolve => {
                    release = () => resolve({ files_restored: 1, files_already_present: 0, dirs_restored: 0, failed: [] });
                });
            }
            if (cmd === 'dropbox_account_type') return Promise.resolve('basic');
            return Promise.resolve(cmd.includes('list') ? [ENTRY] : undefined);
        });
    });
    afterEach(async () => {
        await act(async () => { release?.(); });
        act(() => root.unmount());
        host.remove();
        vi.clearAllMocks();
    });

    const flush = () => act(async () => { await Promise.resolve(); await Promise.resolve(); });
    const mount = async (onClose: () => void) => {
        act(() => root.render(<Component onClose={onClose} />));
        await flush();
    };
    const backdrop = () => host.firstElementChild as HTMLElement;
    const closeX = () => {
        const header = backdrop().firstElementChild!.firstElementChild as HTMLElement;
        const buttons = header.querySelectorAll('button');
        return buttons[buttons.length - 1] as HTMLButtonElement;
    };
    const button = (label: string) => [...document.querySelectorAll('button')].find(b => b.textContent?.includes(label)) as HTMLButtonElement | undefined;
    const click = (el: HTMLElement) => act(() => { el.dispatchEvent(new MouseEvent('click', { bubbles: true })); });
    const escape = () => act(() => { window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true })); });
    const startRestore = async () => {
        if (selectAll) click(button(selectAll)!);
        click(button(restoreButton)!);
        await flush();
        expect(tauri.invoke).toHaveBeenCalledWith(restore, expect.anything());
    };

    it('ignores a backdrop click while a restore runs, and the X asks before closing', async () => {
        const onClose = vi.fn();
        await mount(onClose);
        await startRestore();
        const callsBefore = tauri.invoke.mock.calls.length;

        click(backdrop());
        expect(onClose).not.toHaveBeenCalled();
        expect(button('modalGuard.closeAnyway')).toBeUndefined();
        expect(tauri.invoke.mock.calls.length).toBe(callsBefore);

        // ESC asks like the X, and a second ESC answers the confirm "wait".
        escape();
        expect(onClose).not.toHaveBeenCalled();
        expect(button('modalGuard.closeAnyway') !== undefined).toBe(!xOnly);
        escape();
        expect(button('modalGuard.closeAnyway')).toBeUndefined();
        expect(onClose).not.toHaveBeenCalled();

        click(closeX());
        expect(onClose).not.toHaveBeenCalled();
        expect(document.body.textContent).toContain('modalGuard.busyBodyKeepsRunning');
        click(button('modalGuard.closeAnyway')!);
        expect(onClose).toHaveBeenCalledTimes(1);
        expect(tauri.invoke.mock.calls.length).toBe(callsBefore);
    });

    it.skipIf(name === 'Nextcloud')('sends the generation it was opened at with every command', async () => {
        await mount(vi.fn());
        await startRestore();
        const sent = tauri.invoke.mock.calls;
        expect(sent.length).toBeGreaterThanOrEqual(2);
        for (const [cmd, args] of sent) {
            if (NOT_TRASH.has(String(cmd))) continue;
            expect((args as { generation?: number } | undefined)?.generation, String(cmd)).toBe(GEN);
        }
    });

    it('closes at once from the X, ESC and the backdrop (where they always did) when idle', async () => {
        const onClose = vi.fn();
        await mount(onClose);
        click(backdrop());
        expect(onClose).toHaveBeenCalledTimes(xOnly ? 0 : 1);
        onClose.mockClear();
        escape();
        expect(onClose).toHaveBeenCalledTimes(xOnly ? 0 : 1);
        onClose.mockClear();
        click(closeX());
        expect(onClose).toHaveBeenCalledTimes(1);
        expect(button('modalGuard.closeAnyway')).toBeUndefined();
    });
});

interface ConfirmCase {
    name: string;
    Component: React.ComponentType<Props>;
    /** Button that selects the rows first, when the confirm needs a selection. */
    selectAll?: string;
    /** Button (by its text) that opens the manager's own confirmation. */
    opens: string;
}
const del = { selectAll: 'contextMenu.trashSelectAll', opens: 'contextMenu.permanentDelete' };
const empty = { opens: 'contextMenu.emptyTrash' };
const CONFIRM_CASES: ConfirmCase[] = [
    { name: 'Box', Component: atGen(BoxTrashManager), ...del },
    { name: 'Dropbox', Component: atGen(DropboxTrashManager, { currentPath: '/' }), ...del },
    { name: 'Google Drive', Component: atGen(GoogleDriveTrashManager), ...del },
    { name: 'Jottacloud', Component: atGen(JottacloudTrashManager), ...del },
    { name: 'kDrive', Component: atGen(KDriveTrashManager), ...empty },
    { name: 'Koofr', Component: atGen(KoofrTrashManager), ...empty },
    { name: 'MEGA', Component: atGen(MegaTrashManager), ...del },
    { name: 'Nextcloud', Component: NextcloudTrashManager, ...empty },
    { name: 'OneDrive', Component: atGen(OneDriveTrashManager), ...del },
    { name: 'OpenDrive', Component: atGen(OpenDriveTrashManager), ...empty },
    { name: 'pCloud', Component: atGen(PCloudTrashManager), ...empty },
    { name: 'S3', Component: atGen(S3TrashManager), opens: 'contextMenu.s3TrashPurge' },
    { name: 'Yandex', Component: atGen(YandexTrashManager), ...empty },
    { name: 'Zoho', Component: atGen(ZohoTrashManager), ...del },
    { name: 'B2 hidden files', Component: atGen(B2HiddenManager, { currentPath: '/' }), ...del },
];

/**
 * Discussion #1527: with a delete or empty confirmation open over a trash
 * manager, one ESC closed the whole manager. ESC now dismisses the
 * innermost surface: the confirmation first, the manager on the next press.
 */
describe.each(CONFIRM_CASES)('ESC in the $name trash manager', ({ Component, selectAll, opens }) => {
    let host: HTMLDivElement;
    let root: Root;
    beforeEach(() => {
        host = document.createElement('div');
        document.body.appendChild(host);
        root = createRoot(host);
        tauri.invoke.mockImplementation((cmd: string) => {
            // Dropbox offers a permanent delete only on a business account.
            if (cmd === 'dropbox_account_type') return Promise.resolve('business');
            return Promise.resolve(cmd.includes('list') ? [ENTRY] : undefined);
        });
    });
    afterEach(() => {
        act(() => root.unmount());
        host.remove();
        vi.clearAllMocks();
    });

    const flush = () => act(async () => { await Promise.resolve(); await Promise.resolve(); });
    const button = (label: string) => [...host.querySelectorAll('button')].find(b =>
        !b.disabled && [...b.childNodes].some(n => n.nodeType === Node.TEXT_NODE && n.textContent?.trim() === label)) as HTMLButtonElement | undefined;
    const dialogs = () => host.querySelectorAll('[role="dialog"], [role="alertdialog"]').length;
    const escape = () => act(() => { window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true })); });

    it('closes the open confirmation first, and the manager on the next press', async () => {
        const onClose = vi.fn();
        act(() => root.render(<Component onClose={onClose} />));
        await flush();
        const before = dialogs();
        if (selectAll) act(() => { button(selectAll)!.click(); });
        act(() => { button(opens)!.click(); });
        await flush();
        expect(dialogs()).toBe(before + 1);
        escape();
        expect(dialogs()).toBe(before);
        expect(onClose).not.toHaveBeenCalled();
        escape();
        expect(onClose).toHaveBeenCalledTimes(1);
    });
});
