// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import type { ConnectionParams, OpenRequest, ServerProfile } from '../types';
import { launchFolderToOpen, launchStartFolder, matchSignInProfile, openSessionMatchesLaunch, redactServerForLog, savedProfileMatchesLaunch, splitInlineHostPort } from './launchUrlProfile';

const profile = (id: string, name: string, protocol: string, username = ''): ServerProfile =>
  ({ id, name, protocol, username, host: '', port: 443 }) as ServerProfile;

const saved = [
  profile('a', 'Work', 'dropbox', 'me@work.example'),
  profile('b', 'Home', 'dropbox', 'me@home.example'),
  profile('c', 'Only Drive', 'googledrive'),
  profile('d', 'Work', 'sftp', 'work'),
];

describe('splitInlineHostPort', () => {
  it('splits a bare host and port and leaves everything else whole', () => {
    expect(splitInlineHostPort('example.com:2222')).toEqual({ host: 'example.com', port: 2222 });
    expect(splitInlineHostPort('example.com')).toBeNull();
    expect(splitInlineHostPort('example.com:65536')).toBeNull();
    expect(splitInlineHostPort('example.com:0')).toBeNull();
    expect(splitInlineHostPort('example.com:022')).toBeNull();
    expect(splitInlineHostPort('example.com:abc')).toBeNull();
    expect(splitInlineHostPort('2001:db8::1234')).toBeNull();
    expect(splitInlineHostPort('https://host:8443/dav:1234')).toBeNull();
    expect(splitInlineHostPort('[2001:db8::1]:2222')).toEqual({ host: '[2001:db8::1]', port: 2222 });
  });
});

