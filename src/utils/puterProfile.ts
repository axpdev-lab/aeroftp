// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

/** Puter's preset id and its default WebDAV root template. */
export const PUTER_PROVIDER_ID = 'puter-webdav';
export const PUTER_HOST_TEMPLATE = 'https://dav.puter.com/{username}';

/**
 * A profile saved in the old token form (username `-token`, account written
 * into the host as `https://dav.puter.com/<account>`) shown in the new form:
 * the account goes back into the username field and the host back to the
 * template. Returns null when the profile is not a legacy Puter one, so the
 * caller keeps it as it is. The account keeps its case (it is a URL path).
 */
export function legacyPuterProfileForEdit(
    providerId: string | undefined,
    host: string | undefined,
    username: string | undefined,
): { server: string; username: string } | null {
    if (providerId !== PUTER_PROVIDER_ID) return null;
    if ((username ?? '').trim() !== '-token') return null;
    let url: URL;
    try {
        url = new URL((host ?? '').trim());
    } catch {
        return null;
    }
    const host_ = url.hostname.toLowerCase();
    if (host_ !== 'dav.puter.com' && !host_.endsWith('.dav.puter.com')) return null;
    let account: string;
    try {
        account = decodeURIComponent(url.pathname.split('/').filter(Boolean)[0] ?? '');
    } catch {
        return null;
    }
    if (!account || account === '{username}' || account === '-token') return null;
    return { server: PUTER_HOST_TEMPLATE, username: account };
}
