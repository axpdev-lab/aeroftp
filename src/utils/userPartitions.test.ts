// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
//
// Unit coverage for MU-LS helpers: the localStorage cache round-trip and
// the boot-decision predicate that the lock screen mounting effect depends
// on. Cache write is gated on a successful IPC fetch in production; these
// tests exercise only the pure-TS logic.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
    clearUsersListCache,
    decideBootAccountAction,
    defaultUserIdFromList,
    legacyDefaultToMigrate,
    needsAccountLockScreen,
    readUsersListCache,
    writeUsersListCache,
    type UserMetadata,
} from './userPartitions';

const makeUser = (overrides: Partial<UserMetadata> = {}): UserMetadata => ({
    id: 1,
    name: 'default',
    avatarEmoji: null,
    avatarColor: null,
    hasPassphrase: false,
    sortOrder: 0,
    createdAt: 0,
    updatedAt: 0,
    lastUnlockedAt: null,
    isActive: true,
    isAdmin: true,
    isDefault: false,
    ...overrides,
});

beforeEach(() => {
    const store = new Map<string, string>();
    vi.stubGlobal('localStorage', {
        getItem: (key: string) => (store.has(key) ? store.get(key)! : null),
        setItem: (key: string, value: string) => { store.set(key, value); },
        removeItem: (key: string) => { store.delete(key); },
        clear: () => { store.clear(); },
        key: (i: number) => Array.from(store.keys())[i] ?? null,
        get length() { return store.size; },
    });
});

afterEach(() => { vi.unstubAllGlobals(); });

describe('needsAccountLockScreen', () => {
    it('returns false when no users exist', () => {
        expect(needsAccountLockScreen([])).toBe(false);
    });

    it('returns false for R1 (single passphrase-less user)', () => {
        expect(needsAccountLockScreen([{ hasPassphrase: false }])).toBe(false);
    });

    it('returns true for a single user with an account password', () => {
        expect(needsAccountLockScreen([{ hasPassphrase: true }])).toBe(true);
    });

    it('returns true whenever there is more than one user', () => {
        expect(needsAccountLockScreen([
            { hasPassphrase: false },
            { hasPassphrase: false },
        ])).toBe(true);
        expect(needsAccountLockScreen([
            { hasPassphrase: false },
            { hasPassphrase: true },
        ])).toBe(true);
    });
});

describe('decideBootAccountAction (#270)', () => {
    const A = { id: 1, hasPassphrase: false };
    const B = { id: 2, hasPassphrase: false };
    const locked = (isUnlocked: boolean, unlockedUserId: number | null = null) =>
        ({ isUnlocked, unlockedUserId });

    it('enters directly when there are no users', () => {
        expect(decideBootAccountAction([], locked(false), null)).toEqual({ kind: 'ready' });
    });

    it('enters directly for R1 (single passphrase-less user, no default)', () => {
        expect(decideBootAccountAction([A], locked(false), null)).toEqual({ kind: 'ready' });
    });

    it('shows the picker for multiple users with no default set', () => {
        expect(decideBootAccountAction([A, B], locked(false), null)).toEqual({ kind: 'picker' });
    });

    it('silently unlocks a password-free default on a fresh (locked) boot', () => {
        expect(decideBootAccountAction([A, B], locked(false), A.id))
            .toEqual({ kind: 'unlockDefault', userId: A.id });
    });

    // The regression Ehud reported: after a tray Quit + relaunch the backend
    // reports the persisted password-free default as already unlocked, so the
    // `!isUnlocked` path never runs. Must still enter directly, not the picker.
    it('enters directly when the default is already the unlocked active user', () => {
        expect(decideBootAccountAction([A, B], locked(true, A.id), A.id))
            .toEqual({ kind: 'ready' });
    });

    it('never skips a passphrase-protected default (always shows the picker)', () => {
        const protectedB = { id: 2, hasPassphrase: true };
        expect(decideBootAccountAction([A, protectedB], locked(false), protectedB.id))
            .toEqual({ kind: 'picker' });
    });

    it('shows the picker when the default id no longer matches any user', () => {
        expect(decideBootAccountAction([A, B], locked(false), 999)).toEqual({ kind: 'picker' });
    });

    it('switches to the default when a different account is the active one', () => {
        // The star was set on A while B was the last active account (the backend
        // reports B as the unlocked password-free user after restart). The
        // default must win on boot, so switch to A rather than showing the picker.
        expect(decideBootAccountAction([A, B], locked(true, B.id), A.id))
            .toEqual({ kind: 'unlockDefault', userId: A.id });
    });

    it('prompts a single protected user that is still locked', () => {
        const protectedA = { id: 1, hasPassphrase: true };
        expect(decideBootAccountAction([protectedA], locked(false), null))
            .toEqual({ kind: 'picker' });
    });
});

