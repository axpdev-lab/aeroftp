// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * A count of interactive OAuth logins, in which the user can pick any account.
 * An OAuth account lives in a token the connection params do not carry, so a
 * queued write bound to an OAuth connection cannot tell whether a login since
 * kept its account: a target taken before one is refused (fail closed).
 */
let logins = 0;

/** The logins counted so far. */
export function oauthLoginCount(): number {
  return logins;
}

/** Runs an interactive OAuth login and counts it, whatever its outcome: an
 *  account may have been switched even by a login that failed afterwards. */
export async function countedOAuthLogin<T>(login: () => Promise<T>): Promise<T> {
  try {
    return await login();
  } finally {
    logins += 1;
  }
}
