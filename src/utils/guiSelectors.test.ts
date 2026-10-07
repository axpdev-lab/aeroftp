// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { act, createElement as h, type ReactNode } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { I18nProvider, translate, type Language } from '../i18n';
import { ServerCard } from '../components/IntroHub/ServerCard';
import { BreadcrumbBar } from '../components/BreadcrumbBar';
import { ConfirmDialog, InputDialog } from '../components/Dialogs';
import { PasswordInput } from '../components/common/PasswordInput';
import { OverwriteDialog } from '../components/OverwriteDialog';
import { LargeIconsGrid } from '../components/LargeIconsGrid';
import { SessionTabs } from '../components/SessionTabs';
import { TransferQueue } from '../components/TransferQueue';
import { TID } from './testIds';
import { guiTarget } from './guiTestTarget';
import type { FtpSession, LocalFile, ServerProfile } from '../types';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn().mockResolvedValue([]) }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn().mockResolvedValue(() => {}) }));

let root: Root;
let host: HTMLDivElement;
beforeEach(() => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    vi.stubGlobal('ResizeObserver', class { observe() {} disconnect() {} });
    host = document.createElement('div'); document.body.append(host); root = createRoot(host);
});
afterEach(async () => {
    await act(async () => root.unmount()); host.remove(); vi.unstubAllGlobals();
});
const render = async (node: ReactNode, language: Language = 'en') => {
    await act(async () => root.render(h(I18nProvider, { initialLanguage: language, children: node })));
};
const click = async (id: string, qualifiers: Parameters<typeof guiTarget>[2] = {}) => {
    await act(async () => guiTarget(host, id, qualifiers).click());
};
const profile: ServerProfile = { id: 'profile-1', name: 'Same name', host: 'test.invalid', port: 22, username: 'tester', protocol: 'sftp' };

for (const language of ['en', 'it', 'ja'] as const) {
    it(`connects the addressed profile through the original callback in ${language}`, async () => {
        const connect = vi.fn();
        const props = { isConnecting: false, credentialsMasked: true, isFavorite: false,
            onConnect: connect, onEdit: vi.fn(), onDuplicate: vi.fn(), onDelete: vi.fn(), onToggleFavorite: vi.fn() };
        await render(h('div', {}, h(ServerCard, { ...props, server: profile }),
            h(ServerCard, { ...props, server: { ...profile, id: 'profile-2' } })), language);
        const qualifiers = { 'data-profile-id': profile.id };
        expect(guiTarget(host, TID.serverCardConnect, qualifiers).getAttribute('aria-label'))
            .toBe(translate('common.connect'));
        expect(() => guiTarget(host, TID.serverCardConnect)).toThrow('GUI target count: 2');
        await click(TID.serverCardConnect, qualifiers);
        expect(connect).toHaveBeenCalledExactlyOnceWith(profile);
    });

    it(`edits the correct breadcrumb among two panels in ${language}`, async () => {
        const navigate = vi.fn();
        const t = (key: string) => translate(key);
        await render(h('div', {}, h(BreadcrumbBar, { panel: 'remote', currentPath: '/test', onNavigate: navigate, t }),
            h(BreadcrumbBar, { panel: 'local2', currentPath: '/local', onNavigate: vi.fn(), t })), language);
        await click(TID.breadcrumbEdit, { 'data-panel': 'remote' });
        const input = guiTarget(host, TID.breadcrumbInput, { 'data-panel': 'remote' }) as HTMLInputElement;
        await act(async () => {
            Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(input, '/test/child');
            input.dispatchEvent(new Event('input', { bubbles: true }));
        });
        await act(async () => input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true })));
        expect(navigate).toHaveBeenCalledExactlyOnceWith('/test/child');
    });

    it(`answers ordinary dialogs and overwrite conflicts in ${language}`, async () => {
        const confirm = vi.fn();
        await render(h(ConfirmDialog, { message: 'Confirm', onConfirm: confirm, onCancel: vi.fn() }), language);
        await click(TID.confirmOk); expect(confirm).toHaveBeenCalledOnce();
        await render(h(InputDialog, { title: 'Folder', defaultValue: 'child', onConfirm: confirm, onCancel: vi.fn() }), language);
        await click(TID.inputOk); expect(confirm).toHaveBeenLastCalledWith('child');
        const decision = vi.fn();
        await render(h(OverwriteDialog, { isOpen: true, source: { name: 'file.txt', size: 5, isRemote: false },
            destination: { name: 'file.txt', size: 5, isRemote: true }, onDecision: decision, onCancel: vi.fn() }), language);
        await click(TID.overwriteSkip); expect(decision).toHaveBeenCalledExactlyOnceWith('skip', false);
    });
}

it('removes public dialog addresses when the same dialog asks for a secret', async () => {
    await render(h(InputDialog, { title: 'Password', defaultValue: '', isPassword: true, onConfirm: vi.fn(), onCancel: vi.fn() }));
    expect(host.querySelector('[data-testid]')).toBeNull();
    expect(host.querySelector('[role="dialog"]')?.getAttribute('data-agent')).toBe('deny');
});

