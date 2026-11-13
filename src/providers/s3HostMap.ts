// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// One table for recognizing an S3 provider from an endpoint host
// (src/shared/s3-host-map.json), read here and by the bridge mapper
// (src-tauri/src/bridge_shared.rs map_s3_provider_from_endpoint).

import s3HostMap from '../shared/s3-host-map.json';

interface S3HostEntry {
    providerId: string;
    preset: boolean;
    domains: string[];
    hostPatterns?: string[];
}

const S3_HOST_ENTRIES: readonly (S3HostEntry & { patterns: RegExp[] })[] = (s3HostMap.providers as S3HostEntry[])
    .filter(entry => entry.preset)
    .map(entry => ({ ...entry, patterns: (entry.hostPatterns ?? []).map(p => new RegExp(p)) }));

/** The S3 preset a bare endpoint host belongs to, on a domain boundary, or
 *  undefined. */
export function s3ProviderForHost(host: string): string | undefined {
    const h = host.toLowerCase();
    for (const entry of S3_HOST_ENTRIES) {
        if (entry.domains.some(domain => h === domain || h.endsWith('.' + domain))) return entry.providerId;
        if (entry.patterns.some(pattern => pattern.test(h))) return entry.providerId;
    }
    return undefined;
}

/** The S3 preset an endpoint URL or `host[:port]` belongs to: the scheme,
 *  port and path are dropped before the host is looked up. */
export function s3ProviderForEndpoint(endpoint: string): string | undefined {
    return s3ProviderForHost(endpoint.toLowerCase().replace(/^https?:\/\//, '').split(/[:/]/)[0]);
}

/** The short label of an S3 server: the bucket of an R2 host or the
 *  bucket (and region) of an AWS host, else the host, cut to 30 characters.
 *  An explicit endpoint, where the requests go, wins over the server field.
 *  The vendor is read from the shared table, so a lookalike host
 *  (`x.r2.cloudflarestorage.com.example`) is shown as itself. */
export function s3ServerLabel(server: string, endpoint?: unknown): string {
    const target = typeof endpoint === 'string' && endpoint.trim() ? endpoint.trim() : server;
    try {
        const host = new URL(/^https?:\/\//i.test(target) ? target : `https://${target}`).hostname;
        const provider = s3ProviderForHost(host);
        if (provider === 'cloudflare-r2') return `R2: ${host.split('.')[0].slice(0, 8)}...`;
        if (provider === 'amazon-s3') {
            const bucket = host.replace(/\.s3\.amazonaws\.com$/, '');
            const regional = /^(.+)\.s3\.([a-z0-9-]+)\.amazonaws\.com(?:\.cn)?$/.exec(bucket);
            return regional ? `${regional[1]} (S3 ${regional[2]})` : bucket;
        }
        return `S3: ${host.length > 30 ? host.slice(0, 27) + '...' : host}`;
    } catch {
        return 'S3';
    }
}
