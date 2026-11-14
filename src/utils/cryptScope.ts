import type { ServerProfile } from '../types';
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
// SPDX-License-Identifier: GPL-3.0-or-later
//
// CWP-20B (navigate-out scope): plaintext-absolute scope predicate shared by
// the path-bar badge and (later) the transfer routing, so the two can never
// diverge. Both `scope` (binding.remoteScope) and `displayPath`
// (currentRemoteDisplayPath = the backend's decode_path of pwd) are
// plaintext-absolute, which is the only sound comparison (current_path is
// ciphertext). FAIL-CLOSED: returns true ("inside / encrypted") unless we are
// CONFIDENTLY outside, so a normalization mismatch can only cause a visible
// decrypt error, never silent plaintext exposure.
// Spec: docs/dev/roadmap/APPENDIX-CRYPTO-WRAPPER-PROFILES/tasks/CWP-20B-navigate-out-scope.md
//
// Lifted out of App.tsx so the scope resolution below can be pinned by a test:
// the badge reads it on every render and the difference between "no anchor" and
// "the anchor is the whole remote" is not observable from the rendered tree.

import { isLoginRelativeScope, loginRelativeForm, normalizeRemotePath } from './overlayScope';

export { isLoginRelativeScope };

// Match the backend anchor exactly, including literal leading/trailing spaces.
export const normCryptScope = (s?: string | null): string => normalizeRemotePath(s || '');

/**
 * The Remote Path a connect lands on, as given: `/docs ` is its own folder.
 * Its slashes are normalized as in `normCryptScope`, but not its spaces: the
 * anchor's trim would land on another folder. Empty or `/` is the on-wire
 * root ('').
 */
export const normLandingPath = (s?: string | null): string => normCryptScope(s);

/**
 * A login-relative path placed under the login folder the session reported
 * (`provider_login_folder`), as the backend places it; any other path, or no
 * login folder, comes back as given.
 */
export const placeUnderLoginFolder = (path: string | null | undefined, loginFolder: string | null | undefined): string | null => {
  if (!path || !loginFolder || !isLoginRelativeScope(path)) return path ?? null;
  const segments = path === '~' ? [] : path.slice(2).split('/');
  // A `..` would leave the login folder: left unplaced, so it anchors nothing.
  if (segments.includes('..')) return path;
  // `.` and empty segments name the same folder, written as the server lists it.
  const rest = segments.filter((segment) => segment !== '' && segment !== '.').join('/');
  const base = loginFolder.replace(/\/+$/, '');
  return rest ? `${base}/${rest}` : base || '/';
};

/**
 * A binding's scope as the app can read it before the session resolves it:
 * a login-relative scope reads as '' (the whole remote), so every path counts
 * as inside until the applied anchor replaces it (fail closed).
 */
export const knownCryptScope = (s?: string | null): string =>
  isLoginRelativeScope(s) ? '' : normCryptScope(s);

/**
 * `path` with every `{username}` replaced by the user name exactly as saved,
 * spaces included, as the connect sends it and the backend's `profile_root`
 * inserts it. The text goes in literally (`$&` is no pattern, and a user
 * name that reads `{username}` is not expanded again). A blank user name
 * leaves the placeholder, never an empty folder name.
 */
export const resolveUsernameTemplate = (
  path: string | null | undefined,
  username: string | null | undefined,
): string | null => {
  if (!path) return null;
  if (!path.includes('{username}')) return path;
  const u = username || '';
  if (!u.trim()) return path;
  return path.split('{username}').join(u);
};

/**
 * The connect always opens on the Remote Path. An empty Overlays Path means
 * that folder is the encrypted one, so the opening is already inside it.
 * A Remote Path that is the account root, or any folder above the island,
 * opens outside it: plaintext listing, no decrypt pass until the user enters
 * the island.
 */
