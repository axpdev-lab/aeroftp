// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
import { act, createElement } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import english from '../i18n/locales/en.json';
import { PluginBrowser } from './AISettings/PluginBrowser';
import { GitHubActionsBrowser } from './GitHubActionsBrowser';
import { FileVersionsDialog } from './FileVersionsDialog';
import { VersionBrowser } from './Sync/VersionBrowser';
import { FileCommentsDialog } from './FileCommentsDialog';
import { BoxCollaboratorsDialog } from './BoxCollaboratorsDialog';
import { SharePermissionsDialog } from './SharePermissionsDialog';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => () => undefined) }));
vi.mock('../i18n', () => ({
    useTranslation: () => (key: string) => key.split('.').reduce<unknown>(
        (value, part) => (value as Record<string, unknown>)?.[part], english.translations,
    ) as string,
}));
vi.mock('../hooks/useDraggableModal', () => ({
    useDraggableModal: () => ({ panelProps: {}, dragHandleProps: {} }),
}));
vi.mock('../../hooks/useDraggableModal', () => ({
    useDraggableModal: () => ({ panelProps: {}, dragHandleProps: {} }),
}));
vi.mock('../hooks/useHumanizedLog', () => ({
    useHumanizedLog: () => ({ logRaw: () => 'log', updateEntry: () => undefined }),
}));
vi.mock('../../hooks/useHumanizedLog', () => ({
    useHumanizedLog: () => ({ logRaw: () => 'log', updateEntry: () => undefined }),
}));
vi.mock('../utils/pickPath', () => ({ pickFile: vi.fn(async () => null), pickSave: vi.fn(async () => null) }));
vi.mock('../../utils/pickPath', () => ({ pickFile: vi.fn(async () => null), pickSave: vi.fn(async () => null) }));

let root: Root;
let host: HTMLDivElement;

const deferred = <T,>() => {
    let resolve!: (v: T) => void;
    let reject!: (e?: unknown) => void;
    const promise = new Promise<T>((res, rej) => { resolve = res; reject = rej; });
    return { promise, resolve, reject };
};

const render = async (el: React.ReactElement) => {
    await act(async () => root.render(el));
};
const click = async (el: Element) => {
    await act(async () => { (el as HTMLElement).click(); });
};
const flush = async () => {
    await act(async () => { await Promise.resolve(); await Promise.resolve(); });
};
const xButton = () => [...host.querySelectorAll<HTMLButtonElement>('button')]
    .find(b => b.innerHTML.includes('lucide-x'));
const closeButton = () => host.querySelector<HTMLButtonElement>('button[aria-label="Close"], button[title="Close"]') ?? xButton();
const alertOpen = () => host.querySelector('[role=alertdialog]') !== null;

beforeEach(() => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    vi.mocked(invoke).mockReset();
    host = document.createElement('div'); document.body.append(host); root = createRoot(host);
});
afterEach(async () => { await act(async () => root.unmount()); host.remove(); });

it('PluginBrowser: the guard stays busy until every install finishes', async () => {
    const entry = { id: 'p1', name: 'P1', version: '1.0', author: 'a', description: 'd',
        category: 'ai-tools', downloads: 0, stars: 0, repo_url: '', manifest_url: '', files: [] };
    const installs: ReturnType<typeof deferred>[] = [];
    vi.mocked(invoke).mockImplementation(async cmd => {
        if (cmd === 'fetch_plugin_registry') return [entry, { ...entry, id: 'p2', name: 'P2' }];
        if (cmd === 'install_plugin_from_registry') { const d = deferred<unknown>(); installs.push(d); return d.promise; }
        return [];
    });
    const onClose = vi.fn();
    await render(createElement(PluginBrowser, { isOpen: true, onClose, installedPluginIds: new Set<string>(), onInstalled: vi.fn() }));
    await flush();
    const installButtons = [...host.querySelectorAll('button')].filter(b => b.textContent?.trim() === 'Install');
    expect(installButtons.length).toBeGreaterThanOrEqual(2);
    await click(installButtons[0]);
    await click(installButtons[1]);
    // First install settles while the second is still pending.
    await act(async () => { installs[0].resolve({}); });
    await click(closeButton()!);
    expect(alertOpen()).toBe(true);
    expect(onClose).not.toHaveBeenCalled();
    await act(async () => { installs[1].resolve({}); });
    await flush();
    await click(closeButton()!);
    expect(onClose).toHaveBeenCalledTimes(1);
});

