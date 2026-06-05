// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import { invoke } from '@tauri-apps/api/core';
import type { ServerProfile } from '../types';

export interface UserMetadata {
    id: number;
    name: string;
    avatarEmoji?: string | null;
    avatarColor?: string | null;
    hasPassphrase: boolean;
    sortOrder: number;
    createdAt: number;
    updatedAt: number;
    lastUnlockedAt?: number | null;
    isActive: boolean;
    isAdmin: boolean;
}

export interface UserPartitionMigrationReport {
    schemaVersion: string;
    createdDefaultUser: boolean;
    migratedProfiles: number;
    migratedSettingsScopes: number;
    alreadyMigrated: boolean;
}

export interface UserPartitionDebugState {
    dbPath: string;
    schemaVersion?: string | null;
    activeUserId?: number | null;
    userCount: number;
    profileCount: number;
    settingsCount: number;
}

export interface UserUnlockStatus {
    activeUserId?: number | null;
    unlockedUserId?: number | null;
    isUnlocked: boolean;
}

export interface UserStorageStats {
    userId: number;
    profileCount: number;
    settingsCount: number;
    encryptedBytes: number;
}

export const initUserPartitions = (): Promise<UserPartitionMigrationReport> =>
    invoke<UserPartitionMigrationReport>('user_partitions_init');

export const listUsers = (): Promise<UserMetadata[]> =>
    invoke<UserMetadata[]>('user_partitions_list_users');

export const getActiveUser = (): Promise<UserMetadata | null> =>
    invoke<UserMetadata | null>('user_partitions_get_active_user');

export const loadActiveServerProfiles = (): Promise<ServerProfile[]> =>
    invoke<ServerProfile[]>('user_partitions_load_active_server_profiles');

export const saveActiveServerProfiles = (profiles: ServerProfile[]): Promise<void> =>
    invoke<void>('user_partitions_save_active_server_profiles', { profiles });

export const addUser = (
    name: string,
    avatarEmoji?: string | null,
    avatarColor?: string | null,
    passphrase?: string | null,
): Promise<UserMetadata> =>
    invoke<UserMetadata>('user_partitions_add_user', {
        name,
        avatarEmoji,
        avatarColor,
        passphrase,
    });

export const unlockUser = (
    userId: number,
    passphrase?: string | null,
): Promise<UserUnlockStatus> =>
    invoke<UserUnlockStatus>('user_partitions_unlock_user', { userId, passphrase });

export const lockUserSession = (): Promise<void> =>
    invoke<void>('user_partitions_lock_session');

export const getUnlockStatus = (): Promise<UserUnlockStatus> =>
    invoke<UserUnlockStatus>('user_partitions_unlock_status');

export const changeUserPassphrase = (
    userId: number,
    oldPassphrase?: string | null,
    newPassphrase?: string | null,
): Promise<void> =>
    invoke<void>('user_partitions_change_passphrase', {
        userId,
        oldPassphrase,
        newPassphrase,
    });

export const setActiveUser = (userId: number): Promise<void> =>
    invoke<void>('user_partitions_set_active_user', { userId });

export const renameUser = (userId: number, name: string): Promise<void> =>
    invoke<void>('user_partitions_rename_user', { userId, name });

export const reorderUsers = (userIds: number[]): Promise<void> =>
    invoke<void>('user_partitions_reorder_users', { userIds });

export const deleteUser = (userId: number): Promise<void> =>
    invoke<void>('user_partitions_delete_user', { userId });

export const setUserAdmin = (userId: number, isAdmin: boolean): Promise<void> =>
    invoke<void>('user_partitions_set_admin', { userId, isAdmin });

export const setUserAvatar = (
    userId: number,
    avatarEmoji?: string | null,
    avatarColor?: string | null,
): Promise<void> =>
    invoke<void>('user_partitions_set_user_avatar', {
        userId,
        avatarEmoji,
        avatarColor,
    });

/**
 * Destructive admin-only "lost password" recovery for another user.
 * The target's encrypted server_profiles + user_settings are wiped
 * because a fresh DEK cannot decrypt them. The frontend MUST present
 * this with a triple confirmation (data-loss disclosure + new
 * passphrase + re-confirm).
 */
export const adminResetUserPassphrase = (
    userId: number,
    newPassphrase?: string | null,
): Promise<void> =>
    invoke<void>('user_partitions_admin_reset_passphrase', {
        userId,
        newPassphrase,
    });

export const getUserStorageStats = (): Promise<UserStorageStats[]> =>
    invoke<UserStorageStats[]>('user_partitions_storage_stats');

export const getUserPartitionDebugState = (): Promise<UserPartitionDebugState> =>
    invoke<UserPartitionDebugState>('user_partitions_debug_state');

// MU-4: per-user settings (encrypted with the active user's DEK). Scopes
// starting with `__` are blocked at the Rust boundary to keep the legacy
// backup payload unreachable from the public API.

export const getActiveUserSetting = <T = unknown>(scope: string): Promise<T | null> =>
    invoke<T | null>('user_partitions_get_active_setting', { scope });

