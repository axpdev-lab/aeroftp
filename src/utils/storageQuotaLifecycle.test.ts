// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { transformWithOxc } from 'vite';
import app from '../App.tsx?raw';
import { describe, expect, it, vi } from 'vitest';
import { shouldPersistUsedScan } from './usedScanPersist';
import { usedScanRoot } from './protonPaths';

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
  + closure('scanUsedStorage', 'cancelUsedStorageScan')
  + closure('handleQuotaAfterTransfer', 'keepAliveTick'),
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
  /** Queue functional updates until `flush`, as React may until it renders. */
  deferUpdates?: boolean;
  /** The saved profile's protocol and initial folder (default WebDAV at /A). */
  protocol?: string;
  initialPath?: string;
  /** An S3 preset that reports usage on request (Tigris), as `s3PresetReportsUsage` says. */
  reportsUsage?: boolean;
  /** `provider_storage_info` rejects, as it does for a key that cannot read the stats. */
  storageInfoFails?: boolean;
  /** The figure the saved profile already holds from an earlier scan. */
  lastQuota?: { used: number; total: number; usedSource: string };
} = {}) {
  const holdFirstProfileRead = options.holdFirstProfileRead ?? true;
  let profilesRequested!: () => void;
  const profilesReady = new Promise<void>(resolve => { profilesRequested = resolve; });
  let release!: (value: unknown[]) => void;
  const profilesPending = new Promise<unknown[]>(resolve => { release = resolve; });
  const protocol = options.protocol ?? 'webdav';
  const profile = { id: 'A', protocol, initialPath: options.initialPath ?? '/A',
    options: { autoScanUsedOnConnect: true }, lastQuota: options.lastQuota };
  const params = { protocol, savedServerId: 'A',
    providerId: mode === 'InfiniCloud' ? 'infinicloud' : undefined,
    options: { ...profile.options, apiKey: 'fixture', infinicloudNode: 'fixture' } };
  // React closures belong to render A; changing the backend does not magically
  // update their captured session values. Only the refs are shared.
  let backend: string | null = 'A';
  const commands: Array<{ command: string; backend: string | null }> = [];
  const invoke = vi.fn(async (command: string, _args?: unknown) => {
    commands.push({ command, backend });
    if (command === 'provider_storage_info') {
      if (options.storageInfoFails) throw new Error('not supported');
      return mode === 'usable quota' ? { used: 5, total: 100, free: 95 } : { used: 0, total: 0, free: 0 };
    }
    if (command === 'infinicloud_quota') return { used: 5, total: 100, available: 95 };
    if (command === 'provider_scan_used') return options.scan ? options.scan() : completeScan;
    throw new Error(`Unexpected command: ${command}`);
  });
  const persist = vi.fn(async (..._args: unknown[]) => {});
  const mergeProfile = vi.fn(async (..._args: unknown[]) => {});
  // A real state cell, so a functional update sees what the previous one left.
  const quota: { current: Quota } = { current: null };
  const queued: Array<(prev: Quota) => Quota> = [];
  const display = vi.fn((next: Quota | ((prev: Quota) => Quota)) => {
    if (options.deferUpdates) {
      queued.push(typeof next === 'function' ? next : () => next);
      return;
    }
    quota.current = typeof next === 'function' ? next(quota.current) : next;
  });
  const flush = () => { while (queued.length) quota.current = queued.shift()!(quota.current); };
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
    connectionParams: params, liveConnectionParams: params, invoke, loadSavedServerProfiles: profiles,
    resolveLiveProfile: () => profile, persistQuotaToProfile: persist,
    connectionSupportsStorageQuota: () => mode !== 'no quota API',
    s3PresetReportsUsage: () => options.reportsUsage ?? false,
    effectiveManualCap: () => undefined,
    resolveEffectiveQuota: (used: number, total: number) => ({ used, total }),
    isMegaCmdQuotaProfile: () => false, setStorageQuota: display,
    scanInFlightRef: { current: false }, usedScanStatus: null, storageQuota: null,
    currentRemotePath: '/A', resolveUsernameTemplate: (path: string) => path,
    stripLegacyNextcloudWebdavRoot: (path: string) => path, setUsedScanStatus: vi.fn(),
    notify, activityLog,
    t: (key: string) => key, formatBytes: String, shouldPersistUsedScan, usedScanRoot,
    usedScanBoundKey: () => null, listen: vi.fn(options.listen ?? (async () => vi.fn())),
    console: { warn: vi.fn() },
    quotaRefreshTimer: { current: null as number | null }, window: { setTimeout: vi.fn(), clearTimeout: vi.fn() },
    mergeSavedServerProfile: mergeProfile, setServersRefreshKey: vi.fn(),
    // The slice for the after-transfer handler runs up to the next const and so
    // takes in the effect that follows it: it is declared, never run.
    useEffect: vi.fn(), isConnected: false,
  };
  const runtime = new Function(...Object.keys(context),
    executable + '\nreturn { fetchQuota: fetchStorageQuota, scan: scanUsedStorage, afterTransfer: handleQuotaAfterTransfer };')(...Object.values(context)) as {
    fetchQuota: (protocol: string) => Promise<void>;
    scan: () => Promise<void>;
    afterTransfer: (info: { direction: string; bytes: number }) => void;
  };
  return { runtime, invoke, commands, persist, mergeProfile, display, quota, flush, profiles, profilesReady, version, connection,
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

// A preset that reports usage on request (Tigris) can still be refused it: the
// connection then behaves as plain S3 instead of ending on an empty bar.
describe('App usage on request that is not available', () => {
  // The figure read from the API is not a running total: an upload that
  // replaces an object adds nothing, and one read per upload is billed.
  it('does not add the bytes of an upload to the usage a Tigris profile read', async () => {
    const f = fixture('no quota API', { holdFirstProfileRead: false, reportsUsage: true, protocol: 's3' });
    f.quota.current = { used: 50, total: 0, free: 0 };
    f.runtime.afterTransfer({ direction: 'upload', bytes: 10 });
    await settle();
    expect(f.quota.current).toEqual({ used: 50, total: 0, free: 0 });
    expect(f.mergeProfile).not.toHaveBeenCalled();
    expect(f.commands.map(c => c.command)).not.toContain('provider_storage_info');
  });

  it('still adds the bytes of an upload on plain S3', async () => {
    const f = fixture('no quota API', { holdFirstProfileRead: false, protocol: 's3' });
    f.quota.current = { used: 50, total: 0, free: 0 };
    f.runtime.afterTransfer({ direction: 'upload', bytes: 10 });
    await settle();
    expect(f.quota.current).toMatchObject({ used: 60 });
  });

  it('starts the opted-in first scan when the usage call is refused', async () => {
    const f = fixture('no quota API', { holdFirstProfileRead: false, reportsUsage: true, storageInfoFails: true });
    await f.runtime.fetchQuota('s3');
    await vi.waitFor(() => expect(f.commands.map(c => c.command)).toContain('provider_scan_used'));
    expect(f.commands.map(c => c.command)[0]).toBe('provider_storage_info');
  });

  it('shows the cached figure of an earlier scan when the usage call is refused', async () => {
    const f = fixture('no quota API', {
      holdFirstProfileRead: false, reportsUsage: true, storageInfoFails: true,
      lastQuota: { used: 7, total: 0, usedSource: 'scan' },
    });
    await f.runtime.fetchQuota('s3');
    expect(f.quota.current).toMatchObject({ used: 7 });
    expect(f.commands.map(c => c.command)).not.toContain('provider_scan_used');
  });

  it('starts the opted-in scan when the cached figure came from the API and the call is now refused', async () => {
    const f = fixture('no quota API', {
      holdFirstProfileRead: false, reportsUsage: true, storageInfoFails: true,
      lastQuota: { used: 7, total: 0, usedSource: 'api' },
    });
    await f.runtime.fetchQuota('s3');
    expect(f.quota.current).toMatchObject({ used: 7 });
    await vi.waitFor(() => expect(f.commands.map(c => c.command)).toContain('provider_scan_used'));
  });

  it('still clears the bar when a provider that is not such a preset fails', async () => {
    const f = fixture('usable quota', { holdFirstProfileRead: false, storageInfoFails: true });
    f.quota.current = { used: 5, total: 100, free: 95 };
    await f.runtime.fetchQuota('webdav');
    expect(f.quota.current).toBeNull();
    expect(f.commands.map(c => c.command)).not.toContain('provider_scan_used');
  });
});

// #958: a scan keeps the denominator on screen, reports its running figure as
// it goes, and does not reset the quota to zero when it starts.
describe('App used-storage scan display', () => {
  type Progress = (event: { payload: { used: number; file_count: number; scanning: boolean } }) => void;
  const withProgress = () => {
    let emit!: Progress;
    const listen = async (_event: string, handler: Progress) => { emit = handler; return vi.fn(); };
    return { listen: listen as unknown as () => Promise<() => void>, emit: (...a: Parameters<Progress>) => emit(...a) };
  };

  it('keeps the total on screen while the scan grows and after it ends', async () => {
    const scan = deferred<ScanResult>();
    const progress = withProgress();
    const f = fixture('usable quota', { holdFirstProfileRead: false, scan: () => scan.promise, listen: progress.listen });
    f.quota.current = { used: 5, total: 100, free: 95 };
    const running = f.runtime.scan();
    await vi.waitFor(() => expect(f.commands.map(c => c.command)).toContain('provider_scan_used'));
    // Started, nothing reported yet: the figure on screen is not reset.
    expect(f.quota.current).toEqual({ used: 5, total: 100, free: 95 });
    progress.emit({ payload: { used: 40, file_count: 3, scanning: true } });
    expect(f.quota.current).toEqual({ used: 40, total: 100, free: 60, files: 3 });
    progress.emit({ payload: { used: 80, file_count: 7, scanning: true } });
    expect(f.quota.current).toEqual({ used: 80, total: 100, free: 20, files: 7 });
    scan.resolve({ ...completeScan, used: 90, file_count: 9 });
    await running;
    expect(f.quota.current).toEqual({ used: 90, total: 100, free: 10, files: 9 });
  });

  it('ends on the total on screen even when React runs the updates late', async () => {
    const f = fixture('usable quota', { holdFirstProfileRead: false, deferUpdates: true });
    f.quota.current = { used: 5, total: 100, free: 95 };
    await f.runtime.scan();
    // Nothing was applied while the scan ran; React renders afterwards.
    expect(f.quota.current).toEqual({ used: 5, total: 100, free: 95 });
    f.flush();
    expect(f.quota.current).toEqual({ used: 123, total: 100, free: 0, files: 1 });
  });

  it('ends on the total on screen when the scan reports no progress', async () => {
    const f = fixture('usable quota', { holdFirstProfileRead: false });
    f.quota.current = { used: 5, total: 100, free: 95 };
    await f.runtime.scan();
    expect(f.quota.current).toEqual({ used: 123, total: 100, free: 0, files: 1 });
  });

  it('puts back the figure a suspect empty scan replaced', async () => {
    const scan = deferred<ScanResult>();
    const progress = withProgress();
    const f = fixture('usable quota', { holdFirstProfileRead: false, scan: () => scan.promise, listen: progress.listen });
    f.quota.current = { used: 5, total: 100, free: 95 };
    const running = f.runtime.scan();
    await vi.waitFor(() => expect(f.commands.map(c => c.command)).toContain('provider_scan_used'));
    progress.emit({ payload: { used: 0, file_count: 0, scanning: false } });
    expect(f.quota.current).toEqual({ used: 0, total: 100, free: 100, files: 0 });
    // Folders listed, no file counted: a Depth: infinity read as Depth: 1.
    scan.resolve({ ...completeScan, used: 0, file_count: 0, dir_count: 4 });
    await running;
    expect(f.quota.current).toEqual({ used: 5, total: 100, free: 95 });
  });

  it('shows the running figure with no total too', async () => {
    const scan = deferred<ScanResult>();
    const progress = withProgress();
    const f = fixture('no quota API', { holdFirstProfileRead: false, scan: () => scan.promise, listen: progress.listen });
    const running = f.runtime.scan();
    await vi.waitFor(() => expect(f.commands.map(c => c.command)).toContain('provider_scan_used'));
    progress.emit({ payload: { used: 40, file_count: 3, scanning: true } });
    expect(f.quota.current).toEqual({ used: 40, total: 0, free: 0, files: 3 });
    scan.resolve(completeScan);
    await running;
  });
});

describe('App used-storage scan root', () => {
  // A Proton Drive profile on the CLI opens in My Files (Requested 79), so its
  // saved folder is under My Files: the scan it runs by itself on connect still
  // counts every section from `/`, while a scan asked from the status bar starts
  // where the profile does, as for every other provider.
  it.each([
    ['proton', true, '/'],
    ['proton', false, '/my-files/docs'],
    ['webdav', true, '/my-files/docs'],
  ] as const)('a %s profile with a saved folder, automatic %s, scans from %s', async (protocol, automatic, root) => {
    const f = fixture('no quota API', { holdFirstProfileRead: false, protocol, initialPath: '/my-files/docs' });
    await (f.runtime.scan as (options: { automatic: boolean }) => Promise<void>)({ automatic });
    const roots = f.invoke.mock.calls
      .filter(([command]) => command === 'provider_scan_used')
      .map(([, args]) => (args as { path: string }).path);
    expect(roots).toEqual([root]);
  });
});
