// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { AlertTriangle, Eye, EyeOff, KeyRound, Loader2, RotateCcw, X } from 'lucide-react';
import {
    adminResetUserPassphrase,
    getUserStorageStats,
    type UserMetadata,
    type UserStorageStats,
} from '../../utils/userPartitions';
import { PasswordStrengthBar } from '../vault/PasswordStrengthBar';

interface DestructiveResetDialogProps {
    target: UserMetadata;
    initialStats?: UserStorageStats;
    onClose: () => void;
    onComplete: () => void;
}

const formatBytes = (bytes: number): string => {
    if (bytes < 1024) return `${bytes} B`;
    const kib = bytes / 1024;
    if (kib < 1024) return `${kib.toFixed(1)} KiB`;
    return `${(kib / 1024).toFixed(1)} MiB`;
};

const mapResetError = (err: unknown): string => {
    const message = typeof err === 'string' ? err : String(err);
    if (message.includes('NOT_AUTHORIZED')) return 'You must be an admin to reset another account password.';
    if (message.includes('ADMIN_RESET_NOT_FOR_SELF')) return 'Use Change Password on your own account.';
    if (message.includes('STORE_NOT_READY')) return 'Unlock the vault first, then try again.';
    if (message.includes('VAULT_LOCKED')) return 'Unlock your account first, then try again.';
    if (message.includes('USER_NOT_FOUND')) return 'This user no longer exists.';
    return message;
};

