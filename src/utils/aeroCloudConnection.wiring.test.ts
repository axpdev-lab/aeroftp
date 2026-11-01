// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { transformWithOxc } from 'vite';
import app from '../App.tsx?raw';
import { expect, it, vi } from 'vitest';
import { scpFallbackConnectParam } from './scpFallback';

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
