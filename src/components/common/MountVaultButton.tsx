// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { useState, useEffect, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { HardDrive, FolderOpen, Unplug, Loader2, AlertTriangle } from 'lucide-react';
import { useTranslation } from '../../i18n';

export type VaultMountKind = 'cryptomator' | 'aerovault';

interface VaultMountInfo {
    key: string;
    mountpoint: string;
    kind: string;
    displayName: string;
    pid: number;
}

/**
 * "Mount (read-only)" affordance for the unlocked-vault browse views (AeroMount
 * Deliverable B, #322 Ehud idea #1).
 *
 * Surfaces the decrypted vault as a browsable READ-ONLY filesystem in the OS file
 * manager. The mount is EPHEMERAL: it unmounts when the vault is locked or the app
 * quits (the backend kills the `aeroftp-cli mount-vault` child on those events),
 * so this control makes that explicit. The password is forwarded to the mount
 * child over stdin by the backend and never stored. Shared by CryptomatorBrowser
 * and the `.aerovault` VaultBrowse. Linux only for now.
 */
interface MountVaultButtonProps {
    kind: VaultMountKind;
    /** Stable handle: Cryptomator vault_id, or the `.aerovault` path. */
    vaultKey: string;
    /** Vault directory (Cryptomator) or container file (`.aerovault`/`.aerozip`). */
    vaultPath: string;
    /** Forwarded to the mount child over stdin; empty = plaintext `.aerozip`. */
    password: string;
    displayName: string;
    disabled?: boolean;
    className?: string;
}

export const MountVaultButton: React.FC<MountVaultButtonProps> = ({
    kind,
    vaultKey,
    vaultPath,
    password,
    displayName,
    disabled,
    className,
}) => {
    const t = useTranslation();
    const [mount, setMount] = useState<VaultMountInfo | null>(null);
    const [busy, setBusy] = useState(false);
    const [err, setErr] = useState<string | null>(null);

    // The mount is session-bound: when this browse view closes (component
    // unmounts) the ephemeral mount must go too. Track the live key in a ref so
    // the unmount-on-teardown only fires when something is actually mounted (it
    // also stays a no-op under React StrictMode's dev double-invoke).
    const mountedKeyRef = useRef<string | null>(null);
    useEffect(() => { mountedKeyRef.current = mount ? mount.key : null; }, [mount]);
    useEffect(() => () => {
        if (mountedKeyRef.current) {
            invoke('vault_mount_stop', { key: mountedKeyRef.current }).catch(() => { /* ignore */ });
        }
    }, []);

    const doMount = async () => {
        setBusy(true);
        setErr(null);
        try {
            const info = await invoke<VaultMountInfo>('vault_mount_start', {
                key: vaultKey,
                kind,
                vaultPath,
                password,
                displayName,
            });
            setMount(info);
            // Reveal the freshly mounted tree in the OS file manager.
            try { await invoke('vault_mount_open', { key: vaultKey }); } catch { /* non-fatal */ }
        } catch (e) {
            setErr(String(e));
        } finally {
            setBusy(false);
        }
    };

    const doUnmount = async () => {
        setBusy(true);
        setErr(null);
        try {
            await invoke('vault_mount_stop', { key: vaultKey });
            setMount(null);
        } catch (e) {
            setErr(String(e));
        } finally {
            setBusy(false);
        }
    };

    const doOpen = async () => {
        try { await invoke('vault_mount_open', { key: vaultKey }); } catch (e) { setErr(String(e)); }
    };

    if (mount) {
        return (
            <div className={`flex items-center gap-1 ${className || ''}`}>
                <button
                    onClick={doOpen}
                    title={mount.mountpoint}
                    className="flex items-center gap-1 px-2 py-1 text-xs bg-teal-700 hover:bg-teal-600 text-white rounded"
                >
                    <FolderOpen size={14} /> {t('mountVault.open')}
                </button>
                <button
                    onClick={doUnmount}
                    disabled={busy}
                    title={t('mountVault.unmountHint')}
                    className="flex items-center gap-1 px-2 py-1 text-xs bg-gray-600 hover:bg-gray-500 text-white rounded disabled:opacity-50"
                >
                    {busy ? <Loader2 size={14} className="animate-spin" /> : <Unplug size={14} />} {t('mountVault.unmount')}
                </button>
            </div>
        );
    }

    return (
        <div className={`relative ${className || ''}`}>
            <button
                onClick={doMount}
                disabled={disabled || busy}
                title={t('mountVault.hint')}
                className="flex items-center gap-1 px-2 py-1 text-xs bg-teal-700 hover:bg-teal-600 text-white rounded disabled:opacity-50"
            >
                {busy ? <Loader2 size={14} className="animate-spin" /> : <HardDrive size={14} />} {t('mountVault.button')}
            </button>
            {err && (
                <div className="absolute z-50 mt-1 right-0 w-[260px] flex items-start gap-1 px-2 py-1.5 text-[11px] bg-red-100 dark:bg-red-900/40 text-red-700 dark:text-red-300 rounded shadow border border-red-200 dark:border-red-800">
                    <AlertTriangle size={12} className="mt-0.5 shrink-0" />
                    <span className="break-words">{err}</span>
                </div>
            )}
        </div>
    );
};
