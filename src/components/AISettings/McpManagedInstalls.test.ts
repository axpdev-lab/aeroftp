// @vitest-environment jsdom
import { act, createElement } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { McpManagedInstalls } from './McpManagedInstalls';
const invoke = vi.hoisted(() => vi.fn());
vi.mock('@tauri-apps/api/core', () => ({ invoke }));
vi.mock('../../i18n', async () => {
    const { translations } = (await import('../../i18n/locales/en.json')).default as { translations: Record<string, unknown> };
    const t = (key: string, args: Record<string, string> = {}) => {
        const value = key.split('.').reduce<unknown>((v, part) => (v as Record<string, unknown>)[part], translations) as string;
        return Object.entries(args).reduce((text, [name, replacement]) => text.replace(`{${name}}`, replacement), value);
    };
    return { useTranslation: () => t };
});
let root: Root;
let host: HTMLDivElement;
const refresh = vi.fn(async () => {});
const render = async () => { await act(async () => root.render(createElement(McpManagedInstalls, { installedIds: [], refresh }))); };
const click = async (label: string) => {
    const button = [...document.querySelectorAll('button')].find(b => b.textContent?.trim() === label);
    expect(button).toBeDefined(); await act(async () => button!.click());
};
beforeEach(() => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    invoke.mockReset(); refresh.mockClear();
    host = document.createElement('div'); document.body.append(host); root = createRoot(host);
});
afterEach(async () => { await act(async () => root.unmount()); host.remove(); });
it('keeps an empty reviewed registry hidden', async () => {
    invoke.mockResolvedValue([]); await render();
    expect(invoke).toHaveBeenCalledWith('mcp_client_install_manifests');
    expect(host.textContent).toBe('');
});
it('confirms declared network before installing and cancels only the active operation', async () => {
    let rejectInstall!: (error: string) => void;
    invoke.mockImplementation((command: string) => {
        if (command === 'mcp_client_install_manifests') return Promise.resolve([{ id: 'reviewed', version: '1.0.0', network: true }]);
        if (command === 'mcp_client_install_server') return new Promise((_, reject) => { rejectInstall = reject; });
        if (command === 'mcp_client_install_cancel') { rejectInstall('MCP_INSTALL_CANCELLED'); return Promise.resolve(); }
        return Promise.resolve();
    });
    await render(); await click('Add server');
    expect(document.body.textContent).toContain('including local services');
    expect(invoke.mock.calls.some(([cmd]) => cmd === 'mcp_client_install_server')).toBe(false);
    await click('Cancel');
    expect(invoke.mock.calls.some(([cmd]) => cmd === 'mcp_client_install_server')).toBe(false);
    await click('Add server'); await click('Confirm');
    const args = invoke.mock.calls.find(([cmd]) => cmd === 'mcp_client_install_server')![1];
    expect(args).toEqual({ manifestId: 'reviewed', version: '1.0.0', expectedRevision: 0, networkConsent: true, operationId: expect.any(String) });
    expect(args.operationId).toMatch(/^[a-f0-9-]{36}$/);
    await click('Cancel');
    expect(invoke).toHaveBeenCalledWith('mcp_client_install_cancel', { operationId: args.operationId });
    expect(refresh).not.toHaveBeenCalled();
});
it('refreshes after an offline artifact install without granting network', async () => {
    invoke.mockImplementation(async (command: string) => command === 'mcp_client_install_manifests' ? [{ id: 'offline', version: '1.0.0', network: false }] : undefined);
    await render(); await click('Add server');
    expect(invoke).toHaveBeenCalledWith('mcp_client_install_server', expect.objectContaining({ manifestId: 'offline', networkConsent: false, expectedRevision: 0 }));
    expect(refresh).toHaveBeenCalledOnce();
});
it('cancels an unfinished installation when its settings panel unmounts', async () => {
    let rejectInstall!: (error: string) => void;
    invoke.mockImplementation((command: string) => {
        if (command === 'mcp_client_install_manifests') return Promise.resolve([{ id: 'offline', version: '1.0.0', network: false }]);
        if (command === 'mcp_client_install_server') return new Promise((_, reject) => { rejectInstall = reject; });
        if (command === 'mcp_client_install_cancel') { rejectInstall('MCP_INSTALL_CANCELLED'); return Promise.resolve(); }
        return Promise.resolve();
    });
    await render(); await click('Add server');
    const args = invoke.mock.calls.find(([cmd]) => cmd === 'mcp_client_install_server')![1];
    await act(async () => root.render(null));
    expect(invoke).toHaveBeenCalledWith('mcp_client_install_cancel', { operationId: args.operationId });
    expect(refresh).not.toHaveBeenCalled();
});
