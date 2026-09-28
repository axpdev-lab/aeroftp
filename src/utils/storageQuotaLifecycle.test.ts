// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { transformWithOxc } from 'vite';
import app from '../App.tsx?raw';
import { describe, expect, it, vi } from 'vitest';
import { shouldPersistUsedScan } from './usedScanPersist';

// Execute the actual App closures without mounting Monaco/Tauri. Only their
// surrounding component is omitted; no lifecycle implementation is copied here.
// Each slice runs from one `const` declaration to the next one, never to a
// comment, so rewording a comment cannot move a boundary.
function closure(name: string, next: string) {
  const start = app.indexOf(`\n  const ${name} =`);
  const end = app.indexOf(`\n  const ${next} =`, start + 1);
  if (start < 0 || end < 0) throw new Error(`Missing App closure boundary: ${name} .. ${next}`);
  return app.slice(start, end);
}
const { code: executable } = await transformWithOxc(
  closure('fetchStorageQuota', 'scanUsedStorage')
  + closure('scanUsedStorage', 'cancelUsedStorageScan'),
  'quota-lifecycle.ts',
);

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((ok, fail) => { resolve = ok; reject = fail; });
  return { promise, resolve, reject };
}

type Quota = { used: number; total: number; free: number } | null;
type ScanResult = { used: number; file_count: number; dir_count: number;
  truncated: boolean; cancelled: boolean; method: string };
const completeScan: ScanResult = { used: 123, file_count: 1, dir_count: 0,
  truncated: false, cancelled: false, method: 'bfs' };

type Mode = 'empty quota' | 'usable quota' | 'no quota API' | 'InfiniCloud';
function fixture(mode: Mode, options: {
  holdFirstProfileRead?: boolean;
  scan?: () => Promise<ScanResult>;
  listen?: () => Promise<() => void>;
} = {}) {
  const holdFirstProfileRead = options.holdFirstProfileRead ?? true;
  let profilesRequested!: () => void;
  const profilesReady = new Promise<void>(resolve => { profilesRequested = resolve; });
  let release!: (value: unknown[]) => void;
  const profilesPending = new Promise<unknown[]>(resolve => { release = resolve; });
  const profile = { id: 'A', protocol: 'webdav', initialPath: '/A',
    options: { autoScanUsedOnConnect: true } };
  const params = { protocol: 'webdav', savedServerId: 'A',
    providerId: mode === 'InfiniCloud' ? 'infinicloud' : undefined,
    options: { ...profile.options, apiKey: 'fixture', infinicloudNode: 'fixture' } };
  // React closures belong to render A; changing the backend does not magically
  // update their captured session values. Only the refs are shared.
  let backend: string | null = 'A';
  const commands: Array<{ command: string; backend: string | null }> = [];
  const invoke = vi.fn(async (command: string) => {
    commands.push({ command, backend });
    if (command === 'provider_storage_info') {
      return mode === 'usable quota' ? { used: 5, total: 100, free: 95 } : { used: 0, total: 0, free: 0 };
    }
    if (command === 'infinicloud_quota') return { used: 5, total: 100, available: 95 };
    if (command === 'provider_scan_used') return options.scan ? options.scan() : completeScan;
    throw new Error(`Unexpected command: ${command}`);
  });
  const persist = vi.fn(async (..._args: unknown[]) => {});
  // A real state cell, so a functional update sees what the previous one left.
  const quota: { current: Quota } = { current: null };
  const display = vi.fn((next: Quota | ((prev: Quota) => Quota)) => {
    quota.current = typeof next === 'function' ? next(quota.current) : next;
  });
  const profiles = holdFirstProfileRead
    ? vi.fn().mockImplementationOnce(() => { profilesRequested(); return profilesPending; })
      .mockResolvedValue([profile])
    : vi.fn().mockResolvedValue([profile]);
  const version = { current: 0 };
  const connection = { current: 0 };
  const activityLog = { log: vi.fn(() => 'log'), updateEntry: vi.fn() };
  const notify = { info: vi.fn(), success: vi.fn(), error: vi.fn() };
  const context = {
    fetchBucketEncryption: vi.fn(), quotaVersionRef: version, quotaConnectionRef: connection,
    sessions: [{ id: 'session-A', connectionParams: params }], activeSessionId: 'session-A',
    connectionParams: params, invoke, loadSavedServerProfiles: profiles,
    resolveLiveProfile: () => profile, persistQuotaToProfile: persist,
    supportsStorageQuota: () => mode !== 'no quota API',
    effectiveManualCap: () => undefined,
    resolveEffectiveQuota: (used: number, total: number) => ({ used, total }),
    isMegaCmdQuotaProfile: () => false, setStorageQuota: display,
    scanInFlightRef: { current: false }, usedScanStatus: null, storageQuota: null,
    currentRemotePath: '/A', resolveUsernameTemplate: (path: string) => path,
    stripLegacyNextcloudWebdavRoot: (path: string) => path, setUsedScanStatus: vi.fn(),
    notify, activityLog,
    t: (key: string) => key, formatBytes: String, shouldPersistUsedScan,
    usedScanBoundKey: () => null, listen: vi.fn(options.listen ?? (async () => vi.fn())),
    console: { warn: vi.fn() },
  };
  const runtime = new Function(...Object.keys(context),
    executable + '\nreturn { fetchQuota: fetchStorageQuota, scan: scanUsedStorage };')(...Object.values(context)) as {
    fetchQuota: (protocol: string) => Promise<void>;
    scan: () => Promise<void>;
  };
  return { runtime, commands, persist, display, quota, profiles, profilesReady, version, connection,
    activityLog, notify,
    release: () => release([profile]),
    // A switch or a disconnect changes the connection; both also invalidate
    // any in-flight display update, exactly as App does.
    invalidate: (kind: 'switch' | 'disconnect') => {
      backend = kind === 'switch' ? 'B' : null;
      version.current++;
      connection.current++;
    },
  };
}

