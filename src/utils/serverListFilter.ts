// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// One search vocabulary for every list of saved servers. The My Servers
// search box and the server checklists of the Export/Import dialog filter
// with the same text and the same predicate, so a query typed in one place
// finds the same profiles in the other.

import { ServerProfile, ProviderType, getE2EBits, getProtocolClass, getServerCryptOverlay, NATIVE_PROVIDER_PROTOCOLS } from '../types';
import { getProviderById } from '../providers';

/** The fields the search reads. Saved profiles satisfy it, and so do the
 *  previews a bridge import builds from a third-party config, which carry
 *  no more than these. */
export type SearchableServer = Pick<ServerProfile, 'name' | 'host' | 'username'> & {
    protocol?: string;
    providerId?: string;
    aeroCryptOverlay?: ServerProfile['aeroCryptOverlay'];
};

export function deriveProviderId(server: SearchableServer): string | undefined {
    const proto = server.protocol;
    if (!proto) return undefined;
    if (NATIVE_PROVIDER_PROTOCOLS.has(proto)) return proto;
    const host = (server.host || '').toLowerCase();
    if (proto === 's3') {
        if (host.includes('backblaze')) return 'backblaze';
        if (host.includes('r2.cloudflarestorage')) return 'cloudflare-r2';
        if (host.includes('wasabi')) return 'wasabi';
        if (host.includes('idrive')) return 'idrive-e2';
        // Domain-boundary match: exact s3.filebase.io (+ optional port) or bucket subdomains; reject lookalikes
        const s3Host = host.replace(/^https?:\/\//, '').split(/[:/]/)[0];
        if (s3Host === 's3.filebase.io' || s3Host.endsWith('.s3.filebase.io')) return 'filebase';
        if (host.includes('storj')) return 'storj';
        if (host.includes('mega.io') || host.includes('mega.nz')) return 'mega-s4';
        if (host.includes('amazonaws.com')) return 'amazon-s3';
        if (host.includes('aliyuncs.com')) return 'alibaba-oss';
        if (host.includes('myqcloud.com')) return 'tencent-cos';
        if (host.includes('oraclecloud')) return 'oracle-cloud';
        if (host.includes('digitaloceanspaces')) return 'digitalocean-spaces';
        if (host.includes('storage.yandex')) return 'yandex-storage';
        if (host.includes('filelu')) return 'filelu-s3';
    }
    if (proto === 'webdav') {
        if (host.includes('koofr')) return 'koofr-webdav';
        // Domain-boundary match (same pattern as the filebase S3 rule above):
        // exact mail.ru or *.mail.ru subdomains, so webdav.cloud.mail.ru is not
        // swallowed by the generic 'cloud.' Nextcloud rule below, and lookalikes
        // like gmail.ru are rejected.
        const davHost = host.replace(/^https?:\/\//, '').split(/[:/]/)[0];
        if (davHost === 'mail.ru' || davHost.endsWith('.mail.ru')) return 'mailru-cloud';
        if (host.includes('nextcloud') || host.includes('cloud.')) return 'nextcloud';
        if (host.includes('seafile')) return 'seafile';
        if (host.includes('jianguoyun')) return 'jianguoyun';
        if (host.includes('cloudme')) return 'cloudme';
        if (host.includes('drivehq')) return 'drivehq';
        if (host.includes('infini-cloud') || host.includes('teracloud')) return 'infinicloud';
        if (host.includes('filelu')) return 'filelu-webdav';
        if (host.includes('felicloud')) return 'felicloud-webdav';
        if (host.includes('tab.digital') || host.includes('tabdigital.cloud')) return 'tabdigital-webdav';
    }
    return undefined;
}

export function getServerSearchText(server: SearchableServer): string {
    const protocol = (server.protocol || 'ftp') as ProviderType;
    // A profile that switched protocols (e.g. OpenDrive moved from WebDAV to
    // the native API) can keep a stale `providerId` pointing at the old preset
    // (`opendrive-webdav`). Left as-is, that slug and its display name
    // ("OpenDrive (WebDAV)") leak "webdav" into the search blob, so searching
    // "web" surfaces an API profile that has nothing to do with WebDAV
    // (issue #318). When the stored providerId resolves to a provider whose
    // protocol no longer matches the profile, treat it as stale and re-derive
    // the identity from the current protocol instead.
    const storedProviderId = server.providerId || deriveProviderId(server);
    const storedProvider = storedProviderId ? getProviderById(storedProviderId) : undefined;
    const isStaleProviderId = !!storedProvider?.protocol && storedProvider.protocol !== server.protocol;
    const providerId = isStaleProviderId ? deriveProviderId(server) : storedProviderId;
    const provider = isStaleProviderId
        ? (providerId ? getProviderById(providerId) : undefined)
        : storedProvider;
    const protocolClass = getProtocolClass(protocol);
    const e2eBits = protocolClass === 'E2E' ? getE2EBits(protocol) : null;
    const protocolClassLabel = e2eBits ? `E2E ${e2eBits}-bit` : protocolClass;
    const searchTokens = [
        server.name,
        server.host,
        server.protocol,
        server.username,
        providerId,
        provider?.name,
        protocolClass,
        protocolClassLabel,
        protocolClass === 'OAuth' ? 'oauth2' : '',
        protocolClass === 'S3' ? 's3-compatible' : '',
        protocolClass === 'WebDAV' ? 'webdav' : '',
    ];

    if (e2eBits) {
        searchTokens.push(`${e2eBits}-bit`, `${e2eBits} bit`, `e2e ${e2eBits}`, 'encryption');
    }

    if (
        providerId === 'felicloud' || providerId === 'felicloud-webdav'
        || providerId === 'tabdigital' || providerId === 'tabdigital-webdav'
    ) {
        searchTokens.push('api ocs', 'ocs');
    }

    // Crypt-overlay profiles are searchable by "crypt"/"encrypted" and by kind,
    // while STILL matching their transport tokens above (an S3-backed crypt
    // profile is found by both "s3" and "crypt"). The transport class is kept
    // for search even though the display class is "Crypt".
    const cryptKind = getServerCryptOverlay(server);
    if (cryptKind) {
        searchTokens.push('crypt', 'encrypted', 'overlay');
        searchTokens.push(cryptKind === 'aerocrypt' ? 'aerocrypt' : 'rclone crypt');
    }

    return searchTokens
        .filter((value): value is string => !!value)
        .join(' ')
        .toLowerCase();
}

/**
 * True when every whitespace-separated term of `query` occurs in the
 * lowercased `searchText`. An empty or blank query matches everything.
 * Terms are ANDed, so "axpbuntu sftp" narrows to the SFTP lab servers even
 * though the two words come from different fields of the profile.
 */
export function matchesServerQuery(searchText: string, query: string): boolean {
    const terms = query.toLowerCase().split(/\s+/).filter(Boolean);
    return terms.every(term => searchText.includes(term));
}

/** Servers of `servers` that match `query`, in their original order.
 *  `textOf` lets a caller pass a cached search text (My Servers keeps one
 *  per profile so a keystroke does not rebuild every blob, #221). */
export function filterServersByQuery<T extends SearchableServer>(
    servers: readonly T[],
    query: string,
    textOf: (server: T) => string = getServerSearchText,
): T[] {
    if (!query.trim()) return [...servers];
    return servers.filter(s => matchesServerQuery(textOf(s), query));
}

/**
 * "Select all" over a filtered checklist. It acts on the rows on screen
 * only: when every visible row is already selected it deselects them,
 * otherwise it selects them all. Rows hidden by the filter keep whatever
 * state they had, so narrowing the list never drops a selection made
 * before the filter was typed.
 */
export function toggleVisibleSelection(selected: ReadonlySet<string>, visibleIds: readonly string[]): Set<string> {
    const next = new Set(selected);
    if (visibleIds.length > 0 && visibleIds.every(id => next.has(id))) {
        for (const id of visibleIds) next.delete(id);
    } else {
        for (const id of visibleIds) next.add(id);
    }
    return next;
}

/** Whether every visible row is selected (drives the Select/Deselect label). */
export function allVisibleSelected(selected: ReadonlySet<string>, visibleIds: readonly string[]): boolean {
    return visibleIds.length > 0 && visibleIds.every(id => selected.has(id));
}

/** Selected ids that the current filter hides. Shown in the UI so a
 *  selection the user cannot see is never exported or imported silently. */
export function countSelectedOutside(selected: ReadonlySet<string>, allIds: readonly string[], visibleIds: readonly string[]): number {
    const visible = new Set(visibleIds);
    return allIds.filter(id => selected.has(id) && !visible.has(id)).length;
}

/** i18n key (and count) of the "select all" button above a checklist. With
 *  a filter active it names how many shown rows it touches, so the user
 *  reads that it acts on the filtered set and not on the whole list. */
export function selectShownLabel(
    selected: ReadonlySet<string>,
    shownIds: readonly string[],
    filtering: boolean,
): { key: string; params?: { count: number } } {
    const all = allVisibleSelected(selected, shownIds);
    if (!filtering) return { key: all ? 'settings.deselectAll' : 'settings.selectAll' };
    return { key: all ? 'settings.deselectShown' : 'settings.selectShown', params: { count: shownIds.length } };
}
