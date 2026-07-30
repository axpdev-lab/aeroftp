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
    /** Badge glyph key into `PROTOCOL_GLYPHS` — must stay a member of the closed
     *  `CatalogProtocol` enum. Display text prefers `labelOverride` when set
     *  (GUI badges, CLI catalog, providers markdown). */
    label: CatalogProtocol;
    /** Display override when the branded product name differs from the wire
     *  protocol: e.g. MEGA's paid S3 object storage is "S4" and FileLu's is "S5",
     *  both shown as "S4 (S3)" / "S5 (S3)" while `label` stays 'S3' (correct glyph,
     *  valid enum). Falls back to `label` when unset. Emitted into CLI catalog +
     *  providers markdown via `labelOverride ?? label`. */
    labelOverride?: string;
    // EF-17 (recommended-protocol highlight) — DEFERRED, blocked on the transfer
    // benchmark harness (#368). The intended shape is a `recommended?: boolean` flag
    // here (or a per-company recommended providerId) that the table/grid render as a
    // "fastest" marker. Do NOT add it until #368 lands real per-protocol throughput
    // data — hardcoding a pick now would be editorial guesswork. Stub only.
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
    /** Display name / product (the row identity). Shown in the Provider column. */
    company: string;
    /**
     * Parent company when the product name differs or the same parent ships
     * several products in AeroFTP (Ehud #274: optional Company column so
     * GitHub sorts near Microsoft OneDrive/Azure, kDrive under Infomaniak).
     * Omit when the product name already is the company brand.
     */
    parentCompany?: string;
    /** Key into `PROVIDER_LOGOS`. */
    logoId: string;
    /** ISO 3166-1 alpha-2 HQ code (or 'EU'), '' when unknown/self-hosted. */
    countryCode: string;
    /** Approximate consumer free-tier size in GB; null = no fixed-GB free tier. */
    freeStorageGb: number | null;
    /** Short qualifier when freeStorageGb is null or needs nuance. */
    freeNote?: string;
    /** The company HAS a genuine PERMANENT free allowance, but signup REQUIRES
     *  a credit card / payment method on file (e.g. Google Cloud Storage's
     *  always-free 5 GB-month, Oracle Cloud's 20 GB for the life of the
     *  account, Yandex Object Storage's 1 GB/month). This is the "third state"
     *  between a no-card free tier and a paid-only product: such companies are
     *  kept OUT of the paid bucket but marked distinctly. See `companyTier`.
     *
     *  Permanent is the load-bearing word. AWS S3 and Azure Blob were listed
     *  here for a long time on the strength of a 5 GB figure that is actually
     *  the 12-month new-account tier, not an always-free one — it expires with
     *  the account's first year. A time-limited allowance is a trial, and
     *  belongs in the paid bucket with `freeStorageGb: null`. */
    freeRequiresCard?: boolean;
    /** Reachability probe URL (global API endpoint). Omitted for
     *  per-account / self-hosted services, which then show no health dot. */
    healthCheckUrl?: string;
    /** Ordered connection methods. `protocols[0]` is the company default
     *  used when the user clicks the row (rather than a specific badge). */
    protocols: CatalogProtocolRef[];
    /** Extra lowercase search terms the Add Services search should match on top
     *  of the display name (e.g. 'aws' for Amazon Web Services). */
    searchAliases?: string[];
}

/** Sort/search key for the optional parent-company column: parent when set, else product. */
export function catalogParentKey(c: CatalogCompany): string {
    return (c.parentCompany || c.company).trim();
}

/**
 * Providers kept in the codebase for DEV work but hidden from the production UI
 * until they are ready: currently Google Photos (Photos API problem, fix pending
 * on our side). Single source for the "disabled in production, available in DEV"
 * rule applied across the provider lists (Discover catalog, Providers &
 * Integrations dialog, protocol grid).
 *
 * Blomp graduated to production once its Swift backend was live-verified: the
 * account listing is 403 by design, but swift.rs connects via the per-account
 * container named after the login email (discover_container fallback).
 */
