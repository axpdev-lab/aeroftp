// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import type { ConnectionParams } from '../types';

/** The arguments of `provider_probe_alive` for the tab whose connection is
 *  `params`: the account it expects in the backend's single provider slot.
 *  Protocol and user alone matched another server with the same user, so the
 *  probe also gets the saved profile and the endpoint, and finds the slot not
 *  alive unless it holds that profile (or, without one, that host and port). */
export const probeAliveArgs = (params: ConnectionParams | undefined, protocol?: string | null) => ({
    protocol: protocol ?? params?.protocol ?? null,
    username: params?.username || null,
    profileId: params?.savedServerId || null,
    host: params?.server || null,
    port: params?.port || null,
});
