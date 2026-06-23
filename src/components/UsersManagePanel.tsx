// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import {
    AlertTriangle,
    Check,
    Eye,
    EyeOff,
    GripVertical,
    Info,
    KeyRound,
    Loader2,
    Lock,
    LockOpen,
    Palette,
    Pencil,
    Plus,
    RotateCcw,
    Shield,
    ShieldCheck,
    Star,
    Trash2,
    X,
} from 'lucide-react';
import { UserAvatar } from './UserAvatar';
import { IconPickerDialog } from './IconPickerDialog';
import { DestructiveResetDialog } from './UsersAdmin/DestructiveResetDialog';
import { PasswordStrengthBar } from './vault/PasswordStrengthBar';
import { PasswordMatchHint } from './common/PasswordMatchHint';
import {
    addUser,
    changeUserPassphrase,
    deleteUser,
    getUserStorageStats,
    getUnlockStatus,
    initUserPartitions,
    listUsers,
    readDefaultAccountId,
    renameUser,
    reorderUsers,
    setUserAdmin,
    setUserAvatar,
    writeDefaultAccountId,
    type UserMetadata,
    type UserStorageStats,
    type UserUnlockStatus,
} from '../utils/userPartitions';
import { PROFILES_CHANGED_EVENT } from '../utils/serverProfileStore';
import { mapUserPartitionError } from '../utils/userPartitionErrors';
import { useTranslation } from '../i18n';
import { useDraggableModal } from '../hooks/useDraggableModal';

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

