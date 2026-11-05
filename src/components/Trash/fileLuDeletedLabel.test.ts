// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import { fileLuDeletedAtLabel, formatDeletedAgo } from './fileLuDeletedLabel';
import { formatDate } from '../../utils/formatters';

describe('formatDeletedAgo / fileLuDeletedAtLabel', () => {
  it('returns null for a missing relative age so the table can show "-"', () => {
    // Pin: formatDeletedAgo(null) used to return '', and `??` does not fall
    // through an empty string, so TrashTable rendered a blank cell (review #519).
    expect(formatDeletedAgo(null)).toBeNull();
    expect(fileLuDeletedAtLabel(null, null)).toBeNull();
  });

  it('prefers the absolute date when present', () => {
    expect(fileLuDeletedAtLabel('2026-07-01', 90)).toBe('2026-07-01');
  });

  it('shows a UTC deletion instant on the local clock, not as raw text', () => {
    // The backend turns FileLu's US Eastern time into `...Z`; printed raw, the
    // row would read "2026-11-01T10:31:00Z" in every timezone.
    const utc = '2026-11-01T10:31:00Z';
    const label = fileLuDeletedAtLabel(utc, 90);
    expect(label).toBe(formatDate(utc));
    expect(label).not.toBe(utc);
  });

  it('puts the deletion instant on the local clock of a fixed timezone', () => {
    // Pinned zones, so a formatter that printed the UTC clock (10:31) fails:
    // 10:31Z is 05:31 in New York (EST) and 16:01 in Kolkata (UTC+5:30). The
    // formatter always prints a 24-hour clock (`hour12: false`). Setting TZ at
    // run time needs a process of its own, hence `pool: 'forks'` in
    // vitest.config.ts; the first check fails plainly if the zone did not move.
    const instant = '2026-11-01T10:31:00Z';
    const previous = process.env.TZ;
    try {
      for (const [zone, clock] of [['America/New_York', '05:31'], ['Asia/Kolkata', '16:01']]) {
        process.env.TZ = zone;
        expect(new Date(instant).getHours(), `${zone} not applied`).toBe(Number(clock.slice(0, 2)));
        const label = fileLuDeletedAtLabel(instant, 90);
        expect(label, zone).toContain(clock);
        expect(label, zone).not.toContain('10:31');
      }
    } finally {
      if (previous === undefined) delete process.env.TZ;
      else process.env.TZ = previous;
    }
  });

  it('falls back to a relative label when only seconds are known', () => {
    expect(fileLuDeletedAtLabel(null, 45)).toBe('45s ago');
    expect(fileLuDeletedAtLabel(null, 120)).toBe('2m ago');
    expect(fileLuDeletedAtLabel(null, 7200)).toBe('2h ago');
    expect(fileLuDeletedAtLabel(null, 172800)).toBe('2d ago');
  });

  it('treats empty absolute date as missing (||, not ??)', () => {
    expect(fileLuDeletedAtLabel('', null)).toBeNull();
    expect(fileLuDeletedAtLabel('', 30)).toBe('30s ago');
  });
});
