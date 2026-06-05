// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Company-centric provider catalog (issue #224, T-ADD-SERVICE-TABLE).
 *
 * Single source of truth for the Add Service "All" / list view and for the
 * country-flag + free/paid enrichment in `ProvidersDialog`. One row per
 * COMPANY, not per protocol. The rest of the app stays protocol-centric
 * (`discoverData.ts`, `registry.ts`); this layer re-projects the same
 * providers onto the company axis with the structured metadata Ehud's
 * table needs (country, free storage, free-tier vs paid-only protocols).
 *
 * Every protocol entry carries its Quick Connect launch target
 * (`protocol` + optional `providerId`), so a badge click in the list view
 * opens the connection form pre-selected on that exact connection method.
 * We only list protocols AeroFTP can actually launch: a few paid-only
 * connection methods Ehud mentions (kDrive WebDAV, 4shared FTP/SFTP,
 * Internxt S3/WebDAV) have no preset today and are intentionally omitted
 * rather than rendered as dead badges. See the Ehud reply draft.
 *
 * Curation notes (be honest, this is editorial and drifts):
 * - `freeStorageGb` is the consumer free-tier size in GB, APPROXIMATE.
 *   `null` = no fixed-GB free tier (credit-based, self-hosted, trial, or
 *   paid-only). Verify with the provider before quoting; the footer total
 *   is labelled "approximate".
 * - `countryCode` is the ISO 3166-1 alpha-2 of the company HQ, rendered as
 *   an SVG flag via `country-flag-icons` (works on Windows, unlike emoji).
 *   '' when genuinely unknown / self-hosted. 'EU' for pan-EU hosts.
 * - `paid` on a protocol marks it as paid-plan / credit-card gated (orange
 *   badge) vs free (blue). Also editorial; classification ages, label
 *   carefully. `note` carries the nuance (local bridge, premium plan, ...).
 */

import { ProviderType } from '../types';

/** Protocol / connection-method label shown as a badge. */
export type CatalogProtocol =
    | 'API'
    | 'OAuth'
    | 'WebDAV'
    | 'S3'
    | 'Swift'
    | 'Blob'
    | 'FTP'
    | 'FTPS'
    | 'SFTP'
    | 'MEGAcmd';

/** One connection method for a company, with its Quick Connect target. */
export interface CatalogProtocolRef {
    /** Badge label shown in the list. */
    label: CatalogProtocol;
    /** ProviderType handed to Quick Connect (`onSelectProvider`). */
    protocol: ProviderType;
    /** Registry/discover providerId handed to Quick Connect (optional). */
    providerId?: string;
    /** Paid-plan / credit-card gated (orange badge) vs free (blue). */
    paid?: boolean;
    /** Short qualifier shown as a tooltip (local bridge, premium plan, ...). */
    note?: string;
}

export interface CatalogCompany {
    /** Display name (the row identity). */
    company: string;
    /** Key into `PROVIDER_LOGOS`. */
    logoId: string;
    /** ISO 3166-1 alpha-2 HQ code (or 'EU'), '' when unknown/self-hosted. */
    countryCode: string;
    /** Approximate consumer free-tier size in GB; null = no fixed-GB free tier. */
    freeStorageGb: number | null;
    /** Short qualifier when freeStorageGb is null or needs nuance. */
    freeNote?: string;
    /** Reachability probe URL (global API endpoint). Omitted for
     *  per-account / self-hosted services, which then show no health dot. */
    healthCheckUrl?: string;
    /** Ordered connection methods. `protocols[0]` is the company default
     *  used when the user clicks the row (rather than a specific badge). */
    protocols: CatalogProtocolRef[];
}

/**
 * The catalog. Ordered by descending free storage for readability; the
 * table re-sorts anyway. Storage figures are editorial and approximate.
 */
