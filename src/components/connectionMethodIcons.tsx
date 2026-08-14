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
 * The assignment follows the mapping agreed on #347, expressed in the app's
 * existing icon mechanism (lucide components, plus the app's own S3 bucket
 * mark — lucide ships no bucket glyph):
 *
 *   - WebDAV keeps `Globe` (🌐), the reading every surface already had.
 *   - S3 is the bucket mark (`S3BucketLogo`, the 🪣 of the request) — the same
 *     glyph the Add Service table badges already drew. `Database` is freed for
 *     Azure Blob, and Filen's `Layers` outlier is gone.
 *   - FTP/FTPS and SFTP are the folder family (📁): `Folder` for plain FTP,
 *     `FolderLock` for SFTP. FTPS aliases FTP deliberately (FTP over TLS is
 *     still FTP), as Crypt aliases E2E.
 *   - OAuth is `BadgeCheck` — a "this app was authorized" mark. It cannot keep
 *     `Cloud`: that glyph still names the Azure / AeroCloud surfaces inside the
 *     My Servers table, so OAuth needed a shape of its own.
 *   - A native API is `Braces`. It cannot keep `Database` (freed for Blob) or
 *     `Cloud` (the Azure/AeroCloud surface glyph), and `Key`/`KeyRound` reads
 *     as credentials. Curly braces say "this provider's own JSON API" without
 *     borrowing anyone's meaning.
 *
 * Colours stay per-surface: the table tints its glyphs to match its badge
 * palette, the Quick Connect tabs tint on the active state. Only the shape is
 * shared, because the shape is what carries the meaning.
 */
import * as React from 'react';
import {
    BadgeCheck,
    Boxes,
    Braces,
    Database,
    Folder,
    FolderLock,
    Globe,
    Shield,
    TerminalSquare,
} from 'lucide-react';
import { S3BucketLogo } from './ProviderLogos';

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

/** Any glyph component taking the logo/lucide `{size, className}` prop shape. */
export type ConnectionMethodGlyph = React.ComponentType<{ size?: number; className?: string }>;

/** The glyph component for a method, so each caller picks its own size/colour. */
export const CONNECTION_METHOD_GLYPH: Record<ConnectionMethod, ConnectionMethodGlyph> = {
    OAuth: BadgeCheck,
    API: Braces,
    WebDAV: Globe,
    S3: S3BucketLogo,
    FTP: Folder,
    FTPS: Folder,
    SFTP: FolderLock,
    E2E: Shield,
    Crypt: Shield,
    Swift: Boxes,
    Blob: Database,
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
