// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
//
// Source-of-truth dedup logic for the My Servers Table view (Fase 4).
//
// Two profiles backed by the same physical disk (Koofr WebDAV + native REST,
// OpenDrive WebDAV + native API, multiple Wasabi buckets under the same access
// key, ...) must collapse into a single entry when computing storage totals,
// or the footer triple-counts the same gigabytes.
//
// `getStorageDedupKey` produces a canonical string per profile;
// `aggregateByDedupKey` consumes a list of profiles and returns the global
// summary plus a per-protocol-class breakdown for the secondary table.
//
// The Rust twin lives at `src-tauri/src/storage_dedup.rs`. The two
// implementations need not produce byte-identical dedup keys (TS uses a fast
// non-crypto hash, Rust uses SHA-256/12): only the aggregation result must
// agree on the same input data, which `cargo test` cross-checks against this
// module's logical contract.

import {
    ServerProfile,
    ProviderType,
    ProtocolClass,
    getProfileProtocolClass,
} from '../types';
import { canonicalPresetId } from '../providers/connectionManifest';

const OAUTH_PROTOCOLS: ReadonlyArray<ProviderType> = [
    'googledrive',
    'googlephotos',
    'dropbox',
    'onedrive',
    'box',
    'pcloud',
    'zohoworkdrive',
    'yandexdisk',
    'fourshared',
];

const NATIVE_API_PROTOCOLS: ReadonlyArray<ProviderType> = [
    'mega',
    'filen',
    'internxt',
    'kdrive',
    'drime',
    'filelu',
    'koofr',
    'opendrive',
    'jottacloud',
    'github',
    'gitlab',
];

const WEBDAV_PRESET_IDS = new Set<string>([
    'koofr-webdav',
    'opendrive-webdav',
    'pcloud-webdav',
    'yandex-storage-webdav',
    'infinicloud',
    'nextcloud',
    'seafile',
    'cloudme',
    'drivehq',
    'mailru-cloud',
    'jianguoyun',
    'filelu-webdav',
    'felicloud-webdav',
    'puter-webdav',
    'lifetimecloud',
]);

const S3_PRESET_IDS = new Set<string>([
    'backblaze',
    'wasabi',
    'cloudflare-r2',
    'idrive-e2',
    'storj',
    'mega-s4',
    'digitalocean-spaces',
    'alibaba-oss',
    'tencent-cos',
    'oracle-cloud',
    'yandex-storage',
    'filelu-s3',
    'filebase',
    'ibm-cos',
    'tigris',
    'fastly',
    'neon-s3',
]);

