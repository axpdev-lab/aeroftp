// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import { mapUserPartitionError } from './userPartitionErrors';

// A fake translator that echoes the key, so we can assert which key was picked.
const t = (key: string) => key;

describe('mapUserPartitionError', () => {
    it('maps the F-012 cross-machine unwrap failure to a clear, actionable key', () => {
        expect(mapUserPartitionError('Unwrap user data key: integrity check failed', t)).toBe(
            'accountLock.dataKeyUnreadable',
        );
        expect(mapUserPartitionError('DEK_VERIFIER_MISMATCH', t)).toBe('accountLock.dataKeyUnreadable');
    });

    it('maps the known account error codes to their keys', () => {
        const cases: Array<[string, string]> = [
            ['WRONG_PASSPHRASE', 'accountLock.wrongPassphrase'],
            ['PASSPHRASE_REQUIRED', 'accountLock.passphraseRequired'],
            ['PASSPHRASE_NOT_NEEDED', 'accountLock.passphraseNotNeeded'],
            ['USER_LOCKED', 'accountLock.userLocked'],
            ['NO_ACTIVE_USER', 'accountLock.noActiveUser'],
            ['NOT_ACTIVE_USER', 'accountLock.noActiveUser'],
            ['NOT_AUTHORIZED', 'accountLock.notAuthorized'],
            ['VAULT_LOCKED', 'accountLock.vaultLocked'],
            ['STORE_NOT_READY', 'accountLock.storeNotReady'],
            ['CANNOT_DEMOTE_LAST_ADMIN', 'accountLock.cannotDemoteLastAdmin'],
            ['CANNOT_DELETE_LAST_ADMIN', 'accountLock.cannotDeleteLastAdmin'],
            ['CANNOT_DELETE_LAST_USER', 'accountLock.cannotDeleteLastUser'],
            ['ADMIN_RESET_NOT_FOR_SELF', 'accountLock.adminResetNotForSelf'],
            ['USER_NOT_FOUND', 'accountLock.userNotFound'],
        ];
        for (const [code, key] of cases) {
            expect(mapUserPartitionError(code, t)).toBe(key);
        }
    });

    it('falls through to the raw message for unknown codes', () => {
        expect(mapUserPartitionError('some unexpected backend error', t)).toBe(
            'some unexpected backend error',
        );
    });

    it('accepts Error objects, not just strings', () => {
        expect(mapUserPartitionError(new Error('WRONG_PASSPHRASE'), t)).toBe('accountLock.wrongPassphrase');
    });
});
