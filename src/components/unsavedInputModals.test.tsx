// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import React, { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const mocks = vi.hoisted(() => ({
    invoke: vi.fn(async (_command: string, _args?: unknown): Promise<unknown> => []),
    addComment: vi.fn(),
}));
vi.mock('@tauri-apps/api/core', () => ({ invoke: mocks.invoke }));
vi.mock('@tauri-apps/api/event', () => ({ emit: vi.fn(), listen: vi.fn(async () => () => undefined) }));
vi.mock('../i18n', () => ({ useTranslation: () => (key: string) => key }));
vi.mock('../hooks/useDraggableModal', () => ({ useDraggableModal: () => ({ panelProps: {}, dragHandleProps: {} }) }));
vi.mock('../hooks/useTauriListener', () => ({ useTauriListener: () => undefined }));
vi.mock('../hooks/useTraySync', () => ({ useTraySync: () => ({ isRunning: false }) }));
vi.mock('./AeroCloudProtocols', () => ({ AeroCloudMaturityBadge: () => null, useAeroCloudCaveat: () => () => null }));
vi.mock('./SyncScheduler', () => ({ SyncScheduler: () => null }));
vi.mock('./WatcherStatus', () => ({ WatcherStatus: () => null }));
vi.mock('./Sync/SelectiveSyncTree', () => ({ SelectiveSyncTree: () => null }));
vi.mock('./CloudPairsEditor', () => ({ CloudPairsEditor: () => null }));
vi.mock('../utils/boxDriveSocial', async (original) => ({
    ...(await original<typeof import('../utils/boxDriveSocial')>()),
    listComments: vi.fn(async () => []),
    addComment: mocks.addComment,
    deleteComment: vi.fn(async () => undefined),
}));

import { GitHubCommitDialog } from './GitHubCommitDialog';
import { FileCommentsDialog } from './FileCommentsDialog';
import { CloudPanel } from './CloudPanel';

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

/**
 * Discussion #1527, class C: a click outside dropped what was typed (a commit
 * message, a comment, unsaved AeroCloud settings). With typed input the
 * backdrop is inert and the X asks before discarding; untouched, the
 * backdrop still closes.
 */
let host: HTMLDivElement;
let root: Root;
beforeEach(() => {
    host = document.createElement('div');
    document.body.appendChild(host);
    root = createRoot(host);
});
afterEach(() => {
    act(() => root.unmount());
    host.remove();
    vi.clearAllMocks();
});

const render = (node: React.ReactNode) => act(async () => { root.render(<>{node}</>); });
const click = (el: Element) => act(async () => { el.dispatchEvent(new MouseEvent('click', { bubbles: true })); });
const button = (label: string) => [...host.querySelectorAll('button')].find(b =>
    b.textContent?.trim() === label || b.getAttribute('aria-label') === label || b.getAttribute('title') === label);
const xButton = () => [...host.querySelectorAll('button')].find(b => b.querySelector('svg.lucide-x'))!;
const confirmOpen = () => host.querySelector('[role="alertdialog"]') !== null;
const typeInto = (el: HTMLInputElement | HTMLTextAreaElement, value: string) => act(async () => {
    const proto = el instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
    Object.getOwnPropertyDescriptor(proto, 'value')!.set!.call(el, value);
    el.dispatchEvent(new Event('input', { bubbles: true }));
});
const separateBackdrop = () => host.querySelector('.absolute.inset-0') as HTMLElement;

