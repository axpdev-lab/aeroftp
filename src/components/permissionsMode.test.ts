// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { flagsFromOctal, isCompleteOctal, octalFromFlags, parsePermissions, toggleFlag } from './permissionsMode';

describe('parsePermissions', () => {
  it('reads the forms listings report', () => {
    expect(parsePermissions('-rw-r--r--')).toMatchObject({ octal: '644', known: true });
    expect(parsePermissions('drwxr-x---')).toMatchObject({ octal: '750', known: true });
    expect(parsePermissions('rw-------')).toMatchObject({ octal: '600', known: true });
    expect(parsePermissions('-rw-r--r--+')).toMatchObject({ octal: '644', known: true });
    expect(parsePermissions('-rwsr-xr-t')).toMatchObject({ octal: '755', known: true });
    expect(parsePermissions('640')).toMatchObject({ octal: '640', known: true });
    expect(parsePermissions('0755')).toMatchObject({ octal: '755', known: true });
  });

  it('says so instead of presenting a guess as the file mode', () => {
    for (const value of [undefined, null, '', 'adfrw', 'flcdmpe', '7777', '-rw-r--r']) {
      expect(parsePermissions(value), String(value)).toMatchObject({ octal: '644', known: false });
    }
  });

  it('keeps flags and octal in step', () => {
    const state = parsePermissions('-rwxr-x--x');
    expect(octalFromFlags(state.flags)).toBe(state.octal);
    expect(flagsFromOctal('751')).toEqual(state.flags);
  });
});

describe('isCompleteOctal', () => {
  // The dialog used to send whatever was typed: "6" reached the server as
  // mode 006, taking the owner's own access away.
  it('accepts only three octal digits', () => {
    expect(isCompleteOctal('644')).toBe(true);
    for (const value of ['', '6', '64', '0644', '648', 'rwx']) {
      expect(isCompleteOctal(value), value).toBe(false);
    }
  });
});

describe('toggleFlag', () => {
  it('returns new flags and leaves the given ones untouched', () => {
    const before = flagsFromOctal('644');
    const snapshot = JSON.parse(JSON.stringify(before));
    const after = toggleFlag(before, 'group', 'write');
    expect(octalFromFlags(after)).toBe('664');
    expect(before).toEqual(snapshot);
    expect(after.owner).toBe(before.owner);
  });
});
