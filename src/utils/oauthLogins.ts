// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Interactive OAuth logins, counted per token owner: the provider and the
 * saved profile the token belongs to, or the provider's token of a connection
 * with no profile. In such a login the user can pick any account, and an
 * OAuth account lives in a token the connection params do not carry, so a
 * queued write bound to an OAuth connection cannot tell whether a login since
 * kept its account: a target taken before a login on its own token is
 * refused (fail closed). A login on another profile's token leaves it alone
 * (Greptile on #1526: one global count refused every OAuth target after any
 * login anywhere).
 */
const logins = new Map<string, number>();

/** The token owner a login and a target are counted under. */
export function oauthLoginKey(protocol: string | undefined, profileId: string | null | undefined): string {
  // The OAuth calls spell some providers with an underscore (`google_drive`,
  // `zoho_workdrive`) that their connections' protocol does not have.
  const owner = (protocol ?? '').replace(/_/g, '');
  return `${owner}:${profileId ?? ''}`;
}

/** The logins counted so far on `key`'s token. */
export function oauthLoginCount(key: string): number {
  return logins.get(key) ?? 0;
}

/** Runs an interactive OAuth login on `key`'s token and counts it, whatever
 *  its outcome: an account may have been switched even by a login that
 *  failed or was cancelled afterwards. */
export async function countedOAuthLogin<T>(key: string, login: () => Promise<T>): Promise<T> {
  try {
    return await login();
  } finally {
    logins.set(key, oauthLoginCount(key) + 1);
  }
}
