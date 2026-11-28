// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { legacyPuterProfileForEdit, PUTER_HOST_TEMPLATE, PUTER_PROVIDER_ID } from './puterProfile';

describe('legacyPuterProfileForEdit', () => {
    it('moves the account from the host into the username field and keeps its case', () => {
        expect(legacyPuterProfileForEdit(PUTER_PROVIDER_ID, 'https://dav.puter.com/AeroFTP', '-token')).toEqual({
            server: PUTER_HOST_TEMPLATE,
            username: 'AeroFTP',
        });
    });

    it('takes the first path segment with a trailing slash or a deeper path', () => {
        expect(legacyPuterProfileForEdit(PUTER_PROVIDER_ID, 'https://dav.puter.com/AeroFTP/', '-token')?.username).toBe('AeroFTP');
        expect(legacyPuterProfileForEdit(PUTER_PROVIDER_ID, 'https://dav.puter.com/AeroFTP/Documents', '-token')?.username).toBe('AeroFTP');
    });

    it('returns null for a profile already in the new form', () => {
        expect(legacyPuterProfileForEdit(PUTER_PROVIDER_ID, PUTER_HOST_TEMPLATE, 'AeroFTP')).toBeNull();
    });

    it('returns null for -token on the template host, where no account can be recovered', () => {
        expect(legacyPuterProfileForEdit(PUTER_PROVIDER_ID, PUTER_HOST_TEMPLATE, '-token')).toBeNull();
    });

    it('returns null for another provider id', () => {
        expect(legacyPuterProfileForEdit('nextcloud', 'https://dav.puter.com/AeroFTP', '-token')).toBeNull();
        expect(legacyPuterProfileForEdit(undefined, 'https://dav.puter.com/AeroFTP', '-token')).toBeNull();
    });

    it('returns null for a non-Puter host', () => {
        expect(legacyPuterProfileForEdit(PUTER_PROVIDER_ID, 'https://example.com/AeroFTP', '-token')).toBeNull();
    });

    it('returns null for an empty or garbage host', () => {
        expect(legacyPuterProfileForEdit(PUTER_PROVIDER_ID, '', '-token')).toBeNull();
        expect(legacyPuterProfileForEdit(PUTER_PROVIDER_ID, undefined, '-token')).toBeNull();
        expect(legacyPuterProfileForEdit(PUTER_PROVIDER_ID, 'not a url', '-token')).toBeNull();
        expect(legacyPuterProfileForEdit(PUTER_PROVIDER_ID, 'https://dav.puter.com/%E0%A4%A', '-token')).toBeNull();
    });

    it('decodes a percent-encoded account', () => {
        expect(legacyPuterProfileForEdit(PUTER_PROVIDER_ID, 'https://dav.puter.com/Ali%20ce', '-token')?.username).toBe('Ali ce');
    });

    it('returns null when the path account is -token or empty', () => {
        expect(legacyPuterProfileForEdit(PUTER_PROVIDER_ID, 'https://dav.puter.com/-token', '-token')).toBeNull();
        expect(legacyPuterProfileForEdit(PUTER_PROVIDER_ID, 'https://dav.puter.com/', '-token')).toBeNull();
    });
});
