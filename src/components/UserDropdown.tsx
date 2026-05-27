// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import {
    Check,
    ChevronDown,
    Eye,
    EyeOff,
    KeyRound,
    Loader2,
    Lock,
    LockOpen,
    Plus,
    Shield,
    UserCog,
    Users,
    X,
} from 'lucide-react';
import { UserAvatar } from './UserAvatar';
import { UsersManagePanel } from './UsersManagePanel';
import {
    dispatchAccountLockScreenRequested,
    getUnlockStatus,
    initUserPartitions,
    listUsers,
    lockUserSession,
    unlockUser,
    type UserMetadata,
    type UserUnlockStatus,
} from '../utils/userPartitions';
import { PROFILES_CHANGED_EVENT } from '../utils/serverProfileStore';

interface UserDropdownProps {
    onUsersChanged?: () => void;
}

const notifyProfilesChanged = (onUsersChanged?: () => void) => {
    try {
        window.dispatchEvent(new CustomEvent(PROFILES_CHANGED_EVENT));
    } catch {
        // Browserless tests: best effort.
    }
    onUsersChanged?.();
};

const isUnavailableError = (error: unknown): boolean => {
    const message = String(error ?? '');
    return (
        message.includes('STORE_NOT_READY') ||
        message.includes('unknown command') ||
        message.includes('failed to invoke') ||
        message.includes('window.__TAURI_INTERNALS__')
    );
};

