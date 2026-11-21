// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { connectionProtocol } from '../providers/connectionManifest';
import { resolveUsernameTemplate } from './cryptScope';
import type { ConnectionParams, OpenRequest, ServerProfile } from '../types';

export type SignInProfileMatch =
  | { kind: 'found'; profile: ServerProfile }
  | { kind: 'missing' }
  | { kind: 'ambiguous' };

/** The saved profile a sign-in link (`dropbox://<profile name>/Photos/`)
 *  opens. The link's host names the profile, or else its account, in any
 *  case; a link with none opens the provider's only saved profile. More than
 *  one profile at the deciding step is `ambiguous`: guessing could open the
 *  wrong account. */
export const matchSignInProfile = (
  profiles: ServerProfile[],
  protocol: string,
  hint: string | undefined,
): SignInProfileMatch => {
  const candidates = profiles.filter(s => (s.protocol || 'ftp') === protocol);
  const wanted = (hint || '').trim().toLowerCase();
  if (!wanted) {
    if (candidates.length === 1) return { kind: 'found', profile: candidates[0] };
    return candidates.length > 1 ? { kind: 'ambiguous' } : { kind: 'missing' };
  }
  const byName = candidates.filter(s => s.name.toLowerCase() === wanted);
  const matches = byName.length > 0
    ? byName
    : candidates.filter(s => (s.username || '').toLowerCase() === wanted);
  if (matches.length > 1) return { kind: 'ambiguous' };
  return matches.length === 1 ? { kind: 'found', profile: matches[0] } : { kind: 'missing' };
};

const launchDefaultPort = (protocol: string) => connectionProtocol(protocol)?.defaultPort || 21;

/** An S3 `?endpoint=` as compared: scheme and host in any case, no trailing
 *  slash. The path keeps its case: `/TenantA` and `/tenanta` are two
 *  endpoints to the S3 backend. */
const normalizedEndpoint = (endpoint: string | undefined) => {
  const value = (endpoint || '').trim().replace(/\/+$/, '');
  const parts = value.match(/^([a-z][a-z0-9+.-]*:\/\/)?([^/?#]*)(.*)$/i);
  return parts ? `${(parts[1] || '').toLowerCase()}${parts[2].toLowerCase()}${parts[3]}` : value;
};

/** The user name a connection stores when the link names none: GitHub
 *  connects as `token` (App.tsx), every other protocol keeps it empty. */
const effectiveUsername = (protocol: string, username: string | undefined) =>
  username || (protocol === 'github' ? 'token' : '');

/** The transport options a launch URL pins (`webdavScheme` from an
 *  `https://` or `http://` WebDAV link, `tlsMode` from `ftpes://`): an open
 *  session is reused only when it runs the same one, so a plain session never
 *  stands in for a link that asked for TLS. */
const TRANSPORT_OPTIONS = ['webdavScheme', 'tlsMode'] as const;

/** The endpoint a launch URL names: protocol, host (any case), port (the
 *  manifest default when absent), user, bucket, the S3 endpoint override and
 *  the repository branch. */
const launchEndpoint = (req: OpenRequest) => {
  const protocol = req.protocol || 'sftp';
  return {
    protocol,
    host: (req.server || '').toLowerCase(),
    port: req.port || launchDefaultPort(protocol),
    username: effectiveUsername(protocol, req.username),
    bucket: req.options?.bucket || '',
    endpoint: normalizedEndpoint(req.options?.endpoint),
    branch: req.options?.githubBranch || '',
  };
};

/** Whether an open session already is the one a launch URL names, so a
 *  second launch switches to it instead of opening a duplicate tab. Another
 *  bucket, S3 endpoint or branch on the same server is another session. */
export const openSessionMatchesLaunch = (params: ConnectionParams, req: OpenRequest): boolean => {
  const target = launchEndpoint(req);
  const protocol = params.protocol || 'ftp';
  return protocol === target.protocol
    && (params.server || '').toLowerCase() === target.host
    && (params.port || launchDefaultPort(protocol)) === target.port
    && (params.options?.bucket || '') === target.bucket
    && normalizedEndpoint(params.options?.endpoint) === target.endpoint
    && (params.options?.githubBranch || '') === target.branch
    && TRANSPORT_OPTIONS.every(key => !req.options?.[key] || params.options?.[key] === req.options[key])
    && effectiveUsername(protocol, params.username) === target.username;
};

/** Whether a saved profile supplies the secret for a launch URL that carries
 *  none. A URL without a bucket or an S3 endpoint takes the profile's; the
 *  branch does not change the secret, and the URL's branch is the one used. */
export const savedProfileMatchesLaunch = (profile: ServerProfile, req: OpenRequest): boolean => {
  const target = launchEndpoint(req);
  return (profile.protocol || 'ftp') === target.protocol
    && (profile.host || '').toLowerCase() === target.host
    && (profile.port || launchDefaultPort(target.protocol)) === target.port
    && effectiveUsername(target.protocol, profile.username) === target.username
    && (!target.bucket || (profile.options?.bucket || '') === target.bucket)
    && (!target.endpoint || normalizedEndpoint(profile.options?.endpoint) === target.endpoint);
};

/** The folder a launch opens once its tab is up, the open one or a fresh
 *  connect: the link's own path, `/` included (a `dropbox://work/` link asks
 *  for the root, so a tab left in a subfolder moves there). `null` when the
 *  link names no folder (`sftp://host`, `dropbox://work`): the open tab stays
 *  where it is, and a fresh connect keeps the profile's start folder. */
export const launchFolderToOpen = (initialPath: string | undefined): string | null =>
  initialPath ? initialPath : null;

/** The folder a fresh connect from a link opens on: the link's own path,
 *  else the start folder of the saved profile it matched, else the root. */
export const launchStartFolder = (
  initialPath: string | undefined,
  profile?: Pick<ServerProfile, 'initialPath' | 'username'>,
): string => initialPath || resolveUsernameTemplate(profile?.initialPath, profile?.username) || '/';
