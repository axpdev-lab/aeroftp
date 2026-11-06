// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
// SPDX-License-Identifier: GPL-3.0-or-later
import { describe, it, expect } from 'vitest';
import { isLoginRelativeScope, knownCryptScope, normCryptScope, normLandingPath, restoredTransferCryptScope, restoredTransferRequiresOverlay, connectLandsOutsideOverlay, isWithinCryptScope, resolveBoundCryptScope, resolveUsernameTemplate, shouldAutoUnlockOverlayOnConnect } from './cryptScope';

describe('normCryptScope', () => {
  it('collapses the empty scope and the root onto "whole remote"', () => {
    expect(normCryptScope(undefined)).toBe('');
    expect(normCryptScope(null)).toBe('');
    expect(normCryptScope('')).toBe('');
    expect(normCryptScope('   ')).toBe('/   ');
    expect(normCryptScope('/')).toBe('');
  });

  it('makes the anchor absolute and strips the trailing slashes', () => {
    expect(normCryptScope('vault')).toBe('/vault');
    expect(normCryptScope('/vault/')).toBe('/vault');
    expect(normCryptScope('//vault//sub//')).toBe('/vault/sub');
  });
});

describe('normLandingPath', () => {
  it('keeps the spaces of the Remote Path and normalizes only its slashes', () => {
    for (const root of [undefined, null, '', '/', '//']) expect(normLandingPath(root)).toBe('');
    expect(normLandingPath('/docs ')).toBe('/docs ');
    expect(normLandingPath(' docs/')).toBe('/ docs');
    expect(normLandingPath('   ')).toBe('/   ');
    expect(normLandingPath('docs//')).toBe('/docs');
  });
});

describe('connectLandsOutsideOverlay', () => {
  it('opens a root Remote Path outside a subfolder island', () => {
    expect(connectLandsOutsideOverlay('/', '/Crypt')).toBe(true);
    expect(connectLandsOutsideOverlay('', '/Crypt')).toBe(true);
    expect(connectLandsOutsideOverlay('   ', '/Crypt-rclone')).toBe(true);
    expect(connectLandsOutsideOverlay('/docs', '/docs/secret')).toBe(true);
  });

  it('opens inside the island when the Remote Path is the encrypted folder', () => {
    expect(connectLandsOutsideOverlay('/Crypt', '/Crypt')).toBe(false);
    expect(connectLandsOutsideOverlay('/Crypt/sub', '/Crypt')).toBe(false);
    expect(connectLandsOutsideOverlay('/Crypt/', '/Crypt')).toBe(false);
  });

  // The landing is the folder navigation opens (normLandingPath), spaces
  // included: read with the anchor's trim, `/Crypt ` was inside `/Crypt` and
  // the overlay unlocked on a folder outside it.
  it('distinguishes whitespace names while recognizing an exactly matching anchor', () => {
    expect(connectLandsOutsideOverlay('/Crypt ', '/Crypt')).toBe(true);
    expect(connectLandsOutsideOverlay(' /Crypt', '/Crypt')).toBe(true);
    expect(connectLandsOutsideOverlay('/Crypt /sub', '/Crypt')).toBe(true);
    expect(connectLandsOutsideOverlay('/Crypt /sub', '/Crypt ')).toBe(false);
  });

  it('treats an empty Overlays Path as the Remote Path itself', () => {
    expect(connectLandsOutsideOverlay('/', '')).toBe(false);
    expect(connectLandsOutsideOverlay('/', '/')).toBe(false);
    expect(connectLandsOutsideOverlay('/Crypt', '')).toBe(false);
    expect(connectLandsOutsideOverlay('/Crypt', null)).toBe(false);
  });
});

describe('resolveUsernameTemplate', () => {
  it('resolves both sides of the landing comparison before classification', () => {
    const remotePath = resolveUsernameTemplate('/{username}/Crypt', 'alice');
    const overlayPath = resolveUsernameTemplate('/{username}/Crypt', 'alice');
    expect(connectLandsOutsideOverlay(remotePath, overlayPath)).toBe(false);
  });

  it('keeps the path unchanged without a username', () => {
    expect(resolveUsernameTemplate('/{username}/Crypt', '')).toBe('/{username}/Crypt');
  });
});

