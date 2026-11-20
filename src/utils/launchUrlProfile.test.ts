// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import type { ConnectionParams, OpenRequest, ServerProfile } from '../types';
import { matchSignInProfile, openSessionMatchesLaunch, savedProfileMatchesLaunch } from './launchUrlProfile';

const profile = (id: string, name: string, protocol: string, username = ''): ServerProfile =>
  ({ id, name, protocol, username, host: '', port: 443 }) as ServerProfile;

const saved = [
  profile('a', 'Work', 'dropbox', 'me@work.example'),
  profile('b', 'Home', 'dropbox', 'me@home.example'),
  profile('c', 'Only Drive', 'googledrive'),
  profile('d', 'Work', 'sftp', 'work'),
];

describe('matchSignInProfile', () => {
  it('opens the profile the link names, by name or account, any case', () => {
    expect(matchSignInProfile(saved, 'dropbox', 'work')).toEqual({ kind: 'found', profile: saved[0] });
    expect(matchSignInProfile(saved, 'dropbox', 'ME@HOME.EXAMPLE')).toEqual({ kind: 'found', profile: saved[1] });
  });

  it('opens the only saved profile of the provider when the link names none', () => {
    expect(matchSignInProfile(saved, 'googledrive', '')).toEqual({ kind: 'found', profile: saved[2] });
  });

  it('never guesses between several accounts, and never crosses providers', () => {
    expect(matchSignInProfile(saved, 'dropbox', undefined)).toEqual({ kind: 'ambiguous' });
    expect(matchSignInProfile(saved, 'onedrive', 'Work')).toEqual({ kind: 'missing' });
    expect(matchSignInProfile(saved, 'onedrive', '')).toEqual({ kind: 'missing' });
    expect(matchSignInProfile(saved, 'googledrive', 'Work')).toEqual({ kind: 'missing' });
  });
});

describe('a launch URL and the sessions and profiles it can reuse', () => {
  const req = (over: Partial<OpenRequest>): OpenRequest =>
    ({ kind: 'remoteUrl', path: '', folder: '/', name: '', isDir: true, protocol: 's3', server: 'S3.Example.com', username: 'AK', options: { bucket: 'b1' }, ...over } as OpenRequest);
  const session = (over: Partial<ConnectionParams>): ConnectionParams =>
    ({ protocol: 's3', server: 's3.example.com', port: 443, username: 'AK', password: 'x', options: { bucket: 'b1' }, ...over } as ConnectionParams);

  it('switches to the open session of the same endpoint, user and bucket', () => {
    // The second launch of the live run reused the tab: host in any case, the
    // manifest port standing in for an absent one.
    expect(openSessionMatchesLaunch(session({}), req({}))).toBe(true);
    expect(openSessionMatchesLaunch(session({ port: undefined }), req({ port: 443 }))).toBe(true);
  });

  it('opens another tab for another bucket, user, port or protocol', () => {
    expect(openSessionMatchesLaunch(session({}), req({ options: { bucket: 'b2' } }))).toBe(false);
    expect(openSessionMatchesLaunch(session({}), req({ options: {} }))).toBe(false);
    expect(openSessionMatchesLaunch(session({}), req({ username: 'OTHER' }))).toBe(false);
    expect(openSessionMatchesLaunch(session({}), req({ port: 9000 }))).toBe(false);
    expect(openSessionMatchesLaunch(session({ protocol: 'webdav' }), req({}))).toBe(false);
  });

  it('takes the secret only from a profile of the same endpoint and user', () => {
    const saved = { id: 's', name: 'S3', protocol: 's3', host: 's3.example.com', port: 443, username: 'AK', options: { bucket: 'b1' } } as ServerProfile;
    expect(savedProfileMatchesLaunch(saved, req({}))).toBe(true);
    expect(savedProfileMatchesLaunch(saved, req({ options: {} }))).toBe(true);
    expect(savedProfileMatchesLaunch(saved, req({ options: { bucket: 'b2' } }))).toBe(false);
    expect(savedProfileMatchesLaunch(saved, req({ username: 'OTHER' }))).toBe(false);
    expect(savedProfileMatchesLaunch({ ...saved, port: 0 }, req({ port: 443 }))).toBe(true);
  });
});
