// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import {
    PROVIDER_CATALOG,
    freeProtocols,
    paidProtocols,
    protocolBadgeRank,
    PROTOCOL_BADGE_ORDER,
} from './providerCatalog';
import { PROVIDER_MODE_GROUPS } from './providerModeGroups';

/**
 * One presentation order for connection methods, and one name per method,
 * wherever they are listed (Ehud, #347).
 *
 * Before this, the Add Service badges sorted by catalog *category* — which
 * ranked FTP and a native API the same — while the Quick Connect tabs followed
 * their own hand-written order. FileLu read `API · FTP · WebDAV · S5 (S3)` in
 * one place and `Native API · WebDAV · S3 · FTP` in the other, and its S3 tab
 * was labelled `S3` while its badge said `S5 (S3)`.
 */
describe('connection methods are ordered and named the same everywhere (#347)', () => {
    const methodsOf = (logoId: string) => {
        const c = PROVIDER_CATALOG.find(x => x.logoId === logoId);
        if (!c) throw new Error(`no catalog company for ${logoId}`);
        return [...freeProtocols(c), ...paidProtocols(c)];
    };

    const groupModes = (id: string) => {
        const g = PROVIDER_MODE_GROUPS.find(x => x.id === id);
        if (!g) throw new Error(`no mode group ${id}`);
        return g.modes;
    };

    it('ranks a native API before any wire protocol', () => {
        // A company's own API is what it *is*, rather than a protocol it also
        // speaks, and it carries the fullest feature set.
        expect(protocolBadgeRank('filelu')).toBeLessThan(protocolBadgeRank('webdav'));
        expect(protocolBadgeRank('mega')).toBeLessThan(protocolBadgeRank('s3'));
    });

    it('ranks the wire protocols in the agreed sequence', () => {
        expect([...PROTOCOL_BADGE_ORDER]).toEqual(['webdav', 'sftp', 'ftps', 'ftp', 's3']);
        const ranks = ['webdav', 'sftp', 'ftps', 'ftp', 's3'].map(protocolBadgeRank);
        expect(ranks).toEqual([...ranks].sort((a, b) => a - b));
    });

    it('puts FileLu WebDAV before FTP before S3, the case that was inconsistent', () => {
        // Was `API · FTP · WebDAV · S5 (S3)` because FTP and the API tied.
        const order = methodsOf('filelu').map(p => p.protocol);
        expect(order.indexOf('filelu')).toBeLessThan(order.indexOf('webdav'));
        expect(order.indexOf('webdav')).toBeLessThan(order.indexOf('ftp'));
        expect(order.indexOf('ftp')).toBeLessThan(order.indexOf('s3'));
    });

    it('sorts every company by that rank, with no exception left behind', () => {
        for (const c of PROVIDER_CATALOG) {
            for (const bucket of [freeProtocols(c), paidProtocols(c)]) {
                const ranks = bucket.map(p => protocolBadgeRank(p.protocol));
                expect(
                    ranks,
                    `${c.company} lists its methods out of order`,
                ).toEqual([...ranks].sort((a, b) => a - b));
            }
        }
    });

    it('gives a branded method the same name in the tab and in the badge', () => {
        // The tab said `S3` / `S4` while the badge said `S5 (S3)` / `S4 (S3)`.
        const badgeLabel = (logoId: string, providerId: string) => {
            const m = methodsOf(logoId).find(p => p.providerId === providerId);
            return m?.labelOverride ?? m?.label;
        };
        const tabLabel = (groupId: string, providerId: string) =>
            groupModes(groupId).find(m => m.providerId === providerId)?.label;

        expect(badgeLabel('filelu', 'filelu-s3')).toBe('S5 (S3)');
        expect(tabLabel('filelu', 'filelu-s3')).toBe('S5 (S3)');

        expect(badgeLabel('mega', 'mega-s4')).toBe('S4 (S3)');
        expect(tabLabel('mega', 'mega-s4')).toBe('S4 (S3)');
    });
});
