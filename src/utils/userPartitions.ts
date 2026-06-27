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
    // Default / favourite user: the account auto-unlocked on launch. Persisted
    // in the shared user-partitions DB (single-winner), so it round-trips with
    // the CLI `users -i` Fav verb. Only password-free accounts can be default.
    // Per Ehud #311 (D1).
    isDefault: boolean;
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

// Mark (or clear) the default / favourite user auto-unlocked on launch. Stored
// in the shared user-partitions DB so it round-trips with the CLI `users -i`
// Fav verb (single-winner: setting one clears the others). Per Ehud #311 (D1).
export const setDefaultUser = (userId: number, isDefault: boolean): Promise<void> =>
    invoke<void>('user_partitions_set_default_user', { userId, isDefault });

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

// N4 (discussion #270): copy/move a saved server profile from the ACTIVE
// account into another user account. The source is always the active user, so
// the caller only supplies the target. `newProfileId` is generated frontend
// side (same `srv_<ts>_<rand>` convention as Duplicate) so the relocated copy
// is independent of the original; the backend copies the `server_<id>`
// credential onto it. A passphrase is required only when the target account is
// protected. When the target already holds the same server a Copy is skipped
// (`alreadyPresent` true, `inserted` false), while a Move always materialises
// the profile in the target (`inserted` true) before removing the source, so it
// can never lose the only copy (#366).
export interface ProfileRelocation {
    sourceProfileId: string;
    newProfileId: string;
    profileName: string;
    targetUserId: number;
    moved: boolean;
    alreadyPresent: boolean;
    inserted: boolean;
}

export const relocateServerProfile = (
    targetUserId: number,
    profileId: string,
    newProfileId: string,
    moveProfile: boolean,
    targetPassphrase?: string | null,
): Promise<ProfileRelocation> =>
    invoke<ProfileRelocation>('user_partitions_relocate_server_profile', {
        targetUserId,
        profileId,
        newProfileId,
        targetPassphrase: targetPassphrase ?? null,
        moveProfile,
    });

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

// Boot-time decision for the multi-user lock screen, extracted from App.tsx so
// it can be unit-tested in isolation (discussion #270). Given the user list,
// the backend unlock status, and the saved default account, it returns what the
// boot flow should do:
//   - 'ready'         : enter the app directly, no picker
//   - 'picker'        : show the AccountLockScreen
//   - 'unlockDefault' : silently unlock `userId` (password-free default) then enter
//
// Why the `isUnlocked` branch matters: the backend's user_unlock_status reports a
// persisted password-free *active* user as already unlocked. After a restart the
// default account therefore arrives here as already-unlocked, and without the
// explicit `ready` branch the `!isUnlocked` guard would be false and the picker
// would reappear on every launch (the bug Ehud reported on #270).
export type BootAccountAction =
    | { kind: 'ready' }
    | { kind: 'picker' }
    | { kind: 'unlockDefault'; userId: number };

export const decideBootAccountAction = (
    users: { id: number; hasPassphrase: boolean }[],
    status: { isUnlocked: boolean; unlockedUserId?: number | null },
    defaultAccountId: number | null,
): BootAccountAction => {
    if (users.length === 0) return { kind: 'ready' };
    const singleUserNeedsUnlock =
        users.length === 1 && users[0].hasPassphrase && !status.isUnlocked;
    const finalNeeded = users.length > 1 || singleUserNeedsUnlock;
    if (finalNeeded && defaultAccountId != null) {
        const target = users.find((u) => u.id === defaultAccountId);
        // Only password-free accounts skip the picker; a protected account
        // always shows its prompt so authentication is never bypassed.
        if (target && !target.hasPassphrase) {
            // Already on the default account (the backend reports a persisted
            // password-free active user as unlocked): enter directly.
            if (status.isUnlocked && status.unlockedUserId === target.id) {
                return { kind: 'ready' };
            }
            // Otherwise honor the default on boot: either nobody is unlocked
            // yet, or a *different* account was the last active one (e.g. the
            // star was set on an account other than the current one). The
            // default wins, so switch to it rather than showing the picker.
            return { kind: 'unlockDefault', userId: target.id };
        }
    }
    return finalNeeded ? { kind: 'picker' } : { kind: 'ready' };
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

// The default account for the welcome-screen skip (discussion #270) now lives
// in the shared user-partitions DB as `UserMetadata.isDefault`, so it
// round-trips with the CLI `users -i` Fav verb (#311). This pure helper reads
// it off the user list the boot flow already loads.
export const defaultUserIdFromList = (users: { id: number; isDefault: boolean }[]): number | null =>
    users.find((u) => u.isDefault)?.id ?? null;

// One-time upgrade path: the default account used to be browser-local
// localStorage (`aeroftp_default_account`). When the DB carries no default yet
// but a valid legacy id points to an existing password-free user, return that
// id so the boot flow can persist it into the DB once (then clear the legacy
// key). Returns null when there is nothing to migrate (DB already has a default,
// no legacy value, or the legacy id is stale / now passphrase-protected).
export const legacyDefaultToMigrate = (
    users: { id: number; hasPassphrase: boolean; isDefault: boolean }[],
    legacyId: number | null,
): number | null => {
    if (legacyId == null) return null;
    if (users.some((u) => u.isDefault)) return null;
    const target = users.find((u) => u.id === legacyId);
    return target && !target.hasPassphrase ? legacyId : null;
};

// Legacy localStorage key for the welcome-screen skip default (pre-#311).
// Retained only for the one-time migration into the DB (see above) and to clear
// stale values; new writes go through `setDefaultUser` (the DB command).
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