export const DEV_ONLY_LOGO_IDS: ReadonlySet<string> = new Set(['googlephotos']);

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
          // EF-14/15: MEGA S4 (MEGA's paid, S3-compatible object storage) is one
          // method of the MEGA company, not a duplicate "MEGA S4 Object Storage"
          // row. Shown as "S4 (S3)" — MEGA's product name + the wire protocol.
          { label: 'S3', labelOverride: 'S4 (S3)', protocol: 's3', providerId: 'mega-s4', category: 'object-storage', paid: true, note: 'MEGA S4 object storage (Pro plan)' },
      ] },
    { company: 'Drime', logoId: 'drime', countryCode: 'FR', freeStorageGb: 20,
      healthCheckUrl: 'https://app.drime.cloud',
      protocols: [{ label: 'API', protocol: 'drime', category: 'cloud-storage' }] },
    { company: 'InfiniCloud', logoId: 'infinicloud', countryCode: 'JP', freeStorageGb: 20,
      protocols: [{ label: 'WebDAV', protocol: 'webdav', providerId: 'infinicloud', category: 'webdav' }] },
    { company: 'ImageKit', logoId: 'imagekit', countryCode: 'IN', freeStorageGb: 20,
      freeNote: 'media CDN', healthCheckUrl: 'https://api.imagekit.io',
      protocols: [{ label: 'API', protocol: 'imagekit', providerId: 'imagekit', category: 'media-services' }] },
    { company: 'Blomp', logoId: 'blomp', countryCode: 'US', freeStorageGb: 40,
      freeNote: '+40 GB per referral',
      protocols: [{ label: 'Swift', protocol: 'swift', providerId: 'blomp', category: 'cloud-storage' }] },
    { company: 'Storj', logoId: 'storj', countryCode: 'US', freeStorageGb: null,
      freeNote: '30-day trial',
      protocols: [{ label: 'S3', protocol: 's3', providerId: 'storj', category: 'object-storage', paid: true }] },
    { company: 'Google Drive', parentCompany: 'Google', logoId: 'googledrive', countryCode: 'US', freeStorageGb: 15,
      healthCheckUrl: 'https://www.googleapis.com',
      protocols: [{ label: 'OAuth', protocol: 'googledrive', category: 'cloud-storage' }] },
    { company: '4shared', logoId: '4shared', countryCode: 'VG', freeStorageGb: 15,
      healthCheckUrl: 'https://webdav.4shared.com',
      protocols: [
          { label: 'OAuth', protocol: 'fourshared', category: 'cloud-storage' },
          { label: 'WebDAV', protocol: 'webdav', providerId: '4shared-webdav', category: 'webdav' },
      ] },
    { company: 'kDrive', parentCompany: 'Infomaniak', logoId: 'kdrive', countryCode: 'CH', freeStorageGb: 15,
      healthCheckUrl: 'https://api.infomaniak.com',
      searchAliases: ['infomaniak'],
      protocols: [{ label: 'API', protocol: 'kdrive', category: 'cloud-storage' }] },
    { company: 'Box', logoId: 'box', countryCode: 'US', freeStorageGb: 10,
      healthCheckUrl: 'https://api.box.com',
      protocols: [{ label: 'OAuth', protocol: 'box', category: 'cloud-storage' }] },
    { company: 'pCloud Drive', parentCompany: 'pCloud', logoId: 'pcloud', countryCode: 'CH', freeStorageGb: 10,
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
    { company: 'Backblaze B2', parentCompany: 'Backblaze', logoId: 'backblaze', countryCode: 'US', freeStorageGb: 10,
      healthCheckUrl: 'https://api.backblazeb2.com',
      protocols: [
          { label: 'API', protocol: 'backblaze', providerId: 'backblaze-native', category: 'cloud-storage' },
          { label: 'S3', protocol: 's3', providerId: 'backblaze', category: 'object-storage' },
      ] },
    { company: 'IDrive e2', parentCompany: 'IDrive', logoId: 'idrive-e2', countryCode: 'US', freeStorageGb: null,
      freeNote: '7-day trial',
      protocols: [{ label: 'S3', protocol: 's3', providerId: 'idrive-e2', category: 'object-storage', paid: true }] },
    { company: 'Cloudflare R2', parentCompany: 'Cloudflare', logoId: 'cloudflare-r2', countryCode: 'US', freeStorageGb: 10,
      freeNote: 'egress-free, card req.', freeRequiresCard: true,
      protocols: [{ label: 'S3', protocol: 's3', providerId: 'cloudflare-r2', category: 'object-storage', paid: true }] },
    { company: 'Oracle Cloud', parentCompany: 'Oracle', logoId: 'oracle-cloud', countryCode: 'US', freeStorageGb: 20,
      freeNote: 'always-free, card req.', freeRequiresCard: true,
      protocols: [{ label: 'S3', protocol: 's3', providerId: 'oracle-cloud', category: 'object-storage', paid: true }] },
    { company: 'Microsoft OneDrive', parentCompany: 'Microsoft', logoId: 'onedrive', countryCode: 'US', freeStorageGb: 5,
      healthCheckUrl: 'https://graph.microsoft.com',
      protocols: [{ label: 'OAuth', protocol: 'onedrive', category: 'cloud-storage' }] },
    { company: 'Zoho WorkDrive', parentCompany: 'Zoho', logoId: 'zohoworkdrive', countryCode: 'IN', freeStorageGb: 5,
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
    { company: 'Yandex Disk', parentCompany: 'Yandex', logoId: 'yandexdisk', countryCode: 'RU', freeStorageGb: 5,
      healthCheckUrl: 'https://cloud-api.yandex.net',
      protocols: [
          { label: 'OAuth', protocol: 'yandexdisk', category: 'cloud-storage' },
          { label: 'WebDAV', protocol: 'webdav', providerId: 'yandexdisk-webdav', category: 'webdav', paid: true, note: 'Yandex 360 subscription' },
      ] },
    { company: 'Yandex Object Storage', parentCompany: 'Yandex', logoId: 'yandex-storage', countryCode: 'RU', freeStorageGb: 1,
      freeNote: 'always-free, card req.', freeRequiresCard: true,
      protocols: [{ label: 'S3', protocol: 's3', providerId: 'yandex-storage', category: 'object-storage', paid: true, note: 'Yandex Object Storage' }] },
    { company: 'Google Cloud Storage', parentCompany: 'Google', logoId: 'google-cloud-storage', countryCode: 'US', freeStorageGb: 5,
      freeNote: 'always-free, card req.', freeRequiresCard: true, healthCheckUrl: 'https://storage.googleapis.com',
      protocols: [{ label: 'S3', protocol: 's3', providerId: 'google-cloud-storage', category: 'object-storage', paid: true }] },
    { company: 'CloudMe', logoId: 'cloudme', countryCode: 'SE', freeStorageGb: 3,
      protocols: [{ label: 'WebDAV', protocol: 'webdav', providerId: 'cloudme', category: 'webdav' }] },
    { company: 'Uploadcare', logoId: 'uploadcare', countryCode: 'US', freeStorageGb: 1,
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
          // EF-13: FileLu FTP was the one method missing from Add Service (it already
          // exposes API / WebDAV / S3). Now surfaced as a badge + a Protocols grid tile.
          { label: 'FTP', protocol: 'ftp', providerId: 'filelu-ftp', category: 'protocols' },
          { label: 'WebDAV', protocol: 'webdav', providerId: 'filelu-webdav', category: 'webdav' },
          // EF-14/15: FileLu's S3 object storage is branded "S5" — shown as "S5 (S3)".
          { label: 'S3', labelOverride: 'S5 (S3)', protocol: 's3', providerId: 'filelu-s3', category: 'object-storage' },
      ] },
    { company: 'DriveHQ', logoId: 'drivehq', countryCode: 'US', freeStorageGb: 5,
      protocols: [{ label: 'WebDAV', protocol: 'webdav', providerId: 'drivehq', category: 'webdav' }] },
    { company: 'Jianguoyun', logoId: 'jianguoyun', countryCode: 'CN', freeStorageGb: 1,
      freeNote: 'monthly traffic cap',
      protocols: [{ label: 'WebDAV', protocol: 'webdav', providerId: 'jianguoyun', category: 'webdav' }] },
    { company: 'Felicloud', logoId: 'felicloud', countryCode: 'EU', freeStorageGb: 10,
      freeNote: 'Nextcloud host',
      protocols: [{ label: 'WebDAV', protocol: 'webdav', providerId: 'felicloud', category: 'webdav' }] },
    { company: 'Cloudinary', logoId: 'cloudinary', countryCode: 'US', freeStorageGb: null,
      freeNote: 'credit-based', healthCheckUrl: 'https://api.cloudinary.com',
      protocols: [{ label: 'API', protocol: 'cloudinary', providerId: 'cloudinary', category: 'media-services' }] },
    { company: 'Amazon Web Services (AWS)', parentCompany: 'Amazon', logoId: 'amazon-s3', countryCode: 'US', freeStorageGb: null,
      freeNote: '12-month trial', healthCheckUrl: 'https://s3.amazonaws.com',
      searchAliases: ['aws', 'amazon web services', 's3', 'amazon'],
      protocols: [{ label: 'S3', protocol: 's3', providerId: 'amazon-s3', category: 'object-storage', paid: true }] },
    { company: 'Wasabi', logoId: 'wasabi', countryCode: 'US', freeStorageGb: null,
      freeNote: '30-day trial',
      protocols: [{ label: 'S3', protocol: 's3', providerId: 'wasabi', category: 'object-storage', paid: true }] },
    { company: 'DigitalOcean Spaces', parentCompany: 'DigitalOcean', logoId: 'digitalocean-spaces', countryCode: 'US', freeStorageGb: null,
      freeNote: 'paid plan',
      protocols: [{ label: 'S3', protocol: 's3', providerId: 'digitalocean-spaces', category: 'object-storage', paid: true }] },
    { company: 'Alibaba OSS', parentCompany: 'Alibaba', logoId: 'alibaba-oss', countryCode: 'CN', freeStorageGb: 5,
      freeNote: 'overseas only, card req.', freeRequiresCard: true,
      protocols: [{ label: 'S3', protocol: 's3', providerId: 'alibaba-oss', category: 'object-storage', paid: true }] },
    { company: 'Tencent COS', parentCompany: 'Tencent', logoId: 'tencent-cos', countryCode: 'CN', freeStorageGb: null,
      freeNote: '6-month trial',
      protocols: [{ label: 'S3', protocol: 's3', providerId: 'tencent-cos', category: 'object-storage', paid: true }] },
    { company: 'Microsoft Azure Blob', parentCompany: 'Microsoft', logoId: 'azure', countryCode: 'US', freeStorageGb: null,
      freeNote: '12-month trial', healthCheckUrl: 'https://login.microsoftonline.com/common/v2.0/.well-known/openid-configuration',
      protocols: [{ label: 'Blob', protocol: 'azure', category: 'object-storage', paid: true }] },
    { company: 'Hetzner Storage Box', parentCompany: 'Hetzner', logoId: 'hetzner-storage-box', countryCode: 'DE', freeStorageGb: null,
      freeNote: 'paid plan',
      protocols: [{ label: 'SFTP', protocol: 'sftp', providerId: 'hetzner-storage-box', category: 'protocols', paid: true }] },
    { company: 'TAB.DIGITAL', logoId: 'tabdigital', countryCode: 'EU', freeStorageGb: 8,
      freeNote: 'managed Nextcloud',
      protocols: [{ label: 'WebDAV', protocol: 'webdav', providerId: 'tabdigital', category: 'webdav' }] },
    { company: 'PixelUnion', logoId: 'pixelunion', countryCode: 'EU', freeStorageGb: 16,
      freeNote: 'managed Immich', healthCheckUrl: 'https://pixelunion.eu',
      protocols: [{ label: 'API', protocol: 'immich', providerId: 'pixelunion', category: 'media-services' }] },
    { company: 'MinIO', logoId: 'minio', countryCode: '', freeStorageGb: null,
      freeNote: 'self-hosted',
      protocols: [{ label: 'S3', protocol: 's3', providerId: 'minio', category: 'object-storage' }] },
    // EF-13: real catalog rows for S3Drive + Quotaless. Both are already in the grid
    // (registry S3/WebDAV presets), they were missing only from the company table.
    { company: 'S3Drive', logoId: 's3drive', countryCode: '', freeStorageGb: 12,
      freeNote: 'via Storj', healthCheckUrl: 'https://storage.kapsa.io',
      protocols: [{ label: 'S3', protocol: 's3', providerId: 's3drive', category: 'object-storage' }] },
    { company: 'Quotaless', logoId: 'quotaless-s3', countryCode: '', freeStorageGb: null,
      freeNote: 'trial / invite', healthCheckUrl: 'https://io.quotaless.cloud:8000',
      protocols: [
          // Trial / restricted signup (no fixed free-GB tier), mirroring how the
          // Discover grid already sorts Quotaless to the end. Marked paid so it lands
          // in the paid tier like Storj / Wasabi rather than the no-card free bucket.
          { label: 'S3', protocol: 's3', providerId: 'quotaless-s3', category: 'object-storage', paid: true },
          { label: 'WebDAV', protocol: 'webdav', providerId: 'quotaless-webdav', category: 'webdav', paid: true },
      ] },
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
    { company: 'GitHub', parentCompany: 'Microsoft', logoId: 'github', countryCode: 'US', freeStorageGb: null,
      freeNote: 'repo storage', healthCheckUrl: 'https://api.github.com',
      searchAliases: ['microsoft github'],
      protocols: [{ label: 'API', protocol: 'github', category: 'developer' }] },
    { company: 'GitLab', logoId: 'gitlab', countryCode: 'US', freeStorageGb: null,
      freeNote: 'repo storage', healthCheckUrl: 'https://gitlab.com',
      protocols: [{ label: 'API', protocol: 'gitlab', category: 'developer' }] },
];

