// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it, vi } from 'vitest';
import { transformWithOxc } from 'vite';
import app from '../App.tsx?raw';
import { normCryptScope } from './cryptScope';
import { assertOverlayActivationCurrent } from './profileOverlayUnlock';

// Execute the real re-anchor and activation handlers, including their catches
// and rollback, so changing either root-navigation call changes these results.
const start = app.indexOf('  const smartReanchorOverlay = ');
const end = app.indexOf('  // Cheap, side-effect-free probe:', start);
if (start < 0 || end < 0) throw new Error('Missing production overlay activation handlers');
const source = app.slice(start, end);
const generationError = 'CONNECTION_CHANGED: overlay activation no longer owns this connection';

async function handlers(error: unknown, owned: boolean, staleAfterNavigation = false) {
  let current = true;
  const ownership = owned ? { generation: 42, activationId: 'test-activation', isCurrent: () => current } : undefined;
  const invoke = vi.fn(async (command: string) => {
    if (command === 'provider_crypt_cwd_in_view') return false;
    if (command === 'provider_change_dir') {
      if (staleAfterNavigation) current = false;
      throw error;
    }
    return undefined;
  });
  const bindSessionCryptOverlay = vi.fn();
  const context = {
    invoke, normCryptScope, assertOverlayActivationCurrent,
    positionWrappedProviderAtScope: vi.fn().mockResolvedValue(undefined),
    applyProviderCryptOverlay: vi.fn().mockResolvedValue(''),
    cryptOverlaySentinel: () => 'rclone_crypt_provider',
    setAeroCryptVaultId: vi.fn(), setRcloneCryptVaultId: vi.fn(), bindSessionCryptOverlay,
  };
  const { code } = await transformWithOxc(source, 'overlay-root-navigation.ts');
  const production = new Function(...Object.keys(context), `${code}\nreturn {smartReanchorOverlay, activateProviderCryptOverlay};`)(...Object.values(context));
  return { ...production, invoke, ownership, bindSessionCryptOverlay };
}

for (const branch of ['initial landing', 'whole-remote re-anchor'] as const) {
  for (const owned of [true, false]) {
    describe(`${branch}, ${owned ? 'owned' : 'legacy'} activation`, () => {
      const opts = branch === 'initial landing' ? { initialLandingPath: '' } : undefined;
      it('keeps best-effort root positioning on an ordinary navigation failure', async () => {
        const op = await handlers('Permission denied for root listing', owned);
        await expect(op.smartReanchorOverlay('', opts, op.ownership)).resolves.toBeUndefined();
        expect(op.invoke).toHaveBeenCalledWith('provider_change_dir', { path: '/', ...(owned ? { expectedGeneration: 42 } : {}) });
      });
      it(`${owned ? 'propagates' : 'ignores'} a connection-generation refusal`, async () => {
        const op = await handlers(generationError, owned);
        const result = op.smartReanchorOverlay('', opts, op.ownership);
        if (owned) await expect(result).rejects.toBe(generationError);
        else await expect(result).resolves.toBeUndefined();
      });
    });
  }
  it(`${branch} still rejects obsolete ownership after an ordinary navigation failure`, async () => {
    const op = await handlers('Permission denied for root listing', true, true);
    const opts = branch === 'initial landing' ? { initialLandingPath: '' } : undefined;
    await expect(op.smartReanchorOverlay('', opts, op.ownership)).rejects.toThrow('LISTING_CANCELLED');
  });
}

it('publishes a successfully applied overlay after an ordinary root-navigation failure', async () => {
  const op = await handlers('Permission denied for root listing', true);
  await expect(op.activateProviderCryptOverlay({ sessionId: 's1' }, { kind: 'rclone-crypt' }, undefined, op.ownership)).resolves.toEqual({ vaultId: 'rclone_crypt_provider', scope: '' });
  expect(op.bindSessionCryptOverlay).toHaveBeenCalledWith({ sessionId: 's1' }, 'rclone_crypt_provider', 'rclone-crypt', '');
  expect(op.invoke.mock.calls.some(([command]: [string]) => command === 'provider_clear_crypt_overlay')).toBe(false);
});

it('rolls back only the owned activation after a root-generation refusal', async () => {
  const op = await handlers(generationError, true);
  await expect(op.activateProviderCryptOverlay({ sessionId: 's1' }, { kind: 'rclone-crypt' }, undefined, op.ownership)).rejects.toBe(generationError);
  expect(op.bindSessionCryptOverlay).not.toHaveBeenCalled();
  expect(op.invoke).toHaveBeenCalledWith('provider_clear_crypt_overlay', { expectedGeneration: 42, activationId: 'test-activation' });
});
