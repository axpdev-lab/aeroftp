// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { transformWithOxc } from 'vite';
import app from '../App.tsx?raw';
import { expect, it, vi } from 'vitest';

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
