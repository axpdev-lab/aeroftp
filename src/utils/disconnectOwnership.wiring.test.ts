// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
import { expect, it, vi } from 'vitest';
import { transformWithOxc } from 'vite';
import app from '../App.tsx?raw';

async function fixture(busy: boolean, vaultFailure = false) {
  const start = app.indexOf('  const disconnectFromFtp = async');
  const end = app.indexOf('  // Session Management for Multi-Tab', start);
  if (start < 0 || end < 0) throw new Error('Missing production disconnect');
  const invoke = vi.fn(async (name: string) => {
    if (vaultFailure && name === 'aerovault_overlay_lock') throw new Error('vault cleanup failed');
    if (busy && name === 'provider_disconnect') throw new Error('Transfer still in progress; connection or overlay was not changed');
  });
  const ctx = { invoke, quotaVersionRef: { current: 7 }, quotaConnectionRef: { current: 9 },
    useLatestCallback: (fn: unknown) => fn, switchSession: vi.fn(),
    liveConnectionParams: { server: 'fixture' }, sessions: [{ id: 's' }], activeSessionId: 's',
    aeroVaultOverlaySession: { sessionId: 'vault' }, forgetSiteSession: vi.fn(), closeAeroSyncForTornDownSession: vi.fn(),
    overlayReloadedVaultRef: { current: 'original' }, overlayUnlockInFlightRef: { current: true },
    humanLog: { logStart: vi.fn(() => 'log'), logSuccess: vi.fn(), logError: vi.fn() },
    notify: { error: vi.fn() }, t: (key: string) => key, showToastNotifications: true, toast: { info: vi.fn() },
  } as Record<string, any>;
  const closeStart = app.indexOf('  const closeSession = useLatestCallback');
  const closeEnd = app.indexOf('  // Issue #222', closeStart);
  const allStart = app.indexOf('  const closeAllSessions = async');
  const allEnd = app.indexOf('  const handleNewTabFromSavedServer', allStart);
  const source = app.slice(start, end) + app.slice(closeStart, closeEnd) + app.slice(allStart, allEnd);
  for (const name of new Set(source.match(/\bset[A-Z]\w+/g))) ctx[name] = vi.fn();
  const { code } = await transformWithOxc(source, 'disconnect.ts');
  const run = new Function(...Object.keys(ctx), `${code}; return { disconnectFromFtp, closeSession, closeAllSessions };`)(...Object.values(ctx));
  return { ctx, run: run.disconnectFromFtp, closeSession: run.closeSession, closeAllSessions: run.closeAllSessions, invoke };
}

it('preserves the entire UI and vault session if provider teardown refuses busy', async () => {
  const { ctx, run, invoke } = await fixture(true);
  await run();
  expect(invoke.mock.calls.map(([name]) => name)).toEqual(['provider_disconnect']);
  for (const [name, value] of Object.entries(ctx)) if (/^set[A-Z]/.test(name)) expect(value).not.toHaveBeenCalled();
  expect(ctx.forgetSiteSession).not.toHaveBeenCalled();
  expect(ctx.closeAeroSyncForTornDownSession).not.toHaveBeenCalled();
  expect(ctx.overlayReloadedVaultRef.current).toBe('original');
  expect(ctx.overlayUnlockInFlightRef.current).toBe(true);
  expect(ctx.quotaVersionRef.current).toBe(7);
  expect(ctx.humanLog.logSuccess).not.toHaveBeenCalled();
  expect(ctx.toast.info).not.toHaveBeenCalled();
  expect(ctx.notify.error).toHaveBeenCalledTimes(1);
});

it('resets sessions and vault flags only after authoritative successful teardown', async () => {
  const { ctx, run, invoke } = await fixture(false);
  await run('button');
  expect(invoke.mock.calls.map(([name]) => name)).toEqual(['provider_disconnect', 'disconnect_ftp', 'aerovault_overlay_lock']);
  expect(ctx.setSessions).toHaveBeenCalledWith([]);
  expect(ctx.setCryptOverlayOwner).toHaveBeenCalledWith(null);
  expect(ctx.setIsConnected).toHaveBeenCalledWith(false);
  expect(ctx.humanLog.logSuccess).toHaveBeenCalledTimes(1);
  expect(ctx.notify.error).not.toHaveBeenCalled();
});

it.each(['last-tab', 'close-all'] as const)('does not let %s clear tabs or lock vaults after busy refusal', async action => {
  const f = await fixture(true);
  if (action === 'last-tab') await f.closeSession('s'); else await f.closeAllSessions();
  expect(f.invoke.mock.calls.map(([name]) => name)).toEqual(['provider_disconnect']);
  expect(f.ctx.setSessions).not.toHaveBeenCalled();
  expect(f.ctx.setAeroVaultOverlaySession).not.toHaveBeenCalled();
  expect(f.ctx.forgetSiteSession).not.toHaveBeenCalled();
});

it('reports a vault cleanup failure while reflecting the successful provider disconnect', async () => {
  const { ctx, run } = await fixture(false, true);
  await run();
  expect(ctx.notify.error).toHaveBeenCalledWith('common.error', 'Error: vault cleanup failed');
  expect(ctx.setIsConnected).toHaveBeenCalledWith(false);
  expect(ctx.setSessions).toHaveBeenCalledWith([]);
});
