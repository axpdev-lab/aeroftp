// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import {
    AlertTriangle,
    Check,
    Eye,
    EyeOff,
    GripVertical,
    KeyRound,
    Lock,
    LockOpen,
    Pencil,
    Plus,
    Shield,
    Trash2,
    X,
} from 'lucide-react';
import { UserAvatar } from './UserAvatar';
import {
    addUser,
    changeUserPassphrase,
    deleteUser,
    getUserStorageStats,
    initUserPartitions,
    listUsers,
    renameUser,
    reorderUsers,
    type UserMetadata,
    type UserStorageStats,
} from '../utils/userPartitions';
import { PROFILES_CHANGED_EVENT } from '../utils/serverProfileStore';

interface UsersManagePanelProps {
    isOpen: boolean;
    onClose: () => void;
    onChanged?: () => void;
}

const AVATAR_CHOICES = ['D', 'A', 'S', 'M', 'P', '⚡', '🔐', '☁️'];
const COLOR_CHOICES = ['#3b82f6', '#10b981', '#f59e0b', '#ef4444', '#8b5cf6', '#06b6d4', '#64748b', '#ec4899'];

const formatBytes = (bytes: number): string => {
    if (bytes < 1024) return `${bytes} B`;
    const kib = bytes / 1024;
    if (kib < 1024) return `${kib.toFixed(1)} KiB`;
    return `${(kib / 1024).toFixed(1)} MiB`;
};

const formatLastUnlocked = (timestamp?: number | null): string => {
    if (!timestamp) return 'Never';
    return new Date(timestamp).toLocaleString();
};

const notifyProfilesChanged = (onChanged?: () => void) => {
    try {
        window.dispatchEvent(new CustomEvent(PROFILES_CHANGED_EVENT));
    } catch {
        // Browserless tests: best effort.
    }
    onChanged?.();
};

