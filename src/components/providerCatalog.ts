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
import { CatalogCategoryId } from '../types/catalog';

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
    /** Catalog category this connection method belongs to. Drives the
     *  list-view category filter and the context-aware row launch target,
     *  so a multi-protocol company surfaces in every category it touches. */
    category: CatalogCategoryId;
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
    /** The company HAS a genuine (typically permanent) free allowance, but
     *  signup REQUIRES a credit card / payment method on file (e.g. Amazon S3,
     *  Azure Blob, Yandex Object Storage). This is the "third state" between a
     *  no-card free tier and a paid-only product: such companies are kept OUT
     *  of the paid bucket but marked distinctly. See `companyTier`. */
    freeRequiresCard?: boolean;
    /** Reachability probe URL (global API endpoint). Omitted for
     *  per-account / self-hosted services, which then show no health dot. */
    healthCheckUrl?: string;
    /** Ordered connection methods. `protocols[0]` is the company default
     *  used when the user clicks the row (rather than a specific badge). */
    protocols: CatalogProtocolRef[];
}

/**
 * Providers kept in the codebase for DEV work but hidden from the production UI
 * until they are ready: Blomp (storage proxy 403, awaiting a reliable API) and
 * Google Photos (Photos API problem, fix pending on our side). Single source for
 * the "disabled in production, available in DEV" rule applied across the provider
 * lists (Discover catalog, Providers & Integrations dialog, protocol grid).
 */
export const DEV_ONLY_LOGO_IDS: ReadonlySet<string> = new Set(['blomp', 'googlephotos']);

export const isDevOnlyProvider = (logoId: string): boolean => DEV_ONLY_LOGO_IDS.has(logoId);

/**
 * The catalog. Ordered by descending free storage for readability; the
 * table re-sorts anyway. Storage figures are editorial and approximate.
 */
