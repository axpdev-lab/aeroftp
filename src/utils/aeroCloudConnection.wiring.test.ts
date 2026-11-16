// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { transformWithOxc } from 'vite';
import app from '../App.tsx?raw';
import { describe, expect, it, vi } from 'vitest';
import { scpFallbackConnectParam } from './scpFallback';
import { connectTlsMode, loadedCertificateCheck, loadedOption, loadedOptionOn, makeSafe, webdavConnectBasePath } from './profileSecurity';
import { CONNECT_CANCELLED_MARKER, isConnectCancelledError } from './connectCancel';
import { isSshProtocol } from '../types';
import { AEROCLOUD_OPEN_SUPERSEDED } from './liveConnection';
import locationCases from '../shared/path-location-cases.json';
import { LOCAL, REMOTE, fixture } from '../components/appTransfers.testkit';

const start = app.indexOf('const connectToCloudServer = async');
const end = app.indexOf('// Helper: navigate to remote folder', start);
if (start < 0 || end < 0) throw new Error('Missing AeroCloud connection closure');
const { code } = await transformWithOxc(app.slice(start, end), 'aerocloud-connection.ts');

it('forwards the saved S3 SSE policy through the real AeroCloud connection builder', async () => {
  const runConnect = vi.fn(async () => undefined);
  const context = {
      confirmBackendConnection: () => undefined, remoteConnectPhaseVersionRef: { current: 0 }, phaseVersion: 0, invoke: async () => 0,
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
    connectTlsMode,
    webdavConnectBasePath,
    loadedCertificateCheck,
    loadedOption,
    loadedOptionOn,
    isSshProtocol,
    checkSftpHostKey: vi.fn(async () => true),
    CONNECT_CANCELLED_MARKER,
    AEROCLOUD_OPEN_SUPERSEDED,
    ownsPhase: () => true, supersedeOlderConnects: () => undefined,
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
        confirmBackendConnection: () => undefined, remoteConnectPhaseVersionRef: { current: 0 }, phaseVersion: 0, invoke: async () => 0,
      isProvider: true,
      protocol: 'sftp',
      cloudServer: { id: 'profile-ssh', host: 'router.lan', port: 22, username: 'root', options },
      resolveProfileS3Location: () => ({ endpoint: null, signingRegion: 'us-east-1' }),
      cloudConfig: { remote_folder: '/' },
      cloudPassword: 'dummy-password',
      cloudFilenApiKey: null,
      runConnect,
      scpFallbackConnectParam,
      connectTlsMode,
      webdavConnectBasePath,
      loadedCertificateCheck,
      loadedOption,
      loadedOptionOn,
      isSshProtocol,
      checkSftpHostKey: vi.fn(async () => true),
      CONNECT_CANCELLED_MARKER,
      AEROCLOUD_OPEN_SUPERSEDED,
      ownsPhase: () => true, supersedeOlderConnects: () => undefined,
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
        confirmBackendConnection: () => undefined, remoteConnectPhaseVersionRef: { current: 0 }, phaseVersion: 0, invoke: async () => 0,
      isProvider: true,
      protocol,
      cloudServer: { id: 'profile-ssh', host: 'nas.lan', port, username: 'root', options: {} },
      resolveProfileS3Location: () => ({ endpoint: null, signingRegion: 'us-east-1' }),
      cloudConfig: { remote_folder: '/' },
      cloudPassword: 'dummy-password',
      cloudFilenApiKey: null,
      runConnect,
      scpFallbackConnectParam,
      connectTlsMode,
      webdavConnectBasePath,
      loadedCertificateCheck,
      loadedOption,
      loadedOptionOn,
      isSshProtocol,
      checkSftpHostKey,
      CONNECT_CANCELLED_MARKER,
      AEROCLOUD_OPEN_SUPERSEDED,
      ownsPhase: () => true, supersedeOlderConnects: () => undefined,
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
        confirmBackendConnection: () => undefined, remoteConnectPhaseVersionRef: { current: 0 }, phaseVersion: 0, invoke: async () => 0,
      isProvider: true, protocol,
      cloudServer: { id: 'profile', providerId, host: 'dav.example.test', port: 80, username: 'user', options },
      resolveProfileS3Location: () => ({ endpoint: options.endpoint ?? null, signingRegion: 'us-east-1' }),
      cloudConfig: { remote_folder: '/' }, cloudPassword: 'secret', cloudFilenApiKey: null,
      runConnect, scpFallbackConnectParam, connectTlsMode, webdavConnectBasePath, loadedCertificateCheck, loadedOption, loadedOptionOn,
      isSshProtocol, checkSftpHostKey: vi.fn(async () => true), CONNECT_CANCELLED_MARKER,
      AEROCLOUD_OPEN_SUPERSEDED, ownsPhase: () => true, supersedeOlderConnects: () => undefined,
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
  expect(await connectWith('webdav', 'webdav', { webdavScheme: 'https', basePath: '/dav/alice/' })).toMatchObject({
    protocol: 'webdav',
    tls_mode: 'https',
    base_path: '/dav/alice/',
  });
});

it('bases Navigation Sync on the folder the session lists, not on the saved form', async () => {
  const { relativeSyncSegments } = await import('./syncNavigationPath');
  const { openAeroCloudFolder } = await import('./liveConnection');
  // The AeroCloud tab opens its folder through the helper, with the
  // placement and the listing the app uses; a provider connect listing ''
  // lists the folder it stands in.
  expect(app).toContain('openAeroCloudFolder(\n          cloudConfig.remote_folder,\n          connectOpenedIt,\n          placeStartFolder,\n          folder => navigateToRemoteFolder(folder).then(listed => { owner.listed(listedGeneration); return listed; }),\n          owner.owns,\n        )');
  const nav = app.slice(app.indexOf('const navigateToRemoteFolder = async'), app.indexOf('// Connection params to set'));
  expect(nav).toContain("invoke<FileListResponse & { display_current_path?: string }>('provider_change_dir', { path: folder })");
  expect(nav).toContain("invoke<FileListResponse & { display_current_path?: string }>('provider_list_files', { path: null })");
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
  expect(lock).toMatch(/const listedFolder = await openCloudRemoteFolder\(isProvider && !reusesLiveServer, owner\);\s*if \(!owner\.owns\(\)\) return;\s*setSyncBasePaths\(\{ remote: listedFolder,/);
  expect(lock).toContain('adoptOpenedCloudFolder({ remoteFolder: listedFolder, savedFolder: cloudConfig.remote_folder, localFolder: cloudConfig.local_folder, profileId: cloudServer.id });');
  const fresh = app.slice(app.indexOf('// === SCENARIO 2'), app.indexOf('// === SCENARIO 2') + 3000);
  expect(fresh).toContain('const listedCloudFolder = await openCloudRemoteFolder(isProvider, owner);');
  expect(fresh).toContain('adoptOpenedCloudFolder({ remoteFolder: listedCloudFolder, savedFolder: cloudConfig.remote_folder, localFolder: cloudConfig.local_folder, profileId: cloudServer.id });');
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

it('refuses a saved folder with a parent step or a NUL before listing anything, without naming it', async () => {
  const { openAeroCloudFolder } = await import('./liveConnection');
  const withSecrets = locationCases.cases.filter(c => c.secret && c.savedProblem);
  expect(withSecrets.length).toBeGreaterThanOrEqual(2);
  const folders = ['/docs/../private', '~/docs/../a', 'C:\\sync\\..\\x', '/docs\0a', '/a/../b?sig=example-signature', ...withSecrets.map(c => c.path)];
  for (const folder of folders) {
    const place = vi.fn(async (path: string) => path);
    const navigate = vi.fn(async (path: string) => path);
    const refused = await openAeroCloudFolder(folder, true, place, navigate).then(() => null, (e: Error) => e.message);
    expect(refused, folder).toMatch(/^The AeroCloud remote folder contains (a '\.\.' step|a NUL character): /u);
    expect(refused, folder).not.toContain(folder);
    expect(refused, folder).not.toContain('example-signature');
    for (const { secret } of withSecrets) expect(refused, folder).not.toContain(secret);
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
  expect(kept).toContain('let reusesLiveServer = isSameServer;');
  expect(kept).toContain('openCloudRemoteFolder(isProvider && !reusesLiveServer, owner)');
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
    const check = entry.indexOf('if (!owner.owns()) return;', entry.indexOf("await invoke('get_local_files'"));
    expect(check, marker).toBeGreaterThan(entry.indexOf("await invoke('get_local_files'"));
    expect(check, marker).toBeLessThan(entry.indexOf('adoptOpenedCloudFolder('));
    expect(check, marker).toBeLessThan(entry.indexOf("invoke('trigger_cloud_sync')"));
  }
  // The owner is the remote listing generation: every takeover moves it on
  // first, and the open adopts only the generation its own listing ended on.
  const release_ = app.slice(app.indexOf('const releaseRemoteNavigation = useLatestCallback'));
  expect(release_.slice(0, 120)).toMatch(/useLatestCallback\(\(\) => \{\s*\+\+remoteNavCounter\.current;/);
  const own = app.slice(app.indexOf('const ownSessionFromNow = ()'), app.indexOf('const ownSessionFromNow = ()') + 300);
  expect(own).toContain('let owned = remoteNavCounter.current;');
  expect(own).toContain('owns: () => ownsPhase() && owned === remoteNavCounter.current,');
  expect(own).toContain('listed: (generation: number) => { owned = generation; },');
  // The generation adopted is the one the listing took, checked again when it
  // resumes, and the kept session checks before it sets the Navigation Sync base.
  const nav = app.slice(app.indexOf('const navigateToRemoteFolder = async'), app.indexOf('const openCloudRemoteFolder = '));
  expect(nav.match(/const generation = remoteNavCounter\.current;\s*triedGeneration = generation;\s*const response = await listing;\s*if \(!ownsPhase\(\) \|\| generation !== remoteNavCounter\.current\) throw new Error\(AEROCLOUD_OPEN_SUPERSEDED\);/gu)).toHaveLength(2);
  expect(nav.match(/listedGeneration = generation;/gu)).toHaveLength(2);
  expect(app).toContain('.then(listed => { owner.listed(listedGeneration); return listed; })');
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
  expect(app.match(/adoptOpenedCloudFolder\(\{ remoteFolder: \w+, savedFolder: cloudConfig\.remote_folder, localFolder: cloudConfig\.local_folder, profileId: cloudServer\.id \}\);/g)?.length).toBe(2);
});

// The AeroCloud tab's open as App.tsx has it, run against a stand-in app:
// every name the handler reads that is not given here is a spy.
const cloudTabStart = app.indexOf('const handleCloudTabClick = async');
const cloudTabEnd = app.indexOf('const changeRemoteDirectory = async', cloudTabStart);
const applyStart = app.indexOf('const applyRemoteFileList = (');
const applyEnd = app.indexOf('\n  };\n', applyStart) + 5;
const applyNavigationStart = app.indexOf('const applyRemoteNavigation = (');
const applyNavigationEnd = app.indexOf('\n  };\n', applyNavigationStart) + 5;
// The takeovers outside a connect phase, as App.tsx has them: the release,
// the park and the takeover itself, the "+" new tab and the device open.
const takeoverStart = app.indexOf('  const releaseRemoteNavigation = useLatestCallback(');
const takeoverEnd = app.indexOf('  /** Shared open path', takeoverStart);
const deviceStart = app.indexOf('  const openMtpSessionFromLiveDevice = useCallback(');
const deviceEnd = app.indexOf('  /** PLACES portable row', deviceStart);
const newTabStart = app.indexOf('  const handleNewTabFromSavedServer = () => {');
const newTabEnd = app.indexOf('\n  };', newTabStart) + 5;
if ([cloudTabStart, cloudTabEnd, applyStart, applyNavigationStart, takeoverStart, takeoverEnd, deviceStart, deviceEnd, newTabStart].some(at => at < 0)) throw new Error('Missing AeroCloud tab handler');
const { code: cloudTabCode } = await transformWithOxc([
  app.slice(applyStart, applyEnd), app.slice(applyNavigationStart, applyNavigationEnd), app.slice(cloudTabStart, cloudTabEnd), app.slice(takeoverStart, takeoverEnd),
  app.slice(deviceStart, deviceEnd), app.slice(newTabStart, newTabEnd),
].join('\n'), 'aerocloud-tab.ts');

async function cloudTabApp(overrides: Record<string, unknown> = {}) {
  const { openAeroCloudFolder, selectAeroCloudProfile, AEROCLOUD_OPEN_SUPERSEDED } = await import('./liveConnection');
  // App.tsx's own marker test.
  const isListingCancelled = (error: unknown) => String(error).includes('LISTING_CANCELLED');
  const spies = new Map<string, ReturnType<typeof vi.fn>>();
  const given: Record<string, unknown> = {
    invoke: vi.fn(async () => null),
    loadSavedServerProfiles: vi.fn(async () => [{ id: 'srv_a', name: 'nas', host: 'dav.example.test', port: 443, username: 'u', protocol: 'webdav', options: {} }]),
    selectAeroCloudProfile, openAeroCloudFolder, AEROCLOUD_OPEN_SUPERSEDED, isListingCancelled,
    CONNECT_CANCELLED_MARKER, isConnectCancelledError, isSshProtocol, scpFallbackConnectParam,
    loadedCertificateCheck, loadedOption, loadedOptionOn,
    usesProviderApi: () => true,
    connectionViaLabel: () => 'WebDAV',
    t: (key: string) => key,
    notify: { error: vi.fn(), success: vi.fn(), info: vi.fn() },
    humanLog: { logRaw: vi.fn(), logStart: vi.fn(() => 'log'), logSuccess: vi.fn() },
    toast: { info: vi.fn() },
    logger: { debug: vi.fn(), error: vi.fn(), warn: vi.fn() },
    console: { warn: vi.fn(), error: vi.fn() },
    withRemoteListSpinner: (fn: () => Promise<unknown>) => fn(),
    listingReason: (folder: string) => folder,
    placeStartFolder: async (folder: string) => folder,
    normalizeProviderConnectionParams: (params: unknown) => params,
    isSameConnection: () => true,
    mayReuseForAeroCloud: () => true,
    liveSavedServerId: () => 'srv_a',
    remoteNavCounter: { current: 5 },
    remoteNavInFlightRef: { current: 0 },
    quotaVersionRef: { current: 0 },
    quotaConnectionRef: { current: 0 },
    mtpGvfsMountPathRef: { current: null },
    // A connect under its own tracked token: here, the invoke it wraps.
    phaseCancellable: (run: (token: string) => unknown) => run('device-token'),
    useLatestCallback: (fn: unknown) => fn,
    useCallback: (fn: unknown) => fn,
    ConnectScope: class { assert() {} step<T>(fn: () => T) { return fn(); } },
    remoteConnectPhaseVersionRef: { current: 0 },
    remoteConnectPhaseRef: { current: false },
    isConnected: true,
    liveConnectionParams: {},
    sessions: [], activeSessionId: 'tab-a',
    remoteFiles: [], localFiles: [], currentRemotePath: '/cloud/sub', currentLocalPath: '/local/sub',
    isSyncNavigation: false, syncBasePaths: null, showHiddenFiles: false, showToastNotifications: false,
    cloudServerName: 'nas',
    ...overrides,
  };
  const scope = new Proxy(given, {
    has: (target, key) => typeof key === 'string' && (key in target || !(key in globalThis)),
    get: (target, key) => {
      if (typeof key !== 'string') return undefined;
      if (key in target) return target[key];
      if (!spies.has(key)) spies.set(key, vi.fn());
      return spies.get(key);
    },
  });
  const make = new Function('scope', `with (scope) { ${cloudTabCode}; return { handleCloudTabClick, takeOverActiveSession, handleNewTabFromSavedServer, openMtpSessionFromLiveDevice }; }`);
  const app_ = make(scope) as {
    handleCloudTabClick: () => Promise<void>;
    takeOverActiveSession: (keepDeviceTab?: boolean) => void;
    handleNewTabFromSavedServer: () => void;
    openMtpSessionFromLiveDevice: (device: unknown) => Promise<void>;
  };
  const spy = (name: string) => scope[name as keyof typeof scope] as unknown as ReturnType<typeof vi.fn>;
  return { ...app_, given, spy };
}

const CLOUD_CONFIG = { enabled: true, local_folder: '/local', remote_folder: '/cloud', server_profile: 'nas', server_profile_id: 'srv_a', protocol_type: 'webdav' };

it.each(['get_cloud_config', 'loadSavedServerProfiles', 'get_credential'])('opens nothing for a profile switched away from while %s was awaited', async (awaited) => {
  // The tab was clicked on A; a switch to B (a new phase, a new listing
  // generation) completes while A's reply is still deferred.
  const refs = { remoteConnectPhaseVersionRef: { current: 0 }, remoteNavCounter: { current: 5 } };
  const switchToB = () => { refs.remoteConnectPhaseVersionRef.current += 1; refs.remoteNavCounter.current += 1; };
  const invoke = vi.fn(async (command: string) => {
    if (command === awaited) switchToB();
    if (command === 'get_cloud_config') return CLOUD_CONFIG;
    if (command === 'provider_change_dir') return { files: [], current_path: '/cloud' };
    if (command === 'get_local_files') return [];
    return '';
  });
  const profiles = [{ id: 'srv_a', name: 'nas', host: 'dav.example.test', port: 443, username: 'u', protocol: 'webdav', options: {} }];
  const loadSavedServerProfiles = vi.fn(async () => { if (awaited === 'loadSavedServerProfiles') switchToB(); return profiles; });
  const { handleCloudTabClick, given, spy } = await cloudTabApp({ invoke, loadSavedServerProfiles, ...refs });
  await handleCloudTabClick();
  expect(invoke.mock.calls.map(([command]) => command)).toContain(awaited === 'loadSavedServerProfiles' ? 'get_cloud_config' : awaited);
  const commands = invoke.mock.calls.map(([command]) => command);
  expect(commands).not.toContain('provider_change_dir');
  expect(commands).not.toContain('provider_disconnect');
  expect(commands).not.toContain('trigger_cloud_sync');
  for (const name of ['setActiveSessionId', 'setSessions', 'setConnectionParams', 'setSyncBasePaths', 'setShowCloudPanel', 'adoptOpenedCloudFolder', 'runConnect']) {
    expect(spy(name), name).not.toHaveBeenCalled();
  }
  expect((given.notify as { error: ReturnType<typeof vi.fn> }).error).not.toHaveBeenCalled();
});

it('never lets an AeroCloud open own the session from a generation another phase moved to', async () => {
  const own = app.slice(app.indexOf('const ownsPhase = () =>'), app.indexOf(';', app.indexOf('const ownsPhase = () =>')) + 1);
  const owner = app.slice(app.indexOf('const ownSessionFromNow = ()'), app.indexOf('\n      };\n', app.indexOf('const ownSessionFromNow = ()')) + 9);
  const { code: ownerCode } = await transformWithOxc(`${own}\n${owner}`, 'aerocloud-owner.ts');
  const refs = { phaseVersion: 1, remoteConnectPhaseVersionRef: { current: 1 }, remoteNavCounter: { current: 5 }, connectionAtEntry: 3, quotaConnectionRef: { current: 3 } };
  const ownSessionFromNow = new Function(...Object.keys(refs), `${ownerCode}; return ownSessionFromNow;`)(...Object.values(refs)) as () => { owns: () => boolean; listed: (generation: number) => void };
  const before = ownSessionFromNow();
  expect(before.owns()).toBe(true);
  // A switch takes the phase and the generation.
  refs.remoteConnectPhaseVersionRef.current = 2;
  refs.remoteNavCounter.current = 6;
  const after = ownSessionFromNow();
  expect(before.owns()).toBe(false);
  expect(after.owns()).toBe(false);
  after.listed(6);
  expect(after.owns()).toBe(false);
  // In its own phase, an owner whose listing took generation 7 while a newer
  // one took 8 does not own the panel: it adopts 7, not the current 8.
  refs.phaseVersion = 2;
  const own_ = new Function(...Object.keys(refs), `${ownerCode}; return ownSessionFromNow;`)(...Object.values(refs)) as () => { owns: () => boolean; listed: (generation: number) => void };
  const stale = own_();
  refs.remoteNavCounter.current = 8;
  stale.listed(7);
  expect(stale.owns()).toBe(false);
  stale.listed(8);
  expect(stale.owns()).toBe(true);
});

it.each([
  ['fails', new Error('connection refused')],
  ['is cancelled', new Error(CONNECT_CANCELLED_MARKER)],
])('leaves the tab it ended cached, and the workspace disconnected, when the AeroCloud connect %s', async (_how, failure) => {
  // Connected to another server on tab-a: the open parks the tab, ends its
  // session, and the connect to the AeroCloud server then does not open one.
  const runConnect = vi.fn(async () => { throw failure; });
  const invoke = vi.fn(async (command: string) => command === 'get_cloud_config' ? CLOUD_CONFIG : '');
  const { handleCloudTabClick, spy } = await cloudTabApp({ invoke, runConnect, isSameConnection: () => false, sessions: [{ id: 'tab-a', status: 'connected' }] });
  await handleCloudTabClick();
  // The teardown names the session it saw (its generation).
  expect(invoke).toHaveBeenCalledWith('provider_disconnect', expect.objectContaining({ expectedGeneration: expect.anything() }));
  const tabs = spy('setSessions').mock.calls.reduce(
    (list: { id: string; status: string }[], [update]) => typeof update === 'function' ? update(list) : update,
    [{ id: 'tab-a', status: 'connected' }],
  );
  expect(tabs.find(tab => tab.id === 'tab-a')?.status).toBe('cached');
  expect(spy('setActiveSessionId')).toHaveBeenLastCalledWith(null);
  expect(spy('setIsConnected')).toHaveBeenLastCalledWith(false);
  expect(spy('setIsConnected')).not.toHaveBeenCalledWith(true);
  expect(spy('setShowConnectionScreen')).toHaveBeenLastCalledWith(true);
  expect(invoke.mock.calls.map(([command]) => command)).not.toContain('trigger_cloud_sync');
});

it.each([
  ['replaces the server on screen', { isConnected: true, isSameConnection: () => false, sessions: [{ id: 'tab-a', status: 'connected' }] }],
  ['connects with none on screen', { isConnected: false, activeSessionId: null }],
] as const)('leaves no row of another server actionable when an AeroCloud open that %s cannot list', async (_how, start) => {
  const rowOfA = { name: 'old.txt', path: '/srv-a/old.txt', size: 1, is_dir: false, modified: null, permissions: null };
  const invoke = vi.fn(async (command: string) => {
    if (command === 'get_cloud_config') return CLOUD_CONFIG;
    if (command === 'provider_change_dir') throw new Error('550 listing refused');
    if (command === 'get_local_files') return [];
    return '';
  });
  const runConnect = vi.fn(async () => undefined);
  const { handleCloudTabClick, spy } = await cloudTabApp({ invoke, runConnect, remoteFiles: [rowOfA], remoteSearchResults: [rowOfA], ...start });
  await handleCloudTabClick();
  expect(invoke).toHaveBeenCalledWith('provider_change_dir', { path: '/cloud' });
  const order = (name: string) => spy(name).mock.invocationCallOrder;
  const listedAt = invoke.mock.invocationCallOrder[invoke.mock.calls.findIndex(([command]) => command === 'provider_change_dir')];
  // Cleared before the folder lists, and nothing put back after.
  expect(spy('setRemoteFiles').mock.calls.every(([files]) => Array.isArray(files) && files.length === 0)).toBe(true);
  expect(Math.min(...order('setRemoteFiles'))).toBeLessThan(listedAt);
  expect(Math.min(...order('setRemoteSearchResults'))).toBeLessThan(listedAt);
  expect(spy('setRemoteSearchResults')).toHaveBeenLastCalledWith(null);
  expect(spy('setSelectedRemoteFiles')).toHaveBeenLastCalledWith(new Set());
  // The workspace is disconnected: no remote action, no drop target.
  expect(spy('setIsConnected')).toHaveBeenLastCalledWith(false);
  expect(spy('setShowConnectionScreen')).toHaveBeenLastCalledWith(true);
  expect(invoke.mock.calls.map(([command]) => command)).not.toContain('trigger_cloud_sync');
});

it('shows the connection again once the AeroCloud folder of a replacing open has listed', async () => {
  const listing = { files: [{ name: 'a.txt', path: '/cloud/a.txt', size: 1, is_dir: false, modified: null, permissions: null }], current_path: '/cloud', display_current_path: '/cloud', breadcrumb_labels: {} };
  const invoke = vi.fn(async (command: string) => {
    if (command === 'get_cloud_config') return CLOUD_CONFIG;
    if (command === 'provider_change_dir') return listing;
    if (command === 'get_local_files') return [];
    return '';
  });
  const { handleCloudTabClick, spy } = await cloudTabApp({ invoke, runConnect: vi.fn(async () => undefined), isSameConnection: () => false, sessions: [{ id: 'tab-a', status: 'connected' }] });
  await handleCloudTabClick();
  expect(spy('setIsConnected')).toHaveBeenLastCalledWith(true);
  expect(spy('setRemoteFiles')).toHaveBeenLastCalledWith(listing.files);
  expect(Math.max(...spy('setIsConnected').mock.invocationCallOrder)).toBeGreaterThan(Math.max(...spy('setRemoteFiles').mock.invocationCallOrder));
});

it('leaves the spinner to the takeover when a replacing AeroCloud open loses its phase during the connect', async () => {
  let finishConnect!: () => void;
  const runConnect = vi.fn(() => new Promise<void>(resolve => { finishConnect = resolve; }));
  const invoke = vi.fn(async (command: string) => {
    if (command === 'get_cloud_config') return CLOUD_CONFIG;
    if (command === 'provider_change_dir') return { files: [], current_path: '/cloud' };
    if (command === 'get_local_files') return [];
    return '';
  });
  const opened = await cloudTabApp({ invoke, runConnect, isSameConnection: () => false, sessions: [{ id: 'tab-a', status: 'connected' }] });
  const open = opened.handleCloudTabClick();
  for (let i = 0; i < 50 && runConnect.mock.calls.length === 0; i++) await Promise.resolve();
  expect(runConnect).toHaveBeenCalled();
  opened.takeOverActiveSession();
  await Promise.resolve();
  const loadingBefore = opened.spy('setLoading').mock.calls.length;
  finishConnect();
  await open;
  expect(opened.spy('setLoading').mock.calls.slice(loadingBefore)).toEqual([]);
  expect(invoke.mock.calls.map(([command]) => command)).not.toContain('provider_change_dir');
});

it('keeps a fresh AeroCloud connect disconnected until its folder has listed', async () => {
  // No session on screen; the folder's listing is held. Uploads and drops
  // start with the connection check, so while it is false none can take the
  // folder the panel showed before.
  expect(app.slice(app.indexOf('const uploadMultipleFiles = '), app.indexOf('const uploadMultipleFiles = ') + 400)).toContain('if (!isConnected) return;');
  const listing = { files: [{ name: 'a.txt', path: '/cloud/a.txt', size: 1, is_dir: false, modified: null, permissions: null }], current_path: '/cloud', display_current_path: '/cloud', breadcrumb_labels: {} };
  let releaseListing!: (value: unknown) => void;
  const invoke = vi.fn(async (command: string) => {
    if (command === 'get_cloud_config') return CLOUD_CONFIG;
    if (command === 'provider_change_dir') return new Promise(resolve => { releaseListing = resolve; });
    if (command === 'get_local_files') return [];
    return '';
  });
  const { handleCloudTabClick, spy } = await cloudTabApp({ invoke, runConnect: vi.fn(async () => undefined), isConnected: false, activeSessionId: null });
  const open = handleCloudTabClick();
  for (let i = 0; i < 50 && !releaseListing; i++) await Promise.resolve();
  expect(invoke).toHaveBeenCalledWith('provider_change_dir', { path: '/cloud' });
  expect(spy('setIsConnected')).not.toHaveBeenCalledWith(true);
  releaseListing(listing);
  await open;
  expect(spy('setIsConnected')).toHaveBeenLastCalledWith(true);
  expect(spy('setRemoteFiles')).toHaveBeenLastCalledWith(listing.files);
  expect(Math.max(...spy('setIsConnected').mock.invocationCallOrder)).toBeGreaterThan(Math.max(...spy('setRemoteFiles').mock.invocationCallOrder));
});

it('opens docs under the login folder from a kept session standing in /home/u/other', async () => {
  // App's own placement and reuse rule, run as the control runs them.
  const { mayReuseForAeroCloud } = await import('./liveConnection');
  const { isLoginRelativeScope } = await import('./overlayScope');
  const placeAt = app.indexOf('  const placeStartFolder = async');
  const placeSource = app.slice(placeAt, app.indexOf('\n  );\n', placeAt) + 5);
  const { code: placeCode } = await transformWithOxc(placeSource, 'place-start-folder.ts');
  const login = '/home/u';
  const state = { cwd: '/home/u/other' };
  const listing = (path: string) => ({ files: [], current_path: path, display_current_path: path, breadcrumb_labels: {} });
  const invoke = vi.fn(async (command: string, args?: { path?: string | null }) => {
    if (command === 'get_cloud_config') return { ...CLOUD_CONFIG, remote_folder: 'docs' };
    if (command === 'get_local_files') return [];
    // As crypt_overlay_provider::place_start_folder: a bare relative folder
    // is the one the connect just opened, the folder the session stands in.
    if (command === 'provider_start_folder') return state.cwd;
    if (command === 'provider_change_dir') { state.cwd = args!.path!.startsWith('/') ? args!.path! : `${state.cwd}/${args!.path}`; return listing(state.cwd); }
    if (command === 'provider_list_files') return listing(state.cwd);
    return '';
  });
  const placeStartFolder = new Function('invoke', 'isLoginRelativeScope', `${placeCode}; return placeStartFolder;`)(invoke, isLoginRelativeScope);
  // The connect opens the saved folder from the login folder.
  const runConnect = vi.fn(async () => { state.cwd = `${login}/docs`; });
  const { handleCloudTabClick, spy } = await cloudTabApp({ invoke, runConnect, placeStartFolder, mayReuseForAeroCloud, currentRemotePath: '/home/u/other' });
  await handleCloudTabClick();
  // `docs` from /home/u/other is another folder: the session connects again.
  expect(runConnect).toHaveBeenCalled();
  expect(state.cwd).toBe('/home/u/docs');
  expect(spy('setCurrentRemotePath')).toHaveBeenLastCalledWith('/home/u/docs');
  expect(spy('setSyncBasePaths')).toHaveBeenLastCalledWith({ remote: '/home/u/docs', local: CLOUD_CONFIG.local_folder });
  expect(spy('adoptOpenedCloudFolder')).toHaveBeenCalledWith(expect.objectContaining({ remoteFolder: '/home/u/docs', savedFolder: 'docs' }));
  expect(JSON.stringify(invoke.mock.calls)).not.toContain('/home/u/other/docs');
});

describe('a kept session whose AeroCloud folder entry fails', () => {
  // The server as the session sees it: a folder change moves its folder
  // before the listing runs, so a listing that fails leaves it moved.
  const server = (failing: { list?: string; back?: boolean; local?: boolean }) => {
    const state = { cwd: '/other/sub', shown: '/other/sub' };
    const parent = (path: string) => path.replace(/\/[^/]+\/?$/u, '') || '/';
    const invoke = vi.fn(async (command: string, args?: { path?: string }) => {
      if (command === 'get_cloud_config') return CLOUD_CONFIG;
      if (command === 'get_local_files') { if (failing.local) throw new Error('local folder missing'); return []; }
      if (command === 'provider_change_dir') {
        const path = args?.path ?? '';
        const target = path === '..' ? parent(state.cwd) : path;
        if (path === state.shown && failing.back) throw new Error('550 cannot enter');
        state.cwd = target;
        if (target === failing.list) throw new Error('Failed to list files: 550');
        return { files: [], current_path: target, display_current_path: target, breadcrumb_labels: {} };
      }
      return '';
    });
    return { state, invoke };
  };
  const before = { isSyncNavigation: true, syncBasePaths: { remote: '/other', local: '/l' }, currentRemotePath: '/other/sub' };

  it('sends the session back to the folder shown, so Up goes to the parent of that folder', async () => {
    const { state, invoke } = server({ list: '/cloud' });
    const { handleCloudTabClick, spy } = await cloudTabApp({ invoke, ...before });
    await handleCloudTabClick();
    expect(invoke).toHaveBeenCalledWith('provider_change_dir', { path: '/cloud' });
    expect(invoke).toHaveBeenLastCalledWith('provider_change_dir', { path: '/other/sub' });
    expect(state.cwd).toBe('/other/sub');
    // Up sends '..' as changeRemoteDirectory does: it lands on the parent of
    // the folder shown, not of the folder the failed entry moved to.
    await invoke('provider_change_dir', { path: '..' });
    expect(state.cwd).toBe('/other');
    expect(spy('setIsSyncNavigation')).toHaveBeenLastCalledWith(true);
    expect(spy('setSyncBasePaths')).toHaveBeenLastCalledWith(before.syncBasePaths);
    expect(spy('setIsConnected')).not.toHaveBeenCalledWith(false);
    expect(invoke.mock.calls.map(([command]) => command)).not.toContain('trigger_cloud_sync');
  });

  it('leaves the session to the lost-folder state when the way back fails too', async () => {
    const { invoke } = server({ list: '/cloud', back: true });
    const { handleCloudTabClick, given, spy } = await cloudTabApp({ invoke, ...before });
    await expect(handleCloudTabClick()).resolves.toBeUndefined();
    expect(invoke).toHaveBeenLastCalledWith('provider_change_dir', { path: '/other/sub' });
    // One failure is shown, the navigation one; the connection stays.
    expect((given.notify as { error: ReturnType<typeof vi.fn> }).error).toHaveBeenCalledTimes(1);
    expect(spy('setIsConnected')).not.toHaveBeenCalledWith(false);
    expect(invoke.mock.calls.map(([command]) => command)).not.toContain('trigger_cloud_sync');
  });

  it('leaves the session to a listing that started after the failed entry: no way back', async () => {
    const { state, invoke: serverInvoke } = server({ list: '/cloud' });
    const remoteNavCounter = { current: 5 };
    // The user opens another folder while the entry runs: its listing takes
    // the next generation before the entry fails.
    const invoke = vi.fn(async (command: string, args?: { path?: string }) => {
      if (command === 'provider_change_dir' && args?.path === '/cloud') { await Promise.resolve(); remoteNavCounter.current += 1; }
      return serverInvoke(command, args);
    });
    const { handleCloudTabClick } = await cloudTabApp({ invoke, remoteNavCounter, ...before });
    await handleCloudTabClick();
    expect(invoke.mock.calls.filter(([command]) => command === 'provider_change_dir')).toEqual([['provider_change_dir', { path: '/cloud' }]]);
    expect(state.cwd).toBe('/cloud');
  });

  it('keeps the session in the folder it entered when only the local side fails, with Navigation Sync off', async () => {
    const { state, invoke } = server({ local: true });
    const { handleCloudTabClick, spy } = await cloudTabApp({ invoke, ...before });
    await handleCloudTabClick();
    expect(state.cwd).toBe('/cloud');
    expect(invoke.mock.calls.filter(([command]) => command === 'provider_change_dir')).toEqual([['provider_change_dir', { path: '/cloud' }]]);
    expect(spy('setCurrentRemotePath')).toHaveBeenLastCalledWith('/cloud');
    expect(spy('setIsSyncNavigation')).toHaveBeenLastCalledWith(false);
    expect(spy('setSyncBasePaths')).toHaveBeenLastCalledWith(null);
  });
});

it('shows the folder the server returned when a kept session enters the AeroCloud folder from a subfolder', async () => {
  // An unlocked overlay session at /cloud/sub is kept, and the open enters /cloud.
  const listing = { files: [{ name: 'a.txt', path: '/cloud/a.txt', size: 1, is_dir: false, modified: null, permissions: null }], current_path: '/cloud', display_current_path: '/cloud', breadcrumb_labels: { '/cloud': 'cloud' } };
  const invoke = vi.fn(async (command: string) => {
    if (command === 'get_cloud_config') return CLOUD_CONFIG;
    if (command === 'provider_change_dir') return listing;
    if (command === 'get_local_files') return [];
    return '';
  });
  // The reviewer's state: an unlocked crypt session stands in /cloud/sub,
  // shown as such.
  const { handleCloudTabClick, spy } = await cloudTabApp({ invoke, currentRemotePath: '/cloud/sub', currentRemoteDisplayPath: '/cloud/sub', rcloneCryptVaultId: 'vault-1' });
  await handleCloudTabClick();
  expect(invoke).toHaveBeenCalledWith('provider_change_dir', { path: '/cloud' });
  // The breadcrumb shows the displayed path, and uploads go to the remote
  // path: both are the folder the server returned, not /cloud/sub. An upload
  // with no folder of its own takes currentRemotePath (its default below).
  expect(app).toContain("const uploadMultipleFiles = withBatchLatch(async (filesOverride?: string[], localPanelId: 'local' | 'local2' = focusedLocalPanelId, remoteDir: string = currentRemotePath) => {");
  expect(spy('setCurrentRemoteDisplayPath')).toHaveBeenLastCalledWith('/cloud');
  expect(spy('setCurrentRemotePath')).toHaveBeenLastCalledWith('/cloud');
  expect(spy('setRemoteBreadcrumb')).toHaveBeenLastCalledWith(listing);
  expect(spy('setRemoteFiles')).toHaveBeenLastCalledWith(listing.files);
  // A search shown over the folder before is dropped with the listing, so
  // the rows and their actions are the AeroCloud folder's, as after any
  // navigation; the selection goes too.
  expect(spy('setRemoteSearchResults')).toHaveBeenLastCalledWith(null);
  expect(spy('setSelectedRemoteFiles')).toHaveBeenLastCalledWith(new Set());
  const lastCall = (name: string) => Math.max(...spy(name).mock.invocationCallOrder);
  expect(lastCall('setRemoteSearchResults')).toBeGreaterThan(lastCall('setRemoteFiles'));
});

// The rows the remote panel shows, by App.tsx's own rule (`sortedRemoteFiles`,
// in its file order): the rows and the paths their actions (download, delete,
// preview) take are these.
const rowsStart = app.indexOf('const sortedRemoteFiles = useMemo(() => {');
const rowsEnd = app.indexOf('}, [remoteFiles, remoteSearchResults, remoteSearchQuery', rowsStart);
if (rowsStart < 0 || rowsEnd < 0) throw new Error('Missing the remote rows rule');
const { code: rowsCode } = await transformWithOxc(app.slice(app.indexOf('{', rowsStart) + 1, rowsEnd), 'remote-rows.ts');
type Row = { name: string; path: string };
const shownRemoteRows = new Function('remoteFiles', 'remoteSearchResults', 'remoteSearchQuery', 'sortFiles', 'remoteSortField', 'remoteSortOrder', rowsCode) as
  (files: Row[], results: Row[] | null, query: string, sort: (rows: Row[]) => Row[], field: string, order: string) => Row[];

it.each([
  ['no query', '', ['/cloud/a.txt', '/cloud/b.txt']],
  ['the query still typed', 'a.', ['/cloud/a.txt']],
])('shows the AeroCloud folder, not a completed search over another folder, with %s', async (_label, query, paths) => {
  expect(app).toMatch(/\{sortedRemoteFiles\.map\(\(file, i\) => \(/u);
  const row = (path: string) => ({ name: path.split('/').pop()!, path, size: 1, is_dir: false, modified: null, permissions: null });
  const searched = [row('/other/a.txt'), row('/other/deep/b.txt')];
  const listing = { files: [row('/cloud/a.txt'), row('/cloud/b.txt')], current_path: '/cloud', display_current_path: '/cloud', breadcrumb_labels: { '/cloud': 'cloud' } };
  const invoke = vi.fn(async (command: string) => {
    if (command === 'get_cloud_config') return CLOUD_CONFIG;
    if (command === 'provider_change_dir') return listing;
    if (command === 'get_local_files') return [];
    return '';
  });
  const { handleCloudTabClick, spy } = await cloudTabApp({ invoke, remoteSearchResults: searched, remoteSearchQuery: query, currentRemotePath: '/other' });
  const state: { remoteFiles: Row[]; remoteSearchResults: Row[] | null } = { remoteFiles: [row('/other/z.txt')], remoteSearchResults: searched };
  expect(shownRemoteRows(state.remoteFiles, state.remoteSearchResults, query, rows => rows, 'name', 'asc').map(r => r.path), 'the search shown before').toEqual(searched.map(r => r.path));
  await handleCloudTabClick();
  // The panel's state after the open: every write, in the order it was made.
  const writes = (['setRemoteFiles', 'setRemoteSearchResults'] as const).flatMap(name => spy(name).mock.calls.map((args, index) => ({ name, value: args[0], order: spy(name).mock.invocationCallOrder[index] })));
  for (const { name, value } of writes.sort((a, b) => a.order - b.order)) {
    const key = name === 'setRemoteFiles' ? 'remoteFiles' : 'remoteSearchResults';
    (state as Record<string, unknown>)[key] = typeof value === 'function' ? value(state[key]) : value;
  }
  const rows = shownRemoteRows(state.remoteFiles, state.remoteSearchResults, query, rows => rows, 'name', 'asc');
  // The buttons' own handlers on those rows: App's downloadFile (the row's
  // download, `downloadFile(file.path, file.name, currentLocalPath, false)`)
  // and deleteRemoteFile (`deleteRemoteFile(path, isDir)`), with the IPC
  // mocked. Every path they send is the AeroCloud folder's.
  expect(app).toContain('await downloadFile(file.path, file.name, currentLocalPath, false);');
  const transfers = fixture({ folders: { [REMOTE]: [] } });
  for (const shown of rows) await transfers.downloadFile(shown.path, shown.name, LOCAL, false);
  const deletes = await appDelete();
  for (const shown of rows) deletes.deleteRemoteFile(shown.path, false);
  await new Promise(resolve => setTimeout(resolve, 0));
  const sent = [...transfers.invoke.mock.calls, ...deletes.invoke.mock.calls]
    .filter(([command]) => command === 'provider_download_file' || command === 'provider_delete_file');
  expect(sent.length, 'one download and one delete per row').toBe(rows.length * 2);
  const named = JSON.stringify(sent);
  expect(named, 'a download or delete sent a path of the search shown before').not.toContain('/other/');
  for (const path of paths) expect(named).toContain(path);
  expect(rows.map(r => r.path)).toEqual(paths);
});

/** App's own `deleteRemoteFile`, with the IPC, the guard and the dialogs mocked. */
async function appDelete() {
  const source = app.slice(app.indexOf('  const deleteRemoteFile = ('), app.indexOf('\n  const renameFile = ('));
  const { code } = await transformWithOxc(source, 'delete.ts');
  const invoke = vi.fn(async () => undefined);
  const context: Record<string, unknown> = {
    remoteActionGuard: () => () => {}, invoke, notify: { success: vi.fn(), error: vi.fn() }, t: (key: string) => key,
    liveConnectionParams: { protocol: 'webdav' }, gitHubRepoInfo: null, setGitHubCommitDialog: vi.fn(),
    humanLog: { logStart: () => 'log', logSuccess: vi.fn(), logError: vi.fn() },
    usesProviderApi: () => true, aeroVaultOverlaySession: null, confirmBeforeDelete: false,
    setConfirmDialog: vi.fn(), loadRemoteFiles: vi.fn(async () => undefined),
  };
  const deleteRemoteFile = new Function(...Object.keys(context), `${code}; return deleteRemoteFile;`)(...Object.values(context)) as (path: string, isDir: boolean) => void;
  return { deleteRemoteFile, invoke };
}

it.each([
  ['the "+" new tab', 'newTab'],
  ['a portable device open', 'device'],
  ['the OAuth or 4shared handshake of My Servers', 'oauth'],
] as const)('sets no connection state from a connect that %s took the session from', async (_label, takeover) => {
  // The AeroCloud tab connects (no session on screen); the takeover happens
  // while that connect is still pending, before anything is listed.
  // My Servers parks through takeOverBeforeSignIn, which marks the backend
  // uncertain and then takes the session over (`takeOverActiveSession`).
  expect(app).toContain('onParkActiveSession={takeOverBeforeSignIn}');
  expect(app).toMatch(/const takeOverBeforeSignIn = useLatestCallback\(\(\) => \{\n\s+backendUncertainRef\.current = true;\n\s+takeOverActiveSession\(\);/);
  let finishConnect!: () => void;
  const runConnect = vi.fn(() => new Promise<void>(resolve => { finishConnect = resolve; }));
  const invoke = vi.fn(async (command: string) => {
    if (command === 'get_cloud_config') return CLOUD_CONFIG;
    if (command === 'provider_change_dir' || command === 'provider_list_files') return { files: [], current_path: '/cloud' };
    if (command === 'get_local_files') return [];
    // The device open waits on the device: its own session is not the point.
    if (command === 'mtp_open_device') return new Promise(() => {});
    return '';
  });
  const opened = await cloudTabApp({ invoke, runConnect, isConnected: false, activeSessionId: null });
  const open = opened.handleCloudTabClick();
  for (let i = 0; i < 20 && runConnect.mock.calls.length === 0; i++) await Promise.resolve();
  expect(runConnect).toHaveBeenCalledWith('provider_connect', expect.objectContaining({ server: 'dav.example.test' }));
  if (takeover === 'newTab') opened.handleNewTabFromSavedServer();
  else if (takeover === 'device') void opened.openMtpSessionFromLiveDevice({ deviceId: 'phone', displayName: 'Phone' });
  else opened.takeOverActiveSession();
  await Promise.resolve();
  const loadingBefore = opened.spy('setLoading').mock.calls.length;
  finishConnect();
  await open;
  // The spinner is the takeover's now: the old open does not clear it.
  expect(opened.spy('setLoading').mock.calls.slice(loadingBefore)).toEqual([]);
  // Nothing of the AeroCloud connection lands on the panel the takeover took.
  expect(opened.spy('setIsConnected')).not.toHaveBeenCalledWith(true);
  expect(opened.spy('setIsConnected')).toHaveBeenLastCalledWith(false);
  expect(opened.spy('setConnectionParams')).not.toHaveBeenCalledWith(expect.objectContaining({ server: 'dav.example.test' }));
  expect(opened.spy('setShowConnectionScreen')).not.toHaveBeenCalledWith(false);
  expect(opened.spy('adoptOpenedCloudFolder')).not.toHaveBeenCalled();
  const commands = invoke.mock.calls.map(([command]) => command);
  expect(commands).not.toContain('provider_change_dir');
  expect(commands).not.toContain('provider_list_files');
  expect(commands).not.toContain('trigger_cloud_sync');
  expect((opened.given.notify as { success: ReturnType<typeof vi.fn> }).success).not.toHaveBeenCalled();
  // The "+" new tab keeps its connection form on screen.
  if (takeover === 'newTab') expect(opened.spy('setShowConnectionScreen')).toHaveBeenLastCalledWith(true);
});

describe('the AeroCloud folder change on a session kept at /other', () => {
  const DOCS_CONFIG = { ...CLOUD_CONFIG, remote_folder: '/home/u/docs' };
  const keptAtOther = async (
    changeDir: () => Promise<unknown>,
    navigation: { isSyncNavigation: boolean; syncBasePaths: { remote: string; local: string } | null } = { isSyncNavigation: false, syncBasePaths: null },
  ) => {
    const invoke = vi.fn(async (command: string) => {
      if (command === 'get_cloud_config') return DOCS_CONFIG;
      if (command === 'provider_change_dir') return changeDir();
      if (command === 'get_local_files') return [];
      return '';
    });
    const opened = await cloudTabApp({ invoke, currentRemotePath: '/other', ...navigation });
    await opened.handleCloudTabClick();
    return { ...opened, invoke, commands: invoke.mock.calls.map(([command]) => command) };
  };

  it('enters the folder: shown, remote and navigation base are the folder the server returned', async () => {
    const listing = { files: [], current_path: '/home/u/docs', display_current_path: '/home/u/docs' };
    const { invoke, commands, spy } = await keptAtOther(async () => listing);
    expect(invoke).toHaveBeenCalledWith('provider_change_dir', { path: '/home/u/docs' });
    expect(spy('setCurrentRemoteDisplayPath')).toHaveBeenLastCalledWith('/home/u/docs');
    expect(spy('setCurrentRemotePath')).toHaveBeenLastCalledWith('/home/u/docs');
    expect(spy('setIsSyncNavigation')).toHaveBeenLastCalledWith(true);
    expect(spy('setSyncBasePaths')).toHaveBeenLastCalledWith({ remote: '/home/u/docs', local: '/local' });
    expect(commands).toContain('trigger_cloud_sync');
  });

  it('fails to enter it: no provisional base stays, no sync is sent, and the error shows', async () => {
    const { commands, spy, given } = await keptAtOther(async () => { throw new Error('Permission denied'); });
    // The lock set before the listing is gone: the panel still shows /other.
    expect(spy('setSyncBasePaths')).toHaveBeenCalledWith({ remote: '/home/u/docs', local: '/local' });
    expect(spy('setSyncBasePaths')).toHaveBeenLastCalledWith(null);
    expect(spy('setIsSyncNavigation')).toHaveBeenLastCalledWith(false);
    expect(spy('setCurrentRemotePath')).not.toHaveBeenCalled();
    expect(spy('adoptOpenedCloudFolder')).not.toHaveBeenCalled();
    expect(commands).not.toContain('trigger_cloud_sync');
    expect((given.notify as { error: ReturnType<typeof vi.fn> }).error).toHaveBeenCalledWith('AeroCloud', 'toast.navigationFailed');
  });

  it('fails to enter it: the Navigation Sync the session had comes back', async () => {
    const before = { remote: '/other', local: '/mine' };
    const { commands, spy } = await keptAtOther(
      async () => { throw new Error('Permission denied'); },
      { isSyncNavigation: true, syncBasePaths: before },
    );
    expect(spy('setSyncBasePaths')).toHaveBeenLastCalledWith(before);
    expect(spy('setIsSyncNavigation')).toHaveBeenLastCalledWith(true);
    expect(commands).not.toContain('trigger_cloud_sync');
  });

  it('fails to enter it on a fresh connect: no base, no sync, the error shows', async () => {
    const invoke = vi.fn(async (command: string) => {
      if (command === 'get_cloud_config') return DOCS_CONFIG;
      if (command === 'provider_change_dir' || command === 'provider_list_files') throw new Error('Permission denied');
      return '';
    });
    const { handleCloudTabClick, spy, given } = await cloudTabApp({ invoke, isConnected: false, activeSessionId: null });
    await handleCloudTabClick();
    expect(spy('setIsSyncNavigation')).not.toHaveBeenCalledWith(true);
    expect(spy('setSyncBasePaths')).toHaveBeenLastCalledWith(null);
    expect(invoke.mock.calls.map(([command]) => command)).not.toContain('trigger_cloud_sync');
    expect((given.notify as { error: ReturnType<typeof vi.fn> }).error).toHaveBeenCalledWith('connection.connectionFailed', expect.stringContaining('Permission denied'));
  });
});

it('leaves the tab disconnected with one message when the backend closed a connection it could not return to its folder', async () => {
  const from = app.indexOf("listen<{ kind: string; detail?: string }>('provider-session', (event) => {");
  const to = app.indexOf('        if (cancelled) fn();', from);
  if (from < 0 || to < 0) throw new Error('Missing provider-session listener');
  const handler = app.slice(app.indexOf('(event) =>', from), app.lastIndexOf('});', to) + 1);
  const { code: listener } = await transformWithOxc(`const handler = ${handler};`, 'provider-session.ts');
  for (const kind of ['closed', 'reconnect-failed']) {
    const notify = { info: vi.fn(() => 'pending'), success: vi.fn(), error: vi.fn() };
    const setIsConnected = vi.fn();
    const removeToast = vi.fn();
    const context = { notify, setIsConnected, removeToast, t: (key: string) => key, wentOnOverScpAfter: () => false, wentOnOverScpRef: { current: false } };
    const run = new Function(...Object.keys(context), `let pendingId = null; ${listener}\nhandler({ payload: { kind: 'reconnecting' } }); handler({ payload: { kind: '${kind}', detail: 'x' } });`);
    run(...Object.values(context));
    expect(notify.error, kind).toHaveBeenCalledTimes(1);
    expect(notify.error).toHaveBeenCalledWith('reconnect.toastTitle', 'reconnect.reconnectFailed');
    expect(removeToast).toHaveBeenCalledWith('pending');
    expect(setIsConnected.mock.calls, kind).toEqual(kind === 'closed' ? [[false]] : []);
  }
});

it('sends every legacy FTP path as a folder the tab means, never one the server resolves against its own folder', async () => {
  // A reconnect inside a legacy download opens the new session in the login
  // folder while the tab keeps showing its own, so a relative path ('..',
  // '.', a bare name) would land elsewhere. Every legacy call that names a
  // remote path is pinned here with the origin of its argument: a new call or
  // a changed argument fails until its origin is checked again.
  const legacy = 'change_directory|create_remote_folder|delete_remote_file|download_file|download_folder|rename_remote_file|save_remote_file|upload_file|upload_folder|chmod_remote_file';
  const calls = [...app.matchAll(new RegExp(`invoke(?:<[^>]*>)?\\('(${legacy})'(?:,\\s*(\\{.*?\\}))?\\)`, 'g'))].map(m => `${m[1]} ${m[2] ?? ''}`);
  expect(calls).toEqual([
    'change_directory { path: savedPath }', // a saved session folder, only when it starts with '/'
    'change_directory { path: folder }', // the AeroCloud folder placed by the connection builder
    'change_directory { path: legacyPath }', // the Up / navigation target, resolved below
    'change_directory { path: newRemotePath }', // the sync base joined with a '/'-led tail
    'create_remote_folder { path: syncNavDialog.targetPath }',
    'change_directory { path: syncNavDialog.targetPath }',
    'download_folder { params }', // remote_path from the listing entry
    'download_file { params }', // remote_path from the listing entry
    'upload_folder { params }', // remote_path joined onto the folder the tab shows
    'upload_file { params: { local_path: localFilePath, remote_path: remotePath } as UploadParams }',
    'save_remote_file { path: file.path, content, expectedGeneration }',
    'delete_remote_file { path: file.path }',
    'rename_remote_file { from: file.path, to: destPath }',
    'delete_remote_file { path: file.path, isDir: file.is_dir }',
    'delete_remote_file { path, isDir }',
    'rename_remote_file { from: path, to: newPath }',
    'rename_remote_file { from: path, to: newPath }',
    'rename_remote_file { from: oldPath, to: newPath }',
    'create_remote_folder { path }', // currentRemotePath joined with the new name
  ]);
  expect(calls.join('\n')).not.toMatch(/['"`]\.{1,2}['"`]|path: ['"`][^/]/);

  // The navigation target the legacy branch sends.
  const resolver = app.slice(app.indexOf('const normDisplayPath = '), app.indexOf('/**', app.indexOf('const resolveTargetDisplayPath = (')));
  const { code: resolverCode } = await transformWithOxc(resolver, 'legacy-path.ts');
  const resolve = new Function(`${resolverCode}\nreturn resolveTargetDisplayPath;`)() as (current: string, path: string) => string;
  expect(app).toContain("const legacyPath = currentRemotePath.startsWith('/') ? resolveTargetDisplayPath(currentRemotePath, path) : path;");
  expect(['..', '.', 'docs', '/work/docs'].map(path => resolve('/work', path))).toEqual(['/', '/work', '/work/docs', '/work/docs']);
  expect(resolve('/home/u/work', '..')).toBe('/home/u');
});