export const connectLandsOutsideOverlay = (
  remotePath?: string | null,
  overlayPath?: string | null,
): boolean => {
  const island = normCryptScope(overlayPath);
  if (!island) return false;
  // An island under the login folder compares only with a landing under it,
  // and a landing under it that the session could not place compares with no
  // absolute island: where it stands is known once connected, so the connect
  // unlocks and the backend anchors it (fail closed).
  if (isLoginRelativeScope(overlayPath) !== isLoginRelativeScope(remotePath)) return false;
  // The folder the connect opens, read as navigation reads it: `/Crypt ` is
  // another folder than an island at `/Crypt`, and opens outside it.
  const landing = normLandingPath(remotePath);
  if (!landing) return true;
  if (landing === island || landing.startsWith(island + '/')) return false;
  return true;
};

export const shouldAutoUnlockOverlayOnConnect = (
  hint: { opensOutside: boolean } | null | undefined,
): boolean => !hint?.opensOutside;

export const isWithinCryptScope = (
  scope: string | undefined | null,
  displayPath: string | null | undefined,
): boolean => {
  const s = normCryptScope(scope);
  if (s === '') return true; // whole-remote scope (== legacy anchored session)
  if (displayPath == null) return true; // unknown path => fail closed
  const p = normLandingPath(displayPath);
  return p === s || p.startsWith(s + '/');
};

/**
 * The anchor the active tab is bound to, whatever the vault's lock state.
 *
 * `cryptOverlay` holds the live vault and is cleared when the overlay locks, so
 * reading the scope from it alone made a locked tab fall back to '' — which
 * `isWithinCryptScope` reads as "the whole remote is the anchor". Every folder
 * then looked in-scope and the path bar kept offering the grey crypt toggle in
 * plaintext territory instead of the 'Overlays Path' jump. `cryptOverlayScope`
 * is the same value kept across the lock, exactly like `cryptOverlayKind`.
 */
export const resolveBoundCryptScope = (
  session: { cryptOverlay?: { remoteScope?: string } | null; cryptOverlayScope?: string | null } | null | undefined,
): string => normCryptScope(session?.cryptOverlay?.remoteScope ?? session?.cryptOverlayScope ?? '');

/**
 * The folder a profile's overlay is bound to: its Overlays Path, or its
 * Remote Path when the binding names none, with the `{username}` template
 * resolved. The one place that fallback lives, so the anchor, the badge and
 * restored transfers read the same folder. On SFTP, SCP, FTP and FTPS a relative folder
 * opens under the login folder, so it comes back as `~/...`: read as `/docs`,
 * the anchor was a folder the session never opened.
 */
export const profileOverlayScope = (
  profile: Pick<ServerProfile, 'aeroCryptOverlay' | 'initialPath' | 'username' | 'protocol'>,
): string => loginRelativeForm(
  resolveUsernameTemplate(profile.aeroCryptOverlay?.remoteScope, profile.username)
    || resolveUsernameTemplate(profile.initialPath, profile.username) || '',
  profile.protocol,
);

/** A restored path outside the encrypted island is ordinary plaintext. Unknown
 * relative paths fail closed; the persisted absolute remote path retains the
 * plaintext anchor even when its encrypted tail is opaque. A scope under the
 * login folder is placed where the live session reports it (`loginFolder`,
 * from `provider_login_folder`): the backend compares the active overlay's
 * absolute anchor, so the unplaced `~/docs` refused every restored retry.
 * Without a login folder it reads as the whole remote (fail closed).
 */
export const restoredTransferCryptScope = (
  profile: Pick<ServerProfile, 'aeroCryptOverlay' | 'initialPath' | 'username' | 'protocol'>,
  loginFolder?: string | null,
): string | undefined => profile.aeroCryptOverlay?.enabled
  ? knownCryptScope(placeUnderLoginFolder(profileOverlayScope(profile), loginFolder))
  : undefined;

export const restoredTransferRequiresOverlay = (
  profile: Pick<ServerProfile, 'aeroCryptOverlay' | 'initialPath' | 'username' | 'protocol'>,
  remotePath: string | null | undefined,
  loginFolder?: string | null,
): boolean => {
  if (!profile.aeroCryptOverlay?.enabled) return false;
  if (!remotePath?.startsWith('/')) return true;
  const scope = restoredTransferCryptScope(profile, loginFolder);
  return isWithinCryptScope(scope, remotePath);
};