/** Lowercase, trim, strip scheme, userinfo, www., path, query and fragment. */
export const normalizeHost = (raw: string | undefined): string => {
    if (!raw) return '';
    let s = raw.trim().toLowerCase();
    for (const scheme of ['https://', 'http://', 'webdavs://', 'webdav://']) {
        if (s.startsWith(scheme)) {
            s = s.slice(scheme.length);
            break;
        }
    }
    // The authority ends at the path, the query or the fragment; none of
    // them names the server, and a query can carry a credential that a
    // dedup key printed by the logs must never hold.
    const authorityEnd = s.search(/[/?#]/);
    if (authorityEnd !== -1) {
        s = s.slice(0, authorityEnd);
    }
    // URL userinfo (`user:pass@host`) is never part of the server identity
    // either.
    const at = s.lastIndexOf('@');
    if (at !== -1) {
        s = s.slice(at + 1);
    }
    if (s.startsWith('www.')) {
        s = s.slice(4);
    }
    return s;
};

const looksLikeOpaqueToken = (s: string): boolean => {
    if (!s) return false;
    if (s.length > 40 && !s.includes('@') && !s.includes(' ')) return true;
    if (/^[A-Fa-f0-9]{32,}$/.test(s)) return true;
    if (/^[A-Za-z0-9_-]{36,}$/.test(s) && !s.includes('@')) return true;
    return false;
};

/**
 * Trim + lowercase. Returns `undefined` when the username looks opaque so the
 * caller falls back to `id:<profileId>` and avoids false dedup.
 */
export const normalizeUser = (raw: string | undefined): string | undefined => {
    if (!raw) return undefined;
    const trimmed = raw.trim();
    if (!trimmed) return undefined;
    if (looksLikeOpaqueToken(trimmed)) return undefined;
    return trimmed.toLowerCase();
};

/**
 * Cross-protocol provider families. Koofr (WebDAV + REST), OpenDrive (WebDAV
 * + REST), pCloud (WebDAV + OAuth), Yandex Disk (WebDAV + OAuth + Object
 * Storage), FileLu (WebDAV + REST) all expose the same physical disk through
 * multiple protocols. When a profile belongs to a known family AND the
 * username is dedup-able, the key uses the `family:` prefix so all surfaces
 * collapse into one drive.
 */
const dedupFamily = (
    providerId: string | undefined,
    protocol: ProviderType,
): string | undefined => {
    if (providerId === 'koofr-webdav' || providerId === 'koofr') return 'koofr';
    if (protocol === 'koofr') return 'koofr';
    if (providerId === 'opendrive-webdav' || providerId === 'opendrive') return 'opendrive';
    if (protocol === 'opendrive') return 'opendrive';
    if (providerId === 'pcloud-webdav' || providerId === 'pcloud') return 'pcloud';
    if (protocol === 'pcloud') return 'pcloud';
    if (
        providerId === 'yandex-storage-webdav'
        || providerId === 'yandex-storage'
        || providerId === 'yandexdisk'
    ) return 'yandex';
    if (protocol === 'yandexdisk') return 'yandex';
    if (providerId === 'filelu-webdav' || providerId === 'filelu') return 'filelu';
    if (protocol === 'filelu') return 'filelu';
    return undefined;
};

/** Fast non-crypto hash (FNV-1a 64-bit) → 16 hex chars. Used to avoid leaking
 * S3 access keys into the dedup string while keeping it stable. The Rust port
 * uses SHA-256/12: different output, but each side dedups consistently
 * within itself.
 */
const fastHashHex = (input: string): string => {
    let hi = 0xcbf29ce4 >>> 0;
    let lo = 0x84222325 >>> 0;
    const PRIME_HI = 0x00000100;
    const PRIME_LO = 0x000001b3;
    for (let i = 0; i < input.length; i++) {
        const c = input.charCodeAt(i);
        lo = (lo ^ c) >>> 0;
        // 64-bit multiply by PRIME, split into 32-bit halves.
        const lh = lo & 0xffff;
        const ll = lo >>> 16;
        const hh = hi & 0xffff;
        const hl = hi >>> 16;
        const newLo = (Math.imul(lo, PRIME_LO) >>> 0);
        const carry = Math.imul(ll, PRIME_LO) >>> 16;
        const newHi = (
            Math.imul(hi, PRIME_LO)
            + Math.imul(lo, PRIME_HI)
            + Math.imul(lh, 0)
            + Math.imul(hh, 0)
            + Math.imul(hl, 0)
            + carry
        ) >>> 0;
        lo = newLo;
        hi = newHi;
    }
    const toHex = (n: number) => n.toString(16).padStart(8, '0');
    return `${toHex(hi)}${toHex(lo)}`;
};

const accessKeyHash = (accessKey: string): string => {
    return fastHashHex((accessKey || '').trim()).slice(0, 16);
};

/**
 * WebDAV presets whose server URL is chosen by the user (self-hosted or
 * per-user node): nextcloud, seafile, infinicloud. Their dedup key must
 * carry the normalised host or the same user name on two servers collapses
 * into one drive. Mirrors the Rust `is_user_hosted_webdav_preset`.
 */
const USER_HOSTED_WEBDAV_PRESETS: ReadonlySet<string> = new Set([
    'nextcloud',
    'seafile',
    'infinicloud',
]);

const isUserHostedWebDavPreset = (providerId: string | undefined): boolean =>
    !!providerId && USER_HOSTED_WEBDAV_PRESETS.has(providerId);

const isWebDavPreset = (providerId: string | undefined): boolean =>
    !!providerId && WEBDAV_PRESET_IDS.has(providerId);

/**
 * First path segment of the server URL: for `https://dav.puter.com/alice`
 * that is `alice`, the Puter account the WebDAV root belongs to. Token auth
 * uses the literal username `-token` for every account, so without this the
 * dedup key cannot tell two Puter drives apart. Mirrors the Rust
 * `puter_account`.
 */
const puterAccount = (rawHost: string | undefined): string => {
    if (!rawHost) return '';
    let s = rawHost.trim().toLowerCase();
    for (const scheme of ['https://', 'http://', 'webdavs://', 'webdav://']) {
        if (s.startsWith(scheme)) {
            s = s.slice(scheme.length);
            break;
        }
    }
    // A query or fragment is not part of the path: an account named only
    // inside it is no account.
    const query = s.search(/[?#]/);
    if (query !== -1) s = s.slice(0, query);
    const slash = s.indexOf('/');
    if (slash === -1) return '';
    const segment = s.slice(slash + 1).split('/')[0];
    // The form saves the default `https://dav.puter.com/{username}` as it
    // is: the placeholder names no account, the profile's user name does.
    if (segment.includes('{username}') || segment.includes('%7busername%7d')) return '';
    return segment;
};

const isS3Preset = (providerId: string | undefined): boolean =>
    !!providerId && S3_PRESET_IDS.has(providerId);

const defaultPort = (proto: ProviderType): number => {
    if (proto === 'ftp' || proto === 'ftps') return 21;
    if (proto === 'sftp' || proto === 'scp') return 22;
    if (proto === 'webdav') return 443;
    return 0;
};

/**
 * Compute the canonical dedup key for `server`. See the Phase 4 handoff for
 * the per-category formula. The fallback is `id:<profileId>` so distinct
 * profiles never collapse by mistake.
 */
export const getStorageDedupKey = (server: ServerProfile): string => {
    const proto = (server.protocol || 'ftp') as ProviderType;
    // A stored alias (`fastly-object-storage`) keys like its preset, or one
    // account counts twice, once per spelling. Mirrors the Rust `dedup_key`.
    const providerId = canonicalPresetId(server.providerId) ?? undefined;
    const userNorm = normalizeUser(server.username);
    const userOrId = userNorm || `id:${server.id}`;

    // Family-based dedup wins when both family + dedup-able username exist.
    const family = dedupFamily(providerId, proto);
    if (family && userNorm) {
        return `family:${family}:${userNorm}`;
    }

    if (OAUTH_PROTOCOLS.includes(proto)) {
        const pid = providerId || proto;
        return `oauth:${pid}:${userOrId}`;
    }

    if (proto === 'webdav') {
        if (providerId === 'puter-webdav') {
            const account = puterAccount(server.host);
            // The account path is the drive identity, whichever auth method
            // the profile uses: password and token profiles of one account
            // must produce the same key.
            if (account) return `webdav:puter-webdav:${account}`;
            // Token auth shares the literal username `-token`; with no
            // account path two Puter profiles cannot be told apart, so they
            // must not collapse into one drive.
            if (userNorm === '-token') return `id:${server.id}`;
            return `webdav:puter-webdav:${userOrId}`;
        }
        if (isUserHostedWebDavPreset(providerId)) {
            // An embedded `:port` in the host and the port field name the
            // same endpoint: take the embedded one as effective and keep it
            // out of the host portion so both spellings key alike.
            const normalized = normalizeHost(server.host);
            const embedded = /:(\d+)$/.exec(normalized);
            const host = embedded ? normalized.slice(0, embedded.index) : normalized;
            const port = embedded
                ? Number(embedded[1])
                : server.port || 443;
            const portPart = port === 443 ? '' : `:${port}`;
            return `webdav:${providerId}:${host}${portPart}:${userOrId}`;
        }
        if (isWebDavPreset(providerId)) {
            return `webdav:${providerId}:${userOrId}`;
        }
        return `webdav-host:${normalizeHost(server.host)}:${userOrId}`;
    }

    if (proto === 's3') {
        const hash = accessKeyHash(server.username);
        if (isS3Preset(providerId)) {
            return `s3:${providerId}:${hash}`;
        }
        return `s3-host:${normalizeHost(server.host)}:${hash}`;
    }

    if (proto === 'azure') {
        return `azure:${userOrId}`;
    }

    if (proto === 'aerocloud') {
        return `aerocloud:${server.id}`;
    }

    if (NATIVE_API_PROTOCOLS.includes(proto)) {
        const pid = providerId || proto;
        return `api:${pid}:${userOrId}`;
    }

    // Proton Drive, through the official CLI or the native client: one
    // account is one drive (mirrors the Rust `dedup_key`).
    if (proto === 'proton') {
        return `api:proton:${userOrId}`;
    }

    if (proto === 'ftp' || proto === 'ftps' || proto === 'sftp' || proto === 'scp') {
        const host = normalizeHost(server.host);
        const port = server.port || defaultPort(proto);
        // SFTP and SCP reach the same SSH server: one storage, one key.
        const family = proto === 'scp' ? 'sftp' : proto;
        return `host:${family}:${host}:${port}:${userOrId}`;
    }

    return `id:${server.id}`;
};

export interface ProtocolBreakdownRow {
    protocolClass: ProtocolClass;
    profiles: number;
    unique: number;
    used: number;
    total: number;
    quotaCount: number;
}

export interface DedupAggregate {
    profiles: number;
    uniqueCount: number;
    totalUsed: number;
    totalTotal: number;
    dedupedQuotaCount: number;
    byProtocolClass: ProtocolBreakdownRow[];
}

/**
 * Aggregate a list of profiles. Quotes sum once per dedup key; when two
 * profiles in the same group disagree on `used`/`total`, the maximum is taken
 * (conservative: never undercounts).
 */
export const aggregateByDedupKey = (servers: ServerProfile[]): DedupAggregate => {
    interface Bucket {
        used?: number;
        total?: number;
        classes: Set<ProtocolClass>;
    }
    const buckets = new Map<string, Bucket>();
    const classProfileCount = new Map<ProtocolClass, number>();
    const classBucketKeys = new Map<ProtocolClass, Set<string>>();

    for (const server of servers) {
        const key = getStorageDedupKey(server);
        // Profile-aware: crypt-overlay profiles group under the shared "Crypt"
        // family in the breakdown (the quota dedup key is unaffected: it still
        // keys off the underlying backend host/bucket).
        // SCP counts with SFTP, as `classify` does in the Rust twin: the same
        // SSH server whichever of the two reaches it.
        const profileClass = getProfileProtocolClass(server);
        const cls: ProtocolClass = profileClass === 'SCP' ? 'SFTP' : profileClass;
        classProfileCount.set(cls, (classProfileCount.get(cls) || 0) + 1);
        const ks = classBucketKeys.get(cls) || new Set<string>();
        ks.add(key);
        classBucketKeys.set(cls, ks);

        let bucket = buckets.get(key);
        if (!bucket) {
            bucket = { classes: new Set<ProtocolClass>() };
            buckets.set(key, bucket);
        }
        bucket.classes.add(cls);

        const quota = server.lastQuota;
        if (quota && quota.total > 0 && typeof quota.used === 'number') {
            bucket.used = Math.max(bucket.used ?? 0, quota.used);
            bucket.total = Math.max(bucket.total ?? 0, quota.total);
        }
    }

    let totalUsed = 0;
    let totalTotal = 0;
    let dedupedQuotaCount = 0;
    for (const bucket of buckets.values()) {
        if (typeof bucket.used === 'number' && typeof bucket.total === 'number' && bucket.total > 0) {
            totalUsed += bucket.used;
            totalTotal += bucket.total;
            dedupedQuotaCount += 1;
        }
    }

    const breakdown: ProtocolBreakdownRow[] = [];
    for (const [cls, profileCount] of classProfileCount) {
        const keys = classBucketKeys.get(cls) || new Set<string>();
        let used = 0;
        let total = 0;
        let quotaCount = 0;
        for (const k of keys) {
            const b = buckets.get(k);
            if (b && typeof b.used === 'number' && typeof b.total === 'number' && b.total > 0) {
                used += b.used;
                total += b.total;
                quotaCount += 1;
            }
        }
        breakdown.push({
            protocolClass: cls,
            profiles: profileCount,
            unique: keys.size,
            used,
            total,
            quotaCount,
        });
    }

    breakdown.sort((a, b) => {
        if (a.total === 0 && b.total === 0) return a.protocolClass.localeCompare(b.protocolClass);
        if (a.total === 0) return 1;
        if (b.total === 0) return -1;
        return b.total - a.total;
    });

    return {
        profiles: servers.length,
        uniqueCount: buckets.size,
        totalUsed,
        totalTotal,
        dedupedQuotaCount,
        byProtocolClass: breakdown,
    };
};
