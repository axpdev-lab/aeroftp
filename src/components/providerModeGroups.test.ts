// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import {
    PROVIDER_MODE_GROUPS,
    findActiveMode,
    findActiveModeGroup,
    resolveModeSwitchCredentials,
    type ProviderModeGroup,
} from './providerModeGroups';

// Issue #215 (Ehud, security): switching protocol tabs leaked the API password
// into the S3 "Secret Access Key" field (the same shared `password` slot,
// relabeled). These tests pin the credential-carry decision that prevents it.
describe('resolveModeSwitchCredentials (#215 password leak)', () => {
    const filen = findActiveModeGroup('filen-desktop-s3', 's3') as ProviderModeGroup;
    const koofr = findActiveModeGroup('koofr', 'webdav') as ProviderModeGroup;
    const current = { username: 'me@example.com', password: 'super-secret-api-pw' };

    it('non-shared group, first visit (no stash) -> BLANK, never carries the API password', () => {
        // The exact leak Ehud reported: Filen Native (account password typed) ->
        // Local S3 must NOT pre-fill the Secret Access Key with that password.
        expect(filen.sharedCredentials).not.toBe(true);
        const r = resolveModeSwitchCredentials(filen, undefined, current);
        expect(r).toEqual({ username: '', password: '' });
        expect(r.password).not.toBe(current.password);
    });

    it('non-shared group, return visit (stash) -> restores the user typed keys', () => {
        const stash = { username: 'AKIA-access-key', password: 's3-secret-key' };
        expect(resolveModeSwitchCredentials(filen, stash, current)).toEqual(stash);
    });

    it('non-shared group, stash with an intentionally empty secret -> stays empty', () => {
        const stash = { username: 'AKIA-access-key', password: '' };
        expect(resolveModeSwitchCredentials(filen, stash, current)).toEqual({
            username: 'AKIA-access-key',
            password: '',
        });
    });

    it('shared group (Koofr/OpenDrive), first visit -> carries the single credential set', () => {
        expect(koofr.sharedCredentials).toBe(true);
        expect(resolveModeSwitchCredentials(koofr, undefined, current)).toEqual(current);
    });

    it('every group whose modes mix an account-secret protocol with S3/WebDAV is leak-safe', () => {
        // Regression net for MEGA (API/S4), FileLu (API/S3), Filen (API/WebDAV/S3):
        // any group NOT marked sharedCredentials must start blank on first visit.
        for (const group of PROVIDER_MODE_GROUPS) {
            if (group.sharedCredentials) continue;
            const r = resolveModeSwitchCredentials(group, undefined, current);
            expect(r, `group ${group.id} leaked credentials on first visit`).toEqual({
                username: '',
                password: '',
            });
        }
    });
});

// Proton Drive follows the Filen form: a mode strip with the API (AeroFTP's
// native client) first and the official CLI second. Both tabs use the
// `proton` protocol and are told apart by the `proton_mode` option, so a
// profile without it, which is every profile saved before the native client,
// stays a CLI profile.
describe('Proton Drive mode group', () => {
    const group = findActiveModeGroup(undefined, 'proton') as ProviderModeGroup;

    it('a Proton connection shows the mode strip', () => {
        expect(group?.id).toBe('proton');
    });

    it('lists the native API tab first and the CLI tab second', () => {
        expect(group.modes.map(m => m.label)).toEqual(['API', 'CLI']);
        expect(group.modes[0].options).toEqual({ proton_mode: 'native' });
        expect(group.modes[0].requires).toBe('proton-native');
        expect(group.modes[0].bridgeKind).toBeUndefined();
        expect(group.modes[1].options).toBeUndefined();
        expect(group.modes[1].bridgeKind).toBe('proton-cli');
    });

    it('a profile without proton_mode, or with cli, resolves to the CLI tab', () => {
        expect(findActiveMode(group, undefined, 'proton')?.label).toBe('CLI');
        expect(findActiveMode(group, 'proton', 'proton')?.label).toBe('CLI');
        expect(findActiveMode(group, undefined, 'proton', {})?.label).toBe('CLI');
        expect(findActiveMode(group, undefined, 'proton', { proton_mode: 'cli' })?.label).toBe('CLI');
    });

    it('a native profile resolves to the API tab, without the CLI status bridge', () => {
        const mode = findActiveMode(group, undefined, 'proton', { proton_mode: 'native' });
        expect(mode?.label).toBe('API');
        expect(mode?.bridgeKind).toBeUndefined();
    });
});
