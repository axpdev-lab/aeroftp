// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/** What a connection needs to say which route it takes. */
export type ConnectionRoute = {
    protocol?: string | null;
    providerId?: string | null;
    options?: { mega_mode?: string | null } | null;
};

/**
 * The "via ..." part of "Connected to X via ...". A connection that goes
 * through a program on this machine names that program, the way the
 * connection form's status banner does: "via PROTON" said nothing about the
 * Proton Drive CLI doing the work, and MEGAcmd or Filen Desktop read as
 * plain WebDAV or S3. Everything else keeps the protocol in capitals.
 */
export function connectionViaLabel(route: ConnectionRoute | null | undefined): string {
    const protocol = (route?.protocol || 'ftp').toLowerCase();
    const providerId = route?.providerId || '';
    if (protocol === 'proton') return 'Proton Drive CLI';
    if (protocol === 'mega' && route?.options?.mega_mode === 'megacmd') return 'MEGAcmd';
    if (providerId === 'megacmd' || providerId === 'megacmd-webdav') return 'MEGAcmd (WebDAV)';
    if (providerId === 'filen-desktop-webdav') return 'Filen Desktop (WebDAV)';
    if (providerId === 'filen-desktop-s3') return 'Filen Desktop (S3)';
    return protocol.toUpperCase();
}
