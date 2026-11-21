// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { connectionManifest } from '../providers/connectionManifest';
import { protocolDisplayName } from './protocolDisplayName';

describe('protocolDisplayName', () => {
  it('names every service that signs in through the browser', () => {
    // A sign-in link with no saved account named the service by its token:
    // "dropbox needs its saved sign-in" (live run of a dropbox:// link).
    const signIn = connectionManifest.protocols
      .filter(row => ['oauth1', 'oauth2', 'sign_in'].includes(row.credentialKind ?? ''))
      .map(row => row.token);
    expect(signIn.length).toBeGreaterThan(5);
    for (const token of signIn) expect(protocolDisplayName(token), token).toBeTruthy();
  });

  it('keeps each brand in its own spelling', () => {
    expect(protocolDisplayName('dropbox')).toBe('Dropbox');
    expect(protocolDisplayName('mega')).toBe('MEGA');
    expect(protocolDisplayName('sftp')).toBeUndefined();
  });
});
