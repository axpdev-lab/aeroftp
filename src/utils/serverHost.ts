// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * The host of a server field, without the port it may carry. `host:2222` and
 * `[2001:db8::1]:2222` name the host before the port; a bare IPv6 address
 * (`2001:db8::1`, `::1`) is a host with no port, which cutting at the first
 * `:` would reduce to `2001` or to nothing. Anything else is kept as it is.
 */
export const serverHost = (server: string): string => {
    const bracketed = /^\[([^\]]+)\](?::\d+)?$/.exec(server);
    if (bracketed) return bracketed[1];
    return /^[^:]*:\d+$/.test(server) ? server.slice(0, server.lastIndexOf(':')) : server;
};
