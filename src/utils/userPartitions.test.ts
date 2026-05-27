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
