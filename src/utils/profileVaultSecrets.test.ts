// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { copyProfileVaultSecrets } from './profileVaultSecrets';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));

describe('copyProfileVaultSecrets', () => {
    beforeEach(() => { vi.mocked(invoke).mockReset(); });

    it('returns what the backend copied', async () => {
        vi.mocked(invoke).mockResolvedValue({ server: true, filen_api_key: false });
        await expect(copyProfileVaultSecrets('srv_a', 'srv_b', 'ftp', 7)).resolves.toEqual({ server: true, filen_api_key: false });
        expect(invoke).toHaveBeenCalledWith('copy_profile_secrets', { sourceId: 'srv_a', targetId: 'srv_b', protocol: 'ftp', expectedUserId: 7 });
    });

    // A copy the backend could not complete must not turn into a duplicate
    // without the source's secrets: the caller saves nothing and shows the error.
    it.each([
        'The oauth_dropbox_token secret of the source profile could not be read, so nothing was copied: keystore locked',
        'STORE_NOT_READY',
        'ACCOUNT_CHANGED',
    ])('rejects with the backend error %s', async message => {
        vi.mocked(invoke).mockImplementation(() => Promise.reject(message));
        await expect(copyProfileVaultSecrets('srv_a', 'srv_b', 'dropbox', 7)).rejects.toBe(message);
    });
});
