// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Saved-profile options that turn off a check the connection would otherwise
 * make, read from `src/shared/profile-security-relaxations.json`, the same
 * table the backend reads (`src-tauri/src/profile_security.rs`). Every import
 * brings these in their safe state unless the user keeps them on purpose
 * (#1506 Requested 188 and 198).
 */

import table from '../shared/profile-security-relaxations.json';
import { connectionManifest, connectionProtocol, getConnectionPreset } from '../providers/connectionManifest';

interface RelaxationRule {
    key: string;
    /** Other spellings the connection loader reads as the same option. */
    aliases?: string[];
    unsafe: Array<string | boolean>;
    /** Stored explicitly in the safe state, so no preset default or auto rule brings the relaxed value back. */
    safe: string | boolean;
    loopbackExempt: boolean;
}

/** One option of a profile that relaxes a security check. */
export interface Relaxation {
    key: string;
    value: string;
}

/** The parts of a saved profile the rules read. */
export interface ProfileSecurityInput {
    host?: string;
    providerId?: string;
    protocol?: string;
    options?: Record<string, unknown> | null;
}

/** The rule a WebDAV host's `http://` prefix belongs to. */
const WEBDAV_SCHEME_KEY = 'webdavScheme';
const hostIsPlainHttp = (host: string | undefined): boolean => /^\s*http:\/\//i.test(host ?? '');

const rules = (table as unknown as { relaxations: RelaxationRule[] }).relaxations;

const matches = (actual: unknown, unsafeValue: string | boolean): boolean => {
    if (typeof actual === 'string' && typeof unsafeValue === 'string') {
        return actual.trim().toLowerCase() === unsafeValue.toLowerCase();
    }
    return actual === unsafeValue;
};

/**
 * Whether an endpoint reaches this machine's loopback interface, judged on the
 * host the URL parser reaches (as `util::endpoint_stays_on_this_machine`), so
 * `http://evil.example#@127.0.0.1` is not loopback.
 */
export const endpointStaysOnThisMachine = (endpoint: string): boolean => {
    const trimmed = endpoint.trim();
    if (!trimmed) return false;
    let host: string;
    try {
        host = new URL(trimmed.includes('://') ? trimmed : `http://${trimmed}`).hostname.toLowerCase();
    } catch {
        return false;
    }
    if (host === 'localhost' || host === '[::1]' || host === '::1') return true;
    const octets = host.split('.');
    return octets.length === 4 && octets[0] === '127' && octets.every(o => /^\d{1,3}$/.test(o));
};

const spellings = (rule: RelaxationRule): string[] => [rule.key, ...(rule.aliases ?? [])];

/** The host a URL parser reaches for `endpoint`, lowercase, or undefined. */
const endpointHost = (endpoint: string): string | undefined => {
    const trimmed = endpoint.trim();
    if (!trimmed) return undefined;
    try {
        return new URL(trimmed.includes('://') ? trimmed : `http://${trimmed}`).hostname.toLowerCase() || undefined;
    } catch {
        return undefined;
    }
};

/**
 * The relaxing options of a profile. The loopback exemption holds only when
 * every address the profile names (its `host` and its `options.endpoint`)
 * stays on this machine, and every spelling the connection loader reads counts.
 */
export const securityRelaxations = (profile: ProfileSecurityInput): Relaxation[] => {
    const options: Record<string, unknown> = profile.options && typeof profile.options === 'object' ? profile.options : {};
    // The preset as the connection loader derives it: the providerId, or the
    // one a protocol alias names (a bare `blomp` protocol is the Blomp preset).
    const presetId = profile.providerId
        ?? (profile.protocol ? connectionManifest.aliases[profile.protocol.trim().toLowerCase()]?.providerId : undefined);
    // A preset applies only to its own protocol, as the loader checks.
    const preset = presetId ? getConnectionPreset(presetId) : undefined;
    const profileToken = profile.protocol ? connectionProtocol(profile.protocol)?.token : undefined;
    const presetDefaults = preset && (!profileToken || connectionProtocol(preset.protocol)?.token === profileToken)
        ? preset.defaults
        : undefined;
    const addresses = [profile.host, options.endpoint]
        .filter((a): a is string => typeof a === 'string' && a.trim() !== '');
    const protocol = profile.protocol ? connectionProtocol(profile.protocol)?.token : undefined;
    // Swift's catalog can send the storage traffic elsewhere: a loopback auth
    // host proves nothing about where the token goes.
    const onLoopback = protocol !== 'swift' && addresses.length > 0 && addresses.every(endpointStaysOnThisMachine);
    // A preset owns its defaults only at the address it fixes: an imported
    // file can name a local-bridge preset and point it at a remote server.
    const presetHosts = [presetDefaults?.server, presetDefaults?.endpoint]
        .filter((a): a is string => typeof a === 'string')
        .map(endpointHost)
        .filter((h): h is string => !!h);
    const presetOwnsTheAddress = !!presetDefaults && addresses.every(address =>
        endpointStaysOnThisMachine(address) || presetHosts.includes(endpointHost(address) ?? ''));
    const exempt = (rule: RelaxationRule) =>
        (presetOwnsTheAddress && !!presetDefaults && rule.key in presetDefaults) || (rule.loopbackExempt && onLoopback);
    const found: Relaxation[] = [];
    for (const rule of rules) {
        if (exempt(rule)) continue;
        const actual = spellings(rule)
            .filter(key => key in options)
            .map(key => options[key])
            .find(value => rule.unsafe.some(u => matches(value, u)));
        if (actual === undefined) continue;
        found.push({ key: rule.key, value: typeof actual === 'string' ? actual : String(actual) });
    }
    // A WebDAV host written as http:// is plain HTTP whatever the scheme
    // option says: the host prefix wins when the connection is built.
    const webdavRule = rules.find(r => r.key === WEBDAV_SCHEME_KEY);
    if (protocol === 'webdav' && hostIsPlainHttp(profile.host)
        && !found.some(r => r.key === WEBDAV_SCHEME_KEY) && webdavRule && !exempt(webdavRule)) {
        found.push({ key: WEBDAV_SCHEME_KEY, value: 'http (host)' });
    }
    return found;
};

/**
 * A copy of `profile` with every relaxing option whose key is not in `keep`
 * brought to its safe state (every spelling removed, the rule's safe value
 * stored explicitly, a WebDAV host's `http://` prefix made `https://`), and the
 * keys reset.
 */
export const makeSafe = <T extends ProfileSecurityInput>(profile: T, keep: readonly string[] = []): { profile: T; removed: string[] } => {
    const removed = securityRelaxations(profile).map(r => r.key).filter(key => !keep.includes(key));
    if (removed.length === 0) return { profile, removed: [] };
    const options = { ...(profile.options ?? {}) };
    let host = profile.host;
    for (const key of removed) {
        const rule = rules.find(r => r.key === key)!;
        for (const spelling of spellings(rule)) delete options[spelling];
        options[rule.key] = rule.safe;
        if (rule.key === WEBDAV_SCHEME_KEY && hostIsPlainHttp(host)) host = host!.trim().replace(/^http:\/\//i, 'https://');
    }
    return { profile: { ...profile, host, options }, removed };
};