describe('GitHubCommitDialog (Discussion #1527)', () => {
    const open = (onCancel: () => void) => render(
        <GitHubCommitDialog isOpen files={[{ local: '/w/a.txt', remote: 'docs/a.txt' }]} operation="upload"
            branch="main" writeMode="direct" onCommit={vi.fn()} onCancel={onCancel} />,
    );

    it('closes from the backdrop while the generated message is untouched', async () => {
        const onCancel = vi.fn();
        await open(onCancel);
        await click(separateBackdrop());
        expect(onCancel).toHaveBeenCalledTimes(1);
    });

    it('keeps an edited message from the backdrop, and the X asks before discarding it', async () => {
        const onCancel = vi.fn();
        await open(onCancel);
        await typeInto(host.querySelector('input[type="text"], textarea') as HTMLInputElement, 'sentinel commit message');
        await click(separateBackdrop());
        expect(onCancel).not.toHaveBeenCalled();
        await click(xButton());
        expect(onCancel).not.toHaveBeenCalled();
        expect(confirmOpen()).toBe(true);
        await click(button('modalGuard.discardAndClose')!);
        expect(onCancel).toHaveBeenCalledTimes(1);
    });
});

describe('FileCommentsDialog (Discussion #1527)', () => {
    const open = (onClose: () => void) => render(
        <FileCommentsDialog provider="box" filePath="/docs/a.txt" fileName="a.txt" onClose={onClose} />,
    );

    it('closes from the backdrop with nothing typed', async () => {
        const onClose = vi.fn();
        await open(onClose);
        await click(separateBackdrop());
        expect(onClose).toHaveBeenCalledTimes(1);
    });

    it('keeps a comment being typed from the backdrop, and Close asks before discarding it', async () => {
        const onClose = vi.fn();
        await open(onClose);
        await typeInto(host.querySelector('textarea')!, 'sentinel comment');
        await click(separateBackdrop());
        expect(onClose).not.toHaveBeenCalled();
        await click(button('common.close')!);
        expect(confirmOpen()).toBe(true);
        await click(button('modalGuard.keepEditing')!);
        expect(confirmOpen()).toBe(false);
        expect(onClose).not.toHaveBeenCalled();
    });

    it('keeps a comment being sent from the backdrop', async () => {
        const onClose = vi.fn();
        mocks.addComment.mockImplementation(() => new Promise(() => undefined));
        await open(onClose);
        await typeInto(host.querySelector('textarea')!, 'sentinel comment');
        await click(button('box.addComment')!);
        await typeInto(host.querySelector('textarea')!, '');
        await click(separateBackdrop());
        expect(onClose).not.toHaveBeenCalled();
        await click(xButton());
        expect(button('modalGuard.closeAnyway')).toBeTruthy();
    });
});

describe('CloudPanel settings (Discussion #1527)', () => {
    const open = async (onClose: () => void) => {
        mocks.invoke.mockImplementation(async (command: string) => {
            if (command === 'get_cloud_config') return { enabled: true, cloud_name: 'sentinel-cloud', sync_direction: 'local_to_remote', local_folder: '/local', remote_folder: '/remote', exclude_patterns: [], excluded_folders: [] };
            if (command === 'get_cloud_status') return { type: 'idle' };
            if (command === 'get_cloud_pairs_config') return { pairs: [] };
            return [];
        });
        await render(<CloudPanel isOpen onClose={onClose} />);
        await click(host.querySelector('button[title="common.settings"]')!);
    };
    const settingsBackdrop = () => host.querySelector('[aria-label="cloud.dialogSettingsLabel"]') as HTMLElement;

    it('closes the panel from the backdrop while the settings are untouched', async () => {
        const onClose = vi.fn();
        await open(onClose);
        await click(settingsBackdrop());
        expect(onClose).toHaveBeenCalledTimes(1);
    });

    it('keeps edited settings from a backdrop click', async () => {
        const onClose = vi.fn();
        await open(onClose);
        const name = [...host.querySelectorAll('input[type="text"]')].find(i => (i as HTMLInputElement).value === 'sentinel-cloud') as HTMLInputElement;
        await typeInto(name, 'sentinel-renamed');
        await click(settingsBackdrop());
        expect(onClose).not.toHaveBeenCalled();
        expect(settingsBackdrop()).not.toBeNull();
    });
});
