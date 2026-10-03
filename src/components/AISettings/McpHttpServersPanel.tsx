// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Plus } from 'lucide-react';
import { useTranslation } from '../../i18n';
import { MCP_SERVERS_CHANGED, notifyMcpServersChanged } from '../DevTools/aiChatMcp';
import { describeMcpError } from './mcpErrors';
import { AuthFields, authInput, McpHttpServerCard, type HttpAuthMode, type HttpServerView } from './McpHttpServerCard';

const idValid = (value: string) => /^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$/.test(value);
const endpointValid = (value: string) => {
    try { return new URL(value).protocol === 'https:'; } catch { return false; }
};

export function McpHttpServersPanel() {
    const t = useTranslation();
    const [servers, setServers] = useState<HttpServerView[]>([]);
    const [id, setId] = useState('');
    const [endpoint, setEndpoint] = useState('');
    const [mode, setMode] = useState<HttpAuthMode>('oauth');
    const [clientId, setClientId] = useState('');
    const [metadataUrl, setMetadataUrl] = useState('');
    const [busy, setBusy] = useState(false);
    const [error, setError] = useState('');
    const refreshSequence = useRef(0);
    const refresh = useCallback(async () => {
        const sequence = ++refreshSequence.current;
        const result = await invoke<HttpServerView[]>('mcp_client_http_list_servers');
        if (sequence === refreshSequence.current) setServers(result);
    }, []);
    useEffect(() => { void refresh().catch(cause => setError(describeMcpError(t, cause))); }, [refresh, t]);
    useEffect(() => {
        const changed = () => { void refresh().catch(cause => setError(describeMcpError(t, cause))); };
        window.addEventListener(MCP_SERVERS_CHANGED, changed);
        return () => window.removeEventListener(MCP_SERVERS_CHANGED, changed);
    }, [refresh, t]);

    const add = async () => {
        if (!idValid(id) || !endpointValid(endpoint.trim())) { setError(t('ai.mcpClient.invalidHttp')); return; }
        setBusy(true); setError('');
        try {
            await invoke('mcp_client_http_upsert_server', { server: {
                id, endpoint: endpoint.trim(), auth: authInput(mode, clientId, metadataUrl), enabled: false, expected_revision: 0,
            } });
            setId(''); setEndpoint(''); setClientId(''); setMetadataUrl('');
            await refresh();
            notifyMcpServersChanged();
        } catch (cause) { setError(describeMcpError(t, cause)); }
        finally { setBusy(false); }
    };

    const field = 'rounded bg-gray-900 border border-gray-600 p-2 text-sm';
    return <section className="space-y-4 border-t border-gray-700 pt-4">
        <div>
            <h2 className="font-medium text-white">{t('ai.mcpClient.httpTitle')}</h2>
            <p className="text-xs text-gray-400">{t('ai.mcpClient.httpSubtitle')}</p>
        </div>
        <div className="rounded-lg border border-gray-700 p-3 space-y-2">
            <div className="flex flex-wrap gap-2">
                <input className={`min-w-32 flex-1 ${field}`} value={id} placeholder={t('ai.mcpClient.serverId')}
                    aria-label={t('ai.mcpClient.serverId')} disabled={busy} onChange={event => setId(event.target.value)} />
                <input className={`min-w-56 flex-[2] ${field}`} value={endpoint} placeholder={t('ai.mcpClient.endpoint')}
                    aria-label={t('ai.mcpClient.endpoint')} disabled={busy} onChange={event => setEndpoint(event.target.value)} />
            </div>
            <AuthFields mode={mode} setMode={setMode} clientId={clientId} setClientId={setClientId}
                metadataUrl={metadataUrl} setMetadataUrl={setMetadataUrl} disabled={busy} />
            <button type="button" disabled={busy} onClick={() => void add()}
                className="flex items-center gap-1 rounded bg-purple-700 px-3 py-2 text-sm disabled:opacity-50">
                <Plus size={14} /> {t('ai.mcpClient.addServer')}</button>
        </div>
        {servers.map(server => <McpHttpServerCard key={server.id} server={server} refresh={refresh} />)}
        {servers.length === 0 && <p className="text-sm text-gray-500">{t('ai.mcpClient.noServers')}</p>}
        {error && <p role="alert" className="text-xs text-red-400">{error}</p>}
    </section>;
}
