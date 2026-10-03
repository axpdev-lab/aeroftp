// SPDX-License-Identifier: GPL-3.0-or-later
import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useTranslation } from '../../i18n';
import { describeMcpError } from './mcpErrors';
import { MCP_SERVERS_CHANGED, notifyMcpServersChanged } from '../DevTools/aiChatMcp';

interface HttpPreset { id: string; transport: string; publisher: string; homepage: string; endpoint: string }
interface Listed { id: string; enabled: boolean }
// Card text per reviewed id. An id the app has no text for is not shown.
const PRESET_TEXT: Record<string, { name: string; reason: string }> = {
    deepwiki: { name: 'ai.mcpClient.presetDeepwikiName', reason: 'ai.mcpClient.presetDeepwikiReason' },
};

/** Recommended HTTP servers. Install sends the reviewed preset id only, never an endpoint. */
export function McpRecommendedServers() {
    const t = useTranslation();
    const [presets, setPresets] = useState<HttpPreset[]>([]);
    const [installed, setInstalled] = useState<Listed[]>([]);
    const [busy, setBusy] = useState(false);
    const [error, setError] = useState('');
    // Loads overlap (mount, MCP_SERVERS_CHANGED, install): only the newest may land.
    const loadSequence = useRef(0);
    const load = useCallback(async () => {
        const sequence = ++loadSequence.current;
        let listed: HttpPreset[];
        let servers: Listed[];
        try {
            [listed, servers] = await Promise.all([
                invoke<HttpPreset[]>('mcp_client_presets_list'),
                invoke<Listed[]>('mcp_client_http_list_servers'),
            ]);
        } catch (cause) {
            // A failure of a superseded load says nothing about the current state.
            if (sequence === loadSequence.current) throw cause;
            return;
        }
        if (sequence !== loadSequence.current) return;
        setPresets((listed ?? []).filter(preset => preset.transport === 'http' && PRESET_TEXT[preset.id]));
        setInstalled(servers ?? []);
        setError('');
    }, []);
    useEffect(() => {
        let mounted = true;
        // Removing the server from the HTTP list brings its Install button back.
        const reload = () => { void load().catch(cause => { if (mounted) setError(describeMcpError(t, cause)); }); };
        reload();
        window.addEventListener(MCP_SERVERS_CHANGED, reload);
        return () => { mounted = false; window.removeEventListener(MCP_SERVERS_CHANGED, reload); };
    }, [load, t]);
    const install = async (preset: HttpPreset) => {
        setBusy(true); setError('');
        try {
            await invoke('mcp_client_preset_install_http', { presetId: preset.id });
            await load();
            notifyMcpServersChanged();
        } catch (cause) { setError(describeMcpError(t, cause)); }
        finally { setBusy(false); }
    };
    if (!presets.length && !error) return null;
    return <section className="space-y-2">
        <div>
            <h2 className="font-medium text-white">{t('ai.mcpClient.presetsTitle')}</h2>
            <p className="text-xs text-gray-400">{t('ai.mcpClient.presetsSubtitle')}</p>
        </div>
        {presets.map(preset => {
            const current = installed.find(server => server.id === preset.id);
            return <div key={preset.id} className="rounded border border-gray-700 p-3 text-sm text-gray-300 space-y-1">
                <div className="flex items-center gap-3">
                    <span className="flex-1 font-medium text-white">{t(PRESET_TEXT[preset.id].name)}</span>
                    {current
                        ? <span>{current.enabled ? t('ai.mcpClient.presetReady') : t('ai.mcpClient.presetInstalledDisabled')}</span>
                        : <button type="button" disabled={busy} className="rounded bg-purple-700 px-3 py-1.5 disabled:opacity-50"
                            onClick={() => void install(preset)}>{t('ai.mcpClient.presetInstall')}</button>}
                </div>
                <p className="text-xs text-gray-400">{t(PRESET_TEXT[preset.id].reason)}</p>
                <p className="text-xs text-gray-500 break-all">{preset.publisher} · {preset.endpoint}</p>
            </div>;
        })}
        {error && <p role="alert" className="text-xs text-red-400">{error}</p>}
    </section>;
}
