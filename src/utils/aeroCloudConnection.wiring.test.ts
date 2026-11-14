// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { transformWithOxc } from 'vite';
import app from '../App.tsx?raw';
import { expect, it, vi } from 'vitest';
import { scpFallbackConnectParam } from './scpFallback';
import { loadedCertificateCheck, loadedOption, loadedOptionOn, makeSafe } from './profileSecurity';
import { CONNECT_CANCELLED_MARKER, isConnectCancelledError } from './connectCancel';
import { isSshProtocol } from '../types';

const start = app.indexOf('const connectToCloudServer = async');
const end = app.indexOf('// Helper: navigate to remote folder', start);
if (start < 0 || end < 0) throw new Error('Missing AeroCloud connection closure');
const { code } = await transformWithOxc(app.slice(start, end), 'aerocloud-connection.ts');

it('forwards the saved S3 SSE policy through the real AeroCloud connection builder', async () => {
  const runConnect = vi.fn(async () => undefined);
  const context = {
    isProvider: true,
    protocol: 's3',
    cloudServer: {
      id: 'profile-sse',
      providerId: 's3',
      host: 's3.example.test',
      port: 443,
      username: 'access',
      options: {
        bucket: 'private-bucket',
        pathStyle: true,
        sse_mode: 'aws:kms',
        sse_kms_key_id: 'alias/private-key',
      },
    },
    resolveProfileS3Location: () => ({ endpoint: 'https://s3.example.test', signingRegion: 'eu-west-1' }),
    cloudConfig: { remote_folder: '/backups' },
    cloudPassword: 'dummy-password',
    cloudFilenApiKey: null,
    runConnect,
    scpFallbackConnectParam,
    loadedCertificateCheck,
    loadedOption,
    loadedOptionOn,
    isSshProtocol,
    checkSftpHostKey: vi.fn(async () => true),
    CONNECT_CANCELLED_MARKER,
  };
  const connect = new Function(...Object.keys(context), `${code}; return connectToCloudServer;`)(...Object.values(context));
  await connect();
  expect(runConnect).toHaveBeenCalledWith('provider_connect', expect.objectContaining({
    profile_id: 'profile-sse',
    bucket: 'private-bucket',
    sse_mode: 'aws:kms',
    sse_kms_key_id: 'alias/private-key',
  }));
});

it('keeps an "SFTP only" profile on SFTP through the AeroCloud connection builder', async () => {
  const connectWith = async (options: Record<string, unknown>) => {
    const runConnect = vi.fn(async (_command: string, _params: Record<string, unknown>) => undefined);
    const context = {
      isProvider: true,
      protocol: 'sftp',
      cloudServer: { id: 'profile-ssh', host: 'router.lan', port: 22, username: 'root', options },
      resolveProfileS3Location: () => ({ endpoint: null, signingRegion: 'us-east-1' }),
      cloudConfig: { remote_folder: '/' },
      cloudPassword: 'dummy-password',
      cloudFilenApiKey: null,
      runConnect,
      scpFallbackConnectParam,
      loadedCertificateCheck,
      loadedOption,
      loadedOptionOn,
      isSshProtocol,
      checkSftpHostKey: vi.fn(async () => true),
      CONNECT_CANCELLED_MARKER,
    };
    const connect = new Function(...Object.keys(context), `${code}; return connectToCloudServer;`)(...Object.values(context));
    await connect();
    const call = runConnect.mock.calls.find(([command]) => command === 'provider_connect');
    if (!call) throw new Error('provider_connect was not called');
    return call[1];
  };
  expect(await connectWith({ allowScpFallback: false })).toMatchObject({ protocol: 'sftp', allow_scp_fallback: false });
  // Absent means allowed: the field is not sent at all.
  expect(await connectWith({})).not.toHaveProperty('allow_scp_fallback');
});

