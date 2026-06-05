// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Company-centric provider catalog (issue #224, T-ADD-SERVICE-TABLE).
 *
 * Single source of truth for the "Plans & Regions" view: one row per
 * COMPANY, not per protocol. The rest of the app stays protocol-centric
 * (`discoverData.ts`, `registry.ts`); this layer re-projects the same
 * providers onto the company axis with the structured metadata Ehud's
 * table needs (country, free storage, free-tier vs paid-only protocols).
 *
 * Curation notes (be honest, this is editorial and drifts):
 * - `freeStorageGb` is the consumer free-tier size in GB, APPROXIMATE.
 *   Sourced from the human-readable free-tier strings already curated in
 *   `discoverData.ts`. `null` = no fixed-GB free tier (credit-based,
 *   self-hosted, dev, or paid-only). Verify with the provider before
 *   quoting; the footer total is labelled "approx".
 * - `countryCode` is the ISO 3166-1 alpha-2 of the company HQ, rendered as
 *   a text code (NOT an emoji flag: regional-indicator emoji break on
 *   Windows). `''` when genuinely unknown / self-hosted.
 * - `freeProtocols` / `paidProtocols` classify each connection method as
 *   available on the free tier (blue badge) vs paid/credit-card-gated
 *   (orange badge). Also editorial; classification ages, label carefully.
 */

/** Protocol/connection-method label shown as a badge. */
export type CatalogProtocol =
    | 'API'
    | 'OAuth'
    | 'WebDAV'
    | 'S3'
    | 'FTP'
    | 'FTPS'
    | 'SFTP'
    | 'MEGAcmd'
    | 'Swift';

export interface CatalogCompany {
    /** Display name (the row identity). */
    company: string;
    /** Key into `PROVIDER_LOGOS`. */
    logoId: string;
    /** ISO 3166-1 alpha-2 HQ code, '' when unknown/self-hosted. */
    countryCode: string;
    /** Approximate consumer free-tier size in GB; null = no fixed-GB free tier. */
    freeStorageGb: number | null;
    /** Short qualifier when freeStorageGb is null or needs nuance. */
    freeNote?: string;
    /** Protocols usable on the free tier (no credit card). */
    freeProtocols: CatalogProtocol[];
    /** Protocols gated behind a paid plan / credit card. */
    paidProtocols: CatalogProtocol[];
}

/**
 * The catalog. Ordered by descending free storage (the table re-sorts
 * anyway). Storage figures mirror `discoverData.ts` free-tier strings.
 */
