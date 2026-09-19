// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * One glyph per connection method, for every surface that draws them.
 *
 * Ehud reported three symptoms on #347 that are one defect: the icons were
 * assigned by hand at each site, so they disagreed and, worse, collided.
 *
 *   - `Cloud` meant OAuth in the My Servers table and a native API in MEGA's
 *     Quick Connect page, so the same glyph named two different things.
 *   - `Database` meant a native API in the My Servers table and S3 in the
 *     Quick Connect pages of MEGA and FileLu. Inside the My Servers table
 *     itself API and S3 both drew `Database`, separated only by colour, which
 *     no legend explains and no colour-blind reader can use.
 *   - A native API drew `Cloud` for MEGA, Filen, Koofr and OpenDrive but `Key`
 *     for FileLu, and S3 drew `Database` for MEGA and FileLu but `Layers` for
 *     Filen.
 *
 * The map below is the only place these are decided. Adding a surface means
 * importing it, not inventing a fourth opinion.
 *
 * On the two choices that had to change rather than merely be unified:
 *
 *   - A native API is `Braces`. It cannot keep `Database` (S3's shape at the time) or
 *     `Cloud` (that is OAuth), and `Key`/`KeyRound` reads as SFTP. Curly braces
 *     say "this provider's own JSON API" without borrowing anyone's meaning.
 *     Ehud confirmed `{ }` on 11 Aug (#347).
 *   - S3 is a bucket, as Ehud asked on #347 (the 🪣 emoji, and the pail in
 *     the thumbnail on aws.amazon.com/s3). It was `Database` since #567, and
 *     the bucket was first declined to keep that shape; the owner reversed
 *     that on 2026-09-19. A bucket is what S3 calls its containers, so the
 *     glyph names the concept a user types, where `Database` named nothing
 *     S3 actually has. lucide ships no bucket (`PaintBucket` is the fill
 *     tool, tipped over), so `Bucket` below is drawn in lucide's grid and
 *     stroke so it sits next to the rest without looking borrowed.
 *
 * CatalogTable still kept a private map for three catalog labels the shared
 * map did not know, so those join here without touching the #567 assignments:
 *
 *   - Swift is `Boxes` (the leftover CatalogTable pick).
 *   - MEGAcmd is `TerminalSquare` (`>_` in a square, Ehud 11 Aug).
 *   - Blob cannot reuse S3's bucket (unique-shape fails with "Blob draws the
 *     same glyph as S3"). Azure Blob is organised in containers, so it draws
 *     lucide `Container`.
 *
 * Colours stay per-surface: the table tints its glyphs to match its badge
 * palette, the Quick Connect tabs tint on the active state. Only the shape is
 * shared, because the shape is what carries the meaning.
 */
import * as React from 'react';
import {
    Boxes,
    Braces,
    Cloud,
    Container,
    createLucideIcon,
    Globe,
    KeyRound,
    Server,
    Shield,
    TerminalSquare,
    type LucideIcon,
} from 'lucide-react';

/**
 * A pail seen slightly from above: the rim as an ellipse, tapered sides with a
 * rounded base, and the handle arched over the rim. Same 24-unit grid, 2px
 * stroke and round caps as every lucide icon, so it takes `size`/`className`
 * like the others.
 */
export const Bucket = createLucideIcon('bucket', [
    ['ellipse', { cx: '12', cy: '10', rx: '8', ry: '2', key: 'rim' }],
    ['path', { d: 'M4 10l1.7 9.4C6 21 8.8 22 12 22s6-1 6.3-2.6L20 10', key: 'body' }],
    ['path', { d: 'M4 10C4 1 20 1 20 10', key: 'handle' }],
]);

/** The connection methods that get a glyph. Keys match the catalog badge labels. */
export type ConnectionMethod =
    | 'OAuth'
    | 'API'
    | 'WebDAV'
    | 'S3'
    | 'FTP'
    | 'FTPS'
    | 'SFTP'
    | 'E2E'
    | 'Crypt'
    | 'Swift'
    | 'Blob'
    | 'MEGAcmd';

/** The lucide component for a method, so each caller picks its own size/colour. */
export const CONNECTION_METHOD_GLYPH: Record<ConnectionMethod, LucideIcon> = {
    OAuth: Cloud,
    API: Braces,
    WebDAV: Globe,
    S3: Bucket,
    FTP: Server,
    FTPS: Server,
    SFTP: KeyRound,
    E2E: Shield,
    Crypt: Shield,
    Swift: Boxes,
    Blob: Container,
    MEGAcmd: TerminalSquare,
};

/**
 * Render the glyph for a method. Returns null for an unknown key rather than a
 * placeholder: a method with no glyph should show nothing, not a wrong symbol.
 */
export function methodIcon(
    method: string,
    props: { size?: number; className?: string } = {},
): React.ReactElement | null {
    const Glyph = CONNECTION_METHOD_GLYPH[method as ConnectionMethod];
    return Glyph ? <Glyph {...props} /> : null;
}
