// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/** The port a URL of each scheme implies, so a URL names only another one. */
const SCHEME_DEFAULT_PORT: Record<string, number> = { ftp: 21, sftp: 22, scp: 22 };

/**
 * The host part of a copied URL. The CLI reads the URL with `url::Url`, which
 * takes the port from it and wants an IPv6 address in brackets, so a port
 * other than the scheme's goes in, and a bare IPv6 address is bracketed. A
 * server that already names its port (`host:2121`, `[::1]:2222`) is kept as it is.
 */
const urlAuthority = (scheme: string, server: string, port?: number): string => {
    if (/^\[.*\]:\d+$/.test(server)) return server;
    const colons = server.split(':').length - 1;
    if (colons === 1 && /:\d+$/.test(server)) return server;
    const host = colons > 1 && !server.startsWith('[') ? `[${server}]` : server;
    return port && port !== SCHEME_DEFAULT_PORT[scheme] ? `${host}:${port}` : host;
};

/**
 * The path of a copied URL: each segment percent-encoded, the separators kept,
 * so a `?`, `#`, `%` or space in a name stays part of it for the CLI, which
 * decodes the path back.
 */
const urlPath = (path: string): string =>
    (path.startsWith('/') ? '' : '/') + path.split('/').map(encodeURIComponent).join('/');

/** The `scheme://user@host[:port]/path` URL that Copy URL puts on the clipboard. */
export const remoteCopyUrl = (scheme: string, username: string, server: string, port: number | undefined, path: string): string =>
    `${scheme}://${username ? `${encodeURIComponent(username)}@` : ''}${urlAuthority(scheme, server, port)}${urlPath(path)}`;