/** Sum of approximate free GB across the given companies (footer total). */
export function totalFreeStorageGb(companies: CatalogCompany[]): number {
    return companies.reduce((sum, c) => sum + (c.freeStorageGb ?? 0), 0);
}

/**
 * Presentation data for the logo grid at the top of README.md: the one thing
 * the catalog does not already carry, because a 36px tile needs an icon file,
 * a documentation link and a label short enough to sit under it.
 *
 * The grid used to be hand-maintained and drifted from the catalog exactly the
 * way a reporter found it (#347 17790873): Zoho WorkDrive appeared as "Zoho",
 * MEGA appeared twice because its paid S4 object storage had its own tile, and
 * S3Drive was missing entirely. It is generated from this list now, and
 * `providerCatalog.test.ts` fails unless the list and the public catalog name
 * exactly the same companies, once each, so a new provider cannot be added to
 * one and forgotten in the other.
 *
 * `label` defaults to the catalog company name and is set only where the full
 * name does not fit under a tile; the link carries the full name as its title,
 * so an abbreviation is never the only thing on offer. The order is editorial
 * and categorical (cloud drives, then object storage, then WebDAV, developer,
 * media last), which is the order the website and docs.aeroftp.app mirror.
 */
export interface ProviderGridTile {
    /** `logoId` of the catalog company this tile stands for. */
    logoId: string;
    /** File under `public/icons/providers/grid/`. */
    icon: string;
    /** Path under `https://docs.aeroftp.app/`. */
    docsPath: string;
    /** Short tile caption; defaults to the catalog company name. */
    label?: string;
}