const settle = () => new Promise<void>(resolve => setTimeout(resolve, 0));
const scanWrites = (persist: { mock: { calls: unknown[][] } }) =>
  persist.mock.calls.filter(([, write]) => (write as { usedSource?: string }).usedSource === 'scan');
const cancelledLabel = (log: { updateEntry: { mock: { calls: unknown[][] } } }) =>
  log.updateEntry.mock.calls.filter(([, update]) => (update as { message?: string }).message === 'transfer.cancelled');

const modes: Mode[] = ['empty quota', 'usable quota', 'no quota API', 'InfiniCloud'];
describe.each(modes)('App quota lifecycle: %s', mode => {
  it.each(['switch', 'disconnect'] as const)('discards the suspended profile continuation after %s', async kind => {
    const f = fixture(mode);
    const pending = f.runtime.fetchQuota('webdav');
    // Let the quota response reach the deliberately suspended profile read.
    await f.profilesReady;
    expect(f.profiles).toHaveBeenCalledTimes(1);
    f.invalidate(kind);
    f.display.mockClear(); // Discard UI updates made while A was still current.
    const invalidatedVersion = f.version.current;
    f.release();
    await pending;
    // Drain the real, fire-and-forget scan closure as well (on the old code).
    await settle();
    expect(f.commands.filter(c => c.command === 'provider_scan_used'),
      `commands=${JSON.stringify(f.commands)}; writes=${JSON.stringify(f.persist.mock.calls)}`).toEqual([]);
    expect(f.persist).not.toHaveBeenCalled();
    expect(f.display).not.toHaveBeenCalled();
    expect(f.version.current).toBe(invalidatedVersion);
  });

  it('keeps the current session refresh and its opted-in scan working', async () => {
    const f = fixture(mode);
    const pending = f.runtime.fetchQuota('webdav');
    f.release();
    await pending;
    await settle();
    const scans = f.commands.filter(c => c.command === 'provider_scan_used');
    expect(scans).toEqual(mode === 'InfiniCloud' ? [] : [{ command: 'provider_scan_used', backend: 'A' }]);
    expect(f.persist).toHaveBeenCalledWith('A', expect.objectContaining({
      used: mode === 'InfiniCloud' ? 5 : 123,
    }));
  });
});