it('GitHubActionsBrowser: the guard stays busy until every workflow action finishes', async () => {
    const run = (id: number, status: string, conclusion: string) => ({ id, name: `run${id}`, branch: 'main', sha: 'x',
        status, conclusion, event: 'push', run_number: id, display_title: `t${id}`, created_at: '', updated_at: '',
        duration_seconds: 1, html_url: '', actor_login: '', actor_avatar: '' });
    const actions: ReturnType<typeof deferred>[] = [];
    vi.mocked(invoke).mockImplementation(async (cmd) => {
        if (cmd === 'github_list_actions_runs') return [run(1, 'in_progress', ''), run(2, 'completed', 'failure')];
        const d = deferred<unknown>(); actions.push(d); return d.promise;
    });
    const onClose = vi.fn();
    await render(createElement(GitHubActionsBrowser, { isOpen: true, onClose }));
    await flush();
    const byTitle = (frag: string) => [...host.querySelectorAll<HTMLButtonElement>(`button[title*="${frag}"]`)];
    await click(byTitle('Cancel')[0]);
    await click(byTitle('Re-run failed')[0]);
    await act(async () => { actions[0].resolve({}); });
    await flush();
    await click(closeButton()!);
    expect(alertOpen()).toBe(true);
    expect(onClose).not.toHaveBeenCalled();
    await act(async () => { actions[1].resolve({}); });
    await flush();
    await click(closeButton()!);
    expect(onClose).toHaveBeenCalledTimes(1);
});

it('FileVersionsDialog: the guard stays busy until every version operation finishes', async () => {
    const d1 = deferred<unknown>(); const d2 = deferred<unknown>();
    const pending = new Map<string, ReturnType<typeof deferred>>();
    vi.mocked(invoke).mockImplementation(async (cmd, args) => {
        if (cmd === 'provider_list_versions') return [
            { id: 'v1', modified: null, size: 1, modified_by: null },
            { id: 'v2', modified: null, size: 1, modified_by: null },
            { id: 'v3', modified: null, size: 1, modified_by: null },
        ];
        const d = deferred<unknown>(); pending.set(`${cmd}:${(args as { versionId?: string }).versionId}`, d);
        return d.promise;
    });
    const onClose = vi.fn();
    await render(createElement(FileVersionsDialog, { filePath: '/f.txt', fileName: 'f.txt', onClose }));
    await flush();
    const restores = [...host.querySelectorAll<HTMLButtonElement>('button[title*="Restore"], button[title*="restore"]')];
    expect(restores.length).toBe(2);
    await click(restores[0]);
    await click(restores[1]);
    await act(async () => { pending.get('provider_restore_version:v2')!.resolve({}); });
    await flush();
    await click(closeButton()!);
    expect(alertOpen()).toBe(true);
    expect(onClose).not.toHaveBeenCalled();
    await act(async () => { pending.get('provider_restore_version:v3')!.resolve({}); });
    await flush();
    await click(closeButton()!);
    expect(onClose).toHaveBeenCalledTimes(1);
});

it('VersionBrowser: the guard stays busy until every restore finishes', async () => {
    const pending = new Map<string, ReturnType<typeof deferred>>();
    vi.mocked(invoke).mockImplementation(async (cmd, args) => {
        if (cmd === 'list_file_versions') return [
            { archive_path: '/a1', original_relative: '/f.txt', archived_at: '', size: 1 },
            { archive_path: '/a2', original_relative: '/f.txt', archived_at: '', size: 1 },
        ];
        if (cmd === 'versions_disk_usage') return 0;
        const d = deferred<unknown>(); pending.set((args as { archivePath?: string }).archivePath ?? cmd, d);
        return d.promise;
    });
    const onClose = vi.fn();
    await render(createElement(VersionBrowser, { filePath: '/f.txt', onClose }));
    await flush();
    const restores = [...host.querySelectorAll<HTMLButtonElement>('button[title]')].filter(b => b.title && b.title !== '');
    // One restore button per version row.
    const restoreButtons = [...host.querySelectorAll('button')].filter(b => b.closest('tr, [class*=row], li, div'));
    // Fallback: click every button whose title mentions restore (translated or key).
    const candidates = [...host.querySelectorAll<HTMLButtonElement>('button')].filter(b => /restore/i.test(b.title) || /restore/i.test(b.textContent ?? ''));
    expect(candidates.length).toBeGreaterThanOrEqual(2);
    await click(candidates[0]);
    await click(candidates[1]);
    await act(async () => { pending.get('/a1')!.resolve({}); });
    await flush();
    await click(closeButton()!);
    expect(alertOpen()).toBe(true);
    expect(onClose).not.toHaveBeenCalled();
    await act(async () => { pending.get('/a2')!.resolve({}); });
    await flush();
    await click(closeButton()!);
    expect(onClose).toHaveBeenCalledTimes(1);
});

