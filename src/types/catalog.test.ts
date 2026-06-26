// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { FILTER_CHIPS } from './catalog';

const chip = (id: string) => {
  const c = FILTER_CHIPS.find((c) => c.id === id);
  if (!c) throw new Error(`missing filter chip: ${id}`);
  return c;
};

describe('My Servers filter chips: AeroShare (peer)', () => {
  it('exposes a peer chip that reuses the aeroShare.feature label (no new i18n key)', () => {
    expect(chip('peer').labelKey).toBe('aeroShare.feature');
  });

  it('peer chip matches only the "peer" protocol', () => {
    const peer = chip('peer');
    expect(peer.matchFn('peer')).toBe(true);
    for (const p of ['ftp', 'ftps', 'sftp', 's3', 'azure', 'webdav', 'filen', 'github', 'immich']) {
      expect(peer.matchFn(p)).toBe(false);
    }
  });

  it('cloud chip does NOT swallow peer profiles (regression: peer is in the cloud exclusion list)', () => {
    // Without the explicit exclusion, the catch-all cloud matchFn would count
    // friend drives as cloud accounts and double-list them.
    expect(chip('cloud').matchFn('peer')).toBe(false);
    // Sanity: a genuine cloud protocol still matches.
    expect(chip('cloud').matchFn('filen')).toBe(true);
  });

  it('no other protocol chip matches the peer protocol', () => {
    // The special quick chips (favorites/encrypted/active) are `matchFn: () => true`
    // placeholders, filtered specially in MyServersPanel (matchFn can't see the
    // favorite/overlay/open-session state), so they are not protocol predicates and
    // are excluded here. The assertion guards the real risk: no PROTOCOL chip
    // double-lists a friend drive.
    const SPECIAL = new Set(['all', 'favorites', 'encrypted', 'active']);
    const matching = FILTER_CHIPS
      .filter((c) => !SPECIAL.has(c.id))
      .filter((c) => c.matchFn('peer'))
      .map((c) => c.id);
    expect(matching).toEqual(['peer']);
  });
});
