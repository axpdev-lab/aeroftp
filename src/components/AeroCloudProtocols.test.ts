// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import { aeroCloudMaturity, isAeroCloudProtocol } from './AeroCloudProtocols';

describe('AeroCloud protocol offer', () => {
    it('never offers a protocol the AeroCloud backend does not dispatch', () => {
        for (const protocol of ['github', 'gitlab', 'swift', 'immich', 'drime', 'filelu-rsync', 'googlephotos', 'mtp', undefined]) {
            expect(isAeroCloudProtocol(protocol), String(protocol)).toBe(false);
        }
    });

    it('offers the protocols verified live, each with a maturity', () => {
        for (const protocol of ['sftp', 'ftp', 's3', 'backblaze', 'onedrive', 'zohoworkdrive', 'cloudinary', 'yandexdisk']) {
            expect(isAeroCloudProtocol(protocol), protocol).toBe(true);
            expect(aeroCloudMaturity(protocol), protocol).toBeDefined();
        }
    });

    it('keeps the maturity of a dispatched protocol the wizard does not offer yet', () => {
        expect(isAeroCloudProtocol('fourshared')).toBe(false);
        expect(aeroCloudMaturity('fourshared')).toBe('alpha');
    });
});
