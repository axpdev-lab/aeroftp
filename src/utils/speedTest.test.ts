// SPDX-License-Identifier: GPL-3.0-or-later

import { describe, expect, it } from 'vitest';
import type { ServerProfile } from '../types';
import { speedTestServerName } from './speedTest';

const profile = (partial: Partial<ServerProfile>): ServerProfile => ({
    id: 'test',
    name: 'Test',
    host: 'example.test',
    username: '',
    ...partial,
} as ServerProfile);

describe('speedTestServerName', () => {
    it('uses one pCloud Drive label for OAuth and WebDAV defaults', () => {
        expect(speedTestServerName(profile({ name: 'pCloud', protocol: 'pcloud' }))).toBe('pCloud Drive');
        expect(speedTestServerName(profile({ name: 'pCloud Drive (WebDAV)', protocol: 'webdav', providerId: 'pcloud-webdav' }))).toBe('pCloud Drive');
    });

    it('preserves a user-supplied profile name', () => {
        expect(speedTestServerName(profile({ name: 'Photos EU', protocol: 'pcloud' }))).toBe('Photos EU');
    });
});
