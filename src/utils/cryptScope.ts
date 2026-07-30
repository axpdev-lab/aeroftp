// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
// SPDX-License-Identifier: MIT
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

export const normCryptScope = (s?: string | null): string => {
  const t = (s || '').trim();
  if (!t || t === '/') return ''; // empty / root => whole remote is the scope
  return '/' + t.replace(/^\/+|\/+$/g, '');
};

export const isWithinCryptScope = (
  scope: string | undefined | null,
  displayPath: string | null | undefined,
): boolean => {
  const s = normCryptScope(scope);
  if (s === '') return true; // whole-remote scope (== legacy anchored session)
  if (displayPath == null) return true; // unknown path => fail closed
  const p = '/' + String(displayPath).replace(/^\/+|\/+$/g, '');
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
