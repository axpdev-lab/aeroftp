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

const navStart = app.indexOf('const navigateToRemoteFolder = async');
const navEnd = app.indexOf('// Connection params to set', navStart);
if (navStart < 0 || navEnd < 0) throw new Error('Missing AeroCloud navigation closure');
const navCode = (await transformWithOxc(app.slice(navStart, navEnd), 'aerocloud-navigation.ts')).code;

it('bases Navigation Sync on the folder the session lists, not on the saved form', async () => {
  const { relativeSyncSegments } = await import('./syncNavigationPath');
  // A `~/docs` saved before the first sync placed it, on a server whose home
  // is a symlink: the session lists `/usr/home/u/docs`.
  const listed: string[] = [];
  const context = {
    isProvider: true,
    cloudConfig: { remote_folder: '~/docs' },
    placeStartFolder: vi.fn(async (path: string) => path === '~/docs' ? '/home/u/docs' : path),
    invoke: vi.fn(async (_command: string, args: { path: string }) => { listed.push(args.path); return { files: [], current_path: '/usr/home/u/docs' }; }),
    withRemoteListSpinner: (run: () => Promise<unknown>) => run(),
    listingReason: () => '',
    setRemoteFiles: vi.fn(),
    setCurrentRemotePath: vi.fn(),
  };
  const open = new Function(...Object.keys(context), `${navCode}; return openCloudRemoteFolder;`)(...Object.values(context));
  const base = await open();
  expect(listed).toEqual(['/home/u/docs']);
  expect(base).toBe('/usr/home/u/docs');
  // At the listed folder the lock reads the root (Up disabled), one level
  // down a sub-folder; the saved form read every listing as outside.
  expect(relativeSyncSegments('/usr/home/u/docs', base)).toEqual([]);
  expect(relativeSyncSegments('/usr/home/u/docs/a', base)).toEqual(['a']);
  expect(relativeSyncSegments('/usr/home/u/docs', '~/docs')).toBeNull();
  // Both AeroCloud entries take that folder for the lock, the badges and the share link.
  const lock = app.slice(app.indexOf('// Lock navigation to cloud folders'), app.indexOf('// === SCENARIO 2'));
  expect(lock).toMatch(/const listedFolder = await openCloudRemoteFolder\(\);\s*setSyncBasePaths\(\{ remote: listedFolder,/);
  expect(lock).toContain('setCloudRemoteFolder(listedFolder);');
  const fresh = app.slice(app.indexOf('// === SCENARIO 2'), app.indexOf('// === SCENARIO 2') + 3000);
  expect(fresh).toContain('const listedCloudFolder = await openCloudRemoteFolder();');
  expect(fresh).toContain('setCloudRemoteFolder(listedCloudFolder);');
  expect(app).not.toMatch(/navigateToRemoteFolder\(cloudConfig\.remote_folder\)/);
});
