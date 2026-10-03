// SPDX-License-Identifier: GPL-3.0-or-later
import { useState } from 'react';
import { pickFile } from '../../utils/pickPath';
import { invoke } from '@tauri-apps/api/core';
import { createPortal } from 'react-dom';
import { ConfirmOverlay } from '../common/ConfirmOverlay';
import { MODAL_Z } from '../../utils/modalLayers';
import { useTranslation } from '../../i18n';
import type { ServerConfig } from './McpServersPanel';

export interface SandboxConfig {
    directories: { path: string; device: string; inode: string }[];
    network_consent: boolean;
    managed: { manifest_id: string; version: string; archive_sha256: string; tree_sha256: string; network_declared: boolean } | null;
}
const emptySandbox: SandboxConfig = { directories: [], network_consent: false, managed: null };

export function McpPermissions({ server, busy, perform }: {
    server: ServerConfig; busy: boolean; perform: (action: () => Promise<void>) => Promise<void>;
}) {
    const t = useTranslation();
    const sandbox = server.sandbox ?? emptySandbox;
    const paths = sandbox.directories.map(grant => grant.path);
    const [pending, setPending] = useState<{ paths: string[]; network: boolean; grantPath?: string; revision: number; message: string } | null>(null);
    const choose = () => perform(async () => {
        const selected = await pickFile({ directory: true, multiple: false });
        if (typeof selected === 'string') {
            setPending({ paths: paths.includes(selected) ? paths : [...paths, selected], grantPath: selected, network: sandbox.network_consent,
                revision: server.revision, message: t('ai.mcpClient.confirmDirectory', { path: selected }) });
        }
    });
    return <div className="border-t border-gray-700 pt-3 space-y-2 text-xs text-gray-300">
        {sandbox.managed && <p>{t('ai.mcpClient.managedStatus', { version: sandbox.managed.version })}</p>}
        <p className="font-medium">{t('ai.mcpClient.directoryTitle')}</p>
        {paths.length === 0 && <p className="text-gray-500">{t('ai.mcpClient.noDirectories')}</p>}
        {paths.map(path => <div key={path} className="flex items-center gap-2">
            <span className="break-all flex-1">{path}</span>
            {!sandbox.managed && <button type="button" disabled={busy} className="text-gray-300 disabled:opacity-50"
                onClick={() => setPending({ paths, grantPath: path, network: false, revision: server.revision,
                    message: t('ai.mcpClient.confirmDirectory', { path }) })}>{t('ai.mcpClient.renewDirectory')}</button>}
            {!sandbox.managed && <button type="button" disabled={busy} className="text-red-400 disabled:opacity-50"
                onClick={() => setPending({ paths: paths.filter(p => p !== path), network: false,
                    revision: server.revision, message: t('ai.mcpClient.confirmRevokeDirectory', { path }) })}>{t('ai.mcpClient.revokeDirectory')}</button>}
        </div>)}
        {!sandbox.managed && <button type="button" disabled={busy || paths.length >= 4}
            className="rounded bg-gray-700 px-3 py-1.5 disabled:opacity-50" onClick={() => void choose()}>{t('ai.mcpClient.grantDirectory')}</button>}
        {sandbox.managed?.network_declared ? <label className="flex items-center gap-2">
            <input type="checkbox" checked={sandbox.network_consent} disabled={busy}
                onChange={event => setPending({ paths, network: event.target.checked,
                    revision: server.revision, message: event.target.checked ? t('ai.mcpClient.confirmNetwork', { name: server.id }) : t('ai.mcpClient.confirmBlockNetwork', { name: server.id }) })} />
            {t('ai.mcpClient.networkAccess')}
        </label> : <p className="text-gray-500">{t('ai.mcpClient.networkBlocked')}</p>}
        {pending && createPortal(<ConfirmOverlay message={pending.message} confirmLabel={t('ai.toolApproval.confirm')} confirmColor="blue" onCancel={() => setPending(null)}
            onConfirm={() => {
                const permission = pending;
                setPending(null);
                void perform(() => invoke<void>('mcp_client_set_permissions', {
                    serverId: server.id, expectedRevision: permission.revision,
                    directoryPaths: permission.paths, networkConsent: permission.network, grantPath: permission.grantPath ?? null,
                }));
            }} zClass={MODAL_Z.globalConfirm} />, document.body)}
    </div>;
}