export const PROVIDER_GRID: readonly ProviderGridTile[] = [
    // Cloud drives (OAuth / native API)
    { logoId: 'googledrive', icon: 'Google_Drive.png', docsPath: 'providers/google-drive' },
    { logoId: 'onedrive', icon: 'onedrive.png', docsPath: 'providers/onedrive', label: 'OneDrive' },
    { logoId: 'dropbox', icon: 'dropbox.png', docsPath: 'providers/dropbox' },
    { logoId: 'mega', icon: 'mega.png', docsPath: 'providers/mega' },
    { logoId: 'box', icon: 'box.png', docsPath: 'providers/box' },
    { logoId: 'pcloud', icon: 'pcloud.png', docsPath: 'providers/pcloud' },
    { logoId: 'filen', icon: 'filen.png', docsPath: 'providers/filen' },
    { logoId: 'internxt', icon: 'internxt.png', docsPath: 'providers/internxt' },
    { logoId: 'zohoworkdrive', icon: 'ZohoWorkDrive.png', docsPath: 'providers/zoho' },
    { logoId: 'koofr', icon: 'Koofr.png', docsPath: 'providers/koofr' },
    { logoId: 'kdrive', icon: 'kdrive.png', docsPath: 'providers/kdrive' },
    { logoId: 'jottacloud', icon: 'jottacloud.png', docsPath: 'providers/jottacloud' },
    { logoId: 'drime', icon: 'drime.png', docsPath: 'providers/drime' },
    { logoId: 'filelu', icon: 'filelu.png', docsPath: 'providers/filelu' },
    { logoId: 'opendrive', icon: 'opendrive.png', docsPath: 'providers/opendrive' },
    { logoId: 'yandexdisk', icon: 'YandexDisk.png', docsPath: 'providers/yandex' },
    { logoId: '4shared', icon: '4shared.png', docsPath: 'providers/4shared' },
    { logoId: 'backblaze', icon: 'backblaze.png', docsPath: 'providers/backblaze-b2' },
    { logoId: 'blomp', icon: 'blomp.png', docsPath: 'providers/blomp' },
    // Object storage (S3-compatible)
    { logoId: 'amazon-s3', icon: 'Amazon_Web_Services.png', docsPath: 'providers/aws-s3', label: 'AWS S3' },
    { logoId: 'google-cloud-storage', icon: 'googlecloud.png', docsPath: 'providers/google-cloud-storage' },
    { logoId: 'azure', icon: 'azure.png', docsPath: 'protocols/azure', label: 'Azure Blob' },
    { logoId: 'wasabi', icon: 'wasabi.png', docsPath: 'providers/wasabi' },
    { logoId: 'cloudflare-r2', icon: 'cloudfare.png', docsPath: 'providers/cloudflare-r2' },
    { logoId: 'digitalocean-spaces', icon: 'digitalocean.png', docsPath: 'providers/digitalocean-spaces' },
    { logoId: 'tencent-cos', icon: 'tencent.png', docsPath: 'providers/tencent-cloud-cos' },
    { logoId: 'alibaba-oss', icon: 'alibabacloud.png', docsPath: 'providers/alibaba-cloud-oss' },
    { logoId: 'oracle-cloud', icon: 'oracle_cloud.png', docsPath: 'providers/oracle-cloud' },
    { logoId: 'storj', icon: 'storj.png', docsPath: 'providers/storj' },
    { logoId: 'idrive-e2', icon: 'idrive_e2.png', docsPath: 'providers/idrive-e2' },
    { logoId: 'minio', icon: 'minio.png', docsPath: 'providers/minio' },
    { logoId: 'yandex-storage', icon: 'yandexcloud.png', docsPath: 'providers/yandex-object-storage' },
    { logoId: 's3drive', icon: 's3drive.png', docsPath: 'providers/s3drive' },
    { logoId: 'quotaless-s3', icon: 'quotaless.png', docsPath: 'providers/quotaless' },
    // WebDAV
    { logoId: 'nextcloud', icon: 'nextcloud.png', docsPath: 'providers/nextcloud' },
    { logoId: 'felicloud', icon: 'felicloud.png', docsPath: 'providers/felicloud' },
    { logoId: 'tabdigital', icon: 'tabdigital.png', docsPath: 'providers/tabdigital' },
    { logoId: 'cloudme', icon: 'cloudme.png', docsPath: 'providers/cloudme' },
    { logoId: 'infinicloud', icon: 'infiniCloud.png', docsPath: 'providers/infinicloud' },
    { logoId: 'jianguoyun', icon: 'jianguoyun.png', docsPath: 'providers/jianguoyun' },
    { logoId: 'seafile', icon: 'seafile.png', docsPath: 'providers/seafile' },
    { logoId: 'drivehq', icon: 'drivehq.png', docsPath: 'providers/drivehq' },
    // SFTP
    { logoId: 'hetzner-storage-box', icon: 'hetzner.png', docsPath: 'providers/hetzner-storage-box' },
    // Developer
    { logoId: 'github', icon: 'github.png', docsPath: 'providers/github' },
    { logoId: 'gitlab', icon: 'gitlab.png', docsPath: 'providers/gitlab' },
    { logoId: 'sourceforge', icon: 'sourceforge.png', docsPath: 'providers/sourceforge' },
    // Media services
    { logoId: 'immich', icon: 'immich.png', docsPath: 'providers/immich' },
    { logoId: 'pixelunion', icon: 'pixelunion.png', docsPath: 'providers/pixelunion' },
    { logoId: 'imagekit', icon: 'imagekit.png', docsPath: 'providers/imagekit' },
    { logoId: 'uploadcare', icon: 'uploadcare.png', docsPath: 'providers/uploadcare' },
    { logoId: 'cloudinary', icon: 'cloudinary.png', docsPath: 'providers/cloudinary' },
];

