// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import { Boxes, Braces, Cloud, Container, Database, KeyRound, Server, TerminalSquare } from 'lucide-react';
import { CONNECTION_METHOD_GLYPH, methodIcon } from './connectionMethodIcons';
import modeGroupsRaw from './providerModeGroups.tsx?raw';
import myServersRaw from './IntroHub/MyServersTable.tsx?raw';
import catalogTableRaw from './IntroHub/CatalogTable.tsx?raw';

/**
 * The icons for connection methods were assigned by hand at each site, so they
 * disagreed across surfaces and collided within one (Ehud, #347):
 *
 *   - `Cloud` meant OAuth in the My Servers table and a native API in MEGA's
 *     Quick Connect page.
 *   - `Database` meant a native API in the table and S3 in Quick Connect, and
 *     inside the table API and S3 both drew `Database`, separated only by a
 *     colour that no legend explains.
 *   - A native API drew `Cloud` for MEGA, Filen, Koofr and OpenDrive but `Key`
 *     for FileLu; S3 drew `Database` for MEGA and FileLu but `Layers` for Filen.
 */
describe('one glyph per connection method (#347)', () => {
    it('never gives two methods the same glyph', () => {
        // The collision that mattered: a reader cannot tell API from S3 when the
        // shape is identical and only the tint differs.
        const shapes = Object.entries(CONNECTION_METHOD_GLYPH)
            .filter(([m]) => m !== 'Crypt' && m !== 'FTPS');
        const seen = new Map<unknown, string>();
        for (const [method, glyph] of shapes) {
            const clash = seen.get(glyph);
            expect(clash, `${method} draws the same glyph as ${clash}`).toBeUndefined();
            seen.set(glyph, method);
        }
    });

    it('keeps the two deliberate aliases explicit', () => {
        // FTPS is FTP over TLS and Crypt is an E2E overlay: sharing a shape with
        // their family is the intent, not a leftover.
        expect(CONNECTION_METHOD_GLYPH.FTPS).toBe(CONNECTION_METHOD_GLYPH.FTP);
        expect(CONNECTION_METHOD_GLYPH.Crypt).toBe(CONNECTION_METHOD_GLYPH.E2E);
    });

    it('separates a native API from OAuth and from S3', () => {
        expect(CONNECTION_METHOD_GLYPH.API).not.toBe(CONNECTION_METHOD_GLYPH.OAuth);
        expect(CONNECTION_METHOD_GLYPH.API).not.toBe(CONNECTION_METHOD_GLYPH.S3);
    });

    it('leaves no surface picking its own icon for a method', () => {
        // Both surfaces must go through the map. A literal lucide element on a
        // mode or a protocol row is how the three surfaces drifted apart.
        expect(modeGroupsRaw).not.toMatch(/icon: <(Key|Layers|Cloud|Globe|Database|Server)\b/);
        expect(modeGroupsRaw).toContain("methodIcon('API'");
        expect(modeGroupsRaw).toContain("methodIcon('S3'");

        for (const method of ['OAuth', 'API', 'WebDAV', 'S3', 'SFTP']) {
            expect(
                myServersRaw,
                `My Servers still hand-picks an icon for ${method}`,
            ).toContain(`${method}: methodIcon('${method}'`);
        }

        // CatalogTable used to keep a private PROTOCOL_GLYPHS map, so S3/OAuth
        // /FTP/SFTP disagreed with the two surfaces above. One map, or it
        // drifts again.
        expect(catalogTableRaw).toContain('methodIcon(');
        expect(catalogTableRaw).not.toContain('PROTOCOL_GLYPHS');
    });

    it('keeps the shipped #567 shapes', () => {
        // CatalogTable's leftover private map drew a bucket for S3, KeyRound
        // for OAuth, ShieldCheck for FTPS/SFTP. Unifying through methodIcon
        // must not quietly rewrite the assignments already on main.
        expect(CONNECTION_METHOD_GLYPH.OAuth).toBe(Cloud);
        expect(CONNECTION_METHOD_GLYPH.API).toBe(Braces);
        expect(CONNECTION_METHOD_GLYPH.S3).toBe(Database);
        expect(CONNECTION_METHOD_GLYPH.FTP).toBe(Server);
        expect(CONNECTION_METHOD_GLYPH.SFTP).toBe(KeyRound);
    });

    it('covers the catalog methods CatalogTable used to keep privately', () => {
        for (const method of ['Swift', 'Blob', 'MEGAcmd'] as const) {
            expect(
                CONNECTION_METHOD_GLYPH,
                `${method} is still missing from the shared map`,
            ).toHaveProperty(method);
            expect(methodIcon(method, { size: 11 }), `${method} still renders null`).not.toBeNull();
        }
        expect(CONNECTION_METHOD_GLYPH.Swift).toBe(Boxes);
        expect(CONNECTION_METHOD_GLYPH.Blob).toBe(Container);
        expect(CONNECTION_METHOD_GLYPH.MEGAcmd).toBe(TerminalSquare);
        // Fail-first: Blob = Database exploded unique-shape with
        // "Blob draws the same glyph as S3". Container is the non-colliding
        // pick; S3 stays Database.
        expect(CONNECTION_METHOD_GLYPH.Blob).not.toBe(CONNECTION_METHOD_GLYPH.S3);
    });
});
