// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import { CUSTOM_PROFILES } from './DiscoverPanel';
import { protocolBadgeRank, PROTOCOL_BADGE_ORDER } from '../providerCatalog';

/**
 * One presentation order for connection methods, everywhere they are listed
 * (Ehud, #347). v4.1.7 unified the Add Service badges with the Quick Connect
 * tabs, but the CUSTOM / GENERIC SERVERS strip under the table kept a third
 * opinion: it rendered its array in the order it happened to be authored,
 * `FTP, SFTP, S3, WebDAV`, which is not the agreed sequence.
 *
 * The array is now sorted by the shared rank, so this fails if someone
 * reintroduces a hand-kept order or adds an entry at the wrong rank.
 */
describe('CUSTOM / GENERIC SERVERS follow the one badge order (#347)', () => {
    it('is sorted by the shared protocol rank', () => {
        const ranks = CUSTOM_PROFILES.map(p => protocolBadgeRank(p.protocol));
        expect(ranks).toEqual([...ranks].sort((a, b) => a - b));
    });

    it('reads WebDAV, SFTP, FTP, S3, the sequence the badges already use', () => {
        // Spelled out rather than derived, so a change to the shared order has
        // to be made deliberately here too instead of passing silently.
        expect(CUSTOM_PROFILES.map(p => p.protocol)).toEqual(['webdav', 'sftp', 'ftp', 's3']);
    });

    it('ranks every entry it lists, so none of them sorts as a native API', () => {
        // An unranked protocol gets -1 and would jump to the front of the strip.
        for (const p of CUSTOM_PROFILES) {
            expect(PROTOCOL_BADGE_ORDER).toContain(p.protocol);
        }
    });
});