/** Tiles per row in the generated README grid. */
const GRID_COLUMNS = 9;

/**
 * Render the README logo grid from [`PROVIDER_GRID`] + the catalog, so the
 * tiles carry the catalog's own names and no company can appear twice.
 * Injected between the `PROVIDERS-GRID` anchors by `npm run gen:providers-table`.
 */
export function buildProviderGridHtml(): string {
    const byLogoId = new Map(PROVIDER_CATALOG.map(c => [c.logoId, c]));
    const cells = PROVIDER_GRID.map(tile => {
        const company = byLogoId.get(tile.logoId);
        if (!company) {
            throw new Error(`PROVIDER_GRID references unknown logoId "${tile.logoId}"`);
        }
        const label = tile.label ?? company.company;
        const title = label === company.company ? '' : ` title="${company.company}"`;
        return `    <td align="center" width="80"><a href="https://docs.aeroftp.app/${tile.docsPath}"${title}>`
            + `<img src="public/icons/providers/grid/${tile.icon}" width="36" /></a>`
            + `<br><sub>${label}</sub></td>`;
    });

    const rows: string[] = [];
    for (let i = 0; i < cells.length; i += GRID_COLUMNS) {
        rows.push('  <tr>', ...cells.slice(i, i + GRID_COLUMNS), '  </tr>');
    }

    return [
        '<!-- Generated from PROVIDER_GRID in src/components/providerCatalog.ts by `npm run gen:providers-table`. Do not edit by hand. -->',
        '<table align="center">',
        ...rows,
        '</table>',
    ].join('\n');
}

