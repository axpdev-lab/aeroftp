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
// non-crypto hash, Rust uses SHA-256/12) — only the aggregation result must
// agree on the same input data, which `cargo test` cross-checks against this
// module's logical contract.

import {
    ServerProfile,
    ProviderType,
    ProtocolClass,
    getProtocolClass,
} from '../types';

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
    'yandex-storage-webdav',
    'infinicloud',
    'nextcloud',
    'seafile',
    'cloudme',
    'drivehq',
    'jianguoyun',
    'filelu-webdav',
    'felicloud-webdav',
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
]);

/** Lowercase, trim, strip scheme + www. + trailing path. */
export const normalizeHost = (raw: string | undefined): string => {
    if (!raw) return '';
    let s = raw.trim().toLowerCase();
    for (const scheme of ['https://', 'http://', 'webdavs://', 'webdav://']) {
        if (s.startsWith(scheme)) {
            s = s.slice(scheme.length);
            break;
        }
    }
    if (s.startsWith('www.')) {
        s = s.slice(4);
    }
    const slash = s.indexOf('/');
    if (slash !== -1) {
        s = s.slice(0, slash);
    }
    while (s.endsWith('/')) {
        s = s.slice(0, -1);
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
 * + REST), Yandex Disk (WebDAV + OAuth + Object Storage), FileLu (WebDAV +
 * REST) all expose the same physical disk through multiple protocols. When a
 * profile belongs to a known family AND the username is dedup-able, the key
 * uses the `family:` prefix so all surfaces collapse into one drive.
 */
const dedupFamily = (
    providerId: string | undefined,
    protocol: ProviderType,
): string | undefined => {
    if (providerId === 'koofr-webdav' || providerId === 'koofr') return 'koofr';
    if (protocol === 'koofr') return 'koofr';
    if (providerId === 'opendrive-webdav' || providerId === 'opendrive') return 'opendrive';
    if (protocol === 'opendrive') return 'opendrive';
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
 * uses SHA-256/12 — different output, but each side dedups consistently
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

const isWebDavPreset = (providerId: string | undefined): boolean =>
    !!providerId && WEBDAV_PRESET_IDS.has(providerId);

const isS3Preset = (providerId: string | undefined): boolean =>
    !!providerId && S3_PRESET_IDS.has(providerId);

const defaultPort = (proto: ProviderType): number => {
    if (proto === 'ftp' || proto === 'ftps') return 21;
    if (proto === 'sftp') return 22;
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
    const userNorm = normalizeUser(server.username);
    const userOrId = userNorm || `id:${server.id}`;

    // Family-based dedup wins when both family + dedup-able username exist.
    const family = dedupFamily(server.providerId, proto);
    if (family && userNorm) {
        return `family:${family}:${userNorm}`;
    }

    if (OAUTH_PROTOCOLS.includes(proto)) {
        const pid = server.providerId || proto;
        return `oauth:${pid}:${userOrId}`;
    }

    if (proto === 'webdav') {
        if (isWebDavPreset(server.providerId)) {
            return `webdav:${server.providerId}:${userOrId}`;
        }
        return `webdav-host:${normalizeHost(server.host)}:${userOrId}`;
    }

    if (proto === 's3') {
        const hash = accessKeyHash(server.username);
        if (isS3Preset(server.providerId)) {
            return `s3:${server.providerId}:${hash}`;
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
        const pid = server.providerId || proto;
        return `api:${pid}:${userOrId}`;
    }

    if (proto === 'ftp' || proto === 'ftps' || proto === 'sftp') {
        const host = normalizeHost(server.host);
        const port = server.port || defaultPort(proto);
        return `host:${proto}:${host}:${port}:${userOrId}`;
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
 * (conservative — never undercounts).
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
        const proto = (server.protocol || 'ftp') as ProviderType;
        const cls = getProtocolClass(proto);
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
