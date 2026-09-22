// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { connectionViaLabel } from './connectionViaLabel';

describe('connectionViaLabel', () => {
    it('names the local program a connection goes through', () => {
        expect(connectionViaLabel({ protocol: 'proton' })).toBe('Proton Drive CLI');
        expect(connectionViaLabel({ protocol: 'mega', options: { mega_mode: 'megacmd' } })).toBe('MEGAcmd');
        expect(connectionViaLabel({ protocol: 'webdav', providerId: 'megacmd-webdav' })).toBe('MEGAcmd (WebDAV)');
        expect(connectionViaLabel({ protocol: 'webdav', providerId: 'filen-desktop-webdav' })).toBe('Filen Desktop (WebDAV)');
        expect(connectionViaLabel({ protocol: 's3', providerId: 'filen-desktop-s3' })).toBe('Filen Desktop (S3)');
    });

    it('keeps the protocol in capitals for everything else', () => {
        expect(connectionViaLabel({ protocol: 'mega', options: { mega_mode: 'native' } })).toBe('MEGA');
        expect(connectionViaLabel({ protocol: 'sftp' })).toBe('SFTP');
        expect(connectionViaLabel({ protocol: 's3', providerId: 'backblaze' })).toBe('S3');
        expect(connectionViaLabel(null)).toBe('FTP');
    });
});