describe('shouldAutoUnlockOverlayOnConnect', () => {
  it('defers an outside landing and unlocks an inside one', () => {
    expect(shouldAutoUnlockOverlayOnConnect({ opensOutside: true })).toBe(false);
    expect(shouldAutoUnlockOverlayOnConnect({ opensOutside: false })).toBe(true);
  });

  it('keeps the no-hint path for locked-overlay discovery', () => {
    expect(shouldAutoUnlockOverlayOnConnect(null)).toBe(true);
    expect(shouldAutoUnlockOverlayOnConnect(undefined)).toBe(true);
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

// Requested 133: the displayed and protected folder must be the one opened.
it.each(['/reports ', '/ reports', '/   ', '/reports /inner '])('preserves literal anchor %j', scope => {
  expect(normCryptScope(scope)).toBe(scope);
  expect(isWithinCryptScope(scope, scope + '/a.txt')).toBe(true);
  expect(connectLandsOutsideOverlay(scope, scope)).toBe(false);
});


it('keeps the plaintext sibling of a literal anchor outside it', () => {
  expect(isWithinCryptScope('/reports ', '/reports')).toBe(false);
  expect(isWithinCryptScope('/reports ', '/reports/sub')).toBe(false);
});

it.each(['/vault//sub', '/vault/./sub', '/vault/else/../sub'])('recognizes an equivalent configured landing %s', path => {
  expect(connectLandsOutsideOverlay(path, '/vault/sub')).toBe(false);
  expect(isWithinCryptScope('/vault/sub', path + '/child')).toBe(true);
});

describe('restored entry overlay requirement', () => {
  const profile = { username: 'alice', initialPath: '/public', aeroCryptOverlay: { enabled: true, kind: 'aerocrypt' as const, remoteScope: '/vault' } };
  it('requires the actual vault only for its own paths, retaining boundary and space semantics', () => {
    for (const path of ['/vault', '/vault/file', '/vault/encrypted-tail']) expect(restoredTransferRequiresOverlay(profile, path)).toBe(true);
    for (const path of ['/public/file', '/vaultish/file', '/vault /file']) expect(restoredTransferRequiresOverlay(profile, path)).toBe(false);
    for (const path of [undefined, '', 'relative']) expect(restoredTransferRequiresOverlay(profile, path)).toBe(true);
    expect(restoredTransferRequiresOverlay({ ...profile, aeroCryptOverlay: { ...profile.aeroCryptOverlay, enabled: false } }, '/vault/file')).toBe(false);
  });
  it('matches the connect fallback anchor, literal whitespace and username template', () => {
    expect(restoredTransferRequiresOverlay({ ...profile, initialPath: '/vault', aeroCryptOverlay: { ...profile.aeroCryptOverlay, remoteScope: '' } }, '/public/file')).toBe(false);
    expect(restoredTransferRequiresOverlay({ ...profile, initialPath: '/', aeroCryptOverlay: { ...profile.aeroCryptOverlay, remoteScope: '' } }, '/any/file')).toBe(true);
    expect(restoredTransferRequiresOverlay({ ...profile, aeroCryptOverlay: { ...profile.aeroCryptOverlay, remoteScope: '/{username}/vault ' } }, '/alice/vault /f')).toBe(true);
    expect(restoredTransferRequiresOverlay({ ...profile, aeroCryptOverlay: { ...profile.aeroCryptOverlay, remoteScope: '/{username}/vault ' } }, '/alice/vault/f')).toBe(false);
  });
});

it('serializes the expected binding without collapsing absent, root or literal whitespace scopes', () => {
  expect(restoredTransferCryptScope({ username: '' })).toBeUndefined();
  expect(restoredTransferCryptScope({ username: '', initialPath: '/', aeroCryptOverlay: { enabled: true, kind: 'aerocrypt' } })).toBe('');
  expect(restoredTransferCryptScope({ username: '', initialPath: '/ fallback ', aeroCryptOverlay: { enabled: true, kind: 'aerocrypt', remoteScope: '' } })).toBe('/ fallback ');
  expect(restoredTransferCryptScope({ username: 'alice', aeroCryptOverlay: { enabled: true, kind: 'rclone-crypt', remoteScope: '/{username}/vault ' } })).toBe('/alice/vault ');
});

// The rclone import binds `nas:docs` at `~/docs`. Only the connected session
// knows that folder, so before the overlay is applied nothing reads it as
// `/~/docs`: every path counts as inside, and the connect unlocks.
describe('a scope under the login folder', () => {
  it('is `~` or `~/...`, never a literal `~docs` or an absolute path', () => {
    for (const scope of ['~', '~/docs', '~/ docs']) expect(isLoginRelativeScope(scope)).toBe(true);
    for (const scope of ['~docs', '/~/docs', '/docs', '', null, undefined]) expect(isLoginRelativeScope(scope)).toBe(false);
  });
  it('reads as the whole remote until the session resolves it', () => {
    expect(knownCryptScope('~/docs')).toBe('');
    expect(knownCryptScope('~')).toBe('');
    expect(knownCryptScope('/docs/')).toBe('/docs');
    expect(isWithinCryptScope(knownCryptScope('~/docs'), '/home/u/elsewhere')).toBe(true);
  });
  it('keeps a restored transfer behind the overlay wherever it lands', () => {
    const profile = { username: 'u', initialPath: '~/docs', aeroCryptOverlay: { enabled: true, kind: 'rclone-crypt' as const, remoteScope: '~/docs' } };
    for (const path of ['/home/u/docs/f', '/home/u/other/f', '/~/docs/f']) expect(restoredTransferRequiresOverlay(profile, path)).toBe(true);
    expect(restoredTransferRequiresOverlay({ ...profile, aeroCryptOverlay: { ...profile.aeroCryptOverlay, remoteScope: '' } }, '/srv/f')).toBe(true);
  });
  it('unlocks on connect unless the landing is a login folder outside it', () => {
    expect(connectLandsOutsideOverlay('/home/u', '~/docs')).toBe(false);
    expect(connectLandsOutsideOverlay('~/docs', '~/docs')).toBe(false);
    expect(connectLandsOutsideOverlay('~/docs/sub', '~/docs')).toBe(false);
    expect(connectLandsOutsideOverlay('~', '~/docs')).toBe(true);
  });
});
