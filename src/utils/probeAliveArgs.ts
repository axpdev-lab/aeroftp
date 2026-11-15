// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { connectionProtocol } from '../providers/connectionManifest';
import type { ConnectionParams } from '../types';
import { webdavConnectBasePath } from './profileSecurity';

/** The arguments of `provider_probe_alive` for the tab whose connection is
 *  `params`: the account it expects in the backend's single provider slot.
 *  Protocol and user alone matched another server with the same user, so the
 *  probe also gets the saved profile, the endpoint, and an S3 tab's bucket,
 *  endpoint and region, and finds the slot not alive unless it holds that
 *  profile on that endpoint (or, without one, that host and port). An unset port is sent unset: the backend
 *  resolves the default the connection used. */
export const probeAliveArgs = (params: ConnectionParams | undefined, protocol?: string | null) => {
    const resolvedProtocol = protocol ?? params?.protocol ?? null;
    return {
        protocol: resolvedProtocol,
        username: params?.username || null,
        profileId: params?.savedServerId || null,
        host: params?.server || null,
        port: params?.port || null,
        bucket: params?.options?.bucket || null,
        endpoint: params?.options?.endpoint || null,
        region: params?.options?.region || null,
        basePath: webdavConnectBasePath(resolvedProtocol ?? undefined, params?.options),
    };
};

/** The port a connection without a saved profile reaches: its own, else the
 *  protocol's default. */
const effectivePort = (params: ConnectionParams, protocol: string) =>
    params.port || connectionProtocol(protocol)?.defaultPort || null;

/** Whether an open tab without a saved profile, `tab`, is the target a
 *  connect asks for, `wanted`: same user, server, effective port, S3
 *  bucket, endpoint and region, and WebDAV base path (the connect places the
 *  URL under it, `webdavConnectBasePath`).
 *  Only then may the connect reuse that tab's live session (#128-C). */
export const sameQuickConnectTarget = (tab: ConnectionParams, wanted: ConnectionParams, protocol: string): boolean =>
    (tab.username || '') === (wanted.username || '')
    && (tab.server || '') === (wanted.server || '')
    && effectivePort(tab, protocol) === effectivePort(wanted, protocol)
    && (tab.options?.bucket || '') === (wanted.options?.bucket || '')
    && (tab.options?.endpoint || '') === (wanted.options?.endpoint || '')
    && (tab.options?.region || '') === (wanted.options?.region || '')
    && sameWebdavCollection(tab, wanted, protocol);

/** Whether a WebDAV connect asks for the collection `tab` has open: the base
 *  path the connect places its URL under. Always true for other protocols. */
const sameWebdavCollection = (tab: ConnectionParams, wanted: ConnectionParams, protocol: string): boolean =>
    (webdavConnectBasePath(protocol, tab.options) || '') === (webdavConnectBasePath(protocol, wanted.options) || '');

/** Whether a connect for `wanted` may reactivate the open tab whose
 *  connection is `tab` instead of connecting again (#128-C). Two tabs of
 *  one saved profile are the same account, but a request can carry a saved
 *  profile's id for another WebDAV collection (a Quick Connect that the
 *  profile lookup matched on host and user), so the collection is compared
 *  as well; without saved profiles the whole target is. */
export const reusesOpenTab = (tab: ConnectionParams, wanted: ConnectionParams, protocol: string): boolean => {
    if (tab.protocol !== protocol) return false;
    return (tab.savedServerId && wanted.savedServerId)
        ? tab.savedServerId === wanted.savedServerId && sameWebdavCollection(tab, wanted, protocol)
        : sameQuickConnectTarget(tab, wanted, protocol);
};