describe('App used-storage scan lifecycle', () => {
  // #960 review M2: the debounced refresh after an upload bumps the display
  // version, and used to make a scan that finished afterwards unsaved and
  // labelled "cancelled".
  it('persists a scan that finishes after an ordinary quota refresh', async () => {
    const scan = deferred<ScanResult>();
    const f = fixture('usable quota', { holdFirstProfileRead: false, scan: () => scan.promise });
    const running = f.runtime.scan();
    await vi.waitFor(() => expect(f.commands.map(c => c.command)).toContain('provider_scan_used'));
    await f.runtime.fetchQuota('webdav');
    scan.resolve(completeScan);
    await running;
    expect(scanWrites(f.persist)).toEqual([['A', expect.objectContaining({ used: 123, fileCount: 1 })]]);
    expect(cancelledLabel(f.activityLog)).toEqual([]);
    expect(f.activityLog.updateEntry).toHaveBeenCalledWith('log',
      expect.objectContaining({ status: 'success', message: 'statusBar.usedScanDone' }));
    // The refresh owns the StatusBar: the display stays what it set.
    expect(f.quota.current).toEqual({ used: 5, total: 100, free: 95 });
  });

  it.each(['switch', 'disconnect'] as const)('discards a scan that finishes after a %s', async kind => {
    const scan = deferred<ScanResult>();
    const f = fixture('usable quota', { holdFirstProfileRead: false, scan: () => scan.promise });
    const running = f.runtime.scan();
    await vi.waitFor(() => expect(f.commands.map(c => c.command)).toContain('provider_scan_used'));
    f.invalidate(kind);
    scan.resolve(completeScan);
    await running;
    expect(scanWrites(f.persist)).toEqual([]);
    expect(cancelledLabel(f.activityLog)).toHaveLength(1);
  });

  // #960 review low 1: the automatic scan started inside fetchStorageQuota
  // captured the quota from its render closure, older than the figure the
  // fetch had just shown, and put that stale value back on cancel or failure.
  it.each(['cancelled', 'failed'] as const)('keeps the quota a refresh just set when its automatic scan is %s', async outcome => {
    const f = fixture('usable quota', {
      holdFirstProfileRead: false,
      scan: async () => {
        if (outcome === 'failed') throw new Error('listing refused');
        return { ...completeScan, used: 0, file_count: 0, truncated: true, cancelled: true, method: 'cancelled' };
      },
    });
    await f.runtime.fetchQuota('webdav');
    await vi.waitFor(() => expect(f.activityLog.updateEntry).toHaveBeenCalled());
    expect(f.quota.current).toEqual({ used: 5, total: 100, free: 95 });
    expect(scanWrites(f.persist)).toEqual([]);
  });

  // #960 review low 6: a connection change while the progress listener was
  // being registered returned early and left the entry "running" for good.
  it('closes its activity entry when the connection changes before the scan request', async () => {
    const listening = deferred<() => void>();
    const f = fixture('usable quota', { holdFirstProfileRead: false, listen: () => listening.promise });
    const running = f.runtime.scan();
    await vi.waitFor(() => expect(f.activityLog.log).toHaveBeenCalled());
    f.invalidate('switch');
    listening.resolve(vi.fn());
    await running;
    expect(f.commands.map(c => c.command)).not.toContain('provider_scan_used');
    expect(f.activityLog.updateEntry).toHaveBeenCalledWith('log',
      expect.objectContaining({ status: 'success', message: 'transfer.cancelled' }));
  });
});
