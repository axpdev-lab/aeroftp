// @vitest-environment jsdom
import { act, createElement } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { McpServersPanel } from './McpServersPanel';

const invoke = vi.hoisted(() => vi.fn());
const open = vi.hoisted(() => vi.fn());
vi.mock('../../utils/pickPath', () => ({ pickFile: open }));
vi.mock('@tauri-apps/api/core', () => ({ invoke }));
vi.mock('./McpHttpServersPanel', () => ({ McpHttpServersPanel: () => null }));
vi.mock('../../i18n', async () => {
    const { translations } = (await import('../../i18n/locales/en.json')).default as { translations: Record<string, unknown> };
    const t = (key: string, params?: Record<string, string | number>) => {
        const value = key.split('.').reduce<unknown>((node, part) => (node as Record<string, unknown> | undefined)?.[part], translations);
        return typeof value === 'string' ? value.replace(/\{(\w+)\}/g, (_, name: string) => String(params?.[name] ?? name)) : key;
    };
    return { useTranslation: () => t };
});

let root: Root;
let host: HTMLDivElement;
const saved = { id: 'fixture', command: '/usr/bin/node', args: ['saved'], env: {}, enabled: false, revision: 1 };
const render = async () => { await act(async () => root.render(createElement(McpServersPanel))); };
const click = async (element: Element) => { await act(async () => (element as HTMLElement).click()); };
const input = async (element: HTMLInputElement | HTMLTextAreaElement, value: string) => {
    await act(async () => {
        const prototype = element instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
        Object.getOwnPropertyDescriptor(prototype, 'value')!.set!.call(element, value);
        element.dispatchEvent(new Event('input', { bubbles: true }));
    });
};
beforeEach(() => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    invoke.mockReset(); open.mockReset();
    host = document.createElement('div'); document.body.append(host); root = createRoot(host);
});
afterEach(async () => { await act(async () => root.unmount()); host.remove(); });

describe('MCP settings drafts and refresh ordering', () => {
    it('preserves drafts across toggle and environment refresh but adopts changed saved fields independently', async () => {
        let current = structuredClone(saved);
        invoke.mockImplementation(async (command: string) => command === 'mcp_client_list_servers' ? [structuredClone(current)] : undefined);
        await render();
        // The first executable input belongs to the Add form; select the card's input by its saved value.
        const cardCommand = Array.from(host.querySelectorAll('input')).find(e => e.value === saved.command)!;
        const args = host.querySelector('textarea')!;
        await input(cardCommand, '/draft/node'); await input(args, 'draft-arg');
        current = { ...current, enabled: true, revision: 2 };
        await click(host.querySelector('input[type=checkbox]')!);
        expect(cardCommand.isConnected).toBe(true);
        expect(cardCommand.value).toBe('/draft/node'); expect(args.value).toBe('draft-arg');
        current = { ...current, env: { TOKEN: { vault_account: 'fixture-secret' } }, revision: 3 } as typeof current;
        await click(host.querySelector('[aria-label="Refresh MCP servers"]')!);
        expect(cardCommand.value).toBe('/draft/node'); expect(args.value).toBe('draft-arg');
        current = { ...current, command: '/saved/new', revision: 4 };
        await click(host.querySelector('[aria-label="Refresh MCP servers"]')!);
        expect(cardCommand.value).toBe('/saved/new'); expect(args.value).toBe('draft-arg');
        current = { ...current, args: ['new-arg'], revision: 5 };
        await click(host.querySelector('[aria-label="Refresh MCP servers"]')!);
        expect(args.value).toBe('new-arg');
    });

    it('ignores an older initial response after a newer refresh completes', async () => {
        let initial!: (value: unknown) => void;
        const lists = [() => new Promise(resolve => { initial = resolve; }), async () => [{ ...saved, id: 'newest' }]];
        invoke.mockImplementation((command: string) => command === 'mcp_client_list_servers' ? lists.shift()!() : Promise.resolve([]));
        await render();
        await click(host.querySelector('[aria-label="Refresh MCP servers"]')!);
        expect(host.textContent).toContain('newest');
        await act(async () => initial([{ ...saved, id: 'obsolete' }]));
        expect(host.textContent).toContain('newest'); expect(host.textContent).not.toContain('obsolete');
    });
    it('refreshes the new revision after a secret write fails and retries without another catalog write', async () => {
        let current = structuredClone(saved);
        let writes = 0;
        invoke.mockImplementation(async (command: string, args: Record<string, unknown>) => {
            if (command === 'mcp_client_list_servers') return [structuredClone(current)];
            if (command === 'mcp_client_upsert_server') { current = args.config as typeof current; return; }
            if (command === 'mcp_client_set_secret' && ++writes === 1) throw new Error('vault unavailable');
        });
        await render();
        const name = host.querySelector('[aria-label="Environment variable name"]') as HTMLInputElement;
        const secret = host.querySelector('[aria-label="Secret value"]') as HTMLInputElement;
        const save = Array.from(host.querySelectorAll('button')).find(e => e.textContent === 'Save secret')!;
        await input(name, 'TOKEN'); await input(secret, 'first'); await click(save);
        expect(host.textContent).toContain('vault unavailable');
        expect(secret.value).toBe(''); expect(current.revision).toBe(2);
        await input(secret, 'retry'); await click(save);
        expect(writes).toBe(2);
        expect(invoke.mock.calls.filter(([command]) => command === 'mcp_client_upsert_server')).toHaveLength(1);
        expect(secret.value).toBe('');
    });

    it('preserves the action error and clears busy when its refresh also fails', async () => {
        let lists = 0;
        invoke.mockImplementation(async (command: string) => {
            if (command === 'mcp_client_list_servers') {
                if (++lists > 1) throw new Error('refresh failed');
                return [saved];
            }
            throw new Error('write failed');
        });
        await render();
        const toggle = host.querySelector('input[type=checkbox]') as HTMLInputElement;
        await click(toggle);
        expect(host.textContent).toContain('write failed');
        expect(toggle.disabled).toBe(false);
    });

});