/**
 * Canonical badge display order, independent of the array order in the catalog
 * (which is authored default-first, since `protocols[0]` is the row launch
 * target). Ehud (#274) asked for a consistent S3-vs-WebDAV ordering: FileLu
 * showed WebDAV before S3 while Filen showed S3 before WebDAV. We follow the
 * order FileLu already has (native API/OAuth, then WebDAV, then S3/object),
 * which keeps WebDAV in the middle for every company. Ranks are relative;
 * anything unranked (media, developer, SFTP) sorts first with the native
 * method. A stable sort preserves the authored order within a rank.
 */
const BADGE_CATEGORY_RANK: Partial<Record<CatalogCategoryId, number>> = {
    webdav: 1,
    'object-storage': 2,
};
const badgeRank = (p: CatalogProtocolRef): number => BADGE_CATEGORY_RANK[p.category] ?? 0;
const byBadgeRank = (a: CatalogProtocolRef, b: CatalogProtocolRef): number => badgeRank(a) - badgeRank(b);

/** Connection methods available on the free tier (blue badges). */
export function freeProtocols(c: CatalogCompany): CatalogProtocolRef[] {
    return c.protocols.filter(p => !p.paid).sort(byBadgeRank);
}

/** Connection methods gated behind a paid plan / credit card (orange badges). */
export function paidProtocols(c: CatalogCompany): CatalogProtocolRef[] {
    return c.protocols.filter(p => p.paid).sort(byBadgeRank);
}

