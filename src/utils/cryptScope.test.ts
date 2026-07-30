// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
// SPDX-License-Identifier: MIT
import { describe, it, expect } from 'vitest';
import { normCryptScope, isWithinCryptScope, resolveBoundCryptScope } from './cryptScope';

describe('normCryptScope', () => {
  it('collapses the empty scope and the root onto "whole remote"', () => {
    expect(normCryptScope(undefined)).toBe('');
    expect(normCryptScope(null)).toBe('');
    expect(normCryptScope('')).toBe('');
    expect(normCryptScope('   ')).toBe('');
    expect(normCryptScope('/')).toBe('');
  });

  it('makes the anchor absolute and strips the trailing slashes', () => {
    expect(normCryptScope('vault')).toBe('/vault');
    expect(normCryptScope('/vault/')).toBe('/vault');
    expect(normCryptScope('//vault//sub//')).toBe('/vault//sub');
  });
});

describe('isWithinCryptScope', () => {
  it('treats a whole-remote scope as always inside (legacy anchored session)', () => {
    expect(isWithinCryptScope('', '/anywhere')).toBe(true);
    expect(isWithinCryptScope('/', '/anywhere')).toBe(true);
  });

  it('fails closed on an unknown path', () => {
    expect(isWithinCryptScope('/vault', null)).toBe(true);
    expect(isWithinCryptScope('/vault', undefined)).toBe(true);
  });

  it('separates the anchor and its descendants from everything else', () => {
    expect(isWithinCryptScope('/vault', '/vault')).toBe(true);
    expect(isWithinCryptScope('/vault', '/vault/sub/deeper')).toBe(true);
    expect(isWithinCryptScope('/vault', '/')).toBe(false);
    expect(isWithinCryptScope('/vault', '/other')).toBe(false);
    // a sibling that merely shares the prefix is outside, not inside
    expect(isWithinCryptScope('/vault', '/vault-backup')).toBe(false);
  });
});

describe('resolveBoundCryptScope', () => {
  it('reads the live vault scope while the overlay is unlocked', () => {
    expect(resolveBoundCryptScope({ cryptOverlay: { remoteScope: '/vault' } })).toBe('/vault');
  });

  // The bug Ehud reported on discussion #347: lock the overlay inside the
  // Overlays Path, walk out to the Remote path, and the grey crypt toggle was
  // still there. lockSessionCryptOverlay clears `cryptOverlay`, so the scope
  // fell back to '' — the whole remote — and every folder read as in-scope.
  it('keeps the anchor after the vault locks, so a plaintext folder reads as outside', () => {
    const lockedTab = { cryptOverlay: null, cryptOverlayScope: '/vault' };
    expect(resolveBoundCryptScope(lockedTab)).toBe('/vault');
    expect(isWithinCryptScope(resolveBoundCryptScope(lockedTab), '/vault/sub')).toBe(true);
    expect(isWithinCryptScope(resolveBoundCryptScope(lockedTab), '/elsewhere')).toBe(false);
  });

  it('still reports the whole remote when the tab never had an anchor', () => {
    expect(resolveBoundCryptScope({ cryptOverlay: null, cryptOverlayScope: null })).toBe('');
    expect(resolveBoundCryptScope(null)).toBe('');
    expect(resolveBoundCryptScope(undefined)).toBe('');
  });

  it('normalizes whatever the binding stored', () => {
    expect(resolveBoundCryptScope({ cryptOverlay: null, cryptOverlayScope: 'vault/' })).toBe('/vault');
    expect(resolveBoundCryptScope({ cryptOverlay: null, cryptOverlayScope: '/' })).toBe('');
  });
});