describe('MCP live health', () => {
    const snapshot = (overrides: Record<string, unknown>) => ({
        id: 'fixture', transport: 'stdio', enabled: true, revision: 'r'.repeat(64), health: 'ready', errorCode: null,
        tools: [{ name: 'echo', description: 'Echo text', inputSchema: {}, schemaRevision: 'a'.repeat(64) }], unsupportedTools: 2,
        ...overrides,
    });
    const respond = (snapshots: unknown[]) => invoke.mockImplementation(async (command: string) => {
        if (command === 'mcp_client_list_servers') return [{ ...saved, enabled: true }];
        if (command === 'mcp_client_tool_snapshots') return snapshots;
        return undefined;
    });

    it('shows ready tools, unsupported schemas and a localized failure, never a raw code', async () => {
        respond([snapshot({})]);
        await render();
        expect(host.textContent).toContain('Ready, tools available to AeroAgent: 1');
        expect(host.textContent).toContain('Tools with unsupported schemas: 2');
        expect(host.textContent).toContain('echo');
        expect(invoke).toHaveBeenCalledWith('mcp_client_tool_snapshots', { refresh: false });
        respond([snapshot({ health: 'error', errorCode: 'MCP_STDIO_SANDBOX_UNAVAILABLE', tools: [], unsupportedTools: 0 })]);
        await click(Array.from(host.querySelectorAll('button')).find(b => b.textContent?.trim() === 'Check now')!);
        expect(invoke).toHaveBeenCalledWith('mcp_client_tool_snapshots', { refresh: true });
        expect(host.textContent).toContain('Unavailable: Local MCP servers cannot run on this system yet.');
        expect(host.textContent).not.toContain('MCP_STDIO_SANDBOX_UNAVAILABLE');
        expect(host.textContent).not.toContain('Ready, tools');
    });

    it('shows a failed check as a localized alert instead of blank health', async () => {
        respond([snapshot({})]);
        await render();
        expect(host.textContent).toContain('Ready, tools available to AeroAgent: 1');
        invoke.mockImplementation(async (command: string) => {
            if (command === 'mcp_client_list_servers') return [{ ...saved, enabled: true }];
            if (command === 'mcp_client_tool_snapshots') throw 'MCP_STORE_UNAVAILABLE';
            return undefined;
        });
        await click(Array.from(host.querySelectorAll('button')).find(b => b.textContent?.trim() === 'Check now')!);
        expect(Array.from(host.querySelectorAll('[role=alert]')).map(a => a.textContent)).toContain('The encrypted settings store is unavailable.');
        expect(host.textContent).not.toContain('MCP_STORE_UNAVAILABLE');
        respond([snapshot({})]);
        await click(Array.from(host.querySelectorAll('button')).find(b => b.textContent?.trim() === 'Check now')!);
        expect(host.querySelectorAll('[role=alert]')).toHaveLength(0);
    });

    it('reports a disabled server as not started and rechecks after a change', async () => {
        respond([snapshot({ enabled: false, health: 'disabled', tools: [], unsupportedTools: 0, revision: '' })]);
        await render();
        expect(host.textContent).toContain('Not started while disabled');
        const before = invoke.mock.calls.filter(([command]) => command === 'mcp_client_tool_snapshots').length;
        await click(host.querySelector('input[type=checkbox]')!);
        expect(invoke.mock.calls.filter(([command]) => command === 'mcp_client_tool_snapshots').length).toBeGreaterThan(before);
    });
});


