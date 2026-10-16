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
