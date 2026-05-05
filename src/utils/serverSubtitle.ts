// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { ServerProfile, ProviderType, isOAuthProvider, isFourSharedProvider } from '../types';
import { maskCredential } from './maskCredential';

const HOSTNAME_PROTOCOLS: ReadonlyArray<ProviderType> = ['ftp', 'ftps', 'sftp', 'webdav'];

const OPAQUE_TOKEN_PROTOCOLS: ReadonlyArray<ProviderType> = [
    'jottacloud', 'kdrive', 'drime', 'filelu', 'internxt', 'koofr', 'opendrive', 'yandexdisk',
];

const isHostnameProtocol = (proto: ProviderType): boolean => HOSTNAME_PROTOCOLS.includes(proto);

const looksLikeOpaqueToken = (s: string): boolean => {
    if (!s) return false;
    if (s.length > 40 && !s.includes('@') && !s.includes(' ')) return true;
    if (/^[A-Fa-f0-9]{32,}$/.test(s)) return true;
    if (/^[A-Za-z0-9_-]{36,}$/.test(s) && !s.includes('@')) return true;
    return false;
};

export interface ServerSubtitleOptions {
    credentialsMasked?: boolean;
    /**
     * When `false`, suppress the user identifier on every card: username for
     * hostname protocols, email for OAuth/opaque-token providers, access key
     * (or other identifier stored in `username`) for S3/API providers. Falls
     * back to host when meaningful, otherwise empty.
     */
    showUsername?: boolean;
}

/**
 * Centralized subtitle for a saved server. Replaces the old `${user}@${host}`
 * concatenation that leaked OAuth tokens / opaque API identifiers into the UI.
 *
 * Rules (with `showUsername === true`):
 *  - hostname-based protocols (FTP/FTPS/SFTP/WebDAV custom): `user · host[:port]` or `host[:port]`
 *  - OAuth / OAuth1 / known opaque-token API providers: email when present, else empty
 *  - Other API providers (S3, Mega, Filen, github custom, ...): username, else host
 *
 * `showUsername === false` (driven by the `@` toolbar toggle) hides the
 * identifier in every branch: hostname protocols still show `host[:port]`
 * (the host is the primary identifier, not the user), every other branch
 * collapses to empty. The brand title disambiguates by itself, avoiding the
 * inconsistency where some S3/Cloud cards showed an endpoint and others did
 * not. Eye + `@` are independent.
 */
export const getServerSubtitle = (
    server: ServerProfile,
    opts: ServerSubtitleOptions = {},
): string => {
    const proto = (server.protocol || 'ftp') as ProviderType;
    const masked = opts.credentialsMasked && proto !== 'github';
    // Default to true so callers that don't pass the option preserve previous
    // "show identifier" behavior; the toolbar always passes an explicit value.
    const showUsername = opts.showUsername !== false;
    const port = server.port;
    const host = server.host
        ? (masked ? maskCredential(server.host) : server.host)
        : '';
    const portSuffix = port && port !== 21 && port !== 22 && port !== 80 && port !== 443
        ? `:${port}`
        : '';

    if (isHostnameProtocol(proto)) {
        const hostPort = host ? `${host}${portSuffix}` : '';
        if (showUsername && server.username && !looksLikeOpaqueToken(server.username)) {
            const user = masked ? maskCredential(server.username) : server.username;
            return hostPort ? `${user} · ${hostPort}` : user;
        }
        return showUsername ? hostPort : '';
    }

    // Native API providers known to store opaque identifiers in `username`.
    // These providers (Jotta, kDrive, Drime, FileLu, Internxt, Koofr,
    // OpenDrive, Yandex Disk) authenticate via API key / login token / OAuth
    // client ID rather than an email account. Show the stored value as-is
    // (always with mask applied) so two profiles of the same provider are
    // distinguishable; eye toggles the mask, `@` collapses to empty.
    //
    // NOTE: this branch must precede the OAuth branch because Yandex Disk is
    // both OAuth-based AND stores an opaque Client ID in `username` (no
    // email). Without this ordering, Yandex falls into the OAuth branch and
    // gets filtered out by the `@` requirement.
    if (OPAQUE_TOKEN_PROTOCOLS.includes(proto)) {
        if (showUsername && server.username) {
            return masked ? maskCredential(server.username) : server.username;
        }
        // Yandex Disk stores Client ID/Secret in the vault (not in `username`),
        // leaving the field empty. Fall back to host (e.g. cloud-api.yandex.net)
        // so the card has at least one identifier when `@` is off.
        return showUsername ? (host || '') : '';
    }

    // OAuth / OAuth1: identifier is the account email when stored.
    if (isOAuthProvider(proto) || isFourSharedProvider(proto)) {
        if (showUsername && server.username && !looksLikeOpaqueToken(server.username) && server.username.includes('@')) {
            return masked ? maskCredential(server.username) : server.username;
        }
        return '';
    }

    // Other API providers (S3, github, gitlab, mega, filen, dropbox custom, ...):
    // show username (often access key / email) when allowed, otherwise host.
    // When the user explicitly hid identifiers, also suppress the host fallback
    // so every provider behaves the same instead of "some show endpoint, others
    // don't" depending on whether the preset stored host vs. options.endpoint.
    //
    // For S3 the `username` field IS the access key by design (Tencent, Mega S3,
    // Quotaless and Cloudflare R2 produce 32-56 char keys that the generic
    // opaque-token heuristic would otherwise discard, leaving the card with no
    // identifier at all). Bypass the heuristic for S3 only — masking still
    // hides everything but the prefix.
    const usernameMeaningful = !!server.username && (
        proto === 's3' ? true : !looksLikeOpaqueToken(server.username)
    );
    if (showUsername && usernameMeaningful) {
        return masked ? maskCredential(server.username) : server.username;
    }
    return showUsername ? (host || '') : '';
};