it('marks the shared password field and reveal control without changing human behavior', async () => {
    await render(h(PasswordInput, { value: 'test-secret', onChange: vi.fn() }));
    const button = host.querySelector('button')!;
    expect(button.dataset.agent).toBe('deny');
    expect(host.querySelector('input')?.dataset.agent).toBe('deny');
    expect(host.querySelector('input')?.type).toBe('password');
    await act(async () => button.click());
    expect(host.querySelector('input')?.type).toBe('text');
});

it('refuses denied ancestors, disabled controls and hostile selector characters', () => {
    host.innerHTML = '<div data-agent="deny"><button data-testid="test">secret</button></div>';
    expect(() => guiTarget(host, 'test')).toThrow('GUI target denied');
    host.innerHTML = '<button data-testid="test" disabled>connect</button>';
    expect(() => guiTarget(host, 'test')).toThrow('GUI target disabled');
    host.innerHTML = '<button data-testid="test"></button><button data-testid="test" hidden></button>';
    const button = host.querySelector('button')!;
    button.dataset.profileId = 'id"] [data-agent="deny';
    expect(guiTarget(host, 'test', { 'data-profile-id': button.dataset.profileId })).toBe(button);
});

it('confirms a breadcrumb by click exactly once after the focus-preserving mousedown', async () => {
    const navigate = vi.fn();
    await render(h(BreadcrumbBar, { panel: 'local', currentPath: '/test', onNavigate: navigate, t: translate }));
    await click(TID.breadcrumbEdit, { 'data-panel': 'local' });
    const input = guiTarget(host, TID.breadcrumbInput, { 'data-panel': 'local' }) as HTMLInputElement;
    await act(async () => {
        Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(input, '/next');
        input.dispatchEvent(new Event('input', { bubbles: true }));
    });
    const button = guiTarget(host, TID.breadcrumbConfirm, { 'data-panel': 'local' });
    await act(async () => {
        button.dispatchEvent(new MouseEvent('mousedown', { bubbles: true, cancelable: true }));
        button.click();
    });
    expect(navigate).toHaveBeenCalledExactlyOnceWith('/next');
});

it('addresses same-name large icons by panel and preserves the selection callback', async () => {
    const file: LocalFile = { name: 'sample.txt', path: '/test/sample.txt', size: 8, is_dir: false, modified: null };
    const select = vi.fn();
    const props = { files: [file], selectedFiles: new Set<string>(), currentPath: '/test',
        onFileClick: select, onFileDoubleClick: vi.fn(), onNavigateUp: vi.fn(), isAtRoot: true,
        getFileIcon: () => ({ icon: null, color: '' }), onContextMenu: vi.fn(), dragOverTarget: null,
        inlineRename: null, onInlineRenameChange: vi.fn(), onInlineRenameCommit: vi.fn(),
        onInlineRenameCancel: vi.fn(), formatBytes: String };
    await render(h('div', {}, h(LargeIconsGrid, { ...props, panelKey: 'local2' }),
        h(LargeIconsGrid, { ...props, panelKey: 'remote', isRemote: true })));
    expect(() => guiTarget(host, TID.fileRow, { 'data-file-name': file.name })).toThrow('GUI target count: 2');
    await click(TID.fileRow, { 'data-panel': 'local2', 'data-file-name': file.name });
    expect(select).toHaveBeenCalledOnce();
    expect(select.mock.calls[0][0]).toBe(file);
});

it('refuses a transfer-locked session while keeping the active session address usable', async () => {
    const session: FtpSession = { id: 'first', serverId: 'server', serverName: 'Fixture', status: 'connected',
        remotePath: '/', localPath: '/', remoteFiles: [], localFiles: [], lastActivity: new Date(),
        connectionParams: { server: 'test.invalid', port: 21, username: 'tester', password: '', protocol: 'ftp' } };
    const select = vi.fn();
    await render(h(SessionTabs, { sessions: [session, { ...session, id: 'second' }], activeSessionId: 'first',
        transferLocked: true, onTabClick: select, onTabClose: vi.fn(), onCloseAll: vi.fn(), onNewTab: vi.fn() }));
    expect(() => guiTarget(host, TID.sessionTab, { 'data-session-id': 'second' })).toThrow('GUI target disabled');
    await click(TID.sessionTab, { 'data-session-id': 'first' });
    expect(select).toHaveBeenCalledExactlyOnceWith('first');
});

it('offers one Stop-all address when an expanded queue is paused', async () => {
    const stop = vi.fn();
    await render(h(TransferQueue, { items: [{ id: 'queued', filename: 'sample.txt', path: '/sample.txt',
        size: 8, status: 'pending', type: 'upload' }], isVisible: true, isPaused: true,
        pauseReason: 'Fixture', onStopAll: stop, onToggle: vi.fn() }));
    await click(TID.queueStopAll);
    expect(stop).toHaveBeenCalledOnce();
});
