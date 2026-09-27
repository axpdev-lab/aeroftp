// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { transformWithOxc } from 'vite';
import app from '../App.tsx?raw';
import { describe, expect, it, vi } from 'vitest';
import { shouldPersistUsedScan } from './usedScanPersist';

// Execute the actual App closures without mounting Monaco/Tauri. Only their
// surrounding component is omitted; no lifecycle implementation is copied here.
function closure(name: string, nextMarker: string) {
  const start = app.indexOf(`  const ${name} =`);
  const end = app.indexOf(nextMarker, start);
  if (start < 0 || end < 0) throw new Error(`Missing App closure boundary: ${name}`);
  return app.slice(start, end);
}
const { code: executable } = await transformWithOxc(
  closure('fetchStorageQuota', '  // Item 4b: explicit')
  + closure('scanUsedStorage', '  const cancelUsedStorageScan ='),
  'quota-lifecycle.ts',
);

type Mode = 'empty quota' | 'usable quota' | 'no quota API' | 'InfiniCloud';
function fixture(mode: Mode) {
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
  // update their captured session values. Only the generation ref is shared.
  let backend: string | null = 'A';
  const commands: Array<{ command: string; backend: string | null }> = [];
  const invoke = vi.fn(async (command: string) => {
    commands.push({ command, backend });
    if (command === 'provider_storage_info') {
      return mode === 'usable quota' ? { used: 5, total: 100, free: 95 } : { used: 0, total: 0, free: 0 };
    }
    if (command === 'infinicloud_quota') return { used: 5, total: 100, available: 95 };
    if (command === 'provider_scan_used') return { used: 123, file_count: 1, dir_count: 0,
      truncated: false, cancelled: false, method: 'bfs' };
    throw new Error(`Unexpected command: ${command}`);
  });
  const persist = vi.fn(async () => {});
  const display = vi.fn();
  const profiles = vi.fn().mockImplementationOnce(() => { profilesRequested(); return profilesPending; })
    .mockResolvedValue([profile]);
  const version = { current: 0 };
  const context = {
    fetchBucketEncryption: vi.fn(), quotaVersionRef: version,
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
    notify: { info: vi.fn(), success: vi.fn(), error: vi.fn() },
    activityLog: { log: vi.fn(() => 'log'), updateEntry: vi.fn() },
    t: (key: string) => key, formatBytes: String, shouldPersistUsedScan,
    usedScanBoundKey: () => null, listen: vi.fn(async () => vi.fn()),
    console: { warn: vi.fn() },
  };
  const runtime = new Function(...Object.keys(context),
    executable + '\nreturn { fetchQuota: fetchStorageQuota };')(...Object.values(context)) as {
    fetchQuota: (protocol: string) => Promise<void>;
  };
  return { runtime, commands, persist, display, profiles, profilesReady, version,
    release: () => release([profile]),
    invalidate: (kind: 'switch' | 'disconnect') => { backend = kind === 'switch' ? 'B' : null; version.current++; },
  };
}

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
    await new Promise<void>(resolve => setTimeout(resolve, 0));
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
    await new Promise<void>(resolve => setTimeout(resolve, 0));
    const scans = f.commands.filter(c => c.command === 'provider_scan_used');
    expect(scans).toEqual(mode === 'InfiniCloud' ? [] : [{ command: 'provider_scan_used', backend: 'A' }]);
    expect(f.persist).toHaveBeenCalledWith('A', expect.objectContaining({
      used: mode === 'InfiniCloud' ? 5 : 123,
    }));
  });
});
