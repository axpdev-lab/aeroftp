// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { McpPermissions, type SandboxConfig } from './McpPermissions';
import { McpManagedInstalls } from './McpManagedInstalls';
import { McpRecommendedServers } from './McpRecommendedServers';
import { Plus, RefreshCw, Save, Trash2 } from 'lucide-react';
import { createPortal } from 'react-dom';
import { ConfirmOverlay } from '../common/ConfirmOverlay';
import { MODAL_Z } from '../../utils/modalLayers';
import { useTranslation } from '../../i18n';
import { describeMcpError } from './mcpErrors';
import { McpHttpServersPanel } from './McpHttpServersPanel';
import { McpHealthLine, McpHealthProvider, useMcpHealthState } from './mcpHealth';
import { notifyMcpServersChanged } from '../DevTools/aiChatMcp';

interface SecretRef { vault_account: string }
export interface ServerConfig {
    id: string;
    command: string;
    args: string[];
    env: Record<string, SecretRef>;
    enabled: boolean;
    revision: number;
    sandbox?: SandboxConfig;
}

const idValid = (value: string) => /^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$/.test(value);
const envValid = (value: string) => /^[A-Z_][A-Z0-9_]{0,63}$/.test(value)
    && !['PATH', 'HOME', 'SHELL', 'ENV', 'IFS', 'COMSPEC', 'PATHEXT', 'NODE_OPTIONS', 'NODE_PATH', 'RUSTFLAGS'].includes(value)
    && !/^(LD_|DYLD_|PYTHON)/.test(value);
const accountFor = (serverId: string, envName: string) => `mcp_env_${serverId.length}_${serverId}_${envName}`;

