// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { relativeSyncSegments, resolveSyncNavigationPath, type SyncNavigationEntry } from './syncNavigationPath';

export type SyncMirrorInvoke = (command: string, args: Record<string, unknown>) => Promise<unknown>;

export type SyncMirrorAction =
    | { kind: 'legacy' }
    | { kind: 'skip' }
    | { kind: 'navigate'; path: string }
    | { kind: 'missing'; path: string }
    | { kind: 'conflict'; path: string; candidates: string[] }
    | { kind: 'cancelled' };

/** Canonical matching is plain SFTP only. FTP listings and crypt path grammars stay on the previous route. */
export function plainSftpSyncMirror(protocol: string | undefined, cryptActive: boolean): boolean {
    return protocol === 'sftp' && !cryptActive;
}

export function cryptSyncBoundaryActive(flags: {
    aeroVaultSessionId?: string | null;
    rcloneCryptVaultId?: string | null;
    aeroCryptVaultId?: string | null;
    providerOverlayBound: boolean;
}): boolean {
    return !!(flags.aeroVaultSessionId || flags.rcloneCryptVaultId || flags.aeroCryptVaultId || flags.providerOverlayBound);
}

/** Only a genuinely missing directory may offer mkdir. Conflicts, skips and cancellations must not. */
export function syncMirrorProposesCreate(action: SyncMirrorAction): boolean {
    return action.kind === 'missing';
}

export function syncMirrorConflictText(path: string, candidates: readonly string[]): string {
    return [path, ...candidates].filter(Boolean).join('\n');
}

const asEntries = (value: unknown): SyncNavigationEntry[] => {
    const rows = Array.isArray(value) ? value : [];
    return rows.map(row => {
        const entry = row as { name?: unknown; is_dir?: unknown };
        return { name: String(entry.name ?? ''), is_dir: entry.is_dir === true };
    });
};

async function listLocal(invoke: SyncMirrorInvoke, path: string): Promise<SyncNavigationEntry[]> {
    return asEntries(await invoke('get_local_files', { path, showHidden: true }));
}

async function listRemote(invoke: SyncMirrorInvoke, path: string): Promise<SyncNavigationEntry[]> {
    const response = await invoke('provider_list_files', { path }) as { files?: unknown };
    return asEntries(response?.files);
}

/**
 * Resolve one sync-navigation mirror. Listing failures reject: they are not
 * missing directories. The returned path, when kind is `navigate`, is the
 * destination's own spelling and must be passed to the following I/O unchanged.
 */
export async function planProviderSyncMirror(input: {
    protocol: string | undefined;
    cryptActive: boolean;
    direction: 'local-to-remote' | 'remote-to-local';
    sourcePath: string;
    sourceBase: string;
    destBase: string;
    invoke: SyncMirrorInvoke;
    isCurrent?: () => boolean;
}): Promise<SyncMirrorAction> {
    if (!plainSftpSyncMirror(input.protocol, input.cryptActive)) return { kind: 'legacy' };
    const segments = relativeSyncSegments(input.sourcePath, input.sourceBase);
    if (segments === null) return { kind: 'skip' };
    const localToRemote = input.direction === 'local-to-remote';
    const resolved = await resolveSyncNavigationPath(
        input.destBase,
        segments,
        path => (localToRemote ? listRemote(input.invoke, path) : listLocal(input.invoke, path)),
        {
            base: input.sourceBase,
            listEntries: path => (localToRemote ? listLocal(input.invoke, path) : listRemote(input.invoke, path)),
        },
        input.isCurrent ?? (() => true),
    );
    if (resolved.kind === 'found') return { kind: 'navigate', path: resolved.path };
    if (resolved.kind === 'missing') return { kind: 'missing', path: resolved.path };
    if (resolved.kind === 'conflict') return { kind: 'conflict', path: resolved.path, candidates: resolved.candidates };
    return { kind: 'cancelled' };
}