describe('MCP directory permission consent', () => {
    it('waits for explicit confirmation, preserves sandbox on ordinary edits, and revokes with its revision', async () => {
        const sandbox = { directories: [{ path: '/project/old', device: 1, inode: 2 }], network_consent: false, managed: null };
        const current = { ...saved, sandbox };
        invoke.mockImplementation(async (command: string) => command === 'mcp_client_list_servers' ? [current] : undefined);
        open.mockResolvedValue('/project/new');
        await render();
        const button = (text: string) => Array.from(document.querySelectorAll('button')).find(b => b.textContent?.trim() === text)!;
        await click(button('Save server'));
        expect(invoke).toHaveBeenCalledWith('mcp_client_upsert_server', { config: { ...current, revision: 2 } });
        await click(button('Choose directory'));
        expect(document.body.textContent).toContain('every file in /project/new');
        expect(invoke.mock.calls.filter(([c]) => c === 'mcp_client_set_permissions')).toHaveLength(0);
        await click(button('Cancel'));
        expect(invoke.mock.calls.filter(([c]) => c === 'mcp_client_set_permissions')).toHaveLength(0);
        await click(button('Choose directory')); await click(button('Confirm'));
        expect(invoke).toHaveBeenCalledWith('mcp_client_set_permissions', { serverId: 'fixture', expectedRevision: 1, directoryPaths: ['/project/old', '/project/new'], networkConsent: false, grantPath: '/project/new' });
        await click(button('Renew access')); await click(button('Confirm'));
        expect(invoke).toHaveBeenCalledWith('mcp_client_set_permissions', { serverId: 'fixture', expectedRevision: 1, directoryPaths: ['/project/old'], networkConsent: false, grantPath: '/project/old' });
        await click(button('Revoke access')); await click(button('Confirm'));
        expect(invoke).toHaveBeenCalledWith('mcp_client_set_permissions', { serverId: 'fixture', expectedRevision: 1, directoryPaths: [], networkConsent: false, grantPath: null });
    });

    it('uses the revision at confirmation creation and locks managed launch fields', async () => {
        const managed = { manifest_id: 'fixture', version: '1.0.0', archive_sha256: 'a'.repeat(64), tree_sha256: 'b'.repeat(64), network_declared: true };
        let current = { ...saved, sandbox: { directories: [{ path: '/managed/fixture', device: 1, inode: 2 }], network_consent: false, managed } };
        invoke.mockImplementation(async (command: string) => command === 'mcp_client_list_servers' ? [current] : undefined);
        await render();
        expect(host.textContent).toContain('Managed installation: 1.0.0');
        expect((host.querySelector('textarea') as HTMLTextAreaElement).disabled).toBe(true);
        expect(Array.from(host.querySelectorAll('button')).find(b => b.textContent === 'Choose directory')).toBeUndefined();
        const network = Array.from(host.querySelectorAll('input[type=checkbox]'))[1];
        await click(network);
        expect(document.body.textContent).toContain('including local services');
        current = { ...current, revision: 3 };
        await click(host.querySelector('[aria-label="Refresh MCP servers"]')!);
        const confirm = Array.from(document.querySelectorAll('button')).find(b => b.textContent === 'Confirm')!;
        await click(confirm);
        expect(invoke).toHaveBeenCalledWith('mcp_client_set_permissions', { serverId: 'fixture', expectedRevision: 1, directoryPaths: ['/managed/fixture'], networkConsent: true, grantPath: null });
    });
});