function ServerCard({ server, refresh }: { server: ServerConfig; refresh: () => Promise<void> }) {
    const t = useTranslation();
    const [command, setCommand] = useState(server.command);
    const [args, setArgs] = useState(server.args.join('\n'));
    const [envName, setEnvName] = useState('');
    const [secret, setSecret] = useState('');
    const [busy, setBusy] = useState(false);
    const [error, setError] = useState('');
    const [pendingRemoval, setPendingRemoval] = useState<{ kind: 'server' } | { kind: 'secret'; name: string } | null>(null);

    const storedArgs = server.args.join('\n');
    useEffect(() => { setCommand(server.command); }, [server.command]);
    useEffect(() => { setArgs(storedArgs); }, [storedArgs]);

    const perform = async (action: () => Promise<void>) => {
        setBusy(true); setError('');
        let actionFailed = false;
        try { await action(); }
        catch (cause) { actionFailed = true; setError(describeMcpError(t, cause)); }
        finally {
            try { await refresh(); }
            catch (cause) { if (!actionFailed) setError(describeMcpError(t, cause)); }
            finally { setBusy(false); notifyMcpServersChanged(); }
        }
    };

    const edit = (changes: Partial<ServerConfig>) => invoke<void>('mcp_client_upsert_server', {
        config: { ...server, ...changes, revision: server.revision + 1 },
    });

    const saveSecret = () => {
        if (!envValid(envName) || !secret) { setError(t('ai.mcpClient.invalidSecret')); return; }
        return perform(async () => { try {
            if (!server.env[envName]) {
                await edit({ env: { ...server.env, [envName]: { vault_account: accountFor(server.id, envName) } } });
            }
            await invoke('mcp_client_set_secret', { serverId: server.id, envName, secret });
            setEnvName('');
        } finally { setSecret(''); } });
    };

    return <div className="rounded-lg border border-gray-700 bg-gray-800 p-4 space-y-3">
        <div className="flex items-center justify-between gap-3">
            <div>
                <h3 className="font-medium text-white">{server.id}</h3>
                <McpHealthLine transport="stdio" id={server.id} />
            </div>
            <div className="flex items-center gap-3">
                <label className="flex items-center gap-2 text-sm text-gray-300">
                    <input type="checkbox" checked={server.enabled} disabled={busy}
                        onChange={(event) => perform(() => edit({ enabled: event.target.checked }))} /> {t('ai.mcpClient.enabled')}
                </label>
                <button type="button" disabled={busy} aria-label={t('ai.mcpClient.removeServer', { name: server.id })} className="text-red-400 disabled:opacity-50"
                    onClick={() => setPendingRemoval({ kind: 'server' })}><Trash2 size={16} /></button>
            </div>
        </div>
        <label className="block text-xs text-gray-300">{t('ai.mcpClient.executablePath')}
            <input className="mt-1 w-full rounded bg-gray-900 border border-gray-600 p-2 text-sm" value={command}
                onChange={event => setCommand(event.target.value)} disabled={busy || !!server.sandbox?.managed} />
        </label>
        <label className="block text-xs text-gray-300">{t('ai.mcpClient.arguments')}
            <textarea className="mt-1 w-full rounded bg-gray-900 border border-gray-600 p-2 text-sm" rows={2}
                value={args} onChange={event => setArgs(event.target.value)} disabled={busy || !!server.sandbox?.managed} />
        </label>
        <button type="button" disabled={busy || !!server.sandbox?.managed} className="flex items-center gap-1 rounded bg-purple-700 px-3 py-1.5 text-sm disabled:opacity-50"
            onClick={() => perform(() => edit({ command, args: args.split('\n').filter(Boolean) }))}><Save size={14} /> {t('ai.mcpClient.saveServer')}</button>
        <div className="border-t border-gray-700 pt-3 space-y-2">
            <p className="text-xs text-gray-400">{t('ai.mcpClient.secretsHint')}</p>
            {Object.keys(server.env).map(name => <div key={name} className="flex items-center gap-2 text-xs">
                <span className="font-mono text-gray-300">{name}</span><span className="text-gray-500">{t('ai.mcpClient.savedHidden')}</span>
                <button type="button" disabled={busy} className="text-red-400 disabled:opacity-50" aria-label={t('ai.mcpClient.removeSecret', { name })}
                    onClick={() => setPendingRemoval({ kind: 'secret', name })}><Trash2 size={13} /></button>
            </div>)}
            <div className="flex flex-wrap gap-2">
                <input className="min-w-32 flex-1 rounded bg-gray-900 border border-gray-600 p-2 text-sm" value={envName}
                    placeholder="ENV_NAME" aria-label={t('ai.mcpClient.envName')} disabled={busy}
                    onChange={event => setEnvName(event.target.value)} />
                <input className="min-w-40 flex-1 rounded bg-gray-900 border border-gray-600 p-2 text-sm" value={secret}
                    type="password" autoComplete="new-password" placeholder={t('ai.mcpClient.secretValue')} aria-label={t('ai.mcpClient.secretValue')} disabled={busy}
                    onChange={event => setSecret(event.target.value)} />
                <button type="button" disabled={busy} className="rounded bg-gray-700 px-3 py-2 text-sm disabled:opacity-50"
                    onClick={() => void saveSecret()}>{t('ai.mcpClient.saveSecret')}</button>
            </div>
        </div>
        <McpPermissions server={server} busy={busy} perform={perform} />
        {error && <p role="alert" className="text-xs text-red-400">{error}</p>}
        {pendingRemoval && createPortal(<ConfirmOverlay
            message={pendingRemoval.kind === 'server'
                ? t('ai.mcpClient.confirmRemoveServer', { name: server.id })
                : t('ai.mcpClient.confirmRemoveSecret', { name: pendingRemoval.name })}
            onCancel={() => setPendingRemoval(null)}
            onConfirm={() => {
                const removal = pendingRemoval;
                setPendingRemoval(null);
                if (removal.kind === 'server') {
                    void perform(() => invoke('mcp_client_remove_server', { serverId: server.id }));
                } else {
                    void perform(() => {
                        const env = { ...server.env };
                        delete env[removal.name];
                        return edit({ env });
                    });
                }
            }}
            zClass={MODAL_Z.globalConfirm}
        />, document.body)}
    </div>;
}

