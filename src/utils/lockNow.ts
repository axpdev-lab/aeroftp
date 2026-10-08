// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

export type LockScope = 'account' | 'vault' | 'all';
export interface LockPolicy { account: boolean; vault: boolean }
export interface LockResult {
    accountLocked: boolean;
    vaultLocked: boolean;
    unavailable?: boolean;
    stale?: boolean;
    error?: unknown;
}
interface LockDependencies {
    policy: (scope: LockScope) => Promise<LockPolicy>;
    contextVersion: () => number;
    interruptController: () => void;
    lockAccount: () => Promise<void>;
    lockVault: () => Promise<void>;
    confirmed: (result: LockResult) => void;
}

/** Shared, backend-confirmed policy for the account, vault and combined controls. */
export function createLockNow(deps: LockDependencies) {
    let pending: { scope: LockScope; promise: Promise<LockResult> } | null = null;
    return (scope: LockScope = 'all'): Promise<LockResult> => {
        if (pending) return pending.scope === scope ? pending.promise : Promise.resolve({ accountLocked: false, vaultLocked: false, error: 'LOCK_BUSY' });
        const version = deps.contextVersion();
        const current = () => deps.contextVersion() === version;
        const result: LockResult = { accountLocked: false, vaultLocked: false };
        const promise = (async () => {
            try {
                deps.interruptController();
                const policy = await deps.policy(scope);
                if (!current()) return { ...result, stale: true };
                const account = scope !== 'vault' && policy.account;
                const vault = scope !== 'account' && policy.vault;
                if (!account && !vault) return { ...result, unavailable: true };
                // Clear the account session while the vault is still available.
                // Publish only confirmed layers, including a partial success if
                // the subsequent backend command fails.
                if (account) {
                    await deps.lockAccount();
                    result.accountLocked = true;
                    if (!current()) return { ...result, stale: true };
                }
                if (vault) {
                    await deps.lockVault();
                    result.vaultLocked = true;
                    if (!current()) return { ...result, stale: true };
                }
            } catch (error) { result.error = error; }
            if (!current()) return { ...result, stale: true };
            if (result.accountLocked || result.vaultLocked) deps.confirmed(result);
            return result;
        })();
        pending = { scope, promise };
        void promise.then(() => { pending = null; }, () => { pending = null; });
        return promise;
    };
}

// Capture before Monaco/xterm consume the event. Use physical KeyK as well:
// macOS Option can change event.key even though the K key was pressed.
export function isLockNowShortcut(event: KeyboardEvent): boolean {
    return (event.ctrlKey || event.metaKey) && event.altKey && event.shiftKey &&
        (event.code === 'KeyK' || event.key.toLowerCase() === 'k');
}