export const setActiveUserSetting = <T = unknown>(scope: string, value: T): Promise<void> =>
    invoke<void>('user_partitions_set_active_setting', { scope, value });

export const deleteActiveUserSetting = (scope: string): Promise<void> =>
    invoke<void>('user_partitions_delete_active_setting', { scope });

export const listActiveUserSettingScopes = (): Promise<string[]> =>
    invoke<string[]>('user_partitions_list_active_setting_scopes');

// MU-7: cross-user dedup match metadata. The backend returns only public
// fields about the OTHER user account that already saved the same profile.
// The encrypted profile blob never leaves its owner's partition.
export interface CrossUserDedupMatch {
    userId: number;
    userName: string;
    userAvatarEmoji?: string | null;
    userAvatarColor?: string | null;
}

export const findCrossUserDedup = (
    profile: Record<string, unknown>,
): Promise<CrossUserDedupMatch[]> =>
    invoke<CrossUserDedupMatch[]>('user_partitions_find_cross_user_dedup', { profile });

const USERS_LIST_CACHE_KEY = 'aeroftp-users-list-cache';
const USERS_LIST_CACHE_VERSION = 1;

export interface CachedUserListEntry {
    id: number;
    name: string;
    avatarEmoji?: string | null;
    avatarColor?: string | null;
    hasPassphrase: boolean;
    sortOrder: number;
    isActive: boolean;
    isAdmin: boolean;
}

interface CachedUserListPayload {
    version: number;
    savedAt: number;
    users: CachedUserListEntry[];
}

const slimEntry = (user: UserMetadata): CachedUserListEntry => ({
    id: user.id,
    name: user.name,
    avatarEmoji: user.avatarEmoji ?? null,
    avatarColor: user.avatarColor ?? null,
    hasPassphrase: user.hasPassphrase,
    sortOrder: user.sortOrder,
    isActive: user.isActive,
    isAdmin: user.isAdmin,
});

export const writeUsersListCache = (users: UserMetadata[]): void => {
    try {
        const payload: CachedUserListPayload = {
            version: USERS_LIST_CACHE_VERSION,
            savedAt: Date.now(),
            users: users.map(slimEntry),
        };
        localStorage.setItem(USERS_LIST_CACHE_KEY, JSON.stringify(payload));
    } catch {
        // localStorage quota / private mode: not critical
    }
};

export const readUsersListCache = (): CachedUserListEntry[] | null => {
    try {
        const raw = localStorage.getItem(USERS_LIST_CACHE_KEY);
        if (!raw) return null;
        const payload = JSON.parse(raw) as CachedUserListPayload;
        if (!payload || payload.version !== USERS_LIST_CACHE_VERSION) return null;
        if (!Array.isArray(payload.users)) return null;
        return payload.users;
    } catch {
        return null;
    }
};

export const clearUsersListCache = (): void => {
    try { localStorage.removeItem(USERS_LIST_CACHE_KEY); } catch { /* best effort */ }
};

// True when the boot flow should display the AccountLockScreen.
// Skip rule (R1): single user without passphrase = silent boot.
export const needsAccountLockScreen = (
    users: { hasPassphrase: boolean }[],
): boolean => {
    if (users.length === 0) return false;
    if (users.length === 1 && !users[0].hasPassphrase) return false;
    return true;
};

// Dispatched when the user explicitly asks to return to the AccountLockScreen
// (e.g. from the titlebar UserDropdown "Lock" action). App.tsx listens for this
// and re-arms `accountLockState='needed'` so the L2 picker re-appears with the
// account cards. Behaves as a logout when at least one account has a passphrase.
export const ACCOUNT_LOCK_SCREEN_REQUESTED_EVENT = 'aeroftp-account-lock-screen-requested';

export const dispatchAccountLockScreenRequested = (): void => {
    try {
        window.dispatchEvent(new CustomEvent(ACCOUNT_LOCK_SCREEN_REQUESTED_EVENT));
    } catch {
        // Browserless tests: best effort.
    }
};

// Default account for the welcome-screen skip (discussion #270). When set,
// the boot flow auto-opens this account and bypasses the AccountLockScreen,
// but ONLY for password-free accounts: an account with a passphrase always
// shows the prompt, so authentication is never skipped. The explicit
// "switch account" action (ACCOUNT_LOCK_SCREEN_REQUESTED_EVENT) still forces
// the picker regardless of this preference.
const DEFAULT_ACCOUNT_KEY = 'aeroftp_default_account';

export const readDefaultAccountId = (): number | null => {
    try {
        const raw = localStorage.getItem(DEFAULT_ACCOUNT_KEY);
        if (!raw) return null;
        const n = Number.parseInt(raw, 10);
        return Number.isFinite(n) ? n : null;
    } catch {
        return null;
    }
};

export const writeDefaultAccountId = (id: number | null): void => {
    try {
        if (id == null) {
            localStorage.removeItem(DEFAULT_ACCOUNT_KEY);
        } else {
            localStorage.setItem(DEFAULT_ACCOUNT_KEY, String(id));
        }
    } catch {
        // localStorage quota / private mode: not critical.
    }
};