/** True when the company has at least one free-tier connection method. */
export function hasFreeTier(c: CatalogCompany): boolean {
    return c.protocols.some(p => !p.paid);
}

/**
 * The three commercial buckets the list-view tier filter sorts companies into:
 * - `free`      : a free tier you can use without a credit card (TAB.DIGITAL, MEGA, ...).
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

/**
 * Tier of a company *as seen inside a specific category tab*. A company that is
 * free-to-enter overall can still be paid-only within one category: MEGA is a
 * free cloud drive (`cloud-storage`) yet its object-storage product (S4/S3) is
 * paid-only, so under the S3 tab MEGA is `paid`, not `free`. The list-view tier
 * filter (Free tier / Paid) must classify by the protocols in the ACTIVE
 * category, else a provider that is paid-only in this category is unreachable
 * from Paid and wrongly listed under Free tier. `'all'` (or a category the
 * company has no protocol in) falls back to the whole-company [`companyTier`].
 */
export function companyTierInCategory(
    c: CatalogCompany,
    category: CatalogCategoryId | 'all',
): CompanyTier {
    if (category === 'all') return companyTier(c);
    const inCat = c.protocols.filter(p => p.category === category);
    if (inCat.length === 0) return companyTier(c);
    // A card-gated free tier keeps the company in the 'free-card' bucket even
    // per-category: its free allowance IS this category's product (e.g. Google
    // Cloud Storage's always-free 5 GB is that same paid-flagged S3 method;
    // AWS used to be the example here, until its S3 allowance turned out to be
    // the 12-month tier rather than an always-free one). Mirrors companyTier's
    // precedence and keeps free-card OUT of paid. Only companies with NO free
    // tier at all fall through to the per-category paid/free split below.
    if (c.freeRequiresCard) return 'free-card';
    return inCat.some(p => !p.paid) ? 'free' : 'paid';
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
    // MEGA S4 (mega-s4) was merged into the MEGA row (EF-14/15); its EU/CA storage
    // footprint is already covered by MEGA's 'EU','CA' above, so no separate entry.
    // S3Drive rides Storj's decentralized network (automatic global placement).
    's3drive': ['global'],
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
    /** Display label: `labelOverride ?? label` (may be a branded string such as
     *  "S4 (S3)"; wire protocol stays in `protocol`). */
    label: string;
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
            label: p.labelOverride ?? p.label,
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
    // Public docs never list dev-only providers (currently Google Photos).
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
                .map(p => (p.paid ? `${p.labelOverride ?? p.label}*` : (p.labelOverride ?? p.label)))
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
    // NB: intentionally NO 'mega-s4' -> 'mega' alias. MEGA S4 merged into the MEGA
    // row (EF-14/15), but the S4 method is PAID while MEGA the company has a free
    // tier, so a company-level (logo) alias would mislabel S4 as free wherever
    // findCatalogByLogo drives a free/paid badge (e.g. ProvidersDialog). Paid status
    // for the S4 method resolves correctly through findCatalogByProviderId('mega-s4').
    'filelu-s3': 'filelu',
    'filelu-webdav': 'filelu',
    'filelu-ftp': 'filelu',
    'koofr-webdav': 'koofr',
    // Quotaless catalog row is keyed by its S3 logoId; map the WebDAV id onto it.
    'quotaless-webdav': 'quotaless-s3',
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