export const DestructiveResetDialog: React.FC<DestructiveResetDialogProps> = ({
    target,
    initialStats,
    onClose,
    onComplete,
}) => {
    const [stats, setStats] = React.useState<UserStorageStats | undefined>(initialStats);
    const [loadingStats, setLoadingStats] = React.useState(!initialStats);
    const [confirmText, setConfirmText] = React.useState('');
    const [newPassphrase, setNewPassphrase] = React.useState('');
    const [confirmPassphrase, setConfirmPassphrase] = React.useState('');
    const [showPassphrase, setShowPassphrase] = React.useState(false);
    const [submitting, setSubmitting] = React.useState(false);
    const [error, setError] = React.useState('');

    React.useEffect(() => {
        let cancelled = false;
        setLoadingStats(true);
        getUserStorageStats()
            .then((items) => {
                if (cancelled) return;
                setStats(items.find((item) => item.userId === target.id));
            })
            .catch(() => {
                if (!cancelled) setStats(initialStats);
            })
            .finally(() => {
                if (!cancelled) setLoadingStats(false);
            });
        return () => { cancelled = true; };
    }, [initialStats, target.id]);

    const profileCount = stats?.profileCount ?? 0;
    const settingsCount = stats?.settingsCount ?? 0;
    const encryptedBytes = stats?.encryptedBytes ?? 0;
    const canSubmit = confirmText === 'RESET'
        && newPassphrase.length > 0
        && newPassphrase === confirmPassphrase
        && !submitting;

    const handleSubmit = async (event: React.FormEvent) => {
        event.preventDefault();
        setError('');
        if (confirmText !== 'RESET') {
            setError('Type RESET to confirm this destructive operation.');
            return;
        }
        if (!newPassphrase) {
            setError('Enter a new account password.');
            return;
        }
        if (newPassphrase !== confirmPassphrase) {
            setError('New account passwords do not match.');
            return;
        }
        setSubmitting(true);
        try {
            await adminResetUserPassphrase(target.id, newPassphrase);
            onComplete();
        } catch (err) {
            setError(mapResetError(err));
        } finally {
            setSubmitting(false);
        }
    };

    return (
        <div className="fixed inset-0 z-[95] flex items-start justify-center pt-[9vh]">
            <button
                type="button"
                aria-label="Close reset password dialog"
                className="absolute inset-0 bg-black/60 backdrop-blur-sm"
                onClick={submitting ? undefined : onClose}
            />
            <form
                onSubmit={handleSubmit}
                className="relative w-full max-w-lg overflow-hidden rounded-lg border border-red-200 bg-white shadow-2xl dark:border-red-900/60 dark:bg-gray-800"
            >
                <div className="flex items-center justify-between border-b border-red-200 bg-red-50 px-4 py-3 text-red-900 dark:border-red-900/60 dark:bg-red-950/30 dark:text-red-100">
                    <div className="flex min-w-0 items-center gap-2">
                        <RotateCcw size={18} className="shrink-0" />
                        <div className="min-w-0">
                            <h3 className="truncate text-sm font-semibold">Reset password = data destruction</h3>
                            <p className="truncate text-xs opacity-80">{target.name}</p>
                        </div>
                    </div>
                    <button
                        type="button"
                        onClick={onClose}
                        disabled={submitting}
                        className="flex h-8 w-8 items-center justify-center rounded-lg text-red-700 transition-colors hover:bg-red-100 disabled:opacity-50 dark:text-red-200 dark:hover:bg-red-900/40"
                        aria-label="Close"
                    >
                        <X size={16} />
                    </button>
                </div>

                <div className="space-y-4 p-4">
                    <div className="rounded-lg border border-red-200 bg-red-50 p-3 text-sm text-red-800 dark:border-red-900/60 dark:bg-red-950/30 dark:text-red-200">
                        <div className="mb-2 flex items-start gap-2 font-semibold">
                            <AlertTriangle size={16} className="mt-0.5 shrink-0" />
                            <span>This will permanently wipe the target account's encrypted partition.</span>
                        </div>
                        <p className="text-xs leading-relaxed opacity-90">
                            The current password cannot be recovered. AeroFTP will delete the target account's saved servers, user settings, and lockout state, then issue a new encryption key protected by the new password.
                        </p>
                    </div>

                    <div className="grid gap-2 rounded-lg border border-gray-200 bg-gray-50 p-3 text-xs text-gray-700 dark:border-gray-700 dark:bg-gray-900/40 dark:text-gray-300 sm:grid-cols-3">
                        <div>
                            <div className="font-medium text-gray-900 dark:text-gray-100">{profileCount}</div>
                            <div>profiles</div>
                        </div>
                        <div>
                            <div className="font-medium text-gray-900 dark:text-gray-100">{settingsCount}</div>
                            <div>settings</div>
                        </div>
                        <div>
                            <div className="font-medium text-gray-900 dark:text-gray-100">
                                {loadingStats ? 'Loading...' : formatBytes(encryptedBytes)}
                            </div>
                            <div>encrypted bytes</div>
                        </div>
                    </div>

                    {error && (
                        <div className="rounded-lg border border-red-200 bg-red-50 px-3 py-2 text-xs text-red-700 dark:border-red-900/60 dark:bg-red-950/30 dark:text-red-300">
                            {error}
                        </div>
                    )}

                    <div>
                        <label className="mb-1 block text-xs font-medium text-gray-700 dark:text-gray-300">
                            Type RESET
                        </label>
                        <input
                            value={confirmText}
                            onChange={(event) => setConfirmText(event.target.value)}
                            className="w-full rounded-lg border border-gray-300 bg-white px-3 py-2 text-sm text-gray-900 dark:border-gray-600 dark:bg-gray-900 dark:text-gray-100"
                            autoComplete="off"
                            disabled={submitting}
                        />
                    </div>

                    <div className="grid gap-3 sm:grid-cols-2">
                        <div>
                            <label className="mb-1 block text-xs font-medium text-gray-700 dark:text-gray-300">
                                New account password
                            </label>
                            <div className="relative">
                                <input
                                    type={showPassphrase ? 'text' : 'password'}
                                    value={newPassphrase}
                                    onChange={(event) => setNewPassphrase(event.target.value)}
                                    className="w-full rounded-lg border border-gray-300 bg-white px-3 py-2 pr-9 text-sm text-gray-900 dark:border-gray-600 dark:bg-gray-900 dark:text-gray-100"
                                    autoComplete="new-password"
                                    disabled={submitting}
                                />
                                <button
                                    type="button"
                                    onClick={() => setShowPassphrase((value) => !value)}
                                    className="absolute right-2 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-700 dark:hover:text-gray-200"
                                    aria-label={showPassphrase ? 'Hide password' : 'Show password'}
                                >
                                    {showPassphrase ? <EyeOff size={15} /> : <Eye size={15} />}
                                </button>
                            </div>
                            <div className="mt-2">
                                <PasswordStrengthBar password={newPassphrase} />
                            </div>
                        </div>
                        <div>
                            <label className="mb-1 block text-xs font-medium text-gray-700 dark:text-gray-300">
                                Confirm new password
                            </label>
                            <input
                                type={showPassphrase ? 'text' : 'password'}
                                value={confirmPassphrase}
                                onChange={(event) => setConfirmPassphrase(event.target.value)}
                                className="w-full rounded-lg border border-gray-300 bg-white px-3 py-2 text-sm text-gray-900 dark:border-gray-600 dark:bg-gray-900 dark:text-gray-100"
                                autoComplete="new-password"
                                disabled={submitting}
                            />
                        </div>
                    </div>
                </div>

                <div className="flex items-center justify-end gap-2 border-t border-gray-200 px-4 py-3 dark:border-gray-700">
                    <button
                        type="button"
                        onClick={onClose}
                        disabled={submitting}
                        className="inline-flex h-9 items-center justify-center rounded-lg bg-gray-100 px-3 text-sm font-medium text-gray-700 transition-colors hover:bg-gray-200 disabled:opacity-50 dark:bg-gray-700 dark:text-gray-200 dark:hover:bg-gray-600"
                    >
                        Cancel
                    </button>
                    <button
                        type="submit"
                        disabled={!canSubmit}
                        className="inline-flex h-9 items-center justify-center gap-1.5 rounded-lg bg-red-600 px-3 text-sm font-medium text-white transition-colors hover:bg-red-700 disabled:cursor-not-allowed disabled:bg-red-300 dark:disabled:bg-red-900/50"
                    >
                        {submitting ? <Loader2 size={15} className="animate-spin" /> : <KeyRound size={15} />}
                        Reset password
                    </button>
                </div>
            </form>
        </div>
    );
};

export default DestructiveResetDialog;
