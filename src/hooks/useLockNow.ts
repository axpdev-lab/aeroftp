// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { createLockNow, isLockNowShortcut, type LockScope } from '../utils/lockNow';
import { getUnlockStatus, listUsers, lockUserSession, dispatchAccountLockScreenRequested, ACCOUNT_LOCK_SCREEN_REQUESTED_EVENT } from '../utils/userPartitions';
import { PROFILES_CHANGED_EVENT } from '../utils/serverProfileStore';
import { MASTER_PASSWORD_CHANGED_EVENT } from '../utils/masterPasswordEvents';

interface LockNowOptions {
    vaultConfigured: boolean;
    locked: boolean;
    onVaultLocked: () => void;
}
export function useLockNow(options: LockNowOptions) {
    const current = useRef(options);
    current.current = options;
    const version = useRef(0);
    const mounted = useRef(true);
    const [error, setError] = useState<'failed' | 'unavailable' | 'stale' | null>(null);
    const [busy, setBusy] = useState(false);
    const [action] = useState(() => createLockNow({
        contextVersion: () => version.current,
        interruptController: () => window.__aeroftpController?.interrupt(),
        policy: async scope => {
            if (scope === 'vault') return { account: false, vault: current.current.vaultConfigured };
            // Discover live public metadata; never read credentials or the vault.
            const [users, status] = await Promise.all([listUsers(), getUnlockStatus()]);
            const active = users.find(user => user.id === status.activeUserId);
            return { account: !!active && status.isUnlocked && (active.hasPassphrase || users.length > 1), vault: current.current.vaultConfigured };
        },
        lockAccount: lockUserSession,
        lockVault: () => invoke<void>('lock_credential_store'),
        confirmed: result => {
            if (!mounted.current) return;
            if (result.vaultLocked) current.current.onVaultLocked();
            if (result.accountLocked) {
                dispatchAccountLockScreenRequested();
                window.dispatchEvent(new Event(PROFILES_CHANGED_EVENT));
            }
        },
    }));
    useEffect(() => {
        mounted.current = true;
        const changed = () => { version.current++; };
        const events = [PROFILES_CHANGED_EVENT, MASTER_PASSWORD_CHANGED_EVENT, ACCOUNT_LOCK_SCREEN_REQUESTED_EVENT];
        events.forEach(event => window.addEventListener(event, changed));
        return () => { mounted.current = false; version.current++; events.forEach(event => window.removeEventListener(event, changed)); };
    }, []);
    useEffect(() => { version.current++; }, [options.vaultConfigured, options.locked]);
    const lock = useCallback(async (scope: LockScope = 'all') => {
        if (current.current.locked) return false;
        setBusy(true); setError(null);
        try {
            const result = await action(scope);
            if (!mounted.current) return false;
            if (result.stale) setError('stale');
            else if (result.error) setError('failed');
            else if (result.unavailable) setError('unavailable');
            return !result.stale && !result.error && !result.unavailable;
        } catch {
            if (mounted.current) setError('failed');
            return false;
        } finally { if (mounted.current) setBusy(false); }
    }, [action]);
    useEffect(() => {
        const keydown = (event: KeyboardEvent) => {
            if (!isLockNowShortcut(event)) return;
            event.preventDefault(); event.stopImmediatePropagation();
            if (!event.repeat) void lock('all');
        };
        window.addEventListener('keydown', keydown, true);
        return () => window.removeEventListener('keydown', keydown, true);
    }, [lock]);
    return { lock, busy, error, dismissError: () => setError(null) };
}
