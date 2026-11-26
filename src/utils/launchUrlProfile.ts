// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { connectionProtocol } from '../providers/connectionManifest';
import { resolveUsernameTemplate } from './cryptScope';
import type { ConnectionParams, OpenRequest, ServerProfile } from '../types';

/** A bare `host:port` from the connect form. A URL, a path, and an
 *  unbracketed IPv6 address stay whole: their colons are not a port. */
export function splitInlineHostPort(
  server: string | undefined,
): { host: string; port: number } | null {
  if (!server || server.includes('://') || server.includes('/')) return null;
  const portOf = (possiblePort: string): number | null => {
    if (!/^[0-9]+$/.test(possiblePort)) return null;
    const parsed = Number(possiblePort);
    if (parsed < 1 || parsed > 65535 || String(parsed) !== possiblePort) return null;
    return parsed;
  };
  if (server.startsWith('[')) {
    const end = server.indexOf(']');
    if (end <= 1 || server.charAt(end + 1) !== ':') return null;
    if (server.indexOf(':', end + 2) !== -1) return null;
    const port = portOf(server.slice(end + 2));
    if (port === null) return null;
    return { host: server.slice(0, end + 1), port };
  }
  const colon = server.indexOf(':');
  if (colon <= 0 || colon !== server.lastIndexOf(':')) return null;
  const port = portOf(server.slice(colon + 1));
  if (port === null) return null;
  return { host: server.slice(0, colon), port };
}

/** A server string safe for a debug log: no user, query, or fragment. */
export function redactServerForLog(server: string | undefined): string | undefined {
  if (!server) return server;
  if (server.includes('://')) {
    try {
      const parsed = new URL(server);
      return `${parsed.protocol}//${parsed.host}${parsed.pathname}`;
    } catch {
      return '<redacted-url>';
    }
  }
  const withoutUser = server.includes('@') ? server.slice(server.lastIndexOf('@') + 1) : server;
  const query = withoutUser.indexOf('?');
  const hash = withoutUser.indexOf('#');
  let end = withoutUser.length;
  if (query >= 0) end = Math.min(end, query);
  if (hash >= 0) end = Math.min(end, hash);
  return withoutUser.slice(0, end);
}

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

/** The AWS region an S3 host names, lowercased: `s3.eu-west-1.amazonaws.com`
 *  names `eu-west-1`, the global `s3.amazonaws.com` names `us-east-1`, any
 *  other host names none. */
const awsRegionFromHost = (host: string): string | undefined => {
  if (host === 's3.amazonaws.com') return 'us-east-1';
  const regional = /^s3\.([a-z0-9-]+)\.amazonaws\.com(\.cn)?$/.exec(host);
  return regional ? regional[1] : undefined;
};

/** The AWS host a region answers on: the global endpoint for `us-east-1`,
 *  else the regional one (`.com.cn` for the China regions). */
const awsHostForRegion = (region: string): string => {
  if (region === 'us-east-1') return 's3.amazonaws.com';
  return region.startsWith('cn-') ? `s3.${region}.amazonaws.com.cn` : `s3.${region}.amazonaws.com`;
};

/** Whether two hosts name the same endpoint: equal, or both AWS hosts of the
 *  same region (`s3.amazonaws.com` and `s3.us-east-1.amazonaws.com` are both
 *  us-east-1, so either spelling matches the other). */
const hostsMatch = (a: string, b: string): boolean => {
  if (a === b) return true;
  const region = awsRegionFromHost(a);
  return region !== undefined && region === awsRegionFromHost(b);
};

/** The host and port of an endpoint option: the host without any scheme, and
 *  the port when the endpoint spells one. */
const endpointHostPort = (endpoint: string | undefined): { host: string; port?: number } | null => {
  const value = (endpoint || '').trim();
  if (!value) return null;
  try {
    const url = new URL(/^[a-z][a-z0-9+.-]*:\/\//i.test(value) ? value : `https://${value}`);
    if (!url.hostname) return null;
    return { host: url.hostname.toLowerCase(), port: url.port ? Number(url.port) : undefined };
  } catch {
    return null;
  }
};

/** The host and port an S3 side matches a launch URL by. A side whose `host`
 *  field is empty (the `amazon-s3` preset stores none, only a region and a
 *  bucket) or is the global AWS host resolves the way the backend does: its
 *  explicit endpoint's host, else the AWS host its region names, so a launch
 *  URL's regional host matches the profile and its secret is loaded (#1534). */
const sideHostPort = (
  protocol: string,
  side: { host?: string; port?: number; options?: { endpoint?: string; region?: string } },
): { host: string; port: number } => {
  const fallbackPort = side.port || launchDefaultPort(protocol);
  const host = (side.host || '').toLowerCase();
  if (protocol !== 's3' || (host && host !== 's3.amazonaws.com')) return { host, port: fallbackPort };
  const explicit = endpointHostPort(side.options?.endpoint);
  if (explicit) return { host: explicit.host, port: explicit.port ?? fallbackPort };
  const regionOption = (side.options?.region || '').trim().toLowerCase();
  const region = regionOption || awsRegionFromHost(host) || 'us-east-1';
  return { host: awsHostForRegion(region), port: fallbackPort };
};

/** Whether an open session already is the one a launch URL names, so a
 *  second launch switches to it instead of opening a duplicate tab. Another
 *  bucket, S3 endpoint or branch on the same server is another session. */
export const openSessionMatchesLaunch = (params: ConnectionParams, req: OpenRequest): boolean => {
  const target = launchEndpoint(req);
  const protocol = params.protocol || 'ftp';
  const side = sideHostPort(protocol, { host: params.server, port: params.port, options: params.options });
  const targetSide = sideHostPort(target.protocol, { host: req.server, port: req.port, options: req.options });
  return protocol === target.protocol
    && hostsMatch(side.host, targetSide.host)
    && side.port === targetSide.port
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
  const protocol = profile.protocol || 'ftp';
  const side = sideHostPort(protocol, profile);
  const targetSide = sideHostPort(target.protocol, { host: req.server, port: req.port, options: req.options });
  return protocol === target.protocol
    && hostsMatch(side.host, targetSide.host)
    && side.port === targetSide.port
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
