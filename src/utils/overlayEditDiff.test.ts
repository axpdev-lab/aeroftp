// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import type { AeroCryptOverlayBinding } from '../types';
import { overlayEditDiffers, type OverlayEditForm, type OverlayEditStored } from './overlayEditDiff';

const rcloneBinding: AeroCryptOverlayBinding = {
    enabled: true,
    kind: 'rclone-crypt',
    remoteScope: '/Backups/vault',
    localScope: '/home/u/vault',
    withHeader: false,
    useDefaultSalt: false,
    filenameEncryption: 'standard',
    directoryNameEncryption: true,
    aead: 'auto',
};

// The form exactly as the edit hydration leaves it for `rcloneBinding`.
const hydrated = (over: Partial<OverlayEditForm> = {}): OverlayEditForm => ({
    enabled: true,
    eligible: true,
    kind: 'rclone-crypt',
    withHeader: false,
    useDefaultSalt: false,
    filenameEncryption: 'standard',
    directoryNameEncryption: true,
    overlaysRemotePath: '/Backups/vault',
    remoteDir: '/Backups',
    password: '',
    salt: '',
    keyfilePath: '',
    ...over,
});

const stored = (over: Partial<OverlayEditStored> = {}): OverlayEditStored => ({
    binding: rcloneBinding,
    remotePath: '/Backups',
    hydratedKeyfilePath: '',
    ...over,
});

describe('overlayEditDiffers (#369: OAuth Save without signing in again)', () => {
    it('reads an untouched edit form as unchanged, so Save stays disabled', () => {
        expect(overlayEditDiffers(hydrated(), stored())).toBe(false);
    });

    it('sees each overlay setting a user can change on the OAuth page', () => {
        expect(overlayEditDiffers(hydrated({ enabled: false }), stored())).toBe(true);
        expect(overlayEditDiffers(hydrated({ filenameEncryption: 'obfuscate' }), stored())).toBe(true);
        expect(overlayEditDiffers(hydrated({ directoryNameEncryption: false }), stored())).toBe(true);
        expect(overlayEditDiffers(hydrated({ withHeader: true }), stored())).toBe(true);
        expect(overlayEditDiffers(hydrated({ overlaysRemotePath: '/Backups/vault/sub' }), stored())).toBe(true);
        expect(overlayEditDiffers(hydrated({ password: 'typed' }), stored())).toBe(true);
        expect(overlayEditDiffers(hydrated({ salt: 'typed' }), stored())).toBe(true);
    });

    it('sees an overlay being added to a profile that had none', () => {
        const none = stored({ binding: undefined });
        expect(overlayEditDiffers(hydrated({ enabled: false, kind: null }), none)).toBe(false);
        expect(overlayEditDiffers(hydrated({ kind: 'aerocrypt' }), none)).toBe(true);
    });

    it('treats enabled-without-a-kind and an ineligible backend as no overlay, as the save does', () => {
        const none = stored({ binding: undefined });
        expect(overlayEditDiffers(hydrated({ kind: null }), none)).toBe(false);
        expect(overlayEditDiffers(hydrated({ eligible: false }), none)).toBe(false);
    });

    it('compares the anchor a blank stored scope stands for, not the blank', () => {
        // Stored '' means "the Remote Path"; the form materializes it on edit.
        const blankScope = stored({ binding: { ...rcloneBinding, remoteScope: '' } });
        expect(overlayEditDiffers(hydrated({ overlaysRemotePath: '/Backups' }), blankScope)).toBe(false);
    });

    it('keeps a blank or unchanged keyfile path as no change, like the save does', () => {
        const aero = stored({
            binding: { ...rcloneBinding, kind: 'aerocrypt', directoryNameEncryption: undefined },
            hydratedKeyfilePath: '/keys/a.keyfile',
        });
        const form = (keyfilePath: string) => hydrated({ kind: 'aerocrypt', keyfilePath });
        expect(overlayEditDiffers(form('/keys/a.keyfile'), aero)).toBe(false);
        expect(overlayEditDiffers(form(''), aero)).toBe(false);
        expect(overlayEditDiffers(form('/keys/b.keyfile'), aero)).toBe(true);
    });

    it('ignores the rclone-only fields on a native AeroCrypt overlay, which never persists them', () => {
        const aero = stored({ binding: { ...rcloneBinding, kind: 'aerocrypt', filenameEncryption: 'standard' } });
        expect(overlayEditDiffers(hydrated({ kind: 'aerocrypt', directoryNameEncryption: false, salt: 'x' }), aero)).toBe(false);
    });
});
