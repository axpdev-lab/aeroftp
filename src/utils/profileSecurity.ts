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
import { getConnectionPreset } from '../providers/connectionManifest';

interface RelaxationRule {
    key: string;
    unsafe: Array<string | boolean>;
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
    options?: Record<string, unknown> | null;
}

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

/** The relaxing options of a profile. An S3 profile reaches `options.endpoint`, every other one its `host`. */
export const securityRelaxations = (profile: ProfileSecurityInput): Relaxation[] => {
    const options = profile.options;
    if (!options || typeof options !== 'object') return [];
    const presetDefaults = profile.providerId ? getConnectionPreset(profile.providerId)?.defaults : undefined;
    const endpoint = typeof options.endpoint === 'string' && options.endpoint.trim() ? options.endpoint : profile.host ?? '';
    const onLoopback = endpointStaysOnThisMachine(endpoint);
    const found: Relaxation[] = [];
    for (const rule of rules) {
        if (!(rule.key in options)) continue;
        const actual = options[rule.key];
        if (!rule.unsafe.some(u => matches(actual, u))) continue;
        if (presetDefaults && rule.key in presetDefaults) continue;
        if (rule.loopbackExempt && onLoopback) continue;
        found.push({ key: rule.key, value: typeof actual === 'string' ? actual : String(actual) });
    }
    return found;
};

/**
 * A copy of `profile` with every relaxing option whose key is not in `keep`
 * removed (an absent option is the safe default for each), and the keys removed.
 */
export const makeSafe = <T extends ProfileSecurityInput>(profile: T, keep: readonly string[] = []): { profile: T; removed: string[] } => {
    const removed = securityRelaxations(profile).map(r => r.key).filter(key => !keep.includes(key));
    if (removed.length === 0 || !profile.options) return { profile, removed: [] };
    const options = { ...profile.options };
    for (const key of removed) delete options[key];
    return { profile: { ...profile, options }, removed };
};