export const UserDropdown: React.FC<UserDropdownProps> = ({ onUsersChanged }) => {
    const [users, setUsers] = React.useState<UserMetadata[]>([]);
    const [status, setStatus] = React.useState<UserUnlockStatus | null>(null);
    const [isOpen, setIsOpen] = React.useState(false);
    const [isAvailable, setIsAvailable] = React.useState(true);
    const [isLoading, setIsLoading] = React.useState(false);
    const [error, setError] = React.useState('');
    const [showManagePanel, setShowManagePanel] = React.useState(false);
    const [pendingUnlockUser, setPendingUnlockUser] = React.useState<UserMetadata | null>(null);
    const [unlockPassphrase, setUnlockPassphrase] = React.useState('');
    const [showUnlockPassphrase, setShowUnlockPassphrase] = React.useState(false);
    const menuRef = React.useRef<HTMLDivElement | null>(null);

    const refresh = React.useCallback(async () => {
        try {
            await initUserPartitions();
            const [nextUsers, nextStatus] = await Promise.all([listUsers(), getUnlockStatus()]);
            setUsers(nextUsers);
            setStatus(nextStatus);
            setIsAvailable(true);
            setError('');
        } catch (err) {
            if (isUnavailableError(err)) {
                setIsAvailable(false);
                return;
            }
            setError(String(err));
        }
    }, []);

    React.useEffect(() => {
        void refresh();
        const handler = () => { void refresh(); };
        window.addEventListener(PROFILES_CHANGED_EVENT, handler);
        return () => window.removeEventListener(PROFILES_CHANGED_EVENT, handler);
    }, [refresh]);

    React.useEffect(() => {
        if (!isOpen) return;
        const handleClick = (event: MouseEvent) => {
            if (!menuRef.current?.contains(event.target as Node)) {
                setIsOpen(false);
            }
        };
        const handleKey = (event: KeyboardEvent) => {
            if (event.key === 'Escape') setIsOpen(false);
        };
        document.addEventListener('mousedown', handleClick);
        document.addEventListener('keydown', handleKey);
        return () => {
            document.removeEventListener('mousedown', handleClick);
            document.removeEventListener('keydown', handleKey);
        };
    }, [isOpen]);

    if (!isAvailable) return null;

    const hasUsers = users.length > 0;
    const activeUser = hasUsers
        ? (users.find((user) => user.isActive) ?? users[0])
        : ({ id: 0, name: 'Setup', avatarEmoji: null, avatarColor: null, hasPassphrase: false, isActive: false, sortOrder: 0, isAdmin: false } as unknown as UserMetadata);
    const activeLocked = hasUsers && activeUser.hasPassphrase && status?.isUnlocked === false;
    const sortedUsers = [...users].sort((a, b) => {
        if (a.isActive !== b.isActive) return a.isActive ? -1 : 1;
        return a.sortOrder - b.sortOrder || a.name.localeCompare(b.name);
    });

    const handleSwitch = async (user: UserMetadata) => {
        if (user.isActive && !activeLocked) return;
        setError('');
        if (user.hasPassphrase) {
            setPendingUnlockUser(user);
            setUnlockPassphrase('');
            setShowUnlockPassphrase(false);
            setIsOpen(false);
            return;
        }
        setIsLoading(true);
        try {
            await unlockUser(user.id, null);
            await refresh();
            notifyProfilesChanged(onUsersChanged);
            setIsOpen(false);
        } catch (err) {
            setError(String(err));
        } finally {
            setIsLoading(false);
        }
    };

    const handleLock = async () => {
        setIsLoading(true);
        setError('');
        try {
            await lockUserSession();
            await refresh();
            notifyProfilesChanged(onUsersChanged);
            setIsOpen(false);
            setPendingUnlockUser(null);
            setUnlockPassphrase('');
            dispatchAccountLockScreenRequested();
        } catch (err) {
            setError(String(err));
        } finally {
            setIsLoading(false);
        }
    };

    const handleUnlockSubmit = async (event: React.FormEvent) => {
        event.preventDefault();
        if (!pendingUnlockUser || isLoading) return;
        setIsLoading(true);
        setError('');
        try {
            const MIN_SPINNER_MS = 350;
            await Promise.all([
                unlockUser(pendingUnlockUser.id, pendingUnlockUser.hasPassphrase ? unlockPassphrase : null),
                new Promise<void>((resolve) => setTimeout(resolve, MIN_SPINNER_MS)),
            ]);
            setPendingUnlockUser(null);
            setUnlockPassphrase('');
            await refresh();
            notifyProfilesChanged(onUsersChanged);
        } catch (err) {
            setError(String(err));
        } finally {
            setIsLoading(false);
        }
    };

    return (
        <>
            <div ref={menuRef} className="relative">
                <button
                    type="button"
                    onClick={() => setIsOpen((value) => !value)}
                    className={`flex h-7 max-w-[150px] items-center gap-1.5 rounded-lg px-1.5 text-xs transition-colors ${
                        isOpen
                            ? 'bg-[var(--color-bg-tertiary)] text-[var(--color-text-primary)]'
                            : 'text-[var(--color-text-secondary)] hover:bg-[var(--color-bg-tertiary)] hover:text-[var(--color-text-primary)]'
                    }`}
                    title="Users"
                    aria-label="Users"
                >
                    <UserAvatar
                        name={activeUser.name}
                        avatarEmoji={activeUser.avatarEmoji}
                        avatarColor={activeUser.avatarColor}
                        size="sm"
                    />
                    <span className="min-w-0 max-w-[88px] truncate">{activeUser.name}</span>
                    {activeLocked ? <Lock size={12} className="text-amber-500" /> : <ChevronDown size={12} />}
                </button>

                {isOpen && (
                    <div className="absolute right-0 top-full z-[9999] mt-1 w-[280px] overflow-hidden rounded-lg border border-[var(--color-border)] bg-[var(--color-bg-secondary)] shadow-xl">
                        <div className="border-b border-[var(--color-border)] px-3 py-2">
                            <div className="flex items-center gap-2 text-xs font-medium text-[var(--color-text-primary)]">
                                <Users size={14} className="text-[var(--color-text-secondary)]" />
                                Users
                            </div>
                            {error && (
                                <div className="mt-1 rounded-md bg-red-500/10 px-2 py-1 text-[11px] text-red-500">
                                    {error}
                                </div>
                            )}
                        </div>

                        <div className="max-h-[260px] overflow-y-auto py-1">
                            {hasUsers ? sortedUsers.map((user) => {
                                const isUserLocked = user.hasPassphrase && status?.unlockedUserId !== user.id;
                                return (
                                    <button
                                        key={user.id}
                                        type="button"
                                        onClick={() => { void handleSwitch(user); }}
                                        disabled={isLoading || (user.isActive && !activeLocked)}
                                        className={`flex w-full items-center gap-2 px-3 py-2 text-left text-xs transition-colors ${
                                            user.isActive
                                                ? 'bg-[var(--color-bg-tertiary)] text-[var(--color-text-primary)]'
                                                : 'text-[var(--color-text-primary)] hover:bg-[var(--color-bg-tertiary)]'
                                        } disabled:cursor-default disabled:opacity-90`}
                                    >
                                        <UserAvatar
                                            name={user.name}
                                            avatarEmoji={user.avatarEmoji}
                                            avatarColor={user.avatarColor}
                                            size="sm"
                                        />
                                        <span className="min-w-0 flex-1 truncate">{user.name}</span>
                                        {user.isActive && !activeLocked && <Check size={13} className="text-blue-500" />}
                                        {isUserLocked ? (
                                            <Lock size={13} className="text-emerald-500" />
                                        ) : (
                                            <LockOpen size={13} className="text-[var(--color-text-tertiary)]" />
                                        )}
                                    </button>
                                );
                            }) : (
                                <div className="px-3 py-3 text-center text-xs text-[var(--color-text-tertiary)]">
                                    No users yet. Use "New User" below to add the first one.
                                </div>
                            )}
                        </div>

                        <div className="border-t border-[var(--color-border)] py-1">
                            {hasUsers && (
                                <button
                                    type="button"
                                    onClick={() => { void handleLock(); }}
                                    disabled={isLoading}
                                    className="flex w-full items-center gap-2 px-3 py-2 text-left text-xs text-[var(--color-text-primary)] transition-colors hover:bg-[var(--color-bg-tertiary)] disabled:opacity-50"
                                >
                                    <Lock size={14} className="text-[var(--color-text-secondary)]" />
                                    Lock
                                </button>
                            )}
                            <button
                                type="button"
                                onClick={() => {
                                    setShowManagePanel(true);
                                    setIsOpen(false);
                                }}
                                className="flex w-full items-center gap-2 px-3 py-2 text-left text-xs text-[var(--color-text-primary)] transition-colors hover:bg-[var(--color-bg-tertiary)]"
                            >
                                <Plus size={14} className="text-[var(--color-text-secondary)]" />
                                New User
                            </button>
                            <button
                                type="button"
                                onClick={() => {
                                    setShowManagePanel(true);
                                    setIsOpen(false);
                                }}
                                className="flex w-full items-center gap-2 px-3 py-2 text-left text-xs text-[var(--color-text-primary)] transition-colors hover:bg-[var(--color-bg-tertiary)]"
                            >
                                <UserCog size={14} className="text-[var(--color-text-secondary)]" />
                                Manage Users
                            </button>
                        </div>
                    </div>
                )}
            </div>

            {pendingUnlockUser && (
                <div className="fixed inset-0 z-[100] flex items-center justify-center bg-black/50 backdrop-blur-sm">
                    <form
                        onSubmit={handleUnlockSubmit}
                        className="w-full max-w-md rounded-lg border border-gray-200 bg-white p-4 shadow-2xl dark:border-gray-700 dark:bg-gray-800"
                    >
                        <div className="mb-4 flex items-center justify-between">
                            <div className="flex min-w-0 items-center gap-2">
                                <UserAvatar
                                    name={pendingUnlockUser.name}
                                    avatarEmoji={pendingUnlockUser.avatarEmoji}
                                    avatarColor={pendingUnlockUser.avatarColor}
                                    size="md"
                                />
                                <div className="min-w-0">
                                    <div className="truncate text-sm font-medium text-gray-900 dark:text-gray-100">
                                        {pendingUnlockUser.name}
                                    </div>
                                    <div className="flex items-center gap-1 text-xs text-gray-500 dark:text-gray-400">
                                        <KeyRound size={12} />
                                        Account Password
                                    </div>
                                </div>
                            </div>
                            <button
                                type="button"
                                onClick={() => {
                                    setPendingUnlockUser(null);
                                    setUnlockPassphrase('');
                                    setError('');
                                }}
                                className="flex h-8 w-8 items-center justify-center rounded-lg text-gray-500 transition-colors hover:bg-gray-100 hover:text-gray-800 dark:hover:bg-gray-700 dark:hover:text-gray-100"
                                aria-label="Close"
                            >
                                <X size={16} />
                            </button>
                        </div>
                        {error && (
                            <div className="mb-3 rounded-lg border border-red-200 bg-red-50 px-3 py-2 text-xs text-red-700 dark:border-red-900/60 dark:bg-red-950/30 dark:text-red-300">
                                {error}
                            </div>
                        )}
                        <div className="relative">
                            <input
                                type={showUnlockPassphrase ? 'text' : 'password'}
                                value={unlockPassphrase}
                                onChange={(event) => setUnlockPassphrase(event.target.value)}
                                autoFocus
                                placeholder="Password"
                                autoComplete="current-password"
                                className="w-full rounded-lg border border-gray-300 bg-white px-3 py-2 pr-10 text-sm text-gray-900 dark:border-gray-600 dark:bg-gray-900 dark:text-gray-100"
                            />
                            <button
                                type="button"
                                onClick={() => setShowUnlockPassphrase((value) => !value)}
                                className="absolute right-3 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-700 dark:hover:text-gray-200"
                                aria-label={showUnlockPassphrase ? 'Hide password' : 'Show password'}
                            >
                                {showUnlockPassphrase ? <EyeOff size={16} /> : <Eye size={16} />}
                            </button>
                        </div>
                        <button
                            type="submit"
                            disabled={isLoading || !unlockPassphrase}
                            className="mt-3 flex h-9 w-full items-center justify-center gap-2 rounded-lg bg-blue-600 text-sm font-medium text-white transition-colors hover:bg-blue-700 disabled:cursor-not-allowed disabled:bg-blue-300"
                        >
                            {isLoading ? (
                                <>
                                    <Loader2 size={15} className="animate-spin" />
                                    <span className="opacity-90">Unlocking...</span>
                                </>
                            ) : (
                                <>
                                    <Shield size={15} />
                                    Unlock
                                </>
                            )}
                        </button>
                        <p className="mt-3 text-center text-[11px] text-gray-500 dark:text-gray-400">
                            Your account is protected with Argon2id + AES-KW + AES-256-GCM encryption
                        </p>
                    </form>
                </div>
            )}

            <UsersManagePanel
                isOpen={showManagePanel}
                onClose={() => {
                    setShowManagePanel(false);
                    void refresh();
                }}
                onChanged={() => {
                    void refresh();
                    onUsersChanged?.();
                }}
            />
        </>
    );
};

export default UserDropdown;