it('FileCommentsDialog: the guard stays busy until every delete finishes', async () => {
    const pending = new Map<string, ReturnType<typeof deferred>>();
    vi.mocked(invoke).mockImplementation(async (cmd, args) => {
        if (cmd === 'box_list_comments') return [
            { id: 'c1', message: 'one', created_at: '', created_by: { name: 'A' } },
            { id: 'c2', message: 'two', created_at: '', created_by: { name: 'B' } },
        ];
        const d = deferred<unknown>(); pending.set(`${cmd}:${(args as { commentId?: string }).commentId}`, d);
        return d.promise;
    });
    const onClose = vi.fn();
    await render(createElement(FileCommentsDialog, { provider: 'box', filePath: '/f.txt', fileName: 'f.txt', onClose }));
    await flush();
    const deletes = [...host.querySelectorAll<HTMLButtonElement>('button[aria-label="Delete"], button[title*="Delete"], button[title*="delete"]')];
    expect(deletes.length).toBe(2);
    await click(deletes[0]);
    await click(deletes[1]);
    await act(async () => { pending.get('box_delete_comment:c1')!.resolve({}); });
    await flush();
    await click(closeButton()!);
    expect(alertOpen()).toBe(true);
    expect(onClose).not.toHaveBeenCalled();
    await act(async () => { pending.get('box_delete_comment:c2')!.resolve({}); });
    await flush();
    await click(closeButton()!);
    expect(onClose).toHaveBeenCalledTimes(1);
});

it('BoxCollaboratorsDialog: the guard stays busy until every remove finishes', async () => {
    const pending = new Map<string, ReturnType<typeof deferred>>();
    vi.mocked(invoke).mockImplementation(async (cmd, args) => {
        if (cmd === 'box_list_collaborations') return [
            { id: 'k1', role: 'viewer', accessible_by: { name: 'A' } },
            { id: 'k2', role: 'editor', accessible_by: { name: 'B' } },
        ];
        const d = deferred<unknown>(); pending.set(`${cmd}:${(args as { collabId?: string }).collabId}`, d);
        return d.promise;
    });
    const onClose = vi.fn();
    await render(createElement(BoxCollaboratorsDialog, { path: '/p', name: 'n', onClose }));
    await flush();
    const removes = [...host.querySelectorAll<HTMLButtonElement>('button[aria-label="Remove"], button[title*="Remove"], button[title*="remove"]')];
    expect(removes.length).toBe(2);
    await click(removes[0]);
    await click(removes[1]);
    await act(async () => { pending.get('box_remove_collaboration:k1')!.resolve({}); });
    await flush();
    await click(closeButton()!);
    expect(alertOpen()).toBe(true);
    expect(onClose).not.toHaveBeenCalled();
    await act(async () => { pending.get('box_remove_collaboration:k2')!.resolve({}); });
    await flush();
    await click(closeButton()!);
    expect(onClose).toHaveBeenCalledTimes(1);
});

it('SharePermissionsDialog: the guard stays busy until every remove finishes', async () => {
    const pending = new Map<string, ReturnType<typeof deferred>>();
    vi.mocked(invoke).mockImplementation(async (cmd, args) => {
        if (cmd === 'provider_list_permissions') return [
            { role: 'reader', target_type: 'user', target: 'u1' },
            { role: 'writer', target_type: 'user', target: 'u2' },
        ];
        const d = deferred<unknown>(); pending.set(`${cmd}:${(args as { target?: string }).target}`, d);
        return d.promise;
    });
    const onClose = vi.fn();
    await render(createElement(SharePermissionsDialog, { filePath: '/f.txt', fileName: 'f.txt', onClose }));
    await flush();
    const removes = [...host.querySelectorAll<HTMLButtonElement>('button[title*="Remove"], button[title*="remove"]')];
    expect(removes.length).toBe(2);
    await click(removes[0]);
    await click(removes[1]);
    await act(async () => { pending.get('provider_remove_permission:u1')!.resolve({}); });
    await flush();
    await click(closeButton()!);
    expect(alertOpen()).toBe(true);
    expect(onClose).not.toHaveBeenCalled();
    await act(async () => { pending.get('provider_remove_permission:u2')!.resolve({}); });
    await flush();
    await click(closeButton()!);
    expect(onClose).toHaveBeenCalledTimes(1);
});
