// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

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
    /** Force-show username for hostname protocols even when it's the default. */
    showUsername?: boolean;
}

/**
 * Centralized subtitle for a saved server. Replaces the old `${user}@${host}`
 * concatenation that leaked OAuth tokens / opaque API identifiers into the UI.
 *
 * Rules:
 *  - hostname-based protocols (FTP/FTPS/SFTP/WebDAV custom): `host[:port]`
 *  - OAuth / OAuth1 / known opaque-token API providers: empty (badges convey it)
 *  - Other API providers: hostname only if it is informative (not the bare API base)
 *
 * `showUsername === true` adds `user · host` for hostname protocols when the
 * caller has confirmed the username is meaningful (legacy "show user" toggle).
 */
export const getServerSubtitle = (
    server: ServerProfile,
    opts: ServerSubtitleOptions = {},
): string => {
    const proto = (server.protocol || 'ftp') as ProviderType;
    const masked = opts.credentialsMasked && proto !== 'github';
    const port = server.port;
    const host = server.host
        ? (masked ? maskCredential(server.host) : server.host)
        : '';
    const portSuffix = port && port !== 21 && port !== 22 && port !== 80 && port !== 443
        ? `:${port}`
        : '';

    if (isHostnameProtocol(proto)) {
        const hostPort = host ? `${host}${portSuffix}` : '';
        if (opts.showUsername && server.username && !looksLikeOpaqueToken(server.username)) {
            const user = masked ? maskCredential(server.username) : server.username;
            return hostPort ? `${user} · ${hostPort}` : user;
        }
        return hostPort;
    }

    // OAuth / OAuth1 — never show username, the protocol badge tells the story
    if (isOAuthProvider(proto) || isFourSharedProvider(proto)) {
        if (server.username && !looksLikeOpaqueToken(server.username) && server.username.includes('@')) {
            return masked ? maskCredential(server.username) : server.username;
        }
        return '';
    }

    // Native API providers known to store opaque identifiers in `username`
    if (OPAQUE_TOKEN_PROTOCOLS.includes(proto)) {
        if (server.username && !looksLikeOpaqueToken(server.username) && server.username.includes('@')) {
            return masked ? maskCredential(server.username) : server.username;
        }
        return '';
    }

    // Other API providers (github, gitlab, mega, filen, dropbox custom, ...): show
    // username if meaningful, otherwise the host fallback.
    if (server.username && !looksLikeOpaqueToken(server.username)) {
        return masked ? maskCredential(server.username) : server.username;
    }
    return host || '';
};
