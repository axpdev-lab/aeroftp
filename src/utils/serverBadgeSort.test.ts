// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import type { ServerProfile } from '../types';
import { badgeSortLabel, isOcsBadgeProvider, OCS_BADGE_PROVIDER_IDS } from './serverBadgeSort';

const profile = (providerId: string, extra: Partial<ServerProfile> = {}): ServerProfile => ({
    id: `srv_${providerId}`,
    name: providerId,
    host: 'https://example.invalid/',
    port: 443,
    username: 'alice',
    protocol: 'webdav',
    providerId,
    ...extra,
} as ServerProfile);

describe('the Type column sorts as the card badge reads', () => {
    it('every OCS preset, bare or -webdav, sorts under API OCS', () => {
        for (const id of OCS_BADGE_PROVIDER_IDS) {
            expect(isOcsBadgeProvider(id), id).toBe(true);
            expect(badgeSortLabel(profile(id)), id).toBe('API OCS');
        }
        expect(isOcsBadgeProvider('nextcloud')).toBe(false);
        expect(badgeSortLabel(profile('nextcloud'))).toBe('WebDAV nextcloud');
    });

    // The card replaces the OCS badge with the Crypt badge on an encrypted
    // overlay, so the sort follows it into the Crypt family.
    it.each(['aerocrypt', 'rclone-crypt'] as const)('a %s overlay on an OCS preset sorts under Crypt', (kind) => {
        for (const id of ['murena', 'felicloud', 'tabdigital-webdav']) {
            const bound = profile(id, { aeroCryptOverlay: { enabled: true, kind } as ServerProfile['aeroCryptOverlay'] });
            expect(badgeSortLabel(bound), id).toBe(`Crypt ${id}`);
        }
    });
});
