// @vitest-environment jsdom
import { act, createElement } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { McpRecommendedServers } from './McpRecommendedServers';
import { MCP_SERVERS_CHANGED, notifyMcpServersChanged } from '../DevTools/aiChatMcp';
const invoke = vi.hoisted(() => vi.fn());
vi.mock('@tauri-apps/api/core', () => ({ invoke }));
vi.mock('../../i18n', async () => {
    const { translations } = (await import('../../i18n/locales/en.json')).default as { translations: Record<string, unknown> };
    const t = (key: string) => key.split('.').reduce<unknown>((v, part) => (v as Record<string, unknown>)[part], translations) as string;
    return { useTranslation: () => t };
});
let root: Root;
let host: HTMLDivElement;
const DEEPWIKI = { id: 'deepwiki', transport: 'http', publisher: 'DeepWiki', homepage: 'https://deepwiki.com', endpoint: 'https://mcp.deepwiki.com/mcp' };
const backend = (presets: unknown[], servers: { id: string; enabled: boolean }[]) => {
    invoke.mockImplementation(async (command: string) => {
        if (command === 'mcp_client_presets_list') return presets;
        if (command === 'mcp_client_http_list_servers') return servers;
        return undefined;
    });
};
const render = async () => { await act(async () => root.render(createElement(McpRecommendedServers))); };
const installButton = () => [...document.querySelectorAll('button')].find(b => b.textContent?.trim() === 'Install');
beforeEach(() => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    invoke.mockReset();
    host = document.createElement('div'); document.body.append(host); root = createRoot(host);
});
afterEach(async () => { await act(async () => root.unmount()); host.remove(); });

it('stays hidden with no presets and never shows an id the app has no card text for', async () => {
    backend([], []);
    await render();
    expect(host.textContent).toBe('');
    await act(async () => root.unmount());
    root = createRoot(host);
    backend([{ ...DEEPWIKI, id: 'evil', publisher: 'EVILPUB' }], []);
    await render();
    expect(host.textContent).toBe('');
});

it('installs DeepWiki by preset id only and tells the other MCP panels', async () => {
    const events: string[] = [];
    const onChange = () => events.push(MCP_SERVERS_CHANGED);
    window.addEventListener(MCP_SERVERS_CHANGED, onChange);
    backend([DEEPWIKI], []);
    await render();
    expect(host.textContent).toContain('Recommended servers');
    expect(host.textContent).toContain('https://mcp.deepwiki.com/mcp');
    await act(async () => installButton()!.click());
    const call = invoke.mock.calls.find(([command]) => command === 'mcp_client_preset_install_http');
    expect(call?.[1]).toEqual({ presetId: 'deepwiki' });
    expect(events).toContain(MCP_SERVERS_CHANGED);
    window.removeEventListener(MCP_SERVERS_CHANGED, onChange);
});

it('shows the installed state instead of an Install button', async () => {
    backend([DEEPWIKI], [{ id: 'deepwiki', enabled: false }]);
    await render();
    expect(host.textContent).toContain('Installed and turned off');
    expect(installButton()).toBeUndefined();
    await act(async () => root.unmount());
    root = createRoot(host);
    backend([DEEPWIKI], [{ id: 'deepwiki', enabled: true }]);
    await render();
    expect(host.textContent).toContain('Ready');
});

it('brings Install back when the server is removed from the HTTP list', async () => {
    backend([DEEPWIKI], [{ id: 'deepwiki', enabled: true }]);
    await render();
    expect(installButton()).toBeUndefined();
    backend([DEEPWIKI], []);
    await act(async () => { notifyMcpServersChanged(); });
    expect(installButton()).toBeDefined();
});

it('keeps the newest list when an older load answers last', async () => {
    let releaseOld!: () => void;
    let lists = 0;
    invoke.mockImplementation(async (command: string) => {
        if (command === 'mcp_client_presets_list') return [DEEPWIKI];
        if (command === 'mcp_client_http_list_servers') {
            lists += 1;
            // The first (older) load answers after the newer one.
            if (lists === 1) { await new Promise<void>(resolve => { releaseOld = resolve; }); return []; }
            return [{ id: 'deepwiki', enabled: true }];
        }
        return undefined;
    });
    await act(async () => { root.render(createElement(McpRecommendedServers)); });
    await act(async () => { notifyMcpServersChanged(); });
    expect(host.textContent).toContain('Ready');
    await act(async () => { releaseOld(); });
    expect(host.textContent).toContain('Ready');
    expect(installButton()).toBeUndefined();
});

it('shows no error from an older load once a newer one has answered', async () => {
    const gates: Array<{ fail: () => void }> = [];
    let lists = 0;
    invoke.mockImplementation(async (command: string) => {
        if (command === 'mcp_client_presets_list') return [DEEPWIKI];
        if (command === 'mcp_client_http_list_servers') {
            lists += 1;
            if (lists === 1) {
                // The older load fails, and only after the newer one answered.
                await new Promise<void>((_, reject) => { gates.push({ fail: () => reject('MCP_STORE_UNAVAILABLE') }); });
            }
            return [{ id: 'deepwiki', enabled: true }];
        }
        return undefined;
    });
    await act(async () => { root.render(createElement(McpRecommendedServers)); });
    await act(async () => { notifyMcpServersChanged(); });
    await act(async () => { gates[0].fail(); });
    expect(host.querySelector('[role="alert"]')).toBeNull();
    expect(host.textContent).toContain('Ready');
});

it('clears an earlier load error once a later load succeeds', async () => {
    let lists = 0;
    invoke.mockImplementation(async (command: string) => {
        if (command === 'mcp_client_presets_list') return [DEEPWIKI];
        if (command === 'mcp_client_http_list_servers') {
            lists += 1;
            if (lists === 1) throw 'MCP_STORE_UNAVAILABLE';
            return [];
        }
        return undefined;
    });
    await render();
    expect(host.querySelector('[role="alert"]')).not.toBeNull();
    await act(async () => { notifyMcpServersChanged(); });
    expect(host.querySelector('[role="alert"]')).toBeNull();
    expect(installButton()).toBeDefined();
});

it('reports a refused install', async () => {
    invoke.mockImplementation(async (command: string) => {
        if (command === 'mcp_client_presets_list') return [DEEPWIKI];
        if (command === 'mcp_client_http_list_servers') return [];
        if (command === 'mcp_client_preset_install_http') throw 'MCP_PRESET_UNKNOWN';
        return undefined;
    });
    await render();
    await act(async () => installButton()!.click());
    expect(host.querySelector('[role="alert"]')?.textContent).toBeTruthy();
});
