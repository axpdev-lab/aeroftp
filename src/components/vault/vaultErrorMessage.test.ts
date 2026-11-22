// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { mapVaultError } from './useVaultState';

// mapVaultError keeps the raw text when a key comes back untranslated.
const t = (key: string) => `[${key}]`;

describe('mapVaultError', () => {
    // The texts the aerovault crate gives for a wrong password on an
    // AEROVAULT3 container (pinned on the Rust side by
    // a_wrong_password_gives_a_text_the_gui_reads_as_a_wrong_password).
    // The bare "key unwrap failed" holds the `key` alternative on its own,
    // since the full text also matches /wrong password/.
    it.each([
        'AES-KW unwrap failed',
        'key unwrap failed',
        'key unwrap failed (wrong password?)',
    ])(
        'reads %s as a wrong password',
        (raw) => {
            expect(mapVaultError(raw, t)).toBe('[vault.errors.wrongPassword]');
        },
    );

    it.each([
        ['Vault has damaged blocks: run repair before writing to it', 'Vault has damaged blocks: run repair before writing to it'],
        ['vault is locked by another writer (/tmp/fixture/corrupt.aerovault.aerolock); retry when it has finished', 'vault is locked by another writer (<path>); retry when it has finished'],
        ['/tmp/fixture/corrupt.aerovault is a symbolic link; use the target path', '<path> is a symbolic link; use the target path'],
        ['vault is locked by another writer (C:\\corrupt.aerovault.aerolock); retry when it has finished', 'vault is locked by another writer (<path>); retry when it has finished'],
        ['/corrupt.aerovault is a symbolic link; use the target path', '<path> is a symbolic link; use the target path'],
        ['vault is locked by another writer (/v.aerovault.aerolock); retry and/or wait', 'vault is locked by another writer (<path>); retry and/or wait'],
        // Paths with spaces are redacted whole, not up to their first space.
        ['/home/Alice Smith/corrupt.aerovault is a symbolic link; use the target path', '<path> is a symbolic link; use the target path'],
        ['/home/Alice Smith/my vault.aerovault is a symbolic link: writing through it would replace the link', '<path> is a symbolic link: writing through it would replace the link'],
        ['vault is locked by another writer (/home/Alice Smith/my vault.aerovault.aerolock); retry when it has finished', 'vault is locked by another writer (<path>); retry when it has finished'],
        ['vault lock file /home/Alice Smith/v.aerovault.aerolock: is a symbolic link; remove it', 'vault lock file <path>: is a symbolic link; remove it'],
        ['C:\\Users\\Alice Smith\\corrupt.aerovault is a symbolic link; use the target path', '<path> is a symbolic link; use the target path'],
        ['journal retirement sync failed (No space left on device); relocated journal and rollback copies retained at /home/Alice Smith/state/j 1', 'journal retirement sync failed (No space left on device); relocated journal and rollback copies retained at <path>'],
        ['Conflicting private pair journals at /home/Alice Smith/a and /home/Alice Smith/b. Preserve both for recovery', 'Conflicting private pair journals at <path> and <path>. Preserve both for recovery'],
        // Pair and strip recovery (aerovault 0.7.0): the path or the OS error
        // carries a generic cue, the instruction must survive it.
        ['AeroVault vault/sidecar publication is pending; preserve the vault and sidecar and recover the pair before opening or replacing it: /tmp/fixture/corrupt.aerovault', 'AeroVault vault/sidecar publication is pending; preserve the vault and sidecar and recover the pair before opening or replacing it: <path>'],
        ['Conflicting private pair journals at /tmp/corrupt/a and /tmp/corrupt/b. Preserve both for recovery', 'Conflicting private pair journals at <path> and <path>. Preserve both for recovery'],
        ['Fresh pair conflict; preserve the private journal /tmp/state/j1: No such file or directory (os error 2)', 'Fresh pair conflict; preserve the private journal <path>: No such file or directory (os error 2)'],
        ['An earlier private pair operation was recovered or completed; reopen that vault before creating it: Permission denied (os error 13)', 'An earlier private pair operation was recovered or completed; reopen that vault before creating it: Permission denied (os error 13)'],
        ['journal retirement sync failed (No space left on device); relocated journal and rollback copies retained at /tmp/state/j1', 'journal retirement sync failed (No space left on device); relocated journal and rollback copies retained at <path>'],
        ['Could not publish strip (Permission denied); recovery pending (restored); active journal, if still present: /tmp/v.aerostrip; preserve any relocated recovery material named above', 'Could not publish strip (Permission denied); recovery pending (restored); active journal, if still present: <path>; preserve any relocated recovery material named above'],
    ])('preserves the actionable refusal %s', (raw, message) => {
        expect(mapVaultError(raw, t)).toBe(message);
    });

    // The guard must not swallow the errors the table does translate.
    it.each([
        ['No such file or directory (os error 2)', '[vault.errors.fileNotFound]'],
        ['Permission denied (os error 13)', '[vault.errors.permissionDenied]'],
    ])('still translates %s', (raw, message) => {
        expect(mapVaultError(raw, t)).toBe(message);
    });
});
