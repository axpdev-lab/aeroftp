// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { transformWithOxc } from 'vite';
import app from '../App.tsx?raw';
import { assertOverlayActivationCurrent, profileOverlayApplyParams } from './profileOverlayUnlock';
import { normCryptScope, resolveUsernameTemplate } from './cryptScope';

function block(start: string, end: string): string {
  const a = app.indexOf(start);
  const b = app.indexOf(end, a);
  if (a < 0 || b < 0) throw new Error(`Missing production block: ${start}`);
  return app.slice(a, b);
}

const source = [
  block('  const cryptOverlaySentinel = ', '  // #390 smart re-anchor'),
  block('  const smartReanchorOverlay = ', '  // Cheap, side-effect-free probe:'),
  block('  const maybeAutoUnlockProfileOverlay = ', '  // P3.3 (phase 1)'),
].join('\n');

function signal<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}

async function activation(hold?: string) {
  let current = true;
  const reached = signal<void>();
  const released = signal<void>();
  const effects: string[] = [];
  const calls: Array<{ command: string; args?: any }> = [];
  let installedId: string | null = null;
  const kind = hold === 'keyfile' ? 'aerocrypt' : 'rclone-crypt';
  async function stage<T>(name: string, value: T): Promise<T> {
    if (name === hold) { reached.resolve(); await released.promise; }
    return value;
  }
  const context = {
    assertOverlayActivationCurrent, normCryptScope, resolveUsernameTemplate, profileOverlayApplyParams,
    loadSavedServerProfiles: () => stage('profiles', [{
      id: 'old_profile', initialPath: '/', username: 'test',
      aeroCryptOverlay: { enabled: true, kind, remoteScope: '/Crypt' },
      hasStoredAeroCryptPassword: true, hasStoredAeroCryptKeyfilePath: true,
    }]),
    invoke: async (command: string, args?: any) => {
      calls.push({ command, args });
      switch (command) {
        case 'provider_connection_generation': return 42;
        case 'get_credential': return stage(args.account.includes('keyfile') ? 'keyfile' : args.account.includes('_salt_') ? 'salt' : 'password', 'synthetic-factor');
        case 'provider_arm_crypt_capability': return stage('arm', undefined);
        case 'provider_apply_crypt_overlay':
          installedId = args.params.activationId ?? 'legacy';
          return stage('apply', { scope: '/Crypt', warning: 'synthetic notice', markerRestored: false });
        case 'provider_clear_crypt_overlay':
          if (args.expectedGeneration === 42 && args.activationId === installedId) { installedId = null; return true; }
          return false;
        case 'provider_crypt_cwd_in_view': return stage('view', false);
        case 'provider_change_dir': return stage('cd', undefined);
        default: throw new Error(`Unexpected command: ${command}`);
      }
    },
    overlayRefusalRef: { current: null }, overlayLogIdRef: { current: null as string | null },
    setCryptOverlayOwner: () => effects.push('owner'),
    markSessionOverlayKind: () => effects.push('kind'),
    setOverlayDecrypting: () => effects.push('decrypting'),
    setAeroCryptVaultId: () => effects.push('aero-mirror'),
    setRcloneCryptVaultId: () => effects.push('rclone-mirror'),
    bindSessionCryptOverlay: () => effects.push('binding'),
    humanLog: { logRaw: () => { effects.push('log-start'); return 'log'; }, updateEntry: () => effects.push('log-update') },
    notify: { warning: () => effects.push('notice'), error: () => effects.push('error') },
    logger: { debug: () => effects.push('debug') },
    t: (key: string) => key, overlayWarningTitleKey: () => 'notice',
    isCurrent: () => current,
  };
  const { code } = await transformWithOxc(source, 'overlay-ownership.ts');
  const run: Promise<unknown> = new Function(...Object.keys(context), `return (async () => { ${code}\n return maybeAutoUnlockProfileOverlay('old_profile', isCurrent); })();`)(...Object.values(context));
  return { logRef: context.overlayLogIdRef, run, calls, effects, reached, released, leave: () => { current = false; }, installed: () => installedId, replace: () => { installedId = 'newer'; } };
}

describe('overlay activation owns every asynchronous boundary', () => {
  it.each(['profiles', 'password', 'keyfile', 'arm', 'salt', 'apply', 'view', 'cd'])('does not dispatch or publish old state after a switch during %s', async (hold) => {
    const op = await activation(hold);
    await op.reached.promise;
    const beforeEffects = [...op.effects];
    const beforeCalls = op.calls.length;
    op.leave();
    op.released.resolve();
    await expect(op.run).resolves.toBeNull();
    expect(op.effects).toEqual(beforeEffects.includes('log-start') ? [...beforeEffects, 'log-update'] : beforeEffects);
    const cleanup = op.calls.slice(beforeCalls);
    if (['apply', 'view', 'cd'].includes(hold)) {
      expect(op.installed()).toBeNull();
      expect(cleanup).toHaveLength(1);
      expect(cleanup[0].command).toBe('provider_clear_crypt_overlay');
      expect(cleanup[0].args.expectedGeneration).toBe(42);
    } else expect(cleanup).toEqual([]);
  });

  it('does not discard a newer activation on the same connection', async () => {
    const op = await activation('apply');
    await op.reached.promise;
    op.replace();
    op.leave();
    op.released.resolve();
    await expect(op.run).resolves.toBeNull();
    expect(op.installed()).toBe('newer');
  });

  it('carries the captured connection generation through every owned mutation', async () => {
    const op = await activation();
    await expect(op.run).resolves.toMatchObject({ prefix: 'rclone_crypt_provider', generation: 42 });
    expect(op.calls.find(c => c.command === 'provider_arm_crypt_capability')?.args).toEqual({ expectedGeneration: 42 });
    expect(op.calls.find(c => c.command === 'provider_apply_crypt_overlay')?.args.params.expectedGeneration).toBe(42);
    expect(op.calls.find(c => c.command === 'provider_change_dir')?.args).toEqual({ path: '/Crypt', expectedGeneration: 42 });
    expect(op.effects).toContain('binding');
  });
});


describe('overlay opening log ownership', () => {
  it.each(['arm', 'salt', 'apply', 'view', 'cd'])('settles its abandoned running entry after %s', async (hold) => {
    const op = await activation(hold);
    await op.reached.promise;
    expect(op.logRef.current).toBe('log');
    op.leave();
    op.released.resolve();
    await expect(op.run).resolves.toBeNull();
    expect(op.logRef.current).toBeNull();
    expect(op.effects).toContain('log-update');
  });

  it.each(['profiles', 'arm'])('does not settle a newer log when abandoned at %s', async (hold) => {
    const op = await activation(hold);
    await op.reached.promise;
    op.logRef.current = 'newer-log';
    op.leave();
    op.released.resolve();
    await expect(op.run).resolves.toBeNull();
    expect(op.logRef.current).toBe('newer-log');
    expect(op.effects).not.toContain('log-update');
  });
});