export const PROVIDER_CATALOG: CatalogCompany[] = [
    { company: 'MEGA', logoId: 'mega', countryCode: 'NZ', freeStorageGb: 20,
      freeNote: 'E2E', healthCheckUrl: 'https://g.api.mega.co.nz',
      protocols: [
          { label: 'API', protocol: 'mega', category: 'cloud-storage' },
          { label: 'MEGAcmd', protocol: 'webdav', providerId: 'megacmd-webdav', category: 'webdav', note: 'local bridge' },
      ] },
    { company: 'MEGA S4 Object Storage', logoId: 'mega-s4', countryCode: 'EU', freeStorageGb: null,
      freeNote: 'Pro plan',
      protocols: [{ label: 'S3', protocol: 's3', providerId: 'mega-s4', category: 'object-storage', paid: true, note: 'MEGA S4 object storage' }] },
    { company: 'Drime', logoId: 'drime', countryCode: 'FR', freeStorageGb: 20,
      healthCheckUrl: 'https://app.drime.cloud',
      protocols: [{ label: 'API', protocol: 'drime', category: 'cloud-storage' }] },
    { company: 'InfiniCloud', logoId: 'infinicloud', countryCode: 'JP', freeStorageGb: 20,
      protocols: [{ label: 'WebDAV', protocol: 'webdav', providerId: 'infinicloud', category: 'webdav' }] },
    { company: 'ImageKit', logoId: 'imagekit', countryCode: 'IN', freeStorageGb: 20,
      freeNote: 'media CDN', healthCheckUrl: 'https://api.imagekit.io',
      protocols: [{ label: 'API', protocol: 'imagekit', providerId: 'imagekit', category: 'media-services' }] },
    { company: 'Blomp', logoId: 'blomp', countryCode: 'US', freeStorageGb: 20,
      freeNote: 'referral bonus',
      protocols: [{ label: 'Swift', protocol: 'swift', providerId: 'blomp', category: 'cloud-storage' }] },
    { company: 'Storj', logoId: 'storj', countryCode: 'US', freeStorageGb: null,
      freeNote: '30-day trial',
      protocols: [{ label: 'S3', protocol: 's3', providerId: 'storj', category: 'object-storage', paid: true }] },
    { company: 'Google Drive', logoId: 'googledrive', countryCode: 'US', freeStorageGb: 15,
      healthCheckUrl: 'https://www.googleapis.com',
      protocols: [{ label: 'OAuth', protocol: 'googledrive', category: 'cloud-storage' }] },
    { company: '4shared', logoId: '4shared', countryCode: 'VG', freeStorageGb: 15,
      healthCheckUrl: 'https://webdav.4shared.com',
      protocols: [
          { label: 'OAuth', protocol: 'fourshared', category: 'cloud-storage' },
          { label: 'WebDAV', protocol: 'webdav', providerId: '4shared', category: 'webdav' },
      ] },
    { company: 'kDrive', logoId: 'kdrive', countryCode: 'CH', freeStorageGb: 15,
      healthCheckUrl: 'https://api.infomaniak.com',
      protocols: [{ label: 'API', protocol: 'kdrive', category: 'cloud-storage' }] },
    { company: 'Box', logoId: 'box', countryCode: 'US', freeStorageGb: 10,
      healthCheckUrl: 'https://api.box.com',
      protocols: [{ label: 'OAuth', protocol: 'box', category: 'cloud-storage' }] },
    { company: 'pCloud', logoId: 'pcloud', countryCode: 'CH', freeStorageGb: 10,
      healthCheckUrl: 'https://api.pcloud.com',
      protocols: [
          { label: 'OAuth', protocol: 'pcloud', category: 'cloud-storage' },
          { label: 'WebDAV', protocol: 'webdav', providerId: 'pcloud-webdav', category: 'webdav', paid: true, note: 'premium plan' },
      ] },
    { company: 'Filen', logoId: 'filen', countryCode: 'DE', freeStorageGb: 10,
      freeNote: 'E2E', healthCheckUrl: 'https://gateway.filen.io',
      protocols: [
          { label: 'API', protocol: 'filen', category: 'cloud-storage' },
          { label: 'S3', protocol: 's3', providerId: 'filen-desktop-s3', category: 'object-storage', note: 'desktop bridge' },
          { label: 'WebDAV', protocol: 'webdav', providerId: 'filen-desktop-webdav', category: 'webdav', note: 'desktop bridge' },
      ] },
    { company: 'Koofr', logoId: 'koofr', countryCode: 'SI', freeStorageGb: 10,
      healthCheckUrl: 'https://app.koofr.net',
      protocols: [
          { label: 'API', protocol: 'koofr', category: 'cloud-storage' },
          { label: 'WebDAV', protocol: 'webdav', providerId: 'koofr', category: 'webdav' },
      ] },
    { company: 'Backblaze B2', logoId: 'backblaze', countryCode: 'US', freeStorageGb: 10,
      healthCheckUrl: 'https://api.backblazeb2.com',
      protocols: [
          { label: 'API', protocol: 'backblaze', providerId: 'backblaze-native', category: 'cloud-storage' },
          { label: 'S3', protocol: 's3', providerId: 'backblaze', category: 'object-storage' },
      ] },
    { company: 'IDrive e2', logoId: 'idrive-e2', countryCode: 'US', freeStorageGb: null,
      freeNote: '7-day trial',
      protocols: [{ label: 'S3', protocol: 's3', providerId: 'idrive-e2', category: 'object-storage', paid: true }] },
    { company: 'Cloudflare R2', logoId: 'cloudflare-r2', countryCode: 'US', freeStorageGb: 10,
      freeNote: 'egress-free, card req.', freeRequiresCard: true,
      protocols: [{ label: 'S3', protocol: 's3', providerId: 'cloudflare-r2', category: 'object-storage', paid: true }] },
    { company: 'Oracle Cloud', logoId: 'oracle-cloud', countryCode: 'US', freeStorageGb: 20,
      freeNote: 'always-free, card req.', freeRequiresCard: true,
      protocols: [{ label: 'S3', protocol: 's3', providerId: 'oracle-cloud', category: 'object-storage', paid: true }] },
    { company: 'OneDrive', logoId: 'onedrive', countryCode: 'US', freeStorageGb: 5,
      healthCheckUrl: 'https://graph.microsoft.com',
      protocols: [{ label: 'OAuth', protocol: 'onedrive', category: 'cloud-storage' }] },
    { company: 'Zoho WorkDrive', logoId: 'zohoworkdrive', countryCode: 'IN', freeStorageGb: 5,
      healthCheckUrl: 'https://www.zohoapis.com',
      protocols: [{ label: 'OAuth', protocol: 'zohoworkdrive', category: 'cloud-storage' }] },
    { company: 'Jottacloud', logoId: 'jottacloud', countryCode: 'NO', freeStorageGb: 5,
      healthCheckUrl: 'https://jottacloud.com',
      protocols: [{ label: 'API', protocol: 'jottacloud', category: 'cloud-storage' }] },
    { company: 'OpenDrive', logoId: 'opendrive', countryCode: 'US', freeStorageGb: 5,
      healthCheckUrl: 'https://dev.opendrive.com',
      protocols: [
          { label: 'API', protocol: 'opendrive', category: 'cloud-storage' },
          { label: 'WebDAV', protocol: 'webdav', providerId: 'opendrive-webdav', category: 'webdav' },
      ] },
    { company: 'Yandex Disk', logoId: 'yandexdisk', countryCode: 'RU', freeStorageGb: 5,
      healthCheckUrl: 'https://cloud-api.yandex.net',
      protocols: [
          { label: 'OAuth', protocol: 'yandexdisk', category: 'cloud-storage' },
          { label: 'WebDAV', protocol: 'webdav', providerId: 'yandexdisk-webdav', category: 'webdav' },
      ] },
    { company: 'Yandex Object Storage', logoId: 'yandex-storage', countryCode: 'RU', freeStorageGb: 1,
      freeNote: 'always-free, card req.', freeRequiresCard: true,
      protocols: [{ label: 'S3', protocol: 's3', providerId: 'yandex-storage', category: 'object-storage', paid: true, note: 'Yandex Object Storage' }] },
    { company: 'Google Cloud Storage', logoId: 'google-cloud-storage', countryCode: 'US', freeStorageGb: 5,
      freeNote: 'always-free, card req.', freeRequiresCard: true, healthCheckUrl: 'https://storage.googleapis.com',
      protocols: [{ label: 'S3', protocol: 's3', providerId: 'google-cloud-storage', category: 'object-storage', paid: true }] },
    { company: 'CloudMe', logoId: 'cloudme', countryCode: 'SE', freeStorageGb: 3,
      protocols: [{ label: 'WebDAV', protocol: 'webdav', providerId: 'cloudme', category: 'webdav' }] },
    { company: 'Uploadcare', logoId: 'uploadcare', countryCode: 'US', freeStorageGb: 3,
      freeNote: 'media CDN', healthCheckUrl: 'https://api.uploadcare.com',
      protocols: [{ label: 'API', protocol: 'uploadcare', providerId: 'uploadcare', category: 'media-services' }] },
    { company: 'Dropbox', logoId: 'dropbox', countryCode: 'US', freeStorageGb: 2,
      healthCheckUrl: 'https://api.dropboxapi.com',
      protocols: [{ label: 'OAuth', protocol: 'dropbox', category: 'cloud-storage' }] },
    { company: 'Internxt', logoId: 'internxt', countryCode: 'ES', freeStorageGb: 1,
      freeNote: 'E2E', healthCheckUrl: 'https://api.internxt.com',
      protocols: [{ label: 'API', protocol: 'internxt', category: 'cloud-storage' }] },
    { company: 'FileLu', logoId: 'filelu', countryCode: 'US', freeStorageGb: 10,
      healthCheckUrl: 'https://filelu.com',
      protocols: [
          { label: 'API', protocol: 'filelu', category: 'cloud-storage' },
          { label: 'WebDAV', protocol: 'webdav', providerId: 'filelu-webdav', category: 'webdav' },
          { label: 'S3', protocol: 's3', providerId: 'filelu-s3', category: 'object-storage' },
      ] },
    { company: 'DriveHQ', logoId: 'drivehq', countryCode: 'US', freeStorageGb: 1,
      protocols: [{ label: 'WebDAV', protocol: 'webdav', providerId: 'drivehq', category: 'webdav' }] },
    { company: 'Jianguoyun', logoId: 'jianguoyun', countryCode: 'CN', freeStorageGb: 1,
      freeNote: 'monthly traffic cap',
      protocols: [{ label: 'WebDAV', protocol: 'webdav', providerId: 'jianguoyun', category: 'webdav' }] },
    { company: 'Felicloud', logoId: 'felicloud', countryCode: '', freeStorageGb: 10,
      freeNote: 'Nextcloud host',
      protocols: [{ label: 'WebDAV', protocol: 'webdav', providerId: 'felicloud', category: 'webdav' }] },
    { company: 'Cloudinary', logoId: 'cloudinary', countryCode: 'US', freeStorageGb: null,
      freeNote: 'credit-based', healthCheckUrl: 'https://api.cloudinary.com',
      protocols: [{ label: 'API', protocol: 'cloudinary', providerId: 'cloudinary', category: 'media-services' }] },
    { company: 'Amazon S3', logoId: 'amazon-s3', countryCode: 'US', freeStorageGb: 5,
      freeNote: 'always-free, card req.', freeRequiresCard: true, healthCheckUrl: 'https://s3.amazonaws.com',
      protocols: [{ label: 'S3', protocol: 's3', providerId: 'amazon-s3', category: 'object-storage', paid: true }] },
    { company: 'Wasabi', logoId: 'wasabi', countryCode: 'US', freeStorageGb: null,
      freeNote: '30-day trial',
      protocols: [{ label: 'S3', protocol: 's3', providerId: 'wasabi', category: 'object-storage', paid: true }] },
    { company: 'DigitalOcean Spaces', logoId: 'digitalocean-spaces', countryCode: 'US', freeStorageGb: null,
      freeNote: 'paid plan',
      protocols: [{ label: 'S3', protocol: 's3', providerId: 'digitalocean-spaces', category: 'object-storage', paid: true }] },
    { company: 'Alibaba OSS', logoId: 'alibaba-oss', countryCode: 'CN', freeStorageGb: 5,
      freeNote: 'overseas only, card req.', freeRequiresCard: true,
      protocols: [{ label: 'S3', protocol: 's3', providerId: 'alibaba-oss', category: 'object-storage', paid: true }] },
    { company: 'Tencent COS', logoId: 'tencent-cos', countryCode: 'CN', freeStorageGb: null,
      freeNote: '6-month trial',
      protocols: [{ label: 'S3', protocol: 's3', providerId: 'tencent-cos', category: 'object-storage', paid: true }] },
    { company: 'Azure Blob', logoId: 'azure', countryCode: 'US', freeStorageGb: 5,
      freeNote: 'always-free, card req.', freeRequiresCard: true, healthCheckUrl: 'https://login.microsoftonline.com/common/v2.0/.well-known/openid-configuration',
      protocols: [{ label: 'Blob', protocol: 'azure', category: 'object-storage', paid: true }] },
    { company: 'Hetzner Storage Box', logoId: 'hetzner-storage-box', countryCode: 'DE', freeStorageGb: null,
      freeNote: 'paid plan',
      protocols: [{ label: 'SFTP', protocol: 'sftp', providerId: 'hetzner-storage-box', category: 'protocols', paid: true }] },
    { company: 'Tab.digital', logoId: 'tabdigital', countryCode: 'IN', freeStorageGb: 8,
      freeNote: 'managed Nextcloud',
      protocols: [{ label: 'WebDAV', protocol: 'webdav', providerId: 'tabdigital', category: 'webdav' }] },
    { company: 'PixelUnion', logoId: 'pixelunion', countryCode: 'EU', freeStorageGb: 16,
      freeNote: 'managed Immich', healthCheckUrl: 'https://pixelunion.eu',
      protocols: [{ label: 'API', protocol: 'immich', providerId: 'pixelunion', category: 'media-services' }] },
    { company: 'MinIO', logoId: 'minio', countryCode: '', freeStorageGb: null,
      freeNote: 'self-hosted',
      protocols: [{ label: 'S3', protocol: 's3', providerId: 'minio', category: 'object-storage' }] },
    { company: 'Nextcloud', logoId: 'nextcloud', countryCode: '', freeStorageGb: null,
      freeNote: 'self-hosted',
      protocols: [{ label: 'WebDAV', protocol: 'webdav', providerId: 'nextcloud', category: 'webdav' }] },
    { company: 'Seafile', logoId: 'seafile', countryCode: '', freeStorageGb: null,
      freeNote: 'self-hosted',
      protocols: [{ label: 'WebDAV', protocol: 'webdav', providerId: 'seafile', category: 'webdav' }] },
    { company: 'Immich', logoId: 'immich', countryCode: '', freeStorageGb: null,
      freeNote: 'self-hosted', healthCheckUrl: 'https://immich.app',
      protocols: [{ label: 'API', protocol: 'immich', category: 'media-services' }] },
    { company: 'SourceForge', logoId: 'sourceforge', countryCode: 'US', freeStorageGb: null,
      freeNote: 'OSS hosting',
      protocols: [{ label: 'SFTP', protocol: 'sftp', providerId: 'sourceforge', category: 'developer' }] },
    { company: 'GitHub', logoId: 'github', countryCode: 'US', freeStorageGb: null,
      freeNote: 'repo storage', healthCheckUrl: 'https://api.github.com',
      protocols: [{ label: 'API', protocol: 'github', category: 'developer' }] },
    { company: 'GitLab', logoId: 'gitlab', countryCode: 'US', freeStorageGb: null,
      freeNote: 'repo storage', healthCheckUrl: 'https://gitlab.com',
      protocols: [{ label: 'API', protocol: 'gitlab', category: 'developer' }] },
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
 * The three commercial buckets the list-view tier filter sorts companies into:
 * - `free`      : a free tier you can use without a credit card (Tab.digital, MEGA, ...).
 * - `free-card` : a genuine free allowance, but signup requires a card on file
 *                 (Amazon S3, Azure Blob, Yandex Object Storage). Kept OUT of paid.
 * - `paid`      : no free tier at all, only a trial and/or paid plans (Wasabi,
 *                 DigitalOcean Spaces, Hetzner, MEGA S4, Alibaba OSS, Tencent COS).
 */
export type CompanyTier = 'free' | 'free-card' | 'paid';

/** Classify a company into its commercial bucket for the tier filter. */
export function companyTier(c: CatalogCompany): CompanyTier {
    if (c.freeRequiresCard) return 'free-card';
    return hasFreeTier(c) ? 'free' : 'paid';
}

/** True when any of the company's connection methods belongs to `category`. */
export function companyInCategory(c: CatalogCompany, category: CatalogCategoryId): boolean {
    return c.protocols.some(p => p.category === category);
}

/**
 * The connection method launched when the user clicks the company row inside a
 * given category: the first protocol whose category matches, else the company
 * default (`protocols[0]`). In the virtual "All" view nothing matches, so the
 * default is used: a row click in the S3 category opens the Filen S3 form, the
 * same row in the WebDAV category opens the WebDAV form.
 */
export function companyLaunchProtocol(c: CatalogCompany, category: CatalogCategoryId | 'all'): CatalogProtocolRef {
    return c.protocols.find(p => p.category === category) ?? c.protocols[0];
}

/**
 * Principal storage regions per company, as distinct ISO 3166-1 alpha-2
 * country codes (plus 'EU' for pan-EU multi-region and 'global' for
 * decentralized / self-hosted). Curated to the principal regions, NOT
 * exhaustive: only providers with a meaningful multi-region footprint (mostly
 * S3) are listed; everything else falls back to the single HQ country.
 *
 * Verified against the providers' official region/endpoint pages (2026-06).
 * Storj / Cloudflare R2 / MinIO stay 'global' (decentralized, automatic
 * placement, or self-hosted: no per-country region list applies). 'mega' lists
 * actual storage regions (EU + Canada); the company `countryCode` keeps 'NZ' as
 * brand origin. Re-verify before quoting, region footprints drift.
 */
const REGIONS_BY_LOGO: Record<string, string[]> = {
    'amazon-s3': ['US', 'IE', 'DE', 'GB', 'FR', 'SG', 'JP', 'IN', 'BR', 'AU', 'CA', 'KR'],
    'wasabi': ['US', 'NL', 'JP', 'AU', 'CA', 'SG', 'DE', 'GB', 'FR', 'IT'],
    'storj': ['global'],
    'cloudflare-r2': ['global'],
    'idrive-e2': ['US', 'IE', 'DE', 'CA', 'GB', 'FR', 'SG', 'JP'],
    'oracle-cloud': ['US', 'DE', 'GB', 'JP', 'IN', 'AU', 'BR', 'CA'],
    'digitalocean-spaces': ['US', 'NL', 'SG', 'IN', 'DE', 'CA', 'GB', 'AU'],
    'alibaba-oss': ['CN', 'SG', 'US', 'DE', 'JP', 'KR'],
    'tencent-cos': ['CN', 'SG', 'US', 'DE', 'JP', 'KR'],
    'google-cloud-storage': ['US', 'EU', 'SG', 'JP', 'AU', 'BR', 'IN'],
    'minio': ['global'],
    'backblaze': ['US', 'NL', 'CA'],
    'mega': ['EU', 'CA'],
    'mega-s4': ['NL', 'LU', 'CA'],
    'yandex-storage': ['RU'],
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

/** One connection method as projected into the CLI catalog JSON. */
export interface CliCatalogProtocol {
    label: CatalogProtocol;
    protocol: ProviderType;
    providerId: string | null;
    paid: boolean;
    category: CatalogCategoryId;
    note: string | null;
}

/** One company as projected into the CLI catalog JSON. */
export interface CliCatalogCompany {
    company: string;
    logoId: string;
    country: string;
    freeGb: number | null;
    freeNote: string | null;
    freeRequiresCard: boolean;
    regions: string[];
    protocols: CliCatalogProtocol[];
}

/**
 * Project `PROVIDER_CATALOG` into the flat, dependency-free shape the Rust CLI
 * embeds via `include_str!` (see `scripts/gen-cli-catalog.ts`). Kept here so the
 * generator and the drift-guard test share a single serializer: the committed
 * `src-tauri/src/cli_catalog.json` must equal this projection.
 */
export function buildCliCatalog(): CliCatalogCompany[] {
    return PROVIDER_CATALOG.map(c => ({
        company: c.company,
        logoId: c.logoId,
        country: c.countryCode,
        freeGb: c.freeStorageGb,
        freeNote: c.freeNote ?? null,
        freeRequiresCard: !!c.freeRequiresCard,
        regions: companyRegions(c),
        protocols: c.protocols.map(p => ({
            label: p.label,
            protocol: p.protocol,
            providerId: p.providerId ?? null,
            paid: !!p.paid,
            category: p.category,
            note: p.note ?? null,
        })),
    }));
}

/** Escape a value for a single markdown table cell. */
function mdCell(s: string): string {
    return s.replace(/\|/g, '\\|');
}

/**
 * Project `PROVIDER_CATALOG` into the canonical markdown providers table
 * (issue #270 17104681: the cloud-drive tables drift between README, the site
 * and docs). The generator `scripts/gen-providers-table.ts` injects this block
 * between the `PROVIDERS-TABLE` anchors in `README.md` and `docs/PROVIDERS.md`;
 * a vitest drift guard (`providerCatalog.test.ts`) fails the gate if either copy
 * falls out of sync, so the SSOT stays the single source. Pure and
 * deterministic (sorted by company name, no Date/random), ASCII only (no
 * em-dash in user-facing output).
 */
export function buildProvidersMarkdown(): string {
    // Public docs never list dev-only providers (Blomp, Google Photos).
    const publicCatalog = PROVIDER_CATALOG.filter(c => !isDevOnlyProvider(c.logoId));
    const rows = [...publicCatalog]
        .sort((a, b) => a.company.toLowerCase().localeCompare(b.company.toLowerCase(), 'en'))
        .map(c => {
            const hq = c.countryCode ? c.countryCode.toUpperCase() : '-';
            let free: string;
            if (c.freeStorageGb != null) {
                free = `${c.freeStorageGb} GB`;
                if (c.freeNote) free += ` (${c.freeNote})`;
            } else {
                free = c.freeNote ?? '-';
            }
            const methods = c.protocols
                .map(p => (p.paid ? `${p.label}*` : p.label))
                .join(', ');
            return `| ${mdCell(c.company)} | ${hq} | ${mdCell(free)} | ${mdCell(methods)} |`;
        });

    const methodCount = publicCatalog.reduce((n, c) => n + c.protocols.length, 0);

    return [
        '<!-- Generated from src/components/providerCatalog.ts by `npm run gen:providers-table`. Do not edit by hand. -->',
        '',
        '| Provider | HQ | Free tier | Connection methods |',
        '| --- | --- | --- | --- |',
        ...rows,
        '',
        `<sub>${publicCatalog.length} providers, ${methodCount} connection methods. \`*\` marks a paid / credit-card-gated plan. HQ is the ISO 3166-1 alpha-2 of the company HQ (EU = pan-European). Free-tier sizes are approximate: verify with the provider.</sub>`,
    ].join('\n');
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

/**
 * providerId -> { company, ref } index. Lets callers that key by a saved
 * profile's providerId reach the connection-method metadata (paid flag, bridge
 * note) directly, without scanning PROVIDER_CATALOG per lookup. First match
 * wins, mirroring `catalogByLogo`.
 */
const catalogByProviderId = new Map<string, { company: CatalogCompany; ref: CatalogProtocolRef }>();
for (const company of PROVIDER_CATALOG) {
    for (const ref of company.protocols) {
        if (ref.providerId && !catalogByProviderId.has(ref.providerId)) {
            catalogByProviderId.set(ref.providerId, { company, ref });
        }
    }
}

/** Look up a catalog company + the matching connection method by providerId. */
export function findCatalogByProviderId(
    providerId: string,
): { company: CatalogCompany; ref: CatalogProtocolRef } | undefined {
    return catalogByProviderId.get(providerId);
}

/**
 * True when the providerId is a credit-card-gated (paid) connection method,
 * e.g. MEGA S4 or pCloud WebDAV. Mirrors the `*` paid marker in the providers
 * table. Unknown, self-hosted or generic protocols are not paid.
 */
export function isPaidProvider(providerId?: string): boolean {
    return !!(providerId && catalogByProviderId.get(providerId)?.ref.paid);
}

/**
 * True when the providerId is a local/desktop bridge: a connection that goes
 * through a locally-run daemon (e.g. MEGAcmd WebDAV, or the Filen desktop S3 /
 * WebDAV bridges). Detected via the `note` marker in the SSOT.
 */
export function isLocalBridgeProvider(providerId?: string): boolean {
    if (!providerId) return false;
    const note = catalogByProviderId.get(providerId)?.ref.note ?? '';
    return /bridge/i.test(note);
}

/**
 * HQ country (ISO 3166-1 alpha-2, or 'EU' for pan-European) of the company
 * behind a providerId, or undefined when the providerId is unknown / generic /
 * self-hosted. Drives the My Servers country filter.
 */
export function providerCountry(providerId?: string): string | undefined {
    if (!providerId) return undefined;
    return catalogByProviderId.get(providerId)?.company.countryCode || undefined;
}