export const PROVIDER_CATALOG: CatalogCompany[] = [
    { company: 'MEGA', logoId: 'mega', countryCode: 'NZ', freeStorageGb: 20,
      freeProtocols: ['API', 'MEGAcmd', 'WebDAV'], paidProtocols: ['S3'] },
    { company: 'Drime', logoId: 'drime', countryCode: 'FR', freeStorageGb: 20,
      freeProtocols: ['API'], paidProtocols: [] },
    { company: 'Google Drive', logoId: 'googledrive', countryCode: 'US', freeStorageGb: 15,
      freeProtocols: ['OAuth'], paidProtocols: [] },
    { company: '4shared', logoId: '4shared', countryCode: 'VG', freeStorageGb: 15,
      freeProtocols: ['OAuth', 'WebDAV'], paidProtocols: ['FTP', 'SFTP'] },
    { company: 'kDrive', logoId: 'kdrive', countryCode: 'CH', freeStorageGb: 15,
      freeProtocols: ['API'], paidProtocols: ['WebDAV'] },
    { company: 'ImageKit', logoId: 'imagekit', countryCode: 'IN', freeStorageGb: 20,
      freeNote: 'media CDN', freeProtocols: ['API'], paidProtocols: [] },
    { company: 'Box', logoId: 'box', countryCode: 'US', freeStorageGb: 10,
      freeProtocols: ['OAuth'], paidProtocols: [] },
    { company: 'pCloud', logoId: 'pcloud', countryCode: 'CH', freeStorageGb: 10,
      freeProtocols: ['OAuth'], paidProtocols: ['WebDAV'] },
    { company: 'Filen', logoId: 'filen', countryCode: 'DE', freeStorageGb: 10,
      freeNote: 'E2E', freeProtocols: ['API'], paidProtocols: ['S3', 'WebDAV'] },
    { company: 'Koofr', logoId: 'koofr', countryCode: 'SI', freeStorageGb: 10,
      freeProtocols: ['API', 'WebDAV'], paidProtocols: [] },
    { company: 'Backblaze B2', logoId: 'backblaze', countryCode: 'US', freeStorageGb: 10,
      freeProtocols: ['API', 'S3'], paidProtocols: [] },
    { company: 'OneDrive', logoId: 'onedrive', countryCode: 'US', freeStorageGb: 5,
      freeProtocols: ['OAuth'], paidProtocols: [] },
    { company: 'Zoho WorkDrive', logoId: 'zohoworkdrive', countryCode: 'IN', freeStorageGb: 5,
      freeProtocols: ['OAuth'], paidProtocols: [] },
    { company: 'Jottacloud', logoId: 'jottacloud', countryCode: 'NO', freeStorageGb: 5,
      freeProtocols: ['API'], paidProtocols: [] },
    { company: 'OpenDrive', logoId: 'opendrive', countryCode: 'US', freeStorageGb: 5,
      freeProtocols: ['API', 'WebDAV'], paidProtocols: [] },
    { company: 'Yandex Disk', logoId: 'yandexdisk', countryCode: 'RU', freeStorageGb: 5,
      freeProtocols: ['OAuth', 'WebDAV'], paidProtocols: ['S3'] },
    { company: 'Uploadcare', logoId: 'uploadcare', countryCode: 'US', freeStorageGb: 3,
      freeNote: 'media CDN', freeProtocols: ['API'], paidProtocols: [] },
    { company: 'Dropbox', logoId: 'dropbox', countryCode: 'US', freeStorageGb: 2,
      freeProtocols: ['OAuth'], paidProtocols: [] },
    { company: 'Internxt', logoId: 'internxt', countryCode: 'ES', freeStorageGb: 1,
      freeNote: 'E2E', freeProtocols: ['API'], paidProtocols: ['S3', 'WebDAV'] },
    { company: 'FileLu', logoId: 'filelu', countryCode: 'US', freeStorageGb: 1,
      freeProtocols: ['API', 'WebDAV', 'S3'], paidProtocols: [] },
    { company: 'Cloudinary', logoId: 'cloudinary', countryCode: 'US', freeStorageGb: null,
      freeNote: 'credit-based', freeProtocols: ['API'], paidProtocols: [] },
    { company: 'Amazon S3', logoId: 'amazon-s3', countryCode: 'US', freeStorageGb: null,
      freeNote: '5 GB 12-month trial', freeProtocols: [], paidProtocols: ['S3'] },
    { company: 'InfiniCloud', logoId: 'infinicloud', countryCode: 'JP', freeStorageGb: 20,
      freeProtocols: ['WebDAV'], paidProtocols: [] },
    { company: 'Immich', logoId: 'immich', countryCode: '', freeStorageGb: null,
      freeNote: 'self-hosted', freeProtocols: ['API'], paidProtocols: [] },
    { company: 'Nextcloud', logoId: 'nextcloud', countryCode: '', freeStorageGb: null,
      freeNote: 'self-hosted', freeProtocols: ['WebDAV'], paidProtocols: [] },
    { company: 'GitHub', logoId: 'github', countryCode: 'US', freeStorageGb: null,
      freeNote: 'repo storage', freeProtocols: ['API'], paidProtocols: [] },
    { company: 'GitLab', logoId: 'gitlab', countryCode: 'US', freeStorageGb: null,
      freeNote: 'repo storage', freeProtocols: ['API'], paidProtocols: [] },
];

/** Sum of approximate free GB across the given companies (footer total). */
export function totalFreeStorageGb(companies: CatalogCompany[]): number {
    return companies.reduce((sum, c) => sum + (c.freeStorageGb ?? 0), 0);
}

/** All distinct protocol labels a company exposes (free + paid). */
export function companyProtocols(c: CatalogCompany): CatalogProtocol[] {
    return [...c.freeProtocols, ...c.paidProtocols];
}
