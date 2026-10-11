// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
// SPDX-License-Identifier: GPL-3.0-or-later
import { describe, it, expect } from 'vitest';
import { cryptUnlockTargetKey, overlayAnchor } from './cryptUnlockTarget';

describe('overlayAnchor', () => {
  it('spells the whole remote as "/", never as the empty "current folder"', () => {
    expect(overlayAnchor(undefined)).toBe('/');
    expect(overlayAnchor(null)).toBe('/');
    expect(overlayAnchor('')).toBe('/');
    expect(overlayAnchor('  ')).toBe('/');
    expect(overlayAnchor('/')).toBe('/');
  });

  it('keeps a subfolder absolute and without trailing slashes', () => {
    expect(overlayAnchor('/home/user/Vault')).toBe('/home/user/Vault');
    expect(overlayAnchor('home/user/Vault/')).toBe('/home/user/Vault');
  });
});

describe('cryptUnlockTargetKey', () => {
  it('is the same for the same folder and profile, so an open dialog keeps its state', () => {
    expect(cryptUnlockTargetKey({ scope: '/Vault' })).toBe(cryptUnlockTargetKey({ scope: '/Vault' }));
    expect(cryptUnlockTargetKey({ scope: '/Vault', savedServerId: null })).toBe(cryptUnlockTargetKey({ scope: '/Vault' }));
  });

  it('changes with the folder or the profile, so a new target mounts a new dialog', () => {
    expect(cryptUnlockTargetKey({ scope: '/Vault' })).not.toBe(cryptUnlockTargetKey({ scope: '/Other' }));
    expect(cryptUnlockTargetKey({ scope: '/Vault' })).not.toBe(cryptUnlockTargetKey({ scope: '/Vault', savedServerId: 'p1' }));
    expect(cryptUnlockTargetKey({ scope: '/Vault', savedServerId: 'p1' })).not.toBe(cryptUnlockTargetKey({ scope: '/Vault', savedServerId: 'p2' }));
  });

  it('cannot be forged by a folder name that contains a separator', () => {
    expect(cryptUnlockTargetKey({ scope: '/a|b' }))
      .not.toBe(cryptUnlockTargetKey({ scope: '/a', savedServerId: 'b|' }));
  });
});