export const UsersManagePanel: React.FC<UsersManagePanelProps> = ({ isOpen, onClose, onChanged }) => {
    const [users, setUsers] = React.useState<UserMetadata[]>([]);
    const [stats, setStats] = React.useState<UserStorageStats[]>([]);
    const [loading, setLoading] = React.useState(false);
    const [busyUserId, setBusyUserId] = React.useState<number | null>(null);
    const [error, setError] = React.useState('');
    const [newName, setNewName] = React.useState('');
    const [newAvatar, setNewAvatar] = React.useState(AVATAR_CHOICES[0]);
    const [newColor, setNewColor] = React.useState(COLOR_CHOICES[0]);
    const [newPassphrase, setNewPassphrase] = React.useState('');
    const [showNewPassphrase, setShowNewPassphrase] = React.useState(false);
    const [editingUserId, setEditingUserId] = React.useState<number | null>(null);
    const [editingName, setEditingName] = React.useState('');
    const [passphraseForm, setPassphraseForm] = React.useState<{
        userId: number;
        oldPassphrase: string;
        newPassphrase: string;
        showOld: boolean;
        showNew: boolean;
    } | null>(null);
    const [draggingUserId, setDraggingUserId] = React.useState<number | null>(null);
    // No-recovery acknowledgement gates submit when an account password is
    // being set for the first time (add-user with passphrase, or set
    // passphrase on an existing user). MU-LS gate decision: warn at setup,
    // not on every prompt.
    const [acknowledgeNoRecoveryNew, setAcknowledgeNoRecoveryNew] = React.useState(false);
    const [acknowledgeNoRecoveryForm, setAcknowledgeNoRecoveryForm] = React.useState(false);

    const statsByUserId = React.useMemo(() => {
        const map = new Map<number, UserStorageStats>();
        for (const item of stats) map.set(item.userId, item);
        return map;
    }, [stats]);

    const refresh = React.useCallback(async () => {
        setLoading(true);
        setError('');
        try {
            await initUserPartitions();
            const [nextUsers, nextStats] = await Promise.all([
                listUsers(),
                getUserStorageStats(),
            ]);
            setUsers(nextUsers);
            setStats(nextStats);
        } catch (err) {
            setError(String(err));
        } finally {
            setLoading(false);
        }
    }, []);

    React.useEffect(() => {
        if (isOpen) void refresh();
    }, [isOpen, refresh]);

    React.useEffect(() => {
        if (!isOpen) return;
        const handleKey = (event: KeyboardEvent) => {
            if (event.key === 'Escape') onClose();
        };
        window.addEventListener('keydown', handleKey);
        return () => window.removeEventListener('keydown', handleKey);
    }, [isOpen, onClose]);

    if (!isOpen) return null;

    const handleAddUser = async (event: React.FormEvent) => {
        event.preventDefault();
        if (!newName.trim()) return;
        if (newPassphrase && !acknowledgeNoRecoveryNew) {
            setError('Please acknowledge that account passwords cannot be recovered.');
            return;
        }
        setError('');
        try {
            await addUser(newName.trim(), newAvatar, newColor, newPassphrase || null);
            setNewName('');
            setNewPassphrase('');
            setAcknowledgeNoRecoveryNew(false);
            setNewAvatar(AVATAR_CHOICES[0]);
            setNewColor(COLOR_CHOICES[0]);
            await refresh();
            notifyProfilesChanged(onChanged);
        } catch (err) {
            setError(String(err));
        }
    };

    const startRename = (user: UserMetadata) => {
        setEditingUserId(user.id);
        setEditingName(user.name);
        setError('');
    };

    const commitRename = async (user: UserMetadata) => {
        const name = editingName.trim();
        if (!name || name === user.name) {
            setEditingUserId(null);
            return;
        }
        setBusyUserId(user.id);
        setError('');
        try {
            await renameUser(user.id, name);
            setEditingUserId(null);
            await refresh();
            notifyProfilesChanged(onChanged);
        } catch (err) {
            setError(String(err));
        } finally {
            setBusyUserId(null);
        }
    };

    const handleDelete = async (user: UserMetadata) => {
        if (user.isActive || users.length <= 1) return;
        const userStats = statsByUserId.get(user.id);
        const profileCount = userStats?.profileCount ?? 0;
        const settingsCount = userStats?.settingsCount ?? 0;
        const confirmed = window.confirm(
            `Delete account "${user.name}"?\n\nEncrypted payloads to remove: ${profileCount} profiles, ${settingsCount} settings scopes.`,
        );
        if (!confirmed) return;
        setBusyUserId(user.id);
        setError('');
        try {
            await deleteUser(user.id);
            await refresh();
            notifyProfilesChanged(onChanged);
        } catch (err) {
            setError(String(err));
        } finally {
            setBusyUserId(null);
        }
    };

    const handlePassphraseSubmit = async (event: React.FormEvent, user: UserMetadata) => {
        event.preventDefault();
        if (!passphraseForm || passphraseForm.userId !== user.id) return;
        if (user.hasPassphrase && !passphraseForm.oldPassphrase) {
            setError('Current account password is required.');
            return;
        }
        if (!user.hasPassphrase && !passphraseForm.newPassphrase) {
            setError('New account password is required.');
            return;
        }
        if (user.hasPassphrase && !passphraseForm.newPassphrase) {
            const confirmed = window.confirm(`Remove the account password for "${user.name}"?`);
            if (!confirmed) return;
        }
        // First-time setup requires explicit acknowledgement.
        if (!user.hasPassphrase && passphraseForm.newPassphrase && !acknowledgeNoRecoveryForm) {
            setError('Please acknowledge that account passwords cannot be recovered.');
            return;
        }
        setBusyUserId(user.id);
        setError('');
        try {
            await changeUserPassphrase(
                user.id,
                user.hasPassphrase ? passphraseForm.oldPassphrase : null,
                passphraseForm.newPassphrase || null,
            );
            setPassphraseForm(null);
            setAcknowledgeNoRecoveryForm(false);
            await refresh();
            notifyProfilesChanged(onChanged);
        } catch (err) {
            setError(String(err));
        } finally {
            setBusyUserId(null);
        }
    };

    const handleDrop = async (targetUserId: number) => {
        if (!draggingUserId || draggingUserId === targetUserId) {
            setDraggingUserId(null);
            return;
        }
        const source = users.find((user) => user.id === draggingUserId);
        if (!source) return;
        const next = users.filter((user) => user.id !== draggingUserId);
        const targetIndex = Math.max(0, next.findIndex((user) => user.id === targetUserId));
        next.splice(targetIndex, 0, source);
        setUsers(next);
        setDraggingUserId(null);
        setError('');
        try {
            await reorderUsers(next.map((user) => user.id));
            await refresh();
            notifyProfilesChanged(onChanged);
        } catch (err) {
            setError(String(err));
            await refresh();
        }
    };

    return (
        <div className="fixed inset-0 z-[80] flex items-start justify-center pt-[7vh]">
            <button
                type="button"
                aria-label="Close users panel"
                className="absolute inset-0 bg-black/50 backdrop-blur-sm"
                onClick={onClose}
            />
            <div className="relative w-full max-w-3xl max-h-[84vh] overflow-hidden rounded-lg border border-gray-200 bg-white shadow-2xl dark:border-gray-700 dark:bg-gray-800">
                <div className="flex items-center justify-between border-b border-gray-200 px-4 py-3 dark:border-gray-700">
                    <div className="flex items-center gap-2">
                        <Shield size={18} className="text-blue-500" />
                        <h2 className="text-sm font-semibold text-gray-900 dark:text-gray-100">Manage Users</h2>
                    </div>
                    <button
                        type="button"
                        onClick={onClose}
                        className="flex h-8 w-8 items-center justify-center rounded-lg text-gray-500 transition-colors hover:bg-gray-100 hover:text-gray-800 dark:hover:bg-gray-700 dark:hover:text-gray-100"
                        aria-label="Close"
                    >
                        <X size={17} />
                    </button>
                </div>

                <div className="max-h-[calc(84vh-57px)] overflow-y-auto p-4">
                    {error && (
                        <div className="mb-3 rounded-lg border border-red-200 bg-red-50 px-3 py-2 text-xs text-red-700 dark:border-red-900/60 dark:bg-red-950/30 dark:text-red-300">
                            {error}
                        </div>
                    )}

                    <form onSubmit={handleAddUser} className="mb-4 rounded-lg border border-gray-200 bg-gray-50 p-3 dark:border-gray-700 dark:bg-gray-900/40">
                        <div className="grid gap-3 md:grid-cols-[1fr_auto]">
                            <div className="grid gap-2 sm:grid-cols-[minmax(0,1fr)_auto]">
                                <input
                                    value={newName}
                                    onChange={(event) => setNewName(event.target.value)}
                                    placeholder="Account name"
                                    className="min-w-0 rounded-lg border border-gray-300 bg-white px-3 py-2 text-sm text-gray-900 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-100"
                                />
                                <div className="flex items-center gap-1">
                                    <select
                                        value={newAvatar}
                                        onChange={(event) => setNewAvatar(event.target.value)}
                                        className="h-9 rounded-lg border border-gray-300 bg-white px-2 text-sm dark:border-gray-600 dark:bg-gray-800"
                                        aria-label="Avatar"
                                    >
                                        {AVATAR_CHOICES.map((avatar) => (
                                            <option key={avatar} value={avatar}>{avatar}</option>
                                        ))}
                                    </select>
                                    <div className="flex items-center gap-1">
                                        {COLOR_CHOICES.map((color) => (
                                            <button
                                                key={color}
                                                type="button"
                                                onClick={() => setNewColor(color)}
                                                className={`h-7 w-7 rounded-full border-2 ${newColor === color ? 'border-gray-900 dark:border-white' : 'border-transparent'}`}
                                                style={{ backgroundColor: color }}
                                                aria-label={color}
                                            />
                                        ))}
                                    </div>
                                </div>
                            </div>
                            <button
                                type="submit"
                                disabled={!newName.trim()}
                                className="inline-flex h-9 items-center justify-center gap-1.5 rounded-lg bg-blue-600 px-3 text-sm font-medium text-white transition-colors hover:bg-blue-700 disabled:cursor-not-allowed disabled:bg-blue-300"
                            >
                                <Plus size={15} />
                                Add
                            </button>
                        </div>
                        <div className="mt-2 grid gap-2 sm:grid-cols-[minmax(0,1fr)_auto]">
                            <div className="relative">
                                <input
                                    type={showNewPassphrase ? 'text' : 'password'}
                                    value={newPassphrase}
                                    onChange={(event) => setNewPassphrase(event.target.value)}
                                    placeholder="Account password (optional)"
                                    className="w-full rounded-lg border border-gray-300 bg-white px-3 py-2 pr-9 text-sm text-gray-900 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-100"
                                    autoComplete="new-password"
                                />
                                <button
                                    type="button"
                                    onClick={() => setShowNewPassphrase((value) => !value)}
                                    className="absolute right-2 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-700 dark:hover:text-gray-200"
                                    aria-label={showNewPassphrase ? 'Hide password' : 'Show password'}
                                >
                                    {showNewPassphrase ? <EyeOff size={15} /> : <Eye size={15} />}
                                </button>
                            </div>
                            <div className="flex items-center gap-2 text-xs text-gray-500 dark:text-gray-400">
                                <KeyRound size={14} />
                                No recovery
                            </div>
                        </div>
                        {newPassphrase && (
                            <div className="mt-3 rounded-lg border border-amber-300 bg-amber-50 p-3 text-xs text-amber-900 dark:border-amber-900/60 dark:bg-amber-950/30 dark:text-amber-200">
                                <div className="mb-1.5 flex items-start gap-2 font-medium">
                                    <AlertTriangle size={14} className="mt-0.5 flex-shrink-0" />
                                    <span>Account password cannot be recovered.</span>
                                </div>
                                <p className="mb-2 leading-relaxed opacity-90">
                                    If you forget this password, the account&apos;s saved servers, settings, and history are permanently inaccessible. Store the password somewhere safe before continuing.
                                </p>
                                <label className="flex items-start gap-2 cursor-pointer select-none">
                                    <input
                                        type="checkbox"
                                        checked={acknowledgeNoRecoveryNew}
                                        onChange={(event) => setAcknowledgeNoRecoveryNew(event.target.checked)}
                                        className="mt-0.5 h-3.5 w-3.5"
                                    />
                                    <span>I understand and accept the responsibility for this password.</span>
                                </label>
                            </div>
                        )}
                    </form>

                    <div className="space-y-2">
                        {loading && users.length === 0 ? (
                            <div className="rounded-lg border border-gray-200 p-4 text-sm text-gray-500 dark:border-gray-700 dark:text-gray-400">
                                Loading users...
                            </div>
                        ) : users.map((user) => {
                            const userStats = statsByUserId.get(user.id);
                            const isEditing = editingUserId === user.id;
                            const passphraseOpen = passphraseForm?.userId === user.id;
                            return (
                                <div
                                    key={user.id}
                                    draggable
                                    onDragStart={() => setDraggingUserId(user.id)}
                                    onDragOver={(event) => event.preventDefault()}
                                    onDrop={() => { void handleDrop(user.id); }}
                                    onKeyDown={(event) => {
                                        if (event.key === 'F2') {
                                            event.preventDefault();
                                            startRename(user);
                                        }
                                    }}
                                    tabIndex={0}
                                    className={`rounded-lg border bg-white p-3 outline-none transition-colors focus-visible:ring-2 focus-visible:ring-blue-500 dark:bg-gray-800 ${
                                        user.isActive
                                            ? 'border-blue-300 dark:border-blue-700'
                                            : 'border-gray-200 dark:border-gray-700'
                                    }`}
                                >
                                    <div className="flex items-center gap-3">
                                        <GripVertical size={16} className="shrink-0 cursor-grab text-gray-400" />
                                        <UserAvatar name={user.name} avatarEmoji={user.avatarEmoji} avatarColor={user.avatarColor} size="lg" />
                                        <div className="min-w-0 flex-1">
                                            {isEditing ? (
                                                <form
                                                    onSubmit={(event) => {
                                                        event.preventDefault();
                                                        void commitRename(user);
                                                    }}
                                                    className="flex items-center gap-2"
                                                >
                                                    <input
                                                        value={editingName}
                                                        onChange={(event) => setEditingName(event.target.value)}
                                                        autoFocus
                                                        onBlur={() => { void commitRename(user); }}
                                                        className="min-w-0 rounded-lg border border-gray-300 bg-white px-2 py-1 text-sm text-gray-900 dark:border-gray-600 dark:bg-gray-900 dark:text-gray-100"
                                                    />
                                                    <button
                                                        type="submit"
                                                        className="flex h-7 w-7 items-center justify-center rounded-lg bg-blue-600 text-white"
                                                        aria-label="Save name"
                                                    >
                                                        <Check size={14} />
                                                    </button>
                                                </form>
                                            ) : (
                                                <div className="flex min-w-0 items-center gap-2">
                                                    <span className="truncate text-sm font-medium text-gray-900 dark:text-gray-100">{user.name}</span>
                                                    {user.isActive && (
                                                        <span className="rounded-full bg-blue-100 px-2 py-0.5 text-[11px] font-medium text-blue-700 dark:bg-blue-900/40 dark:text-blue-300">
                                                            Active
                                                        </span>
                                                    )}
                                                    {user.hasPassphrase ? (
                                                        <Lock size={13} className="text-emerald-600 dark:text-emerald-400" />
                                                    ) : (
                                                        <LockOpen size={13} className="text-gray-400" />
                                                    )}
                                                </div>
                                            )}
                                            <div className="mt-1 flex flex-wrap gap-x-3 gap-y-1 text-[11px] text-gray-500 dark:text-gray-400">
                                                <span>{userStats?.profileCount ?? 0} profiles</span>
                                                <span>{userStats?.settingsCount ?? 0} settings</span>
                                                <span>{formatBytes(userStats?.encryptedBytes ?? 0)}</span>
                                                <span>{formatLastUnlocked(user.lastUnlockedAt)}</span>
                                            </div>
                                        </div>
                                        <div className="flex shrink-0 items-center gap-1">
                                            <button
                                                type="button"
                                                onClick={() => startRename(user)}
                                                disabled={busyUserId === user.id}
                                                className="flex h-8 w-8 items-center justify-center rounded-lg text-gray-500 transition-colors hover:bg-gray-100 hover:text-gray-800 disabled:opacity-50 dark:hover:bg-gray-700 dark:hover:text-gray-100"
                                                title="Rename"
                                            >
                                                <Pencil size={15} />
                                            </button>
                                            <button
                                                type="button"
                                                onClick={() => setPassphraseForm(passphraseOpen ? null : {
                                                    userId: user.id,
                                                    oldPassphrase: '',
                                                    newPassphrase: '',
                                                    showOld: false,
                                                    showNew: false,
                                                })}
                                                disabled={busyUserId === user.id}
                                                className="flex h-8 w-8 items-center justify-center rounded-lg text-gray-500 transition-colors hover:bg-gray-100 hover:text-gray-800 disabled:opacity-50 dark:hover:bg-gray-700 dark:hover:text-gray-100"
                                                title={user.hasPassphrase ? 'Change password' : 'Set password'}
                                            >
                                                <KeyRound size={15} />
                                            </button>
                                            <button
                                                type="button"
                                                onClick={() => { void handleDelete(user); }}
                                                disabled={busyUserId === user.id || user.isActive || users.length <= 1}
                                                className="flex h-8 w-8 items-center justify-center rounded-lg text-gray-500 transition-colors hover:bg-red-50 hover:text-red-600 disabled:cursor-not-allowed disabled:opacity-40 dark:hover:bg-red-950/30"
                                                title="Delete"
                                            >
                                                <Trash2 size={15} />
                                            </button>
                                        </div>
                                    </div>

                                    {passphraseOpen && passphraseForm && (
                                        <form onSubmit={(event) => { void handlePassphraseSubmit(event, user); }} className="mt-3 grid gap-2 border-t border-gray-200 pt-3 dark:border-gray-700 sm:grid-cols-[1fr_1fr_auto]">
                                            {user.hasPassphrase && (
                                                <div className="relative">
                                                    <input
                                                        type={passphraseForm.showOld ? 'text' : 'password'}
                                                        value={passphraseForm.oldPassphrase}
                                                        onChange={(event) => setPassphraseForm({ ...passphraseForm, oldPassphrase: event.target.value })}
                                                        placeholder="Current password"
                                                        className="w-full rounded-lg border border-gray-300 bg-white px-3 py-2 pr-9 text-sm dark:border-gray-600 dark:bg-gray-900"
                                                        autoComplete="current-password"
                                                    />
                                                    <button
                                                        type="button"
                                                        onClick={() => setPassphraseForm({ ...passphraseForm, showOld: !passphraseForm.showOld })}
                                                        className="absolute right-2 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-700 dark:hover:text-gray-200"
                                                        aria-label={passphraseForm.showOld ? 'Hide password' : 'Show password'}
                                                    >
                                                        {passphraseForm.showOld ? <EyeOff size={15} /> : <Eye size={15} />}
                                                    </button>
                                                </div>
                                            )}
                                            <div className="relative">
                                                <input
                                                    type={passphraseForm.showNew ? 'text' : 'password'}
                                                    value={passphraseForm.newPassphrase}
                                                    onChange={(event) => setPassphraseForm({ ...passphraseForm, newPassphrase: event.target.value })}
                                                    placeholder={user.hasPassphrase ? 'New password (blank removes)' : 'New password'}
                                                    className="w-full rounded-lg border border-gray-300 bg-white px-3 py-2 pr-9 text-sm dark:border-gray-600 dark:bg-gray-900"
                                                    autoComplete="new-password"
                                                />
                                                <button
                                                    type="button"
                                                    onClick={() => setPassphraseForm({ ...passphraseForm, showNew: !passphraseForm.showNew })}
                                                    className="absolute right-2 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-700 dark:hover:text-gray-200"
                                                    aria-label={passphraseForm.showNew ? 'Hide password' : 'Show password'}
                                                >
                                                    {passphraseForm.showNew ? <EyeOff size={15} /> : <Eye size={15} />}
                                                </button>
                                            </div>
                                            <div className="flex items-center gap-2">
                                                <button
                                                    type="submit"
                                                    disabled={busyUserId === user.id}
                                                    className="inline-flex h-9 items-center gap-1.5 rounded-lg bg-blue-600 px-3 text-sm font-medium text-white transition-colors hover:bg-blue-700 disabled:bg-blue-300"
                                                >
                                                    <Check size={15} />
                                                    Save
                                                </button>
                                                <button
                                                    type="button"
                                                    onClick={() => { setPassphraseForm(null); setAcknowledgeNoRecoveryForm(false); }}
                                                    className="flex h-9 w-9 items-center justify-center rounded-lg bg-gray-100 text-gray-600 transition-colors hover:bg-gray-200 dark:bg-gray-700 dark:text-gray-200 dark:hover:bg-gray-600"
                                                    aria-label="Cancel password edit"
                                                >
                                                    <X size={15} />
                                                </button>
                                            </div>
                                            {!user.hasPassphrase && passphraseForm.newPassphrase && (
                                                <div className="sm:col-span-3 rounded-lg border border-amber-300 bg-amber-50 p-3 text-xs text-amber-900 dark:border-amber-900/60 dark:bg-amber-950/30 dark:text-amber-200">
                                                    <div className="mb-1.5 flex items-start gap-2 font-medium">
                                                        <AlertTriangle size={14} className="mt-0.5 flex-shrink-0" />
                                                        <span>Account password cannot be recovered.</span>
                                                    </div>
                                                    <p className="mb-2 leading-relaxed opacity-90">
                                                        If you forget this password, the account&apos;s saved servers, settings, and history are permanently inaccessible. Store the password somewhere safe before continuing.
                                                    </p>
                                                    <label className="flex items-start gap-2 cursor-pointer select-none">
                                                        <input
                                                            type="checkbox"
                                                            checked={acknowledgeNoRecoveryForm}
                                                            onChange={(event) => setAcknowledgeNoRecoveryForm(event.target.checked)}
                                                            className="mt-0.5 h-3.5 w-3.5"
                                                        />
                                                        <span>I understand and accept the responsibility for this password.</span>
                                                    </label>
                                                </div>
                                            )}
                                        </form>
                                    )}
                                </div>
                            );
                        })}
                    </div>
                </div>
            </div>
        </div>
    );
};

export default UsersManagePanel;