export const PROVIDER_CATALOG: CatalogCompany[] = [
    { company: 'MEGA', logoId: 'mega', countryCode: 'NZ', freeStorageGb: 20,
      freeNote: 'E2E', healthCheckUrl: 'https://g.api.mega.co.nz',
      protocols: [
          { label: 'API', protocol: 'mega' },
          { label: 'MEGAcmd', protocol: 'webdav', providerId: 'megacmd-webdav', note: 'local bridge' },
          { label: 'S3', protocol: 's3', providerId: 'mega-s4', paid: true, note: 'MEGA S4 object storage' },
      ] },
    { company: 'Drime', logoId: 'drime', countryCode: 'FR', freeStorageGb: 20,
      healthCheckUrl: 'https://app.drime.cloud',
      protocols: [{ label: 'API', protocol: 'drime' }] },
    { company: 'InfiniCloud', logoId: 'infinicloud', countryCode: 'JP', freeStorageGb: 20,
      protocols: [{ label: 'WebDAV', protocol: 'webdav', providerId: 'infinicloud' }] },
    { company: 'ImageKit', logoId: 'imagekit', countryCode: 'IN', freeStorageGb: 20,
      freeNote: 'media CDN', healthCheckUrl: 'https://api.imagekit.io',
      protocols: [{ label: 'API', protocol: 'imagekit', providerId: 'imagekit' }] },
    { company: 'Blomp', logoId: 'blomp', countryCode: 'US', freeStorageGb: 20,
      freeNote: 'referral bonus',
      protocols: [{ label: 'Swift', protocol: 'swift', providerId: 'blomp' }] },
    { company: 'Storj', logoId: 'storj', countryCode: 'US', freeStorageGb: 25,
      freeNote: 'decentralized',
      protocols: [{ label: 'S3', protocol: 's3', providerId: 'storj' }] },
    { company: 'Google Drive', logoId: 'googledrive', countryCode: 'US', freeStorageGb: 15,
      healthCheckUrl: 'https://www.googleapis.com',
      protocols: [{ label: 'OAuth', protocol: 'googledrive' }] },
    { company: '4shared', logoId: '4shared', countryCode: 'VG', freeStorageGb: 15,
      healthCheckUrl: 'https://webdav.4shared.com',
      protocols: [
          { label: 'OAuth', protocol: 'fourshared' },
          { label: 'WebDAV', protocol: 'webdav', providerId: '4shared' },
      ] },
    { company: 'kDrive', logoId: 'kdrive', countryCode: 'CH', freeStorageGb: 15,
      healthCheckUrl: 'https://api.infomaniak.com',
      protocols: [{ label: 'API', protocol: 'kdrive' }] },
    { company: 'Box', logoId: 'box', countryCode: 'US', freeStorageGb: 10,
      healthCheckUrl: 'https://api.box.com',
      protocols: [{ label: 'OAuth', protocol: 'box' }] },
    { company: 'pCloud', logoId: 'pcloud', countryCode: 'CH', freeStorageGb: 10,
      healthCheckUrl: 'https://api.pcloud.com',
      protocols: [
          { label: 'OAuth', protocol: 'pcloud' },
          { label: 'WebDAV', protocol: 'webdav', providerId: 'pcloud-webdav', paid: true, note: 'premium plan' },
      ] },
    { company: 'Filen', logoId: 'filen', countryCode: 'DE', freeStorageGb: 10,
      freeNote: 'E2E', healthCheckUrl: 'https://gateway.filen.io',
      protocols: [
          { label: 'API', protocol: 'filen' },
          { label: 'S3', protocol: 's3', providerId: 'filen-desktop-s3', note: 'desktop bridge' },
          { label: 'WebDAV', protocol: 'webdav', providerId: 'filen-desktop-webdav', note: 'desktop bridge' },
      ] },
    { company: 'Koofr', logoId: 'koofr', countryCode: 'SI', freeStorageGb: 10,
      healthCheckUrl: 'https://app.koofr.net',
      protocols: [
          { label: 'API', protocol: 'koofr' },
          { label: 'WebDAV', protocol: 'webdav', providerId: 'koofr' },
      ] },
    { company: 'Backblaze B2', logoId: 'backblaze', countryCode: 'US', freeStorageGb: 10,
      healthCheckUrl: 'https://api.backblazeb2.com',
      protocols: [
          { label: 'API', protocol: 'backblaze', providerId: 'backblaze-native' },
          { label: 'S3', protocol: 's3', providerId: 'backblaze' },
      ] },
    { company: 'IDrive e2', logoId: 'idrive-e2', countryCode: 'US', freeStorageGb: 10,
      protocols: [{ label: 'S3', protocol: 's3', providerId: 'idrive-e2' }] },
    { company: 'Cloudflare R2', logoId: 'cloudflare-r2', countryCode: 'US', freeStorageGb: 10,
      freeNote: 'egress-free',
      protocols: [{ label: 'S3', protocol: 's3', providerId: 'cloudflare-r2' }] },
    { company: 'Oracle Cloud', logoId: 'oracle-cloud', countryCode: 'US', freeStorageGb: 20,
      freeNote: 'always-free',
      protocols: [{ label: 'S3', protocol: 's3', providerId: 'oracle-cloud' }] },
    { company: 'OneDrive', logoId: 'onedrive', countryCode: 'US', freeStorageGb: 5,
      healthCheckUrl: 'https://graph.microsoft.com',
      protocols: [{ label: 'OAuth', protocol: 'onedrive' }] },
    { company: 'Zoho WorkDrive', logoId: 'zohoworkdrive', countryCode: 'IN', freeStorageGb: 5,
      healthCheckUrl: 'https://www.zohoapis.com',
      protocols: [{ label: 'OAuth', protocol: 'zohoworkdrive' }] },
    { company: 'Jottacloud', logoId: 'jottacloud', countryCode: 'NO', freeStorageGb: 5,
      healthCheckUrl: 'https://jottacloud.com',
      protocols: [{ label: 'API', protocol: 'jottacloud' }] },
    { company: 'OpenDrive', logoId: 'opendrive', countryCode: 'US', freeStorageGb: 5,
      healthCheckUrl: 'https://dev.opendrive.com',
      protocols: [
          { label: 'API', protocol: 'opendrive' },
          { label: 'WebDAV', protocol: 'webdav', providerId: 'opendrive-webdav' },
      ] },
    { company: 'Yandex Disk', logoId: 'yandexdisk', countryCode: 'RU', freeStorageGb: 5,
      healthCheckUrl: 'https://cloud-api.yandex.net',
      protocols: [
          { label: 'OAuth', protocol: 'yandexdisk' },
          { label: 'WebDAV', protocol: 'webdav', providerId: 'yandexdisk-webdav' },
          { label: 'S3', protocol: 's3', providerId: 'yandex-storage', paid: true, note: 'Yandex Object Storage' },
      ] },
    { company: 'Google Cloud Storage', logoId: 'google-cloud-storage', countryCode: 'US', freeStorageGb: 5,
      freeNote: 'always-free tier', healthCheckUrl: 'https://storage.googleapis.com',
      protocols: [{ label: 'S3', protocol: 's3', providerId: 'google-cloud-storage' }] },
    { company: 'CloudMe', logoId: 'cloudme', countryCode: 'SE', freeStorageGb: 3,
      protocols: [{ label: 'WebDAV', protocol: 'webdav', providerId: 'cloudme' }] },
    { company: 'Uploadcare', logoId: 'uploadcare', countryCode: 'US', freeStorageGb: 3,
      freeNote: 'media CDN', healthCheckUrl: 'https://api.uploadcare.com',
      protocols: [{ label: 'API', protocol: 'uploadcare', providerId: 'uploadcare' }] },
    { company: 'Dropbox', logoId: 'dropbox', countryCode: 'US', freeStorageGb: 2,
      healthCheckUrl: 'https://api.dropboxapi.com',
      protocols: [{ label: 'OAuth', protocol: 'dropbox' }] },
    { company: 'Internxt', logoId: 'internxt', countryCode: 'ES', freeStorageGb: 1,
      freeNote: 'E2E', healthCheckUrl: 'https://api.internxt.com',
      protocols: [{ label: 'API', protocol: 'internxt' }] },
    { company: 'FileLu', logoId: 'filelu', countryCode: 'US', freeStorageGb: 1,
      healthCheckUrl: 'https://filelu.com',
      protocols: [
          { label: 'API', protocol: 'filelu' },
          { label: 'WebDAV', protocol: 'webdav', providerId: 'filelu-webdav' },
          { label: 'S3', protocol: 's3', providerId: 'filelu-s3' },
      ] },
    { company: 'DriveHQ', logoId: 'drivehq', countryCode: 'US', freeStorageGb: 1,
      protocols: [{ label: 'WebDAV', protocol: 'webdav', providerId: 'drivehq' }] },
    { company: 'Jianguoyun', logoId: 'jianguoyun', countryCode: 'CN', freeStorageGb: 1,
      freeNote: 'monthly traffic cap',
      protocols: [{ label: 'WebDAV', protocol: 'webdav', providerId: 'jianguoyun' }] },
    { company: 'Felicloud', logoId: 'felicloud', countryCode: '', freeStorageGb: null,
      freeNote: 'Nextcloud host',
      protocols: [{ label: 'WebDAV', protocol: 'webdav', providerId: 'felicloud' }] },
    { company: 'Cloudinary', logoId: 'cloudinary', countryCode: 'US', freeStorageGb: null,
      freeNote: 'credit-based', healthCheckUrl: 'https://api.cloudinary.com',
      protocols: [{ label: 'API', protocol: 'cloudinary', providerId: 'cloudinary' }] },
    { company: 'Amazon S3', logoId: 'amazon-s3', countryCode: 'US', freeStorageGb: null,
      freeNote: '5 GB 12-month trial', healthCheckUrl: 'https://s3.amazonaws.com',
      protocols: [{ label: 'S3', protocol: 's3', providerId: 'amazon-s3', paid: true }] },
    { company: 'Wasabi', logoId: 'wasabi', countryCode: 'US', freeStorageGb: null,
      freeNote: '30-day trial',
      protocols: [{ label: 'S3', protocol: 's3', providerId: 'wasabi', paid: true }] },
    { company: 'DigitalOcean Spaces', logoId: 'digitalocean-spaces', countryCode: 'US', freeStorageGb: null,
      freeNote: 'paid plan',
      protocols: [{ label: 'S3', protocol: 's3', providerId: 'digitalocean-spaces', paid: true }] },
    { company: 'Alibaba OSS', logoId: 'alibaba-oss', countryCode: 'CN', freeStorageGb: null,
      freeNote: 'paid plan',
      protocols: [{ label: 'S3', protocol: 's3', providerId: 'alibaba-oss', paid: true }] },
    { company: 'Tencent COS', logoId: 'tencent-cos', countryCode: 'CN', freeStorageGb: null,
      freeNote: 'paid plan',
      protocols: [{ label: 'S3', protocol: 's3', providerId: 'tencent-cos', paid: true }] },
    { company: 'Azure Blob', logoId: 'azure', countryCode: 'US', freeStorageGb: null,
      freeNote: '12-month trial', healthCheckUrl: 'https://login.microsoftonline.com/common/v2.0/.well-known/openid-configuration',
      protocols: [{ label: 'Blob', protocol: 'azure', paid: true }] },
    { company: 'Hetzner Storage Box', logoId: 'hetzner-storage-box', countryCode: 'DE', freeStorageGb: null,
      freeNote: 'paid plan',
      protocols: [{ label: 'SFTP', protocol: 'sftp', providerId: 'hetzner-storage-box', paid: true }] },
    { company: 'Tab.digital', logoId: 'tabdigital', countryCode: 'IN', freeStorageGb: null,
      freeNote: 'managed Nextcloud',
      protocols: [{ label: 'WebDAV', protocol: 'webdav', providerId: 'tabdigital', paid: true }] },
    { company: 'PixelUnion', logoId: 'pixelunion', countryCode: 'EU', freeStorageGb: null,
      freeNote: 'managed Immich', healthCheckUrl: 'https://pixelunion.eu',
      protocols: [{ label: 'API', protocol: 'immich', providerId: 'pixelunion', paid: true }] },
    { company: 'MinIO', logoId: 'minio', countryCode: '', freeStorageGb: null,
      freeNote: 'self-hosted',
      protocols: [{ label: 'S3', protocol: 's3', providerId: 'minio' }] },
    { company: 'Nextcloud', logoId: 'nextcloud', countryCode: '', freeStorageGb: null,
      freeNote: 'self-hosted',
      protocols: [{ label: 'WebDAV', protocol: 'webdav', providerId: 'nextcloud' }] },
    { company: 'Seafile', logoId: 'seafile', countryCode: '', freeStorageGb: null,
      freeNote: 'self-hosted',
      protocols: [{ label: 'WebDAV', protocol: 'webdav', providerId: 'seafile' }] },
    { company: 'Immich', logoId: 'immich', countryCode: '', freeStorageGb: null,
      freeNote: 'self-hosted', healthCheckUrl: 'https://immich.app',
      protocols: [{ label: 'API', protocol: 'immich' }] },
    { company: 'SourceForge', logoId: 'sourceforge', countryCode: 'US', freeStorageGb: null,
      freeNote: 'OSS hosting',
      protocols: [{ label: 'SFTP', protocol: 'sftp', providerId: 'sourceforge' }] },
    { company: 'GitHub', logoId: 'github', countryCode: 'US', freeStorageGb: null,
      freeNote: 'repo storage', healthCheckUrl: 'https://api.github.com',
      protocols: [{ label: 'API', protocol: 'github' }] },
    { company: 'GitLab', logoId: 'gitlab', countryCode: 'US', freeStorageGb: null,
      freeNote: 'repo storage', healthCheckUrl: 'https://gitlab.com',
      protocols: [{ label: 'API', protocol: 'gitlab' }] },
];