it('asks for the host key of an SCP or SFTP profile before it connects', async () => {
  const connectWith = async (protocol: string, port: number | undefined, accepted: boolean) => {
    const order: string[] = [];
    const runConnect = vi.fn(async () => { order.push('connect'); });
    const checkSftpHostKey = vi.fn(async () => { order.push('host-key'); return accepted; });
    const context = {
      isProvider: true,
      protocol,
      cloudServer: { id: 'profile-ssh', host: 'nas.lan', port, username: 'root', options: {} },
      resolveProfileS3Location: () => ({ endpoint: null, signingRegion: 'us-east-1' }),
      cloudConfig: { remote_folder: '/' },
      cloudPassword: 'dummy-password',
      cloudFilenApiKey: null,
      runConnect,
      scpFallbackConnectParam,
      loadedCertificateCheck,
      loadedOption,
      loadedOptionOn,
      isSshProtocol,
      checkSftpHostKey,
      CONNECT_CANCELLED_MARKER,
    };
    const connect = new Function(...Object.keys(context), `${code}; return connectToCloudServer;`)(...Object.values(context));
    const error = await connect().then(() => null, (e: unknown) => e);
    return { order, checkSftpHostKey, runConnect, error };
  };

  const scp = await connectWith('scp', 2222, true);
  expect(scp.order).toEqual(['host-key', 'connect']);
  expect(scp.checkSftpHostKey).toHaveBeenCalledWith('nas.lan', 2222);
  // No saved port is SSH's, not HTTPS's.
  expect((await connectWith('scp', undefined, true)).checkSftpHostKey).toHaveBeenCalledWith('nas.lan', 22);
  expect((await connectWith('sftp', 22, true)).order).toEqual(['host-key', 'connect']);

  // A rejected key ends the connect as a cancel, with no connect attempted.
  const rejected = await connectWith('scp', 22, false);
  expect(rejected.runConnect).not.toHaveBeenCalled();
  expect(isConnectCancelledError(rejected.error)).toBe(true);

  // Other protocols have no host key to ask about.
  expect((await connectWith('webdav', 443, true)).order).toEqual(['connect']);
});


it('forwards the effective scheme, S3 consent and preset id', async () => {
  const connectWith = async (protocol: string, providerId: string, options: Record<string, unknown>) => {
    const runConnect = vi.fn(async (_command: string, _params: Record<string, unknown>) => undefined);
    const context = {
      isProvider: true, protocol,
      cloudServer: { id: 'profile', providerId, host: 'dav.example.test', port: 80, username: 'user', options },
      resolveProfileS3Location: () => ({ endpoint: options.endpoint ?? null, signingRegion: 'us-east-1' }),
      cloudConfig: { remote_folder: '/' }, cloudPassword: 'secret', cloudFilenApiKey: null,
      runConnect, scpFallbackConnectParam, loadedCertificateCheck, loadedOption, loadedOptionOn,
      isSshProtocol, checkSftpHostKey: vi.fn(async () => true), CONNECT_CANCELLED_MARKER,
    };
    const connect = new Function(...Object.keys(context), `${code}; return connectToCloudServer;`)(...Object.values(context));
    await connect();
    return runConnect.mock.calls[0][1] as Record<string, unknown>;
  };
  expect(await connectWith('webdav', 'custom-webdav', { tls_mode: 'https' })).toMatchObject({ tls_mode: 'https', provider_id: 'custom-webdav' });
  expect(await connectWith('s3', 'custom-s3', { allowCleartextEndpoint: true })).toMatchObject({ allow_cleartext_endpoint: true, provider_id: 'custom-s3' });
  expect(await connectWith('swift', 'blomp', {})).toMatchObject({ provider_id: 'blomp' });
  const reset = makeSafe({ host: 'dav.example.test', protocol: 'webdav', options: { webdavScheme: 'http' } }).profile;
  expect(reset.options).toMatchObject({ webdavScheme: 'https' });
  expect(await connectWith('webdav', 'custom-webdav', reset.options)).toMatchObject({ tls_mode: 'https' });
});

