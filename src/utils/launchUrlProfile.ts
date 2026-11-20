// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { connectionProtocol } from '../providers/connectionManifest';
import type { ConnectionParams, OpenRequest, ServerProfile } from '../types';

export type SignInProfileMatch =
  | { kind: 'found'; profile: ServerProfile }
  | { kind: 'missing' }
  | { kind: 'ambiguous' };

/** The saved profile a sign-in link (`dropbox://<profile name>/Photos`) opens.
 *  The link's host names the profile, or its account, case-insensitively;
 *  a link with none opens the provider's only saved profile. Several saved
 *  accounts and no name is `ambiguous`: guessing could open the wrong one. */
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
  const profile = candidates.find(s => s.name.toLowerCase() === wanted)
    || candidates.find(s => (s.username || '').toLowerCase() === wanted);
  return profile ? { kind: 'found', profile } : { kind: 'missing' };
};

const launchDefaultPort = (protocol: string) => connectionProtocol(protocol)?.defaultPort || 21;

/** The endpoint a launch URL names: protocol, host (any case), port (the
 *  manifest default when absent), user and bucket. */
const launchEndpoint = (req: OpenRequest) => {
  const protocol = req.protocol || 'sftp';
  return {
    protocol,
    host: (req.server || '').toLowerCase(),
    port: req.port || launchDefaultPort(protocol),
    username: req.username || '',
    bucket: req.options?.bucket || '',
  };
};

/** Whether an open session already is the one a launch URL names, so a
 *  second launch switches to it instead of opening a duplicate tab. Another
 *  bucket on the same endpoint is another session. */
export const openSessionMatchesLaunch = (params: ConnectionParams, req: OpenRequest): boolean => {
  const target = launchEndpoint(req);
  const protocol = params.protocol || 'ftp';
  return protocol === target.protocol
    && (params.server || '').toLowerCase() === target.host
    && (params.port || launchDefaultPort(protocol)) === target.port
    && (params.options?.bucket || '') === target.bucket
    && (params.username || '') === target.username;
};

/** Whether a saved profile supplies the secret for a launch URL that carries
 *  none. A URL without a bucket takes the profile's. */
export const savedProfileMatchesLaunch = (profile: ServerProfile, req: OpenRequest): boolean => {
  const target = launchEndpoint(req);
  return (profile.protocol || 'ftp') === target.protocol
    && (profile.host || '').toLowerCase() === target.host
    && (profile.port || launchDefaultPort(target.protocol)) === target.port
    && (profile.username || '') === target.username
    && (!target.bucket || (profile.options?.bucket || '') === target.bucket);
};
