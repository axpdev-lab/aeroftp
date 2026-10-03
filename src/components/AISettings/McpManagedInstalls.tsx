// SPDX-License-Identifier: GPL-3.0-or-later
import { useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { createPortal } from 'react-dom';
import { Loader2 } from 'lucide-react';
import { ConfirmOverlay } from '../common/ConfirmOverlay';
import { MODAL_Z } from '../../utils/modalLayers';
import { useTranslation } from '../../i18n';
import { describeMcpError } from './mcpErrors';
import { notifyMcpServersChanged } from '../DevTools/aiChatMcp';

interface Manifest { id: string; version: string; network: boolean }

/** Generic installer controls; the reviewed registry supplies all available artifacts. */
export function McpManagedInstalls({ installedIds, refresh }: { installedIds: string[]; refresh: () => Promise<void> }) {
    const t = useTranslation();
    const [manifests, setManifests] = useState<Manifest[]>([]);
    const [pending, setPending] = useState<Manifest | null>(null);
    const [operation, setOperation] = useState<string | null>(null);
    const [error, setError] = useState('');
    const active = useRef<string | null>(null);
    useEffect(() => {
        let mounted = true;
        void invoke<Manifest[]>('mcp_client_install_manifests').then(result => {
            if (mounted) setManifests(result ?? []);
        }).catch(cause => { if (mounted) setError(describeMcpError(t, cause)); });
        return () => {
            mounted = false;
        };
    }, [t]);
    useEffect(() => () => {
        if (active.current) void invoke('mcp_client_install_cancel', { operationId: active.current }).catch(() => {});
    }, []);
    const install = async (manifest: Manifest, networkConsent: boolean) => {
        if (active.current) return;
        const operationId = crypto.randomUUID();
        active.current = operationId; setOperation(operationId); setError('');
        try {
            await invoke('mcp_client_install_server', { manifestId: manifest.id, version: manifest.version,
                expectedRevision: 0, networkConsent, operationId });
            await refresh(); notifyMcpServersChanged();
        } catch (cause) { setError(describeMcpError(t, cause)); }
        finally { active.current = null; setOperation(null); }
    };
    const available = manifests.filter(manifest => !installedIds.includes(manifest.id));
    if (!available.length && !error && !operation) return null;
    return <div className="space-y-2">
        {available.map(manifest => <div key={`${manifest.id}:${manifest.version}`} className="flex items-center gap-3 rounded border border-gray-700 p-3 text-sm text-gray-300">
            <span className="flex-1">{manifest.id} {manifest.version}</span>
            <button type="button" disabled={!!operation} className="rounded bg-purple-700 px-3 py-1.5 disabled:opacity-50"
                onClick={() => manifest.network ? setPending(manifest) : void install(manifest, false)}>{t('ai.mcpClient.addServer')}</button>
        </div>)}
        {operation && <button type="button" className="flex items-center gap-2 text-sm text-gray-300" onClick={() => {
            void invoke('mcp_client_install_cancel', { operationId: operation }).catch(cause => setError(describeMcpError(t, cause)));
        }}><Loader2 size={14} className="animate-spin" />{t('common.cancel')}</button>}
        {error && <p role="alert" className="text-xs text-red-400">{error}</p>}
        {pending && createPortal(<ConfirmOverlay message={t('ai.mcpClient.confirmNetwork', { name: pending.id })}
            confirmLabel={t('ai.toolApproval.confirm')} confirmColor="blue" onCancel={() => setPending(null)} onConfirm={() => {
                const manifest = pending; setPending(null); void install(manifest, true);
            }} zClass={MODAL_Z.globalConfirm} />, document.body)}
    </div>;
}