/** Sum of approximate free GB across the given companies (footer total). */
export function totalFreeStorageGb(companies: CatalogCompany[]): number {
    return companies.reduce((sum, c) => sum + (c.freeStorageGb ?? 0), 0);
}

/** Connection methods available on the free tier (blue badges). */
export function freeProtocols(c: CatalogCompany): CatalogProtocolRef[] {
    return c.protocols.filter(p => !p.paid);
}

/** Connection methods gated behind a paid plan / credit card (orange badges). */
export function paidProtocols(c: CatalogCompany): CatalogProtocolRef[] {
    return c.protocols.filter(p => p.paid);
}

/** True when the company has at least one free-tier connection method. */
export function hasFreeTier(c: CatalogCompany): boolean {
    return c.protocols.some(p => !p.paid);
}

/**
 * Principal storage regions per company, as distinct ISO 3166-1 alpha-2
 * country codes (plus 'EU' for pan-EU multi-region and 'global' for
 * decentralized / self-hosted). EDITORIAL and APPROXIMATE: only providers
 * with a meaningful multi-region footprint (mostly S3) are listed; the list
 * is trimmed to the principal regions, not exhaustive. Everything else falls
 * back to the single HQ country. Verify with the provider.
 */
const REGIONS_BY_LOGO: Record<string, string[]> = {
    'amazon-s3': ['US', 'IE', 'DE', 'GB', 'FR', 'SG', 'JP', 'IN', 'BR', 'AU', 'CA', 'KR'],
    'wasabi': ['US', 'NL', 'JP', 'AU', 'CA', 'SG'],
    'storj': ['global'],
    'cloudflare-r2': ['global'],
    'idrive-e2': ['US', 'IE', 'DE'],
    'oracle-cloud': ['US', 'DE', 'GB', 'JP', 'IN', 'AU', 'BR', 'CA'],
    'digitalocean-spaces': ['US', 'NL', 'SG', 'IN', 'DE'],
    'alibaba-oss': ['CN', 'SG', 'US', 'DE', 'JP', 'AU'],
    'tencent-cos': ['CN', 'SG', 'US', 'DE', 'JP', 'KR'],
    'google-cloud-storage': ['US', 'EU', 'SG', 'JP', 'AU', 'BR', 'IN'],
    'minio': ['global'],
    'backblaze': ['US', 'NL'],
    'mega': ['NZ', 'EU', 'CA'],
};