export function McpServersPanel() {
    const t = useTranslation();
    const [servers, setServers] = useState<ServerConfig[]>([]);
    const [id, setId] = useState('');
    const [command, setCommand] = useState('');
    const [busy, setBusy] = useState(false);
    const [error, setError] = useState('');
    const health = useMcpHealthState();
    const refreshSequence = useRef(0);
    const refresh = useCallback(async () => {
        const sequence = ++refreshSequence.current;
        const result = await invoke<ServerConfig[]>('mcp_client_list_servers');
        if (sequence === refreshSequence.current) setServers(result);
    }, []);
    useEffect(() => { void refresh().catch(cause => setError(describeMcpError(t, cause))); }, [refresh, t]);

    const add = async () => {
        if (!idValid(id) || !command) { setError(t('ai.mcpClient.invalidStdio')); return; }
        setBusy(true); setError('');
        try {
            await invoke('mcp_client_upsert_server', { config: {
                id, command, args: [], env: {}, enabled: false, revision: 1,
            } satisfies ServerConfig });
            setId(''); setCommand(''); await refresh();
            notifyMcpServersChanged();
        } catch (cause) { setError(describeMcpError(t, cause)); }
        finally { setBusy(false); }
    };

    return <McpHealthProvider value={health}><div className="space-y-4">
        <div className="flex items-center justify-between gap-3 rounded-lg border border-gray-700 bg-gray-900/40 p-3 text-sm text-gray-300">
            <span>{t('ai.mcpClient.routingNotice')}</span>
            <button type="button" disabled={health.checking} onClick={() => void health.check(true)}
                className="flex shrink-0 items-center gap-1 rounded bg-gray-700 px-3 py-1.5 text-sm disabled:opacity-50">
                <RefreshCw size={14} /> {t('ai.mcpClient.checkNow')}</button>
        </div>
        <McpRecommendedServers />
        <McpManagedInstalls installedIds={servers.map(server => server.id)} refresh={refresh} />
        {health.failure && <p role="alert" className="text-xs text-red-400">{describeMcpError(t, health.failure)}</p>}
        <div className="flex items-center justify-between">
            <div><h2 className="font-medium text-white">{t('ai.mcpClient.stdioTitle')}</h2><p className="text-xs text-gray-400">{t('ai.mcpClient.stdioSubtitle')}</p></div>
            <button type="button" onClick={() => void refresh().catch(cause => setError(describeMcpError(t, cause)))} aria-label={t('ai.mcpClient.refresh')}
                className="text-gray-400 hover:text-white"><RefreshCw size={16} /></button>
        </div>
        <div className="rounded-lg border border-gray-700 p-3 space-y-2">
            <div className="flex flex-wrap gap-2">
                <input className="min-w-32 flex-1 rounded bg-gray-900 border border-gray-600 p-2 text-sm" value={id}
                    placeholder={t('ai.mcpClient.serverId')} aria-label={t('ai.mcpClient.serverId')} disabled={busy} onChange={event => setId(event.target.value)} />
                <input className="min-w-56 flex-[2] rounded bg-gray-900 border border-gray-600 p-2 text-sm" value={command}
                    placeholder={t('ai.mcpClient.executablePath')} aria-label={t('ai.mcpClient.executablePath')} disabled={busy}
                    onChange={event => setCommand(event.target.value)} />
                <button type="button" disabled={busy} onClick={() => void add()}
                    className="flex items-center gap-1 rounded bg-purple-700 px-3 py-2 text-sm disabled:opacity-50"><Plus size={14} /> {t('ai.mcpClient.addServer')}</button>
            </div>
            <p className="text-xs text-gray-500">{t('ai.mcpClient.argumentsHint')}</p>
        </div>
        {servers.map(server => <ServerCard key={server.id} server={server} refresh={refresh} />)}
        {servers.length === 0 && <p className="text-sm text-gray-500">{t('ai.mcpClient.noServers')}</p>}
        {error && <p role="alert" className="text-xs text-red-400">{error}</p>}
        <McpHttpServersPanel />
    </div></McpHealthProvider>;
}