describe('default user (DB flag, #311)', () => {
    it('reads the single default off the user list', () => {
        expect(defaultUserIdFromList([])).toBeNull();
        expect(
            defaultUserIdFromList([
                { id: 1, isDefault: false },
                { id: 2, isDefault: true },
            ]),
        ).toBe(2);
        expect(
            defaultUserIdFromList([
                { id: 1, isDefault: false },
                { id: 2, isDefault: false },
            ]),
        ).toBeNull();
    });

    describe('legacyDefaultToMigrate', () => {
        const A = { id: 1, hasPassphrase: false, isDefault: false };
        const protectedB = { id: 2, hasPassphrase: true, isDefault: false };

        it('migrates a valid password-free legacy default into the DB', () => {
            expect(legacyDefaultToMigrate([A, protectedB], 1)).toBe(1);
        });

        it('does nothing when the DB already carries a default', () => {
            expect(
                legacyDefaultToMigrate([{ ...A, isDefault: true }, protectedB], 2),
            ).toBeNull();
        });

        it('does nothing when there is no legacy value', () => {
            expect(legacyDefaultToMigrate([A, protectedB], null)).toBeNull();
        });

        it('skips a stale legacy id that no longer exists', () => {
            expect(legacyDefaultToMigrate([A, protectedB], 999)).toBeNull();
        });

        it('skips a legacy id that is now passphrase-protected', () => {
            expect(legacyDefaultToMigrate([A, protectedB], 2)).toBeNull();
        });
    });
});

describe('users list cache', () => {
    it('round-trips a slimmed payload through localStorage', () => {
        const users: UserMetadata[] = [
            makeUser({ id: 1, name: 'default', hasPassphrase: false, isActive: true }),
            makeUser({ id: 2, name: 'alice', avatarEmoji: '🦊', avatarColor: '#10b981', hasPassphrase: true, sortOrder: 1, isActive: false }),
        ];
        writeUsersListCache(users);
        const cached = readUsersListCache();
        expect(cached).not.toBeNull();
        expect(cached).toHaveLength(2);
        expect(cached![0]).toMatchObject({ id: 1, name: 'default', hasPassphrase: false, isActive: true });
        expect(cached![1]).toMatchObject({ id: 2, name: 'alice', avatarEmoji: '🦊', avatarColor: '#10b981', hasPassphrase: true });
    });

    it('drops keys not on the slim allowlist (no leak of timestamps)', () => {
        const users: UserMetadata[] = [
            makeUser({ id: 9, name: 'bob', lastUnlockedAt: 1234567890, createdAt: 999, updatedAt: 1000 }),
        ];
        writeUsersListCache(users);
        const cached = readUsersListCache();
        expect(cached).not.toBeNull();
        // CachedUserListEntry shape is intentionally narrow: no createdAt /
        // updatedAt / lastUnlockedAt to keep the boot frame paint cheap and
        // avoid persisting metadata that is not needed for the picker.
        expect(Object.keys(cached![0]).sort()).toEqual([
            'avatarColor',
            'avatarEmoji',
            'hasPassphrase',
            'id',
            'isActive',
            'isAdmin',
            'name',
            'sortOrder',
        ]);
    });

    it('returns null when no cache has been written', () => {
        expect(readUsersListCache()).toBeNull();
    });

    it('returns null on a corrupted cache payload (defensive parse)', () => {
        localStorage.setItem('aeroftp-users-list-cache', 'not-json');
        expect(readUsersListCache()).toBeNull();
    });

    it('returns null when the cache version does not match', () => {
        localStorage.setItem(
            'aeroftp-users-list-cache',
            JSON.stringify({ version: 99, savedAt: 0, users: [] }),
        );
        expect(readUsersListCache()).toBeNull();
    });

    it('clears the cache when asked', () => {
        writeUsersListCache([makeUser()]);
        expect(readUsersListCache()).not.toBeNull();
        clearUsersListCache();
        expect(readUsersListCache()).toBeNull();
    });
});
