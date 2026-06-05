// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { ArrowLeft, Eye, EyeOff, KeyRound, Loader2, Lock, LockOpen, Plus, Shield, ShieldCheck, UserCog, X } from 'lucide-react';
import { getVersion } from '@tauri-apps/api/app';
import { useTranslation } from '../i18n';
import { UserAvatar } from './UserAvatar';
import { UsersManagePanel } from './UsersManagePanel';
import { WindowControls } from './WindowControls';
import { LOCK_SCREEN_PATTERNS } from './LockScreen';
import {
    getUnlockStatus,
    initUserPartitions,
    listUsers,
    readDefaultAccountId,
    readUsersListCache,
    unlockUser,
    writeDefaultAccountId,
    writeUsersListCache,
    type CachedUserListEntry,
    type UserMetadata,
    type UserUnlockStatus,
} from '../utils/userPartitions';
import { PROFILES_CHANGED_EVENT } from '../utils/serverProfileStore';
import { mapUserPartitionError } from '../utils/userPartitionErrors';

const LOCK_PATTERN_KEY = 'aeroftp_lock_pattern';
const DEFAULT_PATTERN = 'isometric';

interface AccountLockScreenProps {
    onContinue: () => void;
}

interface DisplayUser extends Omit<CachedUserListEntry, 'avatarEmoji' | 'avatarColor'> {
    avatarEmoji?: string | null;
    avatarColor?: string | null;
    lastUnlockedAt?: number | null;
}

const toDisplayUser = (user: UserMetadata): DisplayUser => ({
    id: user.id,
    name: user.name,
    avatarEmoji: user.avatarEmoji,
    avatarColor: user.avatarColor,
    hasPassphrase: user.hasPassphrase,
    sortOrder: user.sortOrder,
    isActive: user.isActive,
    isAdmin: user.isAdmin,
    lastUnlockedAt: user.lastUnlockedAt,
});

const parseLockedOut = (err: unknown): number | null => {
    const message = typeof err === 'string' ? err : (err as { toString?: () => string })?.toString?.() ?? '';
    const m = /LOCKED_OUT:(\d+)/.exec(message);
    if (!m) return null;
    const ts = Number.parseInt(m[1], 10);
    return Number.isFinite(ts) ? ts : null;
};

const isWrongPassphrase = (err: unknown): boolean => {
    const message = typeof err === 'string' ? err : String(err);
    return message.includes('WRONG_PASSPHRASE') || message.includes('PASSPHRASE_REQUIRED');
};

const sortForDisplay = <T extends { isActive: boolean; sortOrder: number; name: string }>(arr: T[]): T[] =>
    [...arr].sort((a, b) => {
        if (a.isActive !== b.isActive) return a.isActive ? -1 : 1;
        return a.sortOrder - b.sortOrder || a.name.localeCompare(b.name);
    });

