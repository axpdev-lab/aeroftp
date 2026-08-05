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
 *   - A native API is `Braces`. It cannot keep `Database` (that is S3) or
 *     `Cloud` (that is OAuth), and `Key`/`KeyRound` reads as SFTP. Curly braces
 *     say "this provider's own JSON API" without borrowing anyone's meaning.
 *   - S3 keeps `Database`, the reading three of the four surfaces already had;
 *     Filen's `Layers` was the outlier.
 *
 * Colours stay per-surface: the table tints its glyphs to match its badge
 * palette, the Quick Connect tabs tint on the active state. Only the shape is
 * shared, because the shape is what carries the meaning.
 */
import * as React from 'react';
import {
    Braces,
    Cloud,
    Database,
    Globe,
    KeyRound,
    Server,
    Shield,
    type LucideIcon,
} from 'lucide-react';

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
    | 'Crypt';

/** The lucide component for a method, so each caller picks its own size/colour. */
export const CONNECTION_METHOD_GLYPH: Record<ConnectionMethod, LucideIcon> = {
    OAuth: Cloud,
    API: Braces,
    WebDAV: Globe,
    S3: Database,
    FTP: Server,
    FTPS: Server,
    SFTP: KeyRound,
    E2E: Shield,
    Crypt: Shield,
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