describe('redactServerForLog', () => {
  it('drops userinfo, query and fragment, and leaves a bare host', () => {
    const tokenUrl = redactServerForLog('https://host/dav?token=secret');
    expect(tokenUrl).toBe('https://host/dav');
    expect(tokenUrl).not.toContain('secret');
    expect(redactServerForLog('https://alice:pw@host:8443/dav?k=v#frag')).toBe('https://host:8443/dav');
    expect(redactServerForLog('example.com')).toBe('example.com');
  });
});

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

  it('never guesses between two profiles the link names alike', () => {
    const twins = [
      profile('w1', 'Work', 'dropbox', 'one@example.com'),
      profile('w2', 'work', 'dropbox', 'two@example.com'),
      profile('s1', 'Shared A', 'dropbox', 'team@example.com'),
      profile('s2', 'Shared B', 'dropbox', 'team@example.com'),
    ];
    expect(matchSignInProfile(twins, 'dropbox', 'work')).toEqual({ kind: 'ambiguous' });
    expect(matchSignInProfile(twins, 'dropbox', 'team@example.com')).toEqual({ kind: 'ambiguous' });
    // A single name match wins over the account step; a single account match opens.
    expect(matchSignInProfile(twins, 'dropbox', 'shared a')).toEqual({ kind: 'found', profile: twins[2] });
    expect(matchSignInProfile(twins, 'dropbox', 'one@example.com')).toEqual({ kind: 'found', profile: twins[0] });
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

  it('opens another tab for another S3 endpoint or repository branch', () => {
    const withEndpoint = session({ options: { bucket: 'b1', endpoint: 'https://Minio.Example.com/' } });
    expect(openSessionMatchesLaunch(withEndpoint, req({ options: { bucket: 'b1', endpoint: 'https://minio.example.com' } }))).toBe(true);
    expect(openSessionMatchesLaunch(withEndpoint, req({ options: { bucket: 'b1', endpoint: 'https://other.example.com' } }))).toBe(false);
    expect(openSessionMatchesLaunch(withEndpoint, req({}))).toBe(false);

    // The App stores a GitHub session's user name as `token` when the link names none.
    const repo = (over: Partial<ConnectionParams>) =>
      session({ protocol: 'github', server: 'owner/repo', port: undefined, username: 'token', options: {}, ...over });
    const link = (options: OpenRequest['options']) =>
      req({ protocol: 'github', server: 'owner/repo', username: '', options });
    expect(openSessionMatchesLaunch(repo({}), link({}))).toBe(true);
    expect(openSessionMatchesLaunch(repo({}), link({ githubBranch: 'dev' }))).toBe(false);
    expect(openSessionMatchesLaunch(repo({ options: { githubBranch: 'dev' } }), link({ githubBranch: 'dev' }))).toBe(true);
    // `token` is the default on both sides: a session or link that names no user is the same as one naming `token`.
    expect(openSessionMatchesLaunch(repo({ username: '' }), link({}))).toBe(true);
    expect(openSessionMatchesLaunch(repo({ username: '' }), req({ protocol: 'github', server: 'owner/repo', username: 'token', options: {} }))).toBe(true);
    expect(openSessionMatchesLaunch(repo({ username: 'bot' }), link({}))).toBe(false);
  });

  it('keeps the case of an S3 endpoint path, not of its scheme and host', () => {
    const tenant = session({ options: { bucket: 'b1', endpoint: 'HTTPS://Gateway.Example/TenantA' } });
    expect(openSessionMatchesLaunch(tenant, req({ options: { bucket: 'b1', endpoint: 'https://gateway.example/TenantA/' } }))).toBe(true);
    expect(openSessionMatchesLaunch(tenant, req({ options: { bucket: 'b1', endpoint: 'https://gateway.example/tenanta' } }))).toBe(false);
    const saved = { id: 's', name: 'S3', protocol: 's3', host: 's3.example.com', port: 443, username: 'AK', options: { bucket: 'b1', endpoint: 'https://gateway.example/TenantA' } } as ServerProfile;
    expect(savedProfileMatchesLaunch(saved, req({ options: { bucket: 'b1', endpoint: 'https://GATEWAY.example/TenantA' } }))).toBe(true);
    expect(savedProfileMatchesLaunch(saved, req({ options: { bucket: 'b1', endpoint: 'https://gateway.example/tenanta' } }))).toBe(false);
  });

  it('never reuses a session that runs another transport than the link asks for', () => {
    const dav = (scheme: string) => session({ protocol: 'webdav', server: 'nas.example', port: 8443, username: 'u', options: { webdavScheme: scheme } as ConnectionParams['options'] });
    const davLink = req({ protocol: 'webdav', server: 'nas.example', port: 8443, username: 'u', options: { webdavScheme: 'https' } as OpenRequest['options'] });
    expect(openSessionMatchesLaunch(dav('http'), davLink)).toBe(false);
    expect(openSessionMatchesLaunch(dav('https'), davLink)).toBe(true);
    const ftp = (tlsMode?: string) => session({ protocol: 'ftp', server: 'host', port: 21, username: 'u', options: tlsMode ? { tlsMode } as ConnectionParams['options'] : {} });
    const ftpes = req({ protocol: 'ftp', server: 'host', port: 21, username: 'u', options: { tlsMode: 'explicit' } as OpenRequest['options'] });
    expect(openSessionMatchesLaunch(ftp(), ftpes)).toBe(false);
    expect(openSessionMatchesLaunch(ftp('none'), ftpes)).toBe(false);
    expect(openSessionMatchesLaunch(ftp('explicit'), ftpes)).toBe(true);
  });

  it('takes the secret only from a profile of the same endpoint and user', () => {
    const saved = { id: 's', name: 'S3', protocol: 's3', host: 's3.example.com', port: 443, username: 'AK', options: { bucket: 'b1' } } as ServerProfile;
    expect(savedProfileMatchesLaunch(saved, req({}))).toBe(true);
    expect(savedProfileMatchesLaunch(saved, req({ options: {} }))).toBe(true);
    expect(savedProfileMatchesLaunch(saved, req({ options: { bucket: 'b2' } }))).toBe(false);
    expect(savedProfileMatchesLaunch(saved, req({ username: 'OTHER' }))).toBe(false);
    expect(savedProfileMatchesLaunch({ ...saved, port: 0 }, req({ port: 443 }))).toBe(true);
    // An S3 endpoint the URL names must be the profile's; none takes the profile's.
    const minio = { ...saved, options: { bucket: 'b1', endpoint: 'https://minio.example.com' } } as ServerProfile;
    expect(savedProfileMatchesLaunch(minio, req({}))).toBe(true);
    expect(savedProfileMatchesLaunch(minio, req({ options: { bucket: 'b1', endpoint: 'https://MINIO.example.com/' } }))).toBe(true);
    expect(savedProfileMatchesLaunch(minio, req({ options: { bucket: 'b1', endpoint: 'https://other.example.com' } }))).toBe(false);
  });
});

