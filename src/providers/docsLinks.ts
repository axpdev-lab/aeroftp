// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * AeroFTP documentation links for the Quick Connect header (#270).
 *
 * Ehud asked for a Docs link to `docs.aeroftp.app/providers/<slug>` on every
 * Quick Connect page. The slug is NOT always the in-app id/protocol (e.g.
 * `googledrive` -> `google-drive`, `s3` -> `aws-s3`, `fourshared` -> `4shared`),
 * so we map explicitly. Every value here is a real page under
 * `docs.aeroftp.app/providers/`: an unmapped provider returns `undefined` and
 * the caller falls back to the provider's own help URL, then the providers
 * index, so a link always renders and never 404s.
 */

const DOCS_BASE = 'https://docs.aeroftp.app/providers';

/** Providers index, the always-valid fallback for unmapped surfaces. */
export const PROVIDER_DOCS_INDEX = `${DOCS_BASE}/`;

/** Registry preset id -> docs slug. Checked first (most specific). */
const PROVIDER_ID_TO_SLUG: Record<string, string> = {
    'amazon-s3': 'aws-s3',
    wasabi: 'wasabi',
    'digitalocean-spaces': 'digitalocean-spaces',
    'alibaba-oss': 'alibaba-cloud-oss',
    'tencent-cos': 'tencent-cloud-cos',
    'cloudflare-r2': 'cloudflare-r2',
    storj: 'storj',
    'idrive-e2': 'idrive-e2',
    minio: 'minio',
    'oracle-cloud': 'oracle-cloud',
    'google-cloud-storage': 'google-cloud-storage',
    'yandex-storage': 'yandex-object-storage',
    blomp: 'blomp',
    'hetzner-storage-box': 'hetzner-storage-box',
    nextcloud: 'nextcloud',
    seafile: 'seafile',
    cloudme: 'cloudme',
    drivehq: 'drivehq',
    infinicloud: 'infinicloud',
    'mega-s4': 'mega-s4',
    s3drive: 's3drive',
    jianguoyun: 'jianguoyun',
    felicloud: 'felicloud',
    quotaless: 'quotaless',
    pixelunion: 'pixelunion',
    tabdigital: 'tabdigital',
    sourceforge: 'sourceforge',
    immich: 'immich',
    // Mode-group presets resolve to the group's account page.
    megacmd: 'mega',
    'megacmd-webdav': 'mega',
    'filen-desktop-webdav': 'filen-desktop',
    'filen-desktop-s3': 'filen-desktop',
    'yandexdisk-webdav': 'yandex',
    'pcloud-webdav': 'pcloud',
    koofr: 'koofr',
    'opendrive-webdav': 'opendrive',
    filelu: 'filelu',
    'filelu-webdav': 'filelu',
    'filelu-s3': 'filelu',
    'filelu-ftp': 'filelu',
};

/** Protocol (ProviderType) -> docs slug. Checked after the preset id. */
const PROTOCOL_TO_SLUG: Record<string, string> = {
    googledrive: 'google-drive',
    dropbox: 'dropbox',
    onedrive: 'onedrive',
    mega: 'mega',
    box: 'box',
    pcloud: 'pcloud',
    filen: 'filen',
    fourshared: '4shared',
    zohoworkdrive: 'zoho',
    internxt: 'internxt',
    kdrive: 'kdrive',
    jottacloud: 'jottacloud',
    drime: 'drime',
    filelu: 'filelu',
    koofr: 'koofr',
    opendrive: 'opendrive',
    yandexdisk: 'yandex',
    github: 'github',
    gitlab: 'gitlab',
    immich: 'immich',
    imagekit: 'imagekit',
    uploadcare: 'uploadcare',
    backblaze: 'backblaze-b2',
    cloudinary: 'cloudinary',
    s3: 'aws-s3',
};

/** Resolve the per-provider AeroFTP docs slug, or undefined when unmapped. */
export function getProviderDocsSlug(
    providerId?: string | null,
    protocol?: string | null,
): string | undefined {
    if (providerId && PROVIDER_ID_TO_SLUG[providerId]) {
        return PROVIDER_ID_TO_SLUG[providerId];
    }
    if (protocol && PROTOCOL_TO_SLUG[protocol]) {
        return PROTOCOL_TO_SLUG[protocol];
    }
    return undefined;
}

/** Per-provider AeroFTP docs URL, or undefined when there is no specific page. */
export function getProviderDocsUrl(
    providerId?: string | null,
    protocol?: string | null,
): string | undefined {
    const slug = getProviderDocsSlug(providerId, protocol);
    return slug ? `${DOCS_BASE}/${slug}` : undefined;
}
