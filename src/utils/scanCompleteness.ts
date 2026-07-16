// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
//
// CLAUDE-AV-B3-13: shared marker for a compare the backend refused to answer
// because its local scan did not see the whole tree.
//
// `compare_directories` / `provider_compare_directories` / the dual-local twin
// answer with a flat `FileComparison[]`, so there is no field in which to say
// "this scan was partial". The refusal therefore travels in the error string
// behind a stable marker, exactly like CONNECT_CANCELLED and DEST_EXISTS.
//
// Why the caller MUST branch on this instead of treating it as a generic
// failure: the Compare tab's catch arm falls back to a flat top-level
// `compareEntries()` over the already-listed panel entries so the tab still
// shows something actionable. When the local root went away (an unmounted
// drive), that listing is empty, every remote entry lands in the `only-right`
// bucket, and `syncPresets` maps `only-right -> delete-right` under Mirror. The
// fallback would therefore rebuild the exact mass-delete the backend just
// refused, off an even thinner tree. On this marker the UI must fail closed:
// no rows, no plan, a blocking error.
export const SCAN_INCOMPLETE_MARKER = 'SCAN_INCOMPLETE';

export function isScanIncompleteError(error: unknown): boolean {
  if (error == null) return false;
  if (error instanceof Error) return error.message.includes(SCAN_INCOMPLETE_MARKER);
  return String(error).includes(SCAN_INCOMPLETE_MARKER);
}

/**
 * The backend message already explains what was unreadable and why the compare
 * was refused, and the Compare tab surfaces raw backend errors elsewhere too.
 * Strip only the machine marker (and the `Failed to scan ...:` wrapper the
 * commands add) so the toast reads as prose instead of leaking a wire token.
 */
export function describeScanIncompleteError(error: unknown): string {
  const raw = error instanceof Error ? error.message : String(error);
  const at = raw.indexOf(SCAN_INCOMPLETE_MARKER);
  if (at < 0) return raw;
  const tail = raw.slice(at + SCAN_INCOMPLETE_MARKER.length).replace(/^\s*:\s*/, '');
  return tail.length > 0 ? tail : raw;
}