describe('launchFolderToOpen', () => {
  it('opens the root a link asks for, like any other folder', () => {
    expect(launchFolderToOpen('/')).toBe('/');
    expect(launchFolderToOpen('/Photos/')).toBe('/Photos/');
    expect(launchFolderToOpen('')).toBeNull();
    expect(launchFolderToOpen(undefined)).toBeNull();
  });

  // Both ways a launch reaches its tab, the open one and a fresh connect, go
  // through the helper in App.tsx, so neither can skip a root link again.
  it('is what both launch paths in App.tsx navigate by', () => {
    const app = readFileSync(resolve(__dirname, '../App.tsx'), 'utf8');
    const body = (name: string) => {
      const start = app.indexOf(`const ${name} = async`);
      expect(start).toBeGreaterThan(-1);
      return app.slice(start, app.indexOf('\n  };\n', start));
    };
    const saved = body('openSavedProfile');
    expect(saved).toContain('if (await activateSavedSession(matched.id) !== true) return;');
    expect(saved).toContain('profileConnector.current(matched.id, scope)');
    expect(saved).toMatch(/const folder = launchFolderToOpen\(initialPath\);\s*if \(folder\) await navigateAfterLaunch\(folder, matched\.protocol, targetSessionId\);\s*$/);
    const remote = body('openRemoteUrlSession');
    // The open tab reads the link's own path: a link with no folder carries
    // none (open_request.rs) and leaves the tab where it is.
    expect(remote).toMatch(/if \(!await switchSession\(existingSession\.id\)\) return;\s*const folder = launchFolderToOpen\(req\.initialPath\);\s*if \(folder\) await navigateAfterLaunch\(folder, targetProtocol, existingSession\.id\);/);
    expect(remote).toContain('await connectToFtp(params, { initialRemotePath: launchStartFolder(req.initialPath, matchedProfile) });');
    const connectStart = app.indexOf('const connectToFtpImpl = async');
    const normalizeAt = app.indexOf('effectiveParams = normalizeProviderConnectionParams', connectStart);
    const connectGate = app.slice(connectStart, normalizeAt);
    // IntroHub form tabs pass server "example.com:2222" through overrideParams.
    // The split stays off the Quick Connect form. A launch URL already has a
    // separate host and port, so the helper leaves it unchanged.
    expect(connectGate).toContain('splitInlineHostPort(effectiveParams.server)');
    expect(connectGate).toContain('if (!overrideParams)');
    expect(connectGate).toContain('setConnectionParams(prev => ({ ...prev, server: inlineTarget.host, port: inlineTarget.port }))');
    const connectLog = app.slice(normalizeAt, app.indexOf("logger.debug('[connectToFtp] protocol:'", normalizeAt));
    expect(connectLog).toContain('protocol: effectiveParams.protocol');
    expect(connectLog).toContain('server: redactServerForLog(effectiveParams.server)');
    expect(connectLog).toContain('port: effectiveParams.port');
    expect(connectLog).not.toContain('logger.debug(\'[connectToFtp] effectiveParams:\', effectiveParams)');
    expect(remote).toMatch(/const match = all\.find\(s => savedProfileMatchesLaunch\(s, req\)\);\s*matchedProfile = match;/);
    expect(remote).not.toMatch(/req\.initialPath \|\| '\/'/);
    expect(saved + remote).not.toMatch(/!== '\/'/);
    // A chain link has no password in the webview: no saved profile's secret stands in for it.
    expect(remote).toContain('if (!params.password && !req.options?.launchSecret) {');
  });
});

// Requests as open_request.rs builds them: `sftp://u@h`, `ftp://h` and
// `dropbox://work` carry no initialPath, the same links with a final `/` carry `/`.
describe('a link that names no folder', () => {
  const links: Array<[string, OpenRequest, OpenRequest]> = [
    ['sftp', { kind: 'remoteUrl', protocol: 'sftp', server: 'h' } as OpenRequest, { kind: 'remoteUrl', protocol: 'sftp', server: 'h', initialPath: '/' } as OpenRequest],
    ['ftp', { kind: 'remoteUrl', protocol: 'ftp', server: 'h' } as OpenRequest, { kind: 'remoteUrl', protocol: 'ftp', server: 'h', initialPath: '/' } as OpenRequest],
    ['dropbox', { kind: 'remoteUrl', protocol: 'dropbox', server: 'work', signIn: true } as OpenRequest, { kind: 'remoteUrl', protocol: 'dropbox', server: 'work', signIn: true, initialPath: '/' } as OpenRequest],
  ];
  const start = { initialPath: '/srv/{username}/', username: 'alice' } as Pick<ServerProfile, 'initialPath' | 'username'>;

  it.each(links)('%s: an open tab in a subfolder stays there, a final / moves it to the root', (_, bare, root) => {
    expect(launchFolderToOpen(bare.initialPath)).toBeNull();
    expect(launchFolderToOpen(root.initialPath)).toBe('/');
  });

  it.each(links)("%s: a fresh connect opens the profile's start folder unless the link names one", (_, bare, root) => {
    expect(launchStartFolder(bare.initialPath, start)).toBe('/srv/alice/');
    expect(launchStartFolder(root.initialPath, start)).toBe('/');
    expect(launchStartFolder(bare.initialPath)).toBe('/');
    expect(launchStartFolder('/Photos/', start)).toBe('/Photos/');
  });
});