it('bases Navigation Sync on the folder the session lists, not on the saved form', async () => {
  const { relativeSyncSegments } = await import('./syncNavigationPath');
  const { openAeroCloudFolder } = await import('./liveConnection');
  // The AeroCloud tab opens its folder through the helper, with the
  // placement and the listing the app uses; a provider connect listing ''
  // lists the folder it stands in.
  expect(app).toContain('openAeroCloudFolder(\n          cloudConfig.remote_folder,\n          connectOpenedIt,\n          placeStartFolder,\n          folder => navigateToRemoteFolder(folder).then(listed => { owner.listed(); return listed; }),\n          owner.owns,\n        )');
  const nav = app.slice(app.indexOf('const navigateToRemoteFolder = async'), app.indexOf('// Connection params to set'));
  expect(nav).toContain("invoke<{ files: any[]; current_path: string }>('provider_change_dir', { path: folder })");
  expect(nav).toContain("invoke<{ files: any[]; current_path: string }>('provider_list_files', { path: null })");
  // A `~/docs` saved before the first sync placed it, on a server whose home
  // is a symlink: the session lists `/usr/home/u/docs`.
  const listed: string[] = [];
  const navigate = vi.fn(async (folder: string) => { listed.push(folder); return '/usr/home/u/docs'; });
  const base = await openAeroCloudFolder('~/docs', true, async path => path === '~/docs' ? '/home/u/docs' : path, navigate);
  expect(listed).toEqual(['/home/u/docs']);
  expect(base).toBe('/usr/home/u/docs');
  // At the listed folder the lock reads the root (Up disabled), one level
  // down a sub-folder; the saved form read every listing as outside.
  expect(relativeSyncSegments('/usr/home/u/docs', base)).toEqual([]);
  expect(relativeSyncSegments('/usr/home/u/docs/a', base)).toEqual(['a']);
  expect(relativeSyncSegments('/usr/home/u/docs', '~/docs')).toBeNull();
  // Both AeroCloud entries take that folder for the lock, the badges and the share link.
  const lock = app.slice(app.indexOf('// Lock navigation to cloud folders'), app.indexOf('// === SCENARIO 2'));
  expect(lock).toMatch(/const listedFolder = await openCloudRemoteFolder\(isProvider && !isSameServer, owner\);\s*setSyncBasePaths\(\{ remote: listedFolder,/);
  expect(lock).toContain('setCloudRemoteFolder(listedFolder);');
  const fresh = app.slice(app.indexOf('// === SCENARIO 2'), app.indexOf('// === SCENARIO 2') + 3000);
  expect(fresh).toContain('const listedCloudFolder = await openCloudRemoteFolder(isProvider, owner);');
  expect(fresh).toContain('setCloudRemoteFolder(listedCloudFolder);');
  expect(app).not.toMatch(/navigateToRemoteFolder\(cloudConfig\.remote_folder\)/);
});

it('lists the folder the connect opened when the session cannot place the saved one', async () => {
  const { openAeroCloudFolder } = await import('./liveConnection');
  const cannotPlace = async () => null;
  // A provider connect took `docs` as its starting folder: '' lists where it
  // stands. Listed again as `docs` it was resolved from there, `docs/docs`.
  const provider = vi.fn(async (folder: string) => folder);
  await openAeroCloudFolder('docs', true, cannotPlace, provider);
  expect(provider).toHaveBeenCalledWith('');
  expect(provider).not.toHaveBeenCalledWith('docs');
  // FTP's `connect_ftp` opened no folder: `docs` is entered from the login folder.
  const ftp = vi.fn(async (folder: string) => folder);
  await openAeroCloudFolder('docs', false, cannotPlace, ftp);
  expect(ftp).toHaveBeenCalledWith('docs');
});

it('refuses a saved folder with a parent step or a NUL before listing anything, named as written', async () => {
  const { openAeroCloudFolder } = await import('./liveConnection');
  for (const folder of ['/docs/../private', '~/docs/../a', 'C:\\sync\\..\\x', '/docs\0a']) {
    const place = vi.fn(async (path: string) => path);
    const navigate = vi.fn(async (path: string) => path);
    await expect(openAeroCloudFolder(folder, true, place, navigate)).rejects.toThrow(`'${folder}'`);
    expect(place).not.toHaveBeenCalled();
    expect(navigate).not.toHaveBeenCalled();
  }
});

it('lists a saved folder with a doubled slash or a current step as saved, byte for byte', async () => {
  const { openAeroCloudFolder } = await import('./liveConnection');
  for (const folder of ['/backup//docs', '/srv/./cloud', '~//docs']) {
    // FTP: the connect opened no folder and nothing was placed: the folder as saved.
    const navigate = vi.fn(async (path: string) => path);
    await openAeroCloudFolder(folder, false, async () => null, navigate);
    expect(navigate).toHaveBeenCalledWith(folder);
    // A placement is listed as placed, from the folder as saved.
    const place = vi.fn(async (path: string) => path);
    const placed = vi.fn(async (path: string) => path);
    await openAeroCloudFolder(folder, true, place, placed);
    expect(place).toHaveBeenCalledWith(folder);
    expect(placed).toHaveBeenCalledWith(folder);
  }
});

it('keeps the session on screen only for the pinned profile and a folder that names its place', async () => {
  const { mayReuseForAeroCloud } = await import('./liveConnection');
  // Two profiles with the same target parameters are two accounts: the
  // AeroCloud tab pinned to B does not adopt A's connection.
  expect(mayReuseForAeroCloud('srv_a', 'srv_b', '/home/u/docs')).toBe(false);
  expect(mayReuseForAeroCloud('srv_b', 'srv_b', '/home/u/docs')).toBe(true);
  expect(mayReuseForAeroCloud(null, 'srv_b', '/home/u/docs')).toBe(false);
  expect(mayReuseForAeroCloud(null, undefined, '/home/u/docs')).toBe(true);
  // A relative or empty folder starts where the connect starts, which a
  // session at `/home/u/other` no longer is: it connects again.
  for (const folder of ['docs', '~/docs', '']) {
    expect(mayReuseForAeroCloud('srv_b', 'srv_b', folder), folder).toBe(false);
  }
  // The app decides with it, from the live connection's profile, and a kept
  // session is not told its connect opened the folder.
  const kept = app.slice(app.indexOf('// === SCENARIO 1: ALREADY CONNECTED ==='), app.indexOf('// === SCENARIO 2'));
  expect(kept).toMatch(/const isSameServer = isSameConnection\([\s\S]*?\) && mayReuseForAeroCloud\(/);
  expect(kept).toContain('liveSavedServerId(sessions, activeSessionId, liveConnectionParams),');
  expect(kept).toContain('cloudServer.id,');
  expect(kept).toContain('cloudConfig.remote_folder,');
  expect(kept).toContain('openCloudRemoteFolder(isProvider && !isSameServer, owner)');
});

it('drops a folder open that finishes after another connection took the session', async () => {
  const { openAeroCloudFolder, AEROCLOUD_OPEN_SUPERSEDED } = await import('./liveConnection');
  expect(AEROCLOUD_OPEN_SUPERSEDED).toContain('LISTING_CANCELLED');
  // The placement is awaited while a tab switch takes the panel over: the
  // stale open lists nothing on the new session.
  let owns = true;
  let release!: (value: string) => void;
  const place = vi.fn(() => new Promise<string>(resolve => { release = resolve; }));
  const navigate = vi.fn(async (folder: string) => folder);
  const pending = openAeroCloudFolder('/home/u/docs', true, place, navigate, () => owns);
  owns = false;
  release('/home/u/docs');
  await expect(pending).rejects.toThrow(AEROCLOUD_OPEN_SUPERSEDED);
  expect(navigate).not.toHaveBeenCalled();
  // A takeover during the listing: nothing comes back for the AeroCloud state.
  let ownsListing = true;
  const listing = vi.fn(async (folder: string) => { ownsListing = false; return folder; });
  await expect(openAeroCloudFolder('/home/u/docs', true, async p => p, listing, () => ownsListing)).rejects.toThrow(AEROCLOUD_OPEN_SUPERSEDED);
  // Both entries check after the local listing too, before the cloud folder,
  // the profile and the sync; every takeover moves the owner on.
  const scenario2 = app.indexOf('// === SCENARIO 2');
  for (const [marker, entry] of [
    ['kept session', app.slice(app.indexOf('// === SCENARIO 1: ALREADY CONNECTED ==='), scenario2)],
    ['fresh connect', app.slice(scenario2, scenario2 + 4000)],
  ]) {
    const check = entry.indexOf('if (!owner.owns()) return;');
    expect(check, marker).toBeGreaterThan(entry.indexOf("await invoke('get_local_files'"));
    expect(check, marker).toBeLessThan(entry.indexOf('setCloudRemoteFolder('));
    expect(check, marker).toBeLessThan(entry.indexOf("invoke('trigger_cloud_sync')"));
  }
  // The owner is the remote listing generation: every takeover moves it on
  // first, and the open adopts only the generation its own listing ended on.
  const release_ = app.slice(app.indexOf('const releaseRemoteNavigation = useLatestCallback'));
  expect(release_.slice(0, 120)).toMatch(/useLatestCallback\(\(\) => \{\s*\+\+remoteNavCounter\.current;/);
  const own = app.slice(app.indexOf('const ownSessionFromNow = ()'), app.indexOf('const ownSessionFromNow = ()') + 300);
  expect(own).toContain('let owned = remoteNavCounter.current;');
  expect(own).toContain('owns: () => owned === remoteNavCounter.current,');
  expect(own).toContain('listed: () => { owned = remoteNavCounter.current; },');
});

it('offers the sync badges and the share link only on the AeroCloud connection', async () => {
  const { isAeroCloudConnection, liveSavedServerId, selectAeroCloudProfile } = await import('./liveConnection');
  const form = { server: '', username: '', password: '' };
  const cloud = { id: 'srv_cloud', connectionParams: { ...form, savedServerId: 'srv_cloud' }, savedServerId: 'srv_cloud' };
  const other = { id: 'srv_other_tab', connectionParams: { ...form, savedServerId: 'srv_other' }, savedServerId: 'srv_other' };
  // Another server listing the same path: no badge, no link.
  expect(isAeroCloudConnection(liveSavedServerId([cloud, other], 'srv_other_tab', form), 'srv_cloud')).toBe(false);
  // The AeroCloud session, and the AeroCloud tab (no session, its params on the form).
  expect(isAeroCloudConnection(liveSavedServerId([cloud, other], 'srv_cloud', form), 'srv_cloud')).toBe(true);
  expect(isAeroCloudConnection(liveSavedServerId([], null, { ...form, savedServerId: 'srv_cloud' }), 'srv_cloud')).toBe(true);
  // A session that names no profile does not borrow the form's.
  expect(liveSavedServerId([{ id: 'quick', connectionParams: form }], 'quick', { ...form, savedServerId: 'srv_cloud' })).toBeNull();
  expect(isAeroCloudConnection(null, 'srv_cloud')).toBe(false);
  expect(isAeroCloudConnection('srv_cloud', null)).toBe(false);
  // The tab opens the profile the config is pinned to, as the backend selects it.
  const profiles = [{ id: 'a', name: 'nas' }, { id: 'b', name: 'nas' }];
  expect(selectAeroCloudProfile(profiles, 'b', 'nas')?.id).toBe('b');
  expect(selectAeroCloudProfile(profiles, 'gone', 'nas')).toBeUndefined();
  expect(selectAeroCloudProfile(profiles, null, 'nas')?.id).toBe('a');
  expect(selectAeroCloudProfile(profiles, null, 'b')?.id).toBe('b');
  // Both entry points read that predicate, and the tab records the profile it opened.
  const badge = app.slice(app.indexOf('const getSyncBadge = '), app.indexOf('const lastSyncTime', app.indexOf('const getSyncBadge = ')));
  expect(badge).toContain('if (!isLocal && !onAeroCloudConnection()) return null;');
  expect(app).toMatch(/cloudRemoteFolder && onAeroCloudConnection\(\) && isWithinSyncFolder\(file\.path, cloudRemoteFolder\)/);
  expect(app).toContain('const cloudServer = selectAeroCloudProfile(savedServers, cloudConfig.server_profile_id, cloudConfig.server_profile);');
  expect(app.match(/setCloudProfileId\(cloudServer\.id\);/g)?.length).toBe(2);
});