/**
 * Distinct storage regions for a company (country codes, 'EU', or 'global'),
 * falling back to the HQ country when no multi-region data exists. The table
 * shows the first few as flags and a "+N" overflow.
 */
export function companyRegions(c: CatalogCompany): string[] {
    const regions = REGIONS_BY_LOGO[c.logoId];
    if (regions && regions.length) return regions;
    return c.countryCode ? [c.countryCode] : [];
}

/**
 * Logo-id → company index, with a small alias table so callers that key by
 * a slightly different id (e.g. `ProvidersDialog` uses 'hetzner' /
 * 'tab-digital') still resolve. Returns the first matching company.
 */
const LOGO_ALIASES: Record<string, string> = {
    'hetzner': 'hetzner-storage-box',
    'tab-digital': 'tabdigital',
    'zoho-workdrive': 'zohoworkdrive',
    'backblaze-native': 'backblaze',
    'mega-s4': 'mega',
    'megacmd-webdav': 'mega',
    'filelu-s3': 'filelu',
    'filelu-webdav': 'filelu',
    'koofr-webdav': 'koofr',
};

const catalogByLogo = new Map<string, CatalogCompany>();
for (const company of PROVIDER_CATALOG) {
    if (!catalogByLogo.has(company.logoId)) catalogByLogo.set(company.logoId, company);
}

/** Look up a catalog company by its logo id (with alias normalization). */
export function findCatalogByLogo(logoId: string): CatalogCompany | undefined {
    return catalogByLogo.get(logoId) ?? catalogByLogo.get(LOGO_ALIASES[logoId] ?? '');
}