export const AccountLockScreen: React.FC<AccountLockScreenProps> = ({ onContinue }) => {
    const t = useTranslation();

    const cachedUsers = React.useMemo<DisplayUser[]>(() => {
        const cache = readUsersListCache();
        return cache ? cache.map((u) => ({ ...u })) : [];
    }, []);

    const [users, setUsers] = React.useState<DisplayUser[]>(cachedUsers);
    const [status, setStatus] = React.useState<UserUnlockStatus | null>(null);
    const [pendingUser, setPendingUser] = React.useState<DisplayUser | null>(null);
    const [passphrase, setPassphrase] = React.useState('');
    const [showPassphrase, setShowPassphrase] = React.useState(false);
    const [error, setError] = React.useState('');
    const [isLoading, setIsLoading] = React.useState(false);
    const [lockedUntilMs, setLockedUntilMs] = React.useState<number | null>(null);
    const [nowMs, setNowMs] = React.useState<number>(() => Date.now());
    const [showManagePanel, setShowManagePanel] = React.useState(false);
    const [appVersion, setAppVersion] = React.useState('');
    const [refreshKey, setRefreshKey] = React.useState(0);
    // Welcome-screen skip: true when a default account is already remembered.
    const [skipWelcome, setSkipWelcome] = React.useState<boolean>(() => readDefaultAccountId() != null);

    React.useEffect(() => { getVersion().then(setAppVersion).catch(() => {}); }, []);

    const refresh = React.useCallback(async () => {
        try {
            await initUserPartitions();
            const [list, st] = await Promise.all([listUsers(), getUnlockStatus()]);
            const display = list.map(toDisplayUser);
            setUsers(display);
            setStatus(st);
            writeUsersListCache(list);
            return { list, status: st };
        } catch (err) {
            setError(mapUserPartitionError(err, t));
            return null;
        }
    }, [t]);

    React.useEffect(() => { void refresh(); }, [refresh, refreshKey]);

    // Countdown ticker when locked out
    React.useEffect(() => {
        if (lockedUntilMs === null) return;
        if (lockedUntilMs <= Date.now()) {
            setLockedUntilMs(null);
            return;
        }
        const interval = setInterval(() => {
            const now = Date.now();
            setNowMs(now);
            if (lockedUntilMs <= now) {
                setLockedUntilMs(null);
                setError('');
            }
        }, 500);
        return () => clearInterval(interval);
    }, [lockedUntilMs]);

    const patternId = (typeof localStorage !== 'undefined' && localStorage.getItem(LOCK_PATTERN_KEY)) || DEFAULT_PATTERN;
    const pattern = LOCK_SCREEN_PATTERNS.find((p) => p.id === patternId) || LOCK_SCREEN_PATTERNS[0];

    const sortedUsers = sortForDisplay(users);

    const handleUnlock = React.useCallback(async (user: DisplayUser, providedPassphrase: string | null) => {
        setError('');
        setIsLoading(true);
        try {
            const MIN_SPINNER_MS = 350;
            await Promise.all([
                unlockUser(user.id, providedPassphrase),
                new Promise<void>((resolve) => setTimeout(resolve, MIN_SPINNER_MS)),
            ]);
            // Remember (or clear) the default account for the welcome-screen
            // skip. Only password-free accounts qualify, mirroring the boot
            // guard in App.tsx so authentication is never skipped (#270).
            if (skipWelcome && !user.hasPassphrase) {
                writeDefaultAccountId(user.id);
            } else if (!skipWelcome) {
                writeDefaultAccountId(null);
            }
            try { window.dispatchEvent(new CustomEvent(PROFILES_CHANGED_EVENT)); } catch { /* best effort */ }
            onContinue();
        } catch (err) {
            const lockedUntil = parseLockedOut(err);
            if (lockedUntil !== null) {
                setLockedUntilMs(lockedUntil);
                setError(t('accountLock.lockedOut'));
            } else if (isWrongPassphrase(err)) {
                setError(t('accountLock.wrongPassphrase'));
                setPassphrase('');
            } else {
                setError(mapUserPartitionError(err, t));
            }
        } finally {
            setIsLoading(false);
        }
    }, [onContinue, skipWelcome, t]);

    const handleCardClick = (user: DisplayUser) => {
        if (isLoading) return;
        setError('');
        setLockedUntilMs(null);
        if (user.hasPassphrase) {
            setPendingUser(user);
            setPassphrase('');
            setShowPassphrase(false);
        } else {
            void handleUnlock(user, null);
        }
    };

    const handlePassphraseSubmit = (event: React.FormEvent) => {
        event.preventDefault();
        if (!pendingUser || !passphrase || isLoading || lockedUntilMs !== null) return;
        void handleUnlock(pendingUser, passphrase);
    };

    const handleBackToList = () => {
        if (isLoading) return;
        setPendingUser(null);
        setPassphrase('');
        setShowPassphrase(false);
        setError('');
        setLockedUntilMs(null);
    };

    const handleManagePanelClose = () => {
        setShowManagePanel(false);
        setRefreshKey((value) => value + 1);
    };

    const handleManagePanelChanged = () => {
        setRefreshKey((value) => value + 1);
    };

    const remainingSeconds = lockedUntilMs !== null
        ? Math.max(0, Math.ceil((lockedUntilMs - nowMs) / 1000))
        : 0;

    const formatLastUnlocked = (timestamp?: number | null): string | null => {
        if (!timestamp) return null;
        const delta = Date.now() - timestamp;
        if (delta < 0) return null;
        const days = Math.floor(delta / 86_400_000);
        if (days <= 0) return t('accountLock.lastUsedToday');
        if (days === 1) return t('accountLock.lastUsedYesterday');
        return t('accountLock.lastUsedDaysAgo').replace('{days}', String(days));
    };

    React.useEffect(() => {
        const handleKey = (event: KeyboardEvent) => {
            if (event.key === 'Escape' && pendingUser) handleBackToList();
        };
        window.addEventListener('keydown', handleKey);
        return () => window.removeEventListener('keydown', handleKey);
        // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [pendingUser, isLoading]);

    return (
        <div
            className="fixed inset-0 z-[200] flex flex-col items-center justify-center overflow-auto"
            style={{
                background: 'linear-gradient(135deg, var(--color-bg-primary, #111827) 0%, var(--color-bg-secondary, #1f2937) 100%)',
                color: 'var(--color-text-primary, #f3f4f6)',
            }}
        >
            {pattern.svg && (
                <div className="pointer-events-none absolute inset-0 opacity-[0.05]">
                    <div className="absolute inset-0" style={{ backgroundImage: pattern.svg }} />
                </div>
            )}

            {/* Window controls + drag region. The lock screen renders above the
                main titlebar, so without this the minimize/maximize/close
                buttons would be unreachable on this screen (discussion #270). */}
            <div
                data-tauri-drag-region
                className="absolute inset-x-0 top-0 z-20 flex h-9 items-center justify-end px-2"
            >
                <WindowControls />
            </div>

            <div className="relative flex w-full max-w-5xl flex-col items-center px-6 py-10">
                <div className="mb-8 flex flex-col items-center gap-2">
                    <div className="relative">
                        <Shield size={56} className="text-emerald-400" />
                        <div className="absolute -bottom-1 -right-1 rounded-full bg-emerald-500 p-1">
                            <Lock size={12} className="text-white" />
                        </div>
                    </div>
                    <div className="mt-1 text-center">
                        <div className="text-lg font-semibold tracking-tight">AeroFTP</div>
                        <div className="text-sm opacity-70">
                            {pendingUser ? t('accountLock.signInAs').replace('{name}', pendingUser.name) : t('accountLock.title')}
                        </div>
                    </div>
                </div>

                {error && (
                    <div className="mb-4 max-w-md rounded-lg border border-red-500/40 bg-red-500/10 px-3 py-2 text-center text-sm text-red-200">
                        {error}
                        {lockedUntilMs !== null && remainingSeconds > 0 && (
                            <div className="mt-1 text-xs opacity-90">
                                {t('accountLock.lockedOutCountdown').replace('{seconds}', String(remainingSeconds))}
                            </div>
                        )}
                    </div>
                )}

                {!pendingUser && (
                    <>
                        <div className="grid w-full grid-cols-1 gap-4 sm:grid-cols-2 md:grid-cols-3 lg:grid-cols-4">
                            {sortedUsers.map((user) => {
                                const isUserLocked = user.hasPassphrase;
                                const isCurrentUnlocked = status?.unlockedUserId === user.id && status.isUnlocked;
                                const lastUnlockedLabel = formatLastUnlocked(user.lastUnlockedAt);
                                return (
                                    <button
                                        key={user.id}
                                        type="button"
                                        disabled={isLoading}
                                        onClick={() => handleCardClick(user)}
                                        className={`group relative flex flex-col items-center gap-2 rounded-lg border border-white/10 bg-white/[0.04] px-4 py-5 text-center backdrop-blur transition-all hover:-translate-y-0.5 hover:bg-white/[0.08] hover:shadow-lg hover:shadow-emerald-500/10 focus:outline-none focus:ring-2 focus:ring-emerald-500/60 disabled:opacity-50 disabled:hover:translate-y-0 ${
                                            user.isActive ? 'ring-1 ring-emerald-400/30' : ''
                                        }`}
                                        aria-label={user.name}
                                    >
                                        {user.isActive && (
                                            <span className="absolute right-2 top-2 inline-flex items-center gap-1 rounded-full bg-emerald-500/20 px-2 py-0.5 text-[10px] font-medium text-emerald-300">
                                                {t('accountLock.activeBadge')}
                                            </span>
                                        )}
                                        <UserAvatar
                                            name={user.name}
                                            avatarEmoji={user.avatarEmoji}
                                            avatarColor={user.avatarColor}
                                            size="lg"
                                            className="!h-14 !w-14 !text-xl shadow-md"
                                        />
                                        <div className="mt-1 max-w-full truncate text-sm font-medium">
                                            {user.name}
                                        </div>
                                        <div className="flex items-center gap-1 text-[11px] opacity-70">
                                            {isUserLocked ? (
                                                <>
                                                    <Lock size={11} className="text-amber-400" />
                                                    {t('accountLock.passphraseProtected')}
                                                </>
                                            ) : (
                                                <>
                                                    <LockOpen size={11} className="text-emerald-400" />
                                                    {isCurrentUnlocked ? t('accountLock.unlocked') : t('accountLock.openAccount')}
                                                </>
                                            )}
                                        </div>
                                        {lastUnlockedLabel && (
                                            <div className="mt-0.5 text-[10px] opacity-50">{lastUnlockedLabel}</div>
                                        )}
                                    </button>
                                );
                            })}

                            <button
                                type="button"
                                onClick={() => setShowManagePanel(true)}
                                className="group flex flex-col items-center justify-center gap-2 rounded-lg border border-dashed border-white/15 bg-white/[0.02] px-4 py-5 text-center transition-all hover:-translate-y-0.5 hover:bg-white/[0.05] focus:outline-none focus:ring-2 focus:ring-emerald-500/60"
                                aria-label={t('accountLock.newUser')}
                            >
                                <div className="flex h-14 w-14 items-center justify-center rounded-full border border-white/10 bg-white/5">
                                    <Plus size={22} className="opacity-70 transition-opacity group-hover:opacity-100" />
                                </div>
                                <div className="mt-1 text-sm font-medium opacity-80">{t('accountLock.newUser')}</div>
                                <div className="text-[11px] opacity-50">{t('accountLock.newUserHint')}</div>
                            </button>
                        </div>

                        {/* Default-account skip (discussion #270). Applies on the
                            next startup, and only to password-free accounts. */}
                        <label className="mt-6 inline-flex cursor-pointer items-center gap-2 text-xs opacity-70 transition-opacity hover:opacity-100">
                            <input
                                type="checkbox"
                                checked={skipWelcome}
                                onChange={(e) => {
                                    const checked = e.target.checked;
                                    setSkipWelcome(checked);
                                    if (!checked) writeDefaultAccountId(null);
                                }}
                                className="cursor-pointer accent-emerald-500"
                            />
                            <span>{t('accountLock.skipNextTime')}</span>
                            <span className="opacity-50">{t('accountLock.skipNextTimeHint')}</span>
                        </label>

                        <div className="mt-8 flex items-center gap-3 text-xs opacity-60">
                            <button
                                type="button"
                                onClick={() => setShowManagePanel(true)}
                                className="inline-flex items-center gap-1.5 rounded-md px-3 py-1.5 transition-colors hover:bg-white/[0.06] hover:text-white"
                            >
                                <UserCog size={13} />
                                {t('accountLock.manageUsers')}
                            </button>
                            {appVersion && <span className="opacity-50">AeroFTP v{appVersion}</span>}
                        </div>
                    </>
                )}

                {pendingUser && (
                    <div className="w-full max-w-md">
                        <div className="rounded-lg border border-white/10 bg-white/[0.04] p-6 backdrop-blur">
                            <div className="mb-5 flex items-center gap-3">
                                <UserAvatar
                                    name={pendingUser.name}
                                    avatarEmoji={pendingUser.avatarEmoji}
                                    avatarColor={pendingUser.avatarColor}
                                    size="lg"
                                    className="!h-12 !w-12 !text-lg"
                                />
                                <div className="min-w-0 flex-1">
                                    <div className="truncate text-base font-medium">{pendingUser.name}</div>
                                    <div className="flex items-center gap-1 text-xs opacity-70">
                                        <KeyRound size={12} />
                                        {t('accountLock.accountPasswordLabel')}
                                    </div>
                                </div>
                                <button
                                    type="button"
                                    onClick={handleBackToList}
                                    className="inline-flex h-9 w-9 items-center justify-center rounded-lg opacity-60 transition-all hover:bg-white/[0.06] hover:opacity-100"
                                    aria-label={t('accountLock.backToList')}
                                    title={t('accountLock.backToList')}
                                >
                                    <X size={16} />
                                </button>
                            </div>

                            <form onSubmit={handlePassphraseSubmit} className="space-y-3">
                                <div className="relative">
                                    <input
                                        id="account-lock-passphrase"
                                        type={showPassphrase ? 'text' : 'password'}
                                        value={passphrase}
                                        onChange={(event) => setPassphrase(event.target.value)}
                                        autoFocus
                                        autoComplete="current-password"
                                        className="w-full rounded-lg border border-white/10 bg-white/[0.04] px-3 py-2.5 pr-11 text-sm outline-none transition-colors focus:border-emerald-400/60 focus:ring-2 focus:ring-emerald-500/30"
                                        placeholder={t('accountLock.unlockPlaceholder')}
                                        disabled={isLoading || lockedUntilMs !== null}
                                    />
                                    <button
                                        type="button"
                                        onClick={() => setShowPassphrase((value) => !value)}
                                        className="absolute right-2 top-1/2 -translate-y-1/2 rounded-md p-1.5 opacity-60 hover:bg-white/[0.06] hover:opacity-100"
                                        tabIndex={-1}
                                        aria-label={showPassphrase ? t('accountLock.hidePassword') : t('accountLock.showPassword')}
                                    >
                                        {showPassphrase ? <EyeOff size={16} /> : <Eye size={16} />}
                                    </button>
                                </div>

                                <button
                                    type="submit"
                                    disabled={!passphrase || isLoading || lockedUntilMs !== null}
                                    className="flex h-10 w-full items-center justify-center gap-2 rounded-lg bg-emerald-600 text-sm font-medium text-white transition-colors hover:bg-emerald-500 disabled:cursor-not-allowed disabled:bg-emerald-600/40"
                                >
                                    {isLoading ? (
                                        <>
                                            <Loader2 size={16} className="animate-spin" />
                                            <span className="opacity-90">{t('accountLock.unlocking')}</span>
                                        </>
                                    ) : (
                                        <>
                                            <ShieldCheck size={16} />
                                            {t('accountLock.unlock')}
                                        </>
                                    )}
                                </button>

                                <button
                                    type="button"
                                    onClick={handleBackToList}
                                    className="flex h-9 w-full items-center justify-center gap-1.5 rounded-lg text-xs opacity-70 transition-colors hover:bg-white/[0.04] hover:opacity-100"
                                    disabled={isLoading}
                                >
                                    <ArrowLeft size={12} />
                                    {t('accountLock.backToList')}
                                </button>
                                <p className="mt-2 text-center text-[11px] opacity-50">
                                    {t('accountLock.securityNote')}
                                </p>
                            </form>
                        </div>
                    </div>
                )}
            </div>

            <UsersManagePanel
                isOpen={showManagePanel}
                onClose={handleManagePanelClose}
                onChanged={handleManagePanelChanged}
            />
        </div>
    );
};

export default AccountLockScreen;
