// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * The `provider_discover_targets` request, and the reset key derived from it.
 *
 * The two used to be written separately, and they drifted: the request carried
 * sixteen fields while the reset key hashed three of them (username, password,
 * endpoint). Everything else, region, path style, session token and the whole
 * STS set included, could be changed without invalidating the targets already
 * listed. Editing only the role ARN left the previous role's buckets on screen
 * and selectable, with no race involved at all.
 *
 * So the key is now computed from the request itself. A field added to the
 * request is in the key the moment it is sent, which is the only way these two
 * stay in agreement.
 */

import type { ConnectionParams } from '../../types';
import { discoveryResetKey } from './DiscoverableTargetField';

/** Providers whose targets (buckets, drives) can be listed before connecting. */
export type DiscoveryProtocol = 's3' | 'backblaze' | 'kdrive';

/** The payload `provider_discover_targets` receives, in its snake_case wire shape. */
export function discoveryRequestParams(
    protocol: DiscoveryProtocol,
    params: ConnectionParams,
): Record<string, unknown> {
    const options = params.options || {};
    return {
        protocol,
        providerId: params.providerId,
        server: params.server,
        port: params.port,
        // kDrive discovery authenticates with the API token in the password
        // field; the username is fixed and carries no account identity.
        username: protocol === 'kdrive' ? 'api-token' : params.username,
        password: params.password,
        bucket: options.bucket,
        region: options.region,
        endpoint: options.endpoint,
        path_style: options.pathStyle,
        session_token: options.sessionToken,
        role_arn: options.roleArn,
        role_external_id: options.roleExternalId,
        role_session_name: options.roleSessionName,
        role_duration_seconds: options.roleDurationSeconds,
        role_mfa_serial: options.roleMfaSerial,
        role_mfa_token_code: options.roleMfaTokenCode,
    };
}

/**
 * Fields of the request that must NOT reset the picker.
 *
 * `bucket` is the value of the field the picker edits. Hashing it would clear
 * the list on every keystroke the user types into it, including the moment
 * discovery itself writes the single result back. It is sent to the backend as
 * context, it is not an input that changes what discovery would return.
 */
const NOT_AN_INPUT = new Set(['bucket']);

/**
 * Non-secret change token for a discovery picker: changes whenever any input
 * the request depends on changes, and never contains the credentials.
 */
export function discoveryRequestResetKey(
    protocol: DiscoveryProtocol,
    params: ConnectionParams,
): string {
    const request = discoveryRequestParams(protocol, params);
    const parts = Object.keys(request)
        .filter((field) => !NOT_AN_INPUT.has(field))
        .sort()
        .map((field) => {
            const value = request[field];
            // The field name goes in with the value so that moving a value from
            // one field to another still changes the key.
            return `${field}=${value == null ? '' : String(value)}`;
        });
    return discoveryResetKey(...parts);
}
