// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import type { ConnectionParams } from '../types';

/** What a queued write is bound to, taken when it starts: the server, the
 *  account on it and the crypt overlay over it. A remote file opened in the
 *  editor and remote files cut or copied keep one too. */
export interface QueuedTarget {
  params: ConnectionParams;
  /** The saved profile the connection was opened from, `null` for a quick
   *  connect. Two accounts of one provider can share every connection field
   *  (an OAuth token is not in the params): their profiles differ. */
  profileId: string | null;
  /** The session tab and the connection it opened, as `connectionNumberOf`
   *  numbers it: what tells two OAuth accounts apart when neither has a
   *  saved profile, since their token is not in the params. */
  sessionId: string | null;
  connection: number;
  /** The interactive OAuth logins counted when it was taken: a login since
   *  may have picked another account on the same connection. */
  oauthLogins: number;
  /** The crypt overlay over the connection, as its vault, kind and scope
   *  (`null` without one): a vault id is the kind and the owner, so another
   *  vault of the same kind in the same session has the same one. */
  overlay: string | null;
}

/**
 * A queued write refused because the live connection is no longer the server,
 * or the crypt overlay, the batch was queued for. Retrying cannot help until
 * the user switches back, so the batch loops neither retry it nor count it
 * toward the circuit breaker's pause, which would reconnect the other server.
 */
export class QueuedTargetChangedError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'QueuedTargetChangedError';
  }

  /** The reason alone, as the queue row and the toast show it. */
  override toString(): string {
    return this.message;
  }
}
