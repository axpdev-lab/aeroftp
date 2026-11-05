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
    const local = new Date(Date.UTC(2026, 10, 1, 10, 31, 0));
    expect(label).toContain(String(local.getHours()).padStart(2, '0'));
    expect(label).toContain(String(local.getMinutes()).padStart(2, '0'));
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
