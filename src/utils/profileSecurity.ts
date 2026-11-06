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
    /** Other spellings the connection loader reads as the same option. */
    aliases?: string[];
    unsafe: Array<string | boolean>;
    /** Stored in the safe state when leaving the option out is not safe on its own. */
    safe?: string | boolean;
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

const spellings = (rule: RelaxationRule): string[] => [rule.key, ...(rule.aliases ?? [])];

/**
 * The relaxing options of a profile. The loopback exemption holds only when
 * every address the profile names (its `host` and its `options.endpoint`)
 * stays on this machine, and every spelling the connection loader reads counts.
 */
export const securityRelaxations = (profile: ProfileSecurityInput): Relaxation[] => {
    const options = profile.options;
    if (!options || typeof options !== 'object') return [];
    const presetDefaults = profile.providerId ? getConnectionPreset(profile.providerId)?.defaults : undefined;
    const addresses = [profile.host, options.endpoint]
        .filter((a): a is string => typeof a === 'string' && a.trim() !== '');
    const onLoopback = addresses.length > 0 && addresses.every(endpointStaysOnThisMachine);
    const found: Relaxation[] = [];
    for (const rule of rules) {
        const actual = spellings(rule)
            .filter(key => key in options)
            .map(key => options[key])
            .find(value => rule.unsafe.some(u => matches(value, u)));
        if (actual === undefined) continue;
        if (presetDefaults && rule.key in presetDefaults) continue;
        if (rule.loopbackExempt && onLoopback) continue;
        found.push({ key: rule.key, value: typeof actual === 'string' ? actual : String(actual) });
    }
    return found;
};

/**
 * A copy of `profile` with every relaxing option whose key is not in `keep`
 * brought to its safe state (every spelling removed, the rule's `safe` value
 * stored where it has one), and the keys reset.
 */
export const makeSafe = <T extends ProfileSecurityInput>(profile: T, keep: readonly string[] = []): { profile: T; removed: string[] } => {
    const removed = securityRelaxations(profile).map(r => r.key).filter(key => !keep.includes(key));
    if (removed.length === 0 || !profile.options) return { profile, removed: [] };
    const options = { ...profile.options };
    for (const key of removed) {
        const rule = rules.find(r => r.key === key)!;
        for (const spelling of spellings(rule)) delete options[spelling];
        if (rule.safe !== undefined) options[rule.key] = rule.safe;
    }
    return { profile: { ...profile, options }, removed };
};