const formatLastUnlocked = (timestamp: number | null | undefined, neverLabel: string): string => {
    if (!timestamp) return neverLabel;
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
    const t = useTranslation();
    const modalDrag = useDraggableModal();
    const [users, setUsers] = React.useState<UserMetadata[]>([]);
    const [stats, setStats] = React.useState<UserStorageStats[]>([]);
    const [unlockStatus, setUnlockStatus] = React.useState<UserUnlockStatus | null>(null);
    const [loading, setLoading] = React.useState(false);
    const [busyUserId, setBusyUserId] = React.useState<number | null>(null);
    const [error, setError] = React.useState('');
    const [newName, setNewName] = React.useState('');
    const [newAvatar, setNewAvatar] = React.useState(AVATAR_CHOICES[0]);
    const [newColor, setNewColor] = React.useState(COLOR_CHOICES[0]);
    const [newPassphrase, setNewPassphrase] = React.useState('');
    const [newConfirmPassphrase, setNewConfirmPassphrase] = React.useState('');
    const [showNewPassphrase, setShowNewPassphrase] = React.useState(false);
    const [editingUserId, setEditingUserId] = React.useState<number | null>(null);
    const [editingName, setEditingName] = React.useState('');
    const [passphraseForm, setPassphraseForm] = React.useState<{
        userId: number;
        oldPassphrase: string;
        newPassphrase: string;
        confirmNewPassphrase: string;
        showOld: boolean;
        showNew: boolean;
    } | null>(null);
    const [draggingUserId, setDraggingUserId] = React.useState<number | null>(null);
    const [resetTarget, setResetTarget] = React.useState<UserMetadata | null>(null);
    const [avatarEditingUser, setAvatarEditingUser] = React.useState<UserMetadata | null>(null);
    const [avatarDraftEmoji, setAvatarDraftEmoji] = React.useState('');
    const [avatarDraftColor, setAvatarDraftColor] = React.useState(COLOR_CHOICES[0]);
    const [avatarSaving, setAvatarSaving] = React.useState(false);
    const [showIconPicker, setShowIconPicker] = React.useState(false);
    // N3 (#270): the default account that skips the welcome screen on next
    // boot. Mirrors the AccountLockScreen checkbox and shares the same
    // localStorage helpers; only applies to password-free accounts.
    const [defaultAccountId, setDefaultAccountId] = React.useState<number | null>(() => readDefaultAccountId());
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
    const selfId = unlockStatus?.unlockedUserId ?? null;
    const currentUser = React.useMemo(
        () => users.find((user) => user.id === selfId) ?? null,
        [selfId, users],
    );
    const currentUserIsAdmin = !!currentUser?.isAdmin;
    const adminCount = React.useMemo(
        () => users.filter((user) => user.isAdmin).length,
        [users],
    );

    const refresh = React.useCallback(async () => {
        setLoading(true);
        setError('');
        try {
            await initUserPartitions();
            const [nextUsers, nextStats, nextStatus] = await Promise.all([
                listUsers(),
                getUserStorageStats(),
                getUnlockStatus(),
            ]);
            setUsers(nextUsers);
            setStats(nextStats);
            setUnlockStatus(nextStatus);
            // Reconcile the stored default against reality: a deleted or now
            // passphrase-protected account must not stay the silent-boot target.
            const storedDefault = readDefaultAccountId();
            const validDefault = nextUsers.find(
                (u) => u.id === storedDefault && !u.hasPassphrase,
            );
            if (storedDefault != null && !validDefault) {
                writeDefaultAccountId(null);
                setDefaultAccountId(null);
            } else {
                setDefaultAccountId(storedDefault);
            }
        } catch (err) {
            setError(mapUserPartitionError(err, t));
        } finally {
            setLoading(false);
        }
    }, [t]);

    const toggleDefaultAccount = React.useCallback((user: UserMetadata) => {
        // Only password-free accounts can skip the welcome screen (a protected
        // account always shows its prompt), matching the AccountLockScreen rule.
        if (user.hasPassphrase) return;
        setDefaultAccountId((current) => {
            const next = current === user.id ? null : user.id;
            writeDefaultAccountId(next);
            return next;
        });
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
            setError(t('manageUsers.errAckRequired'));
            return;
        }
        // #322: a mistyped no-recovery password would lock the account out, so
        // require the confirm to match when a password is being set.
        if (newPassphrase && newConfirmPassphrase !== newPassphrase) {
            setError(t('password.mismatch'));
            return;
        }
        setError('');
        try {
            await addUser(newName.trim(), newAvatar, newColor, newPassphrase || null);
            setNewName('');
            setNewPassphrase('');
            setNewConfirmPassphrase('');
            setAcknowledgeNoRecoveryNew(false);
            setNewAvatar(AVATAR_CHOICES[0]);
            setNewColor(COLOR_CHOICES[0]);
            await refresh();
            notifyProfilesChanged(onChanged);
        } catch (err) {
            setError(mapUserPartitionError(err, t));
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
            setError(mapUserPartitionError(err, t));
        } finally {
            setBusyUserId(null);
        }
    };

    const handleDelete = async (user: UserMetadata) => {
        const isSelf = user.id === selfId;
        const canModify = currentUserIsAdmin || isSelf;
        const isLastAdmin = user.isAdmin && adminCount <= 1;
        if (!canModify || user.isActive || users.length <= 1 || isLastAdmin) return;
        const userStats = statsByUserId.get(user.id);
        const profileCount = userStats?.profileCount ?? 0;
        const settingsCount = userStats?.settingsCount ?? 0;
        const confirmed = window.confirm(
            t('manageUsers.confirmDelete')
                .replace('{name}', user.name)
                .replace('{profiles}', String(profileCount))
                .replace('{settings}', String(settingsCount)),
        );
        if (!confirmed) return;
        setBusyUserId(user.id);
        setError('');
        try {
            await deleteUser(user.id);
            await refresh();
            notifyProfilesChanged(onChanged);
        } catch (err) {
            setError(mapUserPartitionError(err, t));
        } finally {
            setBusyUserId(null);
        }
    };

    const handlePassphraseSubmit = async (event: React.FormEvent, user: UserMetadata) => {
        event.preventDefault();
        if (!passphraseForm || passphraseForm.userId !== user.id) return;
        if (user.hasPassphrase && !passphraseForm.oldPassphrase) {
            setError(t('manageUsers.errCurrentRequired'));
            return;
        }
        if (!user.hasPassphrase && !passphraseForm.newPassphrase) {
            setError(t('manageUsers.errNewRequired'));
            return;
        }
        if (user.hasPassphrase && !passphraseForm.newPassphrase) {
            const confirmed = window.confirm(
                t('manageUsers.confirmRemovePassword').replace('{name}', user.name),
            );
            if (!confirmed) return;
        }
        // First-time setup requires explicit acknowledgement.
        if (!user.hasPassphrase && passphraseForm.newPassphrase && !acknowledgeNoRecoveryForm) {
            setError(t('manageUsers.errAckRequired'));
            return;
        }
        // #322: when a new password is being set, its confirm must match.
        if (passphraseForm.newPassphrase && passphraseForm.confirmNewPassphrase !== passphraseForm.newPassphrase) {
            setError(t('password.mismatch'));
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
            setError(mapUserPartitionError(err, t));
        } finally {
            setBusyUserId(null);
        }
    };

    const handleSetAdmin = async (user: UserMetadata, isAdmin: boolean) => {
        setBusyUserId(user.id);
        setError('');
        try {
            await setUserAdmin(user.id, isAdmin);
            await refresh();
            notifyProfilesChanged(onChanged);
        } catch (err) {
            setError(mapUserPartitionError(err, t));
        } finally {
            setBusyUserId(null);
        }
    };

    const handleResetComplete = async () => {
        setResetTarget(null);
        await refresh();
        notifyProfilesChanged(onChanged);
    };

    const openAvatarEditor = (user: UserMetadata) => {
        setAvatarEditingUser(user);
        setAvatarDraftEmoji(user.avatarEmoji || user.name.trim()[0]?.toUpperCase() || 'U');
        setAvatarDraftColor(user.avatarColor || COLOR_CHOICES[0]);
        setShowIconPicker(false);
        setError('');
    };

    const handleAvatarSave = async () => {
        if (!avatarEditingUser) return;
        setAvatarSaving(true);
        setBusyUserId(avatarEditingUser.id);
        setError('');
        try {
            await setUserAvatar(avatarEditingUser.id, avatarDraftEmoji || null, avatarDraftColor || null);
            setAvatarEditingUser(null);
            setShowIconPicker(false);
            await refresh();
            notifyProfilesChanged(onChanged);
        } catch (err) {
            setError(mapUserPartitionError(err, t));
        } finally {
            setAvatarSaving(false);
            setBusyUserId(null);
        }
    };

    const handleDrop = async (targetUserId: number) => {
        if (!currentUserIsAdmin) {
            setDraggingUserId(null);
            return;
        }
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
            setError(mapUserPartitionError(err, t));
            await refresh();
        }
    };

    return (
        <div className="fixed inset-0 z-[80] flex items-start justify-center pt-[7vh]">
            <button
                type="button"
                aria-label={t('manageUsers.closePanel')}
                className="absolute inset-0 bg-black/50 backdrop-blur-sm"
                onClick={onClose}
            />
            <div
                {...modalDrag.panelProps}
                className="relative w-full max-w-3xl max-h-[84vh] overflow-hidden rounded-lg border border-gray-200 bg-white shadow-2xl dark:border-gray-700 dark:bg-gray-800"
            >
                <div
                    {...modalDrag.dragHandleProps}
                    className="flex items-center justify-between border-b border-gray-200 px-4 py-3 dark:border-gray-700 cursor-grab select-none active:cursor-grabbing"
                >
                    <div className="flex items-center gap-2">
                        <Shield size={18} className="text-blue-500" />
                        <h2 className="text-sm font-semibold text-gray-900 dark:text-gray-100">{t('manageUsers.title')}</h2>
                    </div>
                    <button
                        type="button"
                        onClick={onClose}
                        className="flex h-8 w-8 items-center justify-center rounded-lg text-gray-500 transition-colors hover:bg-gray-100 hover:text-gray-800 dark:hover:bg-gray-700 dark:hover:text-gray-100"
                        aria-label={t('manageUsers.close')}
                    >
                        <X size={17} />
                    </button>
                </div>

                <div className="max-h-[calc(84vh-57px)] overflow-y-auto p-4">
                    {/* Offline-only clarification (discussion #270): these are
                        local partitions on this device, not online sign-ins. */}
                    <p className="mb-3 text-[11px] leading-snug text-gray-500 dark:text-gray-400">
                        {t('accountLock.offlineNote')}
                    </p>
                    {error && (
                        <div className="mb-3 rounded-lg border border-red-200 bg-red-50 px-3 py-2 text-xs text-red-700 dark:border-red-900/60 dark:bg-red-950/30 dark:text-red-300">
                            {error}
                        </div>
                    )}

                    {currentUserIsAdmin ? (
                        <form onSubmit={handleAddUser} className="mb-4 rounded-lg border border-gray-200 bg-gray-50 p-3 dark:border-gray-700 dark:bg-gray-900/40">
                            <div className="grid gap-3 md:grid-cols-[1fr_auto]">
                                <div className="grid gap-2 sm:grid-cols-[minmax(0,1fr)_auto]">
                                    <input
                                        value={newName}
                                        onChange={(event) => setNewName(event.target.value)}
                                        placeholder={t('manageUsers.accountName')}
                                        className="min-w-0 rounded-lg border border-gray-300 bg-white px-3 py-2 text-sm text-gray-900 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-100"
                                    />
                                    <div className="flex items-center gap-1">
                                        <select
                                            value={newAvatar}
                                            onChange={(event) => setNewAvatar(event.target.value)}
                                            className="h-9 rounded-lg border border-gray-300 bg-white px-2 text-sm dark:border-gray-600 dark:bg-gray-800"
                                            aria-label={t('manageUsers.avatar')}
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
                                    {t('manageUsers.add')}
                                </button>
                            </div>
                            <div className="mt-2 grid gap-2 sm:grid-cols-[minmax(0,1fr)_auto]">
                                <div className="relative">
                                    <input
                                        type={showNewPassphrase ? 'text' : 'password'}
                                        value={newPassphrase}
                                        onChange={(event) => setNewPassphrase(event.target.value)}
                                        placeholder={t('manageUsers.accountPasswordOptional')}
                                        className="w-full rounded-lg border border-gray-300 bg-white px-3 py-2 pr-9 text-sm text-gray-900 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-100"
                                        autoComplete="new-password"
                                    />
                                    <button tabIndex={-1}
                                        type="button"
                                        onClick={() => setShowNewPassphrase((value) => !value)}
                                        className="absolute right-2 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-700 dark:hover:text-gray-200"
                                        aria-label={showNewPassphrase ? t('accountLock.hidePassword') : t('accountLock.showPassword')}
                                    >
                                        {showNewPassphrase ? <EyeOff size={15} /> : <Eye size={15} />}
                                    </button>
                                </div>
                                <div className="flex items-center gap-2 text-xs text-gray-500 dark:text-gray-400">
                                    <KeyRound size={14} />
                                    {t('manageUsers.noRecovery')}
                                </div>
                            </div>
                            {newPassphrase && (
                                <div className="mt-2 sm:max-w-[calc(100%-0px)] sm:grid sm:grid-cols-[minmax(0,1fr)_auto] sm:gap-2">
                                    <div className="relative">
                                        <input
                                            type={showNewPassphrase ? 'text' : 'password'}
                                            value={newConfirmPassphrase}
                                            onChange={(event) => setNewConfirmPassphrase(event.target.value)}
                                            placeholder={t('password.confirmPlaceholder')}
                                            className="w-full rounded-lg border border-gray-300 bg-white px-3 py-2 pr-9 text-sm text-gray-900 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-100"
                                            autoComplete="new-password"
                                            aria-label={t('password.confirm')}
                                        />
                                        <PasswordMatchHint password={newPassphrase} confirm={newConfirmPassphrase} />
                                    </div>
                                </div>
                            )}
                            <p className="mt-2 flex items-start gap-2 text-[11px] leading-snug text-gray-500 dark:text-gray-400">
                                <Info size={13} className="mt-0.5 flex-shrink-0" />
                                <span>{t('manageUsers.passwordChoiceNote')}</span>
                            </p>
                            <div className="mt-2 max-w-md">
                                <PasswordStrengthBar password={newPassphrase} />
                            </div>
                            {newPassphrase && (
                                <div className="mt-3 rounded-lg border border-amber-300 bg-amber-50 p-3 text-xs text-amber-900 dark:border-amber-900/60 dark:bg-amber-950/30 dark:text-amber-200">
                                    <div className="mb-1.5 flex items-start gap-2 font-medium">
                                        <AlertTriangle size={14} className="mt-0.5 flex-shrink-0" />
                                        <span>{t('manageUsers.noRecoveryTitle')}</span>
                                    </div>
                                    <p className="mb-2 leading-relaxed opacity-90">
                                        {t('manageUsers.noRecoveryBody')}
                                    </p>
                                    <label className="flex items-start gap-2 cursor-pointer select-none">
                                        <input
                                            type="checkbox"
                                            checked={acknowledgeNoRecoveryNew}
                                            onChange={(event) => setAcknowledgeNoRecoveryNew(event.target.checked)}
                                            className="mt-0.5 h-3.5 w-3.5"
                                        />
                                        <span>{t('manageUsers.noRecoveryAck')}</span>
                                    </label>
                                </div>
                            )}
                        </form>
                    ) : (
                        <div className="mb-4 rounded-lg border border-gray-200 bg-gray-50 px-3 py-2 text-xs text-gray-500 dark:border-gray-700 dark:bg-gray-900/40 dark:text-gray-400">
                            {t('manageUsers.adminRequired')}
                        </div>
                    )}

                    <div className="space-y-2">
                        {loading && users.length === 0 ? (
                            <div className="rounded-lg border border-gray-200 p-4 text-sm text-gray-500 dark:border-gray-700 dark:text-gray-400">
                                {t('manageUsers.loading')}
                            </div>
                        ) : users.map((user) => {
                            const userStats = statsByUserId.get(user.id);
                            const isEditing = editingUserId === user.id;
                            const passphraseOpen = passphraseForm?.userId === user.id;
                            const isSelf = user.id === selfId;
                            const canModify = currentUserIsAdmin || isSelf;
                            const isLastAdmin = user.isAdmin && adminCount <= 1;
                            const canUseSelfPasswordFlow = isSelf && canModify;
                            const canAdminReset = currentUserIsAdmin && !isSelf && user.hasPassphrase;
                            const canDelete = canModify && !user.isActive && users.length > 1 && !isLastAdmin;
                            return (
                                <div
                                    key={user.id}
                                    draggable={currentUserIsAdmin}
                                    onDragStart={() => {
                                        if (currentUserIsAdmin) setDraggingUserId(user.id);
                                    }}
                                    onDragOver={(event) => event.preventDefault()}
                                    onDrop={() => { void handleDrop(user.id); }}
                                    onKeyDown={(event) => {
                                        if (event.key === 'F2' && canModify) {
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
                                        <GripVertical size={16} className={`shrink-0 text-gray-400 ${currentUserIsAdmin ? 'cursor-grab' : 'opacity-30'}`} />
                                        {/* The avatar itself is the "Edit avatar"
                                            control (#270): clicking the element you
                                            want to change is more intuitive than a
                                            separate palette button. A pencil overlay
                                            appears on hover/focus to signal it. */}
                                        {canModify ? (
                                            <button
                                                type="button"
                                                onClick={() => openAvatarEditor(user)}
                                                disabled={busyUserId === user.id}
                                                className="group/avatar relative shrink-0 rounded-full outline-none transition-transform hover:scale-105 focus-visible:ring-2 focus-visible:ring-blue-500 disabled:opacity-50"
                                                title={t('manageUsers.editAvatar')}
                                                aria-label={t('manageUsers.editAvatar')}
                                            >
                                                <UserAvatar name={user.name} avatarEmoji={user.avatarEmoji} avatarColor={user.avatarColor} size="lg" />
                                                <span className="absolute -bottom-0.5 -right-0.5 flex h-4 w-4 items-center justify-center rounded-full bg-blue-600 text-white opacity-0 shadow transition-opacity group-hover/avatar:opacity-100 group-focus-visible/avatar:opacity-100">
                                                    <Pencil size={9} />
                                                </span>
                                            </button>
                                        ) : (
                                            <UserAvatar name={user.name} avatarEmoji={user.avatarEmoji} avatarColor={user.avatarColor} size="lg" />
                                        )}
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
                                                        aria-label={t('manageUsers.saveName')}
                                                    >
                                                        <Check size={14} />
                                                    </button>
                                                </form>
                                            ) : (
                                                <div className="flex min-w-0 items-center gap-2">
                                                    <span className="truncate text-sm font-medium text-gray-900 dark:text-gray-100">{user.name}</span>
                                                    {user.isActive && (
                                                        <span className="rounded-full bg-blue-100 px-2 py-0.5 text-[11px] font-medium text-blue-700 dark:bg-blue-900/40 dark:text-blue-300">
                                                            {t('manageUsers.active')}
                                                        </span>
                                                    )}
                                                    {user.isAdmin && (
                                                        <span className="inline-flex items-center gap-1 rounded-full bg-emerald-100 px-2 py-0.5 text-[11px] font-semibold text-emerald-700 dark:bg-emerald-900/40 dark:text-emerald-300">
                                                            <ShieldCheck size={11} />
                                                            {t('manageUsers.admin')}
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
                                                <span>{t('manageUsers.profilesCount').replace('{count}', String(userStats?.profileCount ?? 0))}</span>
                                                <span>{t('manageUsers.settingsCount').replace('{count}', String(userStats?.settingsCount ?? 0))}</span>
                                                <span>{formatBytes(userStats?.encryptedBytes ?? 0)}</span>
                                                <span>{formatLastUnlocked(user.lastUnlockedAt, t('manageUsers.never'))}</span>
                                            </div>
                                        </div>
                                        <div className="flex shrink-0 items-center gap-1">
                                            {!user.hasPassphrase && (
                                                <button
                                                    type="button"
                                                    onClick={() => toggleDefaultAccount(user)}
                                                    disabled={busyUserId === user.id}
                                                    className={`flex h-8 w-8 items-center justify-center rounded-lg transition-colors disabled:opacity-50 ${
                                                        defaultAccountId === user.id
                                                            ? 'text-amber-500 hover:bg-amber-50 dark:hover:bg-amber-950/30'
                                                            : 'text-gray-400 hover:bg-gray-100 hover:text-gray-700 dark:hover:bg-gray-700 dark:hover:text-gray-100'
                                                    }`}
                                                    title={`${t('accountLock.skipNextTime')} ${t('accountLock.skipNextTimeHint')}`}
                                                    aria-pressed={defaultAccountId === user.id}
                                                >
                                                    <Star size={15} fill={defaultAccountId === user.id ? 'currentColor' : 'none'} />
                                                </button>
                                            )}
                                            {canModify && (
                                                <>
                                                    <button
                                                        type="button"
                                                        onClick={() => startRename(user)}
                                                        disabled={busyUserId === user.id}
                                                        className="flex h-8 w-8 items-center justify-center rounded-lg text-gray-500 transition-colors hover:bg-gray-100 hover:text-gray-800 disabled:opacity-50 dark:hover:bg-gray-700 dark:hover:text-gray-100"
                                                        title={t('manageUsers.rename')}
                                                    >
                                                        <Pencil size={15} />
                                                    </button>
                                                </>
                                            )}
                                            {canUseSelfPasswordFlow && (
                                                <button
                                                    type="button"
                                                    onClick={() => setPassphraseForm(passphraseOpen ? null : {
                                                        userId: user.id,
                                                        oldPassphrase: '',
                                                        newPassphrase: '',
                                                        confirmNewPassphrase: '',
                                                        showOld: false,
                                                        showNew: false,
                                                    })}
                                                    disabled={busyUserId === user.id}
                                                    className="flex h-8 w-8 items-center justify-center rounded-lg text-gray-500 transition-colors hover:bg-gray-100 hover:text-gray-800 disabled:opacity-50 dark:hover:bg-gray-700 dark:hover:text-gray-100"
                                                    title={user.hasPassphrase ? t('manageUsers.changeMyPassword') : t('manageUsers.setPassword')}
                                                >
                                                    <KeyRound size={15} />
                                                </button>
                                            )}
                                            {canAdminReset && (
                                                <button
                                                    type="button"
                                                    onClick={() => setResetTarget(user)}
                                                    disabled={busyUserId === user.id}
                                                    className="flex h-8 w-8 items-center justify-center rounded-lg text-red-500 transition-colors hover:bg-red-50 hover:text-red-700 disabled:opacity-50 dark:hover:bg-red-950/30"
                                                    title={t('manageUsers.resetPassword')}
                                                >
                                                    <RotateCcw size={15} />
                                                </button>
                                            )}
                                            {currentUserIsAdmin && (
                                                <button
                                                    type="button"
                                                    onClick={() => { void handleSetAdmin(user, !user.isAdmin); }}
                                                    disabled={busyUserId === user.id || (user.isAdmin && isLastAdmin)}
                                                    className="flex h-8 w-8 items-center justify-center rounded-lg text-gray-500 transition-colors hover:bg-emerald-50 hover:text-emerald-700 disabled:cursor-not-allowed disabled:opacity-40 dark:hover:bg-emerald-950/30"
                                                    title={user.isAdmin ? t('manageUsers.revokeAdmin') : t('manageUsers.promoteAdmin')}
                                                >
                                                    <ShieldCheck size={15} />
                                                </button>
                                            )}
                                            {canModify && (
                                                <button
                                                    type="button"
                                                    onClick={() => { void handleDelete(user); }}
                                                    disabled={busyUserId === user.id || !canDelete}
                                                    className="flex h-8 w-8 items-center justify-center rounded-lg text-gray-500 transition-colors hover:bg-red-50 hover:text-red-600 disabled:cursor-not-allowed disabled:opacity-40 dark:hover:bg-red-950/30"
                                                    title={isLastAdmin ? t('manageUsers.cannotDeleteLastAdmin') : t('manageUsers.delete')}
                                                >
                                                    <Trash2 size={15} />
                                                </button>
                                            )}
                                        </div>
                                    </div>

                                    {passphraseOpen && passphraseForm && canUseSelfPasswordFlow && (
                                        <form onSubmit={(event) => { void handlePassphraseSubmit(event, user); }} className="mt-3 grid gap-2 border-t border-gray-200 pt-3 dark:border-gray-700 sm:grid-cols-[1fr_1fr_auto]">
                                            {user.hasPassphrase && (
                                                <div className="relative">
                                                    <input
                                                        type={passphraseForm.showOld ? 'text' : 'password'}
                                                        value={passphraseForm.oldPassphrase}
                                                        onChange={(event) => setPassphraseForm({ ...passphraseForm, oldPassphrase: event.target.value })}
                                                        placeholder={t('manageUsers.currentPassword')}
                                                        className="w-full rounded-lg border border-gray-300 bg-white px-3 py-2 pr-9 text-sm dark:border-gray-600 dark:bg-gray-900"
                                                        autoComplete="current-password"
                                                    />
                                                    <button tabIndex={-1}
                                                        type="button"
                                                        onClick={() => setPassphraseForm({ ...passphraseForm, showOld: !passphraseForm.showOld })}
                                                        className="absolute right-2 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-700 dark:hover:text-gray-200"
                                                        aria-label={passphraseForm.showOld ? t('accountLock.hidePassword') : t('accountLock.showPassword')}
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
                                                    placeholder={user.hasPassphrase ? t('manageUsers.newPasswordBlankRemoves') : t('manageUsers.newPassword')}
                                                    className="w-full rounded-lg border border-gray-300 bg-white px-3 py-2 pr-9 text-sm dark:border-gray-600 dark:bg-gray-900"
                                                    autoComplete="new-password"
                                                />
                                                <button tabIndex={-1}
                                                    type="button"
                                                    onClick={() => setPassphraseForm({ ...passphraseForm, showNew: !passphraseForm.showNew })}
                                                    className="absolute right-2 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-700 dark:hover:text-gray-200"
                                                    aria-label={passphraseForm.showNew ? t('accountLock.hidePassword') : t('accountLock.showPassword')}
                                                >
                                                    {passphraseForm.showNew ? <EyeOff size={15} /> : <Eye size={15} />}
                                                </button>
                                            </div>
                                            {passphraseForm.newPassphrase && (
                                                <div className="relative sm:col-span-3">
                                                    <input
                                                        type={passphraseForm.showNew ? 'text' : 'password'}
                                                        value={passphraseForm.confirmNewPassphrase}
                                                        onChange={(event) => setPassphraseForm({ ...passphraseForm, confirmNewPassphrase: event.target.value })}
                                                        placeholder={t('password.confirmPlaceholder')}
                                                        className="w-full rounded-lg border border-gray-300 bg-white px-3 py-2 pr-9 text-sm dark:border-gray-600 dark:bg-gray-900"
                                                        autoComplete="new-password"
                                                        aria-label={t('password.confirm')}
                                                    />
                                                    <PasswordMatchHint password={passphraseForm.newPassphrase} confirm={passphraseForm.confirmNewPassphrase} />
                                                </div>
                                            )}
                                            <div className="flex items-center gap-2">
                                                <button
                                                    type="submit"
                                                    disabled={busyUserId === user.id}
                                                    className="inline-flex h-9 items-center gap-1.5 rounded-lg bg-blue-600 px-3 text-sm font-medium text-white transition-colors hover:bg-blue-700 disabled:bg-blue-300"
                                                >
                                                    <Check size={15} />
                                                    {t('manageUsers.save')}
                                                </button>
                                                <button
                                                    type="button"
                                                    onClick={() => { setPassphraseForm(null); setAcknowledgeNoRecoveryForm(false); }}
                                                    className="flex h-9 w-9 items-center justify-center rounded-lg bg-gray-100 text-gray-600 transition-colors hover:bg-gray-200 dark:bg-gray-700 dark:text-gray-200 dark:hover:bg-gray-600"
                                                    aria-label={t('manageUsers.cancelPasswordEdit')}
                                                >
                                                    <X size={15} />
                                                </button>
                                            </div>
                                            {!user.hasPassphrase && (
                                                <p className="sm:col-span-3 flex items-start gap-2 text-[11px] leading-snug text-gray-500 dark:text-gray-400">
                                                    <Info size={13} className="mt-0.5 flex-shrink-0" />
                                                    <span>{t('manageUsers.passwordChoiceNote')}</span>
                                                </p>
                                            )}
                                            <div className="sm:col-span-3 max-w-md">
                                                <PasswordStrengthBar password={passphraseForm.newPassphrase} />
                                            </div>
                                            {!user.hasPassphrase && passphraseForm.newPassphrase && (
                                                <div className="sm:col-span-3 rounded-lg border border-amber-300 bg-amber-50 p-3 text-xs text-amber-900 dark:border-amber-900/60 dark:bg-amber-950/30 dark:text-amber-200">
                                                    <div className="mb-1.5 flex items-start gap-2 font-medium">
                                                        <AlertTriangle size={14} className="mt-0.5 flex-shrink-0" />
                                                        <span>{t('manageUsers.noRecoveryTitle')}</span>
                                                    </div>
                                                    <p className="mb-2 leading-relaxed opacity-90">
                                                        {t('manageUsers.noRecoveryBody')}
                                                    </p>
                                                    <label className="flex items-start gap-2 cursor-pointer select-none">
                                                        <input
                                                            type="checkbox"
                                                            checked={acknowledgeNoRecoveryForm}
                                                            onChange={(event) => setAcknowledgeNoRecoveryForm(event.target.checked)}
                                                            className="mt-0.5 h-3.5 w-3.5"
                                                        />
                                                        <span>{t('manageUsers.noRecoveryAck')}</span>
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
            {resetTarget && (
                <DestructiveResetDialog
                    target={resetTarget}
                    initialStats={statsByUserId.get(resetTarget.id)}
                    onClose={() => setResetTarget(null)}
                    onComplete={() => { void handleResetComplete(); }}
                />
            )}
            {avatarEditingUser && (
                <div className="fixed inset-0 z-[90] flex items-start justify-center pt-[10vh]">
                    <button
                        type="button"
                        aria-label={t('manageUsers.closeAvatarEditor')}
                        className="absolute inset-0 bg-black/55 backdrop-blur-sm"
                        onClick={avatarSaving ? undefined : () => setAvatarEditingUser(null)}
                    />
                    <div className="relative w-full max-w-md overflow-hidden rounded-lg border border-gray-200 bg-white shadow-2xl dark:border-gray-700 dark:bg-gray-800">
                        <div className="flex items-center justify-between border-b border-gray-200 px-4 py-3 dark:border-gray-700">
                            <div className="flex min-w-0 items-center gap-2">
                                <Palette size={18} className="text-blue-500" />
                                <h3 className="truncate text-sm font-semibold text-gray-900 dark:text-gray-100">{t('manageUsers.editAvatar')}</h3>
                            </div>
                            <button
                                type="button"
                                onClick={() => setAvatarEditingUser(null)}
                                disabled={avatarSaving}
                                className="flex h-8 w-8 items-center justify-center rounded-lg text-gray-500 transition-colors hover:bg-gray-100 hover:text-gray-800 disabled:opacity-50 dark:hover:bg-gray-700 dark:hover:text-gray-100"
                                aria-label={t('manageUsers.close')}
                            >
                                <X size={16} />
                            </button>
                        </div>
                        <div className="space-y-4 p-4">
                            <div className="flex items-center gap-3">
                                <UserAvatar
                                    name={avatarEditingUser.name}
                                    avatarEmoji={avatarDraftEmoji}
                                    avatarColor={avatarDraftColor}
                                    size="lg"
                                    className="!h-14 !w-14 !text-xl"
                                />
                                <div className="min-w-0">
                                    <div className="truncate text-sm font-medium text-gray-900 dark:text-gray-100">{avatarEditingUser.name}</div>
                                    <div className="text-xs text-gray-500 dark:text-gray-400">{t('manageUsers.accountIdentity')}</div>
                                </div>
                            </div>

                            <div>
                                <label className="mb-2 block text-xs font-medium text-gray-700 dark:text-gray-300">{t('manageUsers.avatar')}</label>
                                <div className="flex flex-wrap gap-2">
                                    {AVATAR_CHOICES.map((avatar) => (
                                        <button
                                            key={avatar}
                                            type="button"
                                            onClick={() => setAvatarDraftEmoji(avatar)}
                                            className={`flex h-9 w-9 items-center justify-center rounded-lg border text-sm font-semibold transition-colors ${
                                                avatarDraftEmoji === avatar
                                                    ? 'border-blue-500 bg-blue-50 text-blue-700 dark:bg-blue-950/40 dark:text-blue-300'
                                                    : 'border-gray-200 text-gray-700 hover:bg-gray-50 dark:border-gray-700 dark:text-gray-200 dark:hover:bg-gray-700'
                                            }`}
                                        >
                                            {avatar}
                                        </button>
                                    ))}
                                    <button
                                        type="button"
                                        onClick={() => setShowIconPicker(true)}
                                        className="inline-flex h-9 items-center justify-center gap-1.5 rounded-lg border border-gray-200 px-3 text-xs font-medium text-gray-700 transition-colors hover:bg-gray-50 dark:border-gray-700 dark:text-gray-200 dark:hover:bg-gray-700"
                                    >
                                        <Palette size={14} />
                                        {t('manageUsers.iconLibrary')}
                                    </button>
                                </div>
                            </div>

                            <div>
                                <label className="mb-2 block text-xs font-medium text-gray-700 dark:text-gray-300">{t('manageUsers.color')}</label>
                                <div className="flex flex-wrap gap-2">
                                    {COLOR_CHOICES.map((color) => (
                                        <button
                                            key={color}
                                            type="button"
                                            onClick={() => setAvatarDraftColor(color)}
                                            className={`h-8 w-8 rounded-full border-2 ${avatarDraftColor === color ? 'border-gray-900 dark:border-white' : 'border-transparent'}`}
                                            style={{ backgroundColor: color }}
                                            aria-label={color}
                                        />
                                    ))}
                                </div>
                            </div>
                        </div>
                        <div className="flex items-center justify-end gap-2 border-t border-gray-200 px-4 py-3 dark:border-gray-700">
                            <button
                                type="button"
                                onClick={() => setAvatarEditingUser(null)}
                                disabled={avatarSaving}
                                className="inline-flex h-9 items-center justify-center rounded-lg bg-gray-100 px-3 text-sm font-medium text-gray-700 transition-colors hover:bg-gray-200 disabled:opacity-50 dark:bg-gray-700 dark:text-gray-200 dark:hover:bg-gray-600"
                            >
                                {t('manageUsers.cancel')}
                            </button>
                            <button
                                type="button"
                                onClick={() => { void handleAvatarSave(); }}
                                disabled={avatarSaving}
                                className="inline-flex h-9 items-center justify-center gap-1.5 rounded-lg bg-blue-600 px-3 text-sm font-medium text-white transition-colors hover:bg-blue-700 disabled:bg-blue-300"
                            >
                                {avatarSaving ? <Loader2 size={15} className="animate-spin" /> : <Check size={15} />}
                                {t('manageUsers.save')}
                            </button>
                        </div>
                    </div>
                    {showIconPicker && (
                        <IconPickerDialog
                            currentIcon={avatarDraftEmoji}
                            onSelect={(dataUrl) => {
                                setAvatarDraftEmoji(dataUrl);
                                setShowIconPicker(false);
                            }}
                            onClose={() => setShowIconPicker(false)}
                        />
                    )}
                </div>
            )}
        </div>
    );
};

export default UsersManagePanel;
