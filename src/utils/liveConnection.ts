// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import type { ConnectionParams } from '../types';

/** The part of a session tab this resolution reads. */
export interface LiveConnectionSession {
    id: string;
    connectionParams?: ConnectionParams;
}

/**
 * The parameters of the connection on screen.
 *
 * App.tsx keeps two kinds of connection params. The global `connectionParams`
 * state is the connection FORM: a connect from a saved profile resets it to
 * empty for the next "Add New Server", the servers screen can be opened over a
 * live session (command palette, Edit) and fill it with another profile, and
 * it is never cleared on a tab switch back to the same session. Each session
 * tab keeps its own `connectionParams`, copied at connect, and those are the
 * facts of the live connection.
 *
 * So when a session is active, its params are the answer, as a whole: no
 * field falls back to the form, because a field the session lacks (an OAuth
 * server, a preset-less provider id) is a fact about that connection, while
 * the form's value for it belongs to some other connection. Only without an
 * active session (before the first connect, during a connect from the
 * servers screen, the AeroCloud tab, which has no session of its own) is the
 * form what the app is connected with or about to connect with.
 */
export function resolveLiveConnectionParams(
    sessions: ReadonlyArray<LiveConnectionSession>,
    activeSessionId: string | null | undefined,
    formParams: ConnectionParams,
): ConnectionParams {
    if (activeSessionId) {
        const active = sessions.find(session => session.id === activeSessionId);
        if (active?.connectionParams) return active.connectionParams;
    }
    return formParams;
}

/**
 * The saved profile the live connection was opened from, or null when it was
 * opened without one (a quick connect). A sync history records this id
 * (Requested 79), so nothing else may stand in for it: a host name or a
 * session id would name a profile that does not exist, which a later sync of
 * the same profile reads as another profile's history and a Proton Drive mode
 * switch never moves.
 */
export function savedProfileIdOf(
    live: ConnectionParams,
    session?: { savedServerId?: string | null } | null,
): string | null {
    return live.savedServerId || session?.savedServerId || null;
}

/**
 * Option keys that do not choose what a connection reaches: secrets and
 * one-shot codes (the account is named elsewhere), values the app records
 * after a connect (expiry, observed auth version, discovered node), and
 * preferences for uploads or display. Every other option is compared, so a
 * key added later counts as part of the target until it is listed here: the
 * cost of a wrong "different" is one reconnect, of a wrong "same" a sync on
 * another bucket.
 */
const OPTIONS_OUTSIDE_THE_TARGET: ReadonlySet<string> = new Set([
    'sessionToken', 'roleMfaTokenCode', 'roleMfaSerial', 'two_factor_code', 'key_passphrase',
    'clientSecret', 'accessKey', 'sasToken', 'apiKey', 'filen_api_key', 'totp_secret',
    'session_expires_at', 'githubTokenExpiresAt', 'filen_auth_version',
    'infinicloudNode', 'infinicloudCapacityGb', 'infinicloudIntroduceCode',
    'save_session', 'logout_on_disconnect', 'timeout', 'manualTotalBytes', 'autoScanUsedOnConnect',
    'storage_class', 'sse_mode', 'sse_kms_key_id', 'opendriveDefaultPrivacy',
    'roleSessionName', 'roleDurationSeconds', 'peerDriveName',
]);

const isAbsent = (value: unknown) => value === undefined || value === null || value === '';

/** `server` without a `:port` suffix that repeats `port`, so `host:2222` with port 2222 reads as `host`. */
function hostOf(params: ConnectionParams): string {
    const server = params.server || '';
    const suffix = params.port ? `:${params.port}` : null;
    return suffix && server.endsWith(suffix) ? server.slice(0, -suffix.length) : server;
}

/**
 * Whether the live connection reaches what `target` describes: the same
 * protocol, host, port and account, and the same value for every option that
 * chooses the storage (bucket, region, endpoint, container, branch, ...).
 * Absent, null and empty values are the same absence. Which saved profile
 * opened either side does not matter: two profiles can name one target, and
 * one profile edited after its connect names another.
 */
export function isSameConnection(live: ConnectionParams, target: ConnectionParams): boolean {
    if ((live.protocol || '') !== (target.protocol || '')) return false;
    if (hostOf(live) !== hostOf(target)) return false;
    if ((live.port ?? null) !== (target.port ?? null)) return false;
    if ((live.username || '') !== (target.username || '')) return false;
    const liveOptions = (live.options ?? {}) as Record<string, unknown>;
    const targetOptions = (target.options ?? {}) as Record<string, unknown>;
    const keys = new Set([...Object.keys(liveOptions), ...Object.keys(targetOptions)]);
    for (const key of keys) {
        if (OPTIONS_OUTSIDE_THE_TARGET.has(key)) continue;
        const a = liveOptions[key];
        const b = targetOptions[key];
        if (isAbsent(a) && isAbsent(b)) continue;
        if (JSON.stringify(a) !== JSON.stringify(b)) return false;
    }
    return true;
}
