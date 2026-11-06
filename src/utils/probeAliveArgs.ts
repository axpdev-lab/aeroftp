// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { connectionProtocol } from '../providers/connectionManifest';
import type { ConnectionParams } from '../types';

/** The arguments of `provider_probe_alive` for the tab whose connection is
 *  `params`: the account it expects in the backend's single provider slot.
 *  Protocol and user alone matched another server with the same user, so the
 *  probe also gets the saved profile, the endpoint and an S3 bucket, and finds
 *  the slot not alive unless it holds that profile on that endpoint (or,
 *  without one, that host and port). An unset port is sent unset: the backend
 *  resolves the default the connection used. */
export const probeAliveArgs = (params: ConnectionParams | undefined, protocol?: string | null) => ({
    protocol: protocol ?? params?.protocol ?? null,
    username: params?.username || null,
    profileId: params?.savedServerId || null,
    host: params?.server || null,
    port: params?.port || null,
    bucket: params?.options?.bucket || null,
});

/** The port a connection without a saved profile reaches: its own, else the
 *  protocol's default. */
const effectivePort = (params: ConnectionParams, protocol: string) =>
    params.port || connectionProtocol(protocol)?.defaultPort || null;

/** Whether an open tab without a saved profile, `tab`, is the target a
 *  connect asks for, `wanted`: same user, server, effective port and bucket.
 *  Only then may the connect reuse that tab's live session (#128-C). */
export const sameQuickConnectTarget = (tab: ConnectionParams, wanted: ConnectionParams, protocol: string): boolean =>
    (tab.username || '') === (wanted.username || '')
    && (tab.server || '') === (wanted.server || '')
    && effectivePort(tab, protocol) === effectivePort(wanted, protocol)
    && (tab.options?.bucket || '') === (wanted.options?.bucket || '');
