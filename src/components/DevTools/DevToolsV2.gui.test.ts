// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
import { act, createElement as h, useEffect } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { beforeEach, afterEach, expect, it, vi } from 'vitest';
import { DevToolsV2 } from './DevToolsV2';
import type { ToolsControl } from '../../gui/toolsWorkspace';
const counts = vi.hoisted(() => ({ terminalMount: 0, terminalUnmount: 0 }));
vi.mock('./SSHTerminal', () => ({ SSHTerminal: () => {
    useEffect(() => { counts.terminalMount++; return () => { counts.terminalUnmount++; }; }, []);
    return h('div', { 'data-terminal': true }, 'ARTIFICIAL-TERMINAL-OUTPUT');
} }));
vi.mock('./CodeEditor', () => ({ CodeEditor: () => h('div', {}, 'ARTIFICIAL-EDITOR-CONTENT') }));
vi.mock('./AIChat', () => ({ AIChat: () => h('div', {}, 'ARTIFICIAL-CHAT-CONTENT') }));
vi.mock('../CyberToolsPanel', () => ({ CyberToolsPanel: () => h('div', {}, 'ARTIFICIAL-SECURITY-CONTENT') }));
vi.mock('../../i18n', () => ({ useTranslation: () => (key: string) => key }));
vi.mock('@tauri-apps/plugin-fs', () => ({ readTextFile: vi.fn() }));
let root: Root, host: HTMLDivElement, control: ToolsControl | null;
const metadata = vi.fn();
const onClose = vi.fn();
const register = (next: ToolsControl) => { control = next; return () => { if (control === next) control = null; }; };
const render = (isOpen: boolean) => root.render(h(DevToolsV2, { isOpen, previewFile: null, onClose,
    registerGuiTools: register, onGuiToolsState: metadata }));
beforeEach(async () => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    vi.stubGlobal('ResizeObserver', class { observe() {} disconnect() {} });
    window.innerWidth = 1024; counts.terminalMount = 0; counts.terminalUnmount = 0;
    metadata.mockClear(); onClose.mockClear(); control = null;
    host = document.createElement('div'); document.body.append(host); root = createRoot(host);
    await act(async () => render(false));
});
afterEach(async () => { await act(async () => root.unmount()); host.remove(); vi.unstubAllGlobals(); });
it('registers before the first workspace mount and reports actual committed panels', async () => {
    expect(control).not.toBeNull();
    await act(async () => { control!.ensure('terminal'); render(true); });
    expect(metadata).toHaveBeenLastCalledWith({ open: true, protected: false, visible_panels: ['editor', 'terminal'] });
    expect(host.querySelector('[data-terminal]')).not.toBeNull();
    expect(JSON.stringify(metadata.mock.calls)).not.toMatch(/ARTIFICIAL/);
});
it('ensures another panel without destroying a live terminal; workspace hide/reopen preserves it', async () => {
    await act(async () => { control!.ensure('terminal'); render(true); });
    await act(async () => control!.ensure('agent'));
    expect(metadata).toHaveBeenLastCalledWith({ open: true, protected: false, visible_panels: ['editor', 'terminal', 'agent'] });
    expect(counts).toEqual({ terminalMount: 1, terminalUnmount: 0 });
    await act(async () => { control!.close(); render(false); });
    expect(onClose).toHaveBeenCalledTimes(1);
    expect(metadata).toHaveBeenLastCalledWith({ open: false, protected: false, visible_panels: [] });
    await act(async () => render(true));
    expect(counts).toEqual({ terminalMount: 1, terminalUnmount: 0 });
});
it('rechecks a human-enabled Security panel and refuses agent open/close without clearing it', async () => {
    await act(async () => render(true));
    const security = [...host.querySelectorAll('button')].find(button => button.title === 'cyberTools.title')!;
    expect(security).toBeTruthy();
    await act(async () => security.click());
    expect(metadata.mock.lastCall![0].protected).toBe(true);
    expect(() => control!.ensure('agent')).toThrow(); expect(() => control!.close()).toThrow();
    expect(onClose).not.toHaveBeenCalled();
    expect(host.textContent).toContain('ARTIFICIAL-SECURITY-CONTENT');
});
