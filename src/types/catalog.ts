/**
 * Types for the IntroHub Service Catalog system.
 * Used by the redesigned intro page (Tab Hub layout).
 */

import { isLocalBridgeProvider } from '../components/providerCatalog';

export interface ServiceCatalogCategory {
    id: CatalogCategoryId;
    labelKey: string;
    icon: string; // Lucide icon name
    sortOrder: number;
}

export type CatalogCategoryId =
    | 'protocols'
    | 'object-storage'
    | 'webdav'
    | 'cloud-storage'
    | 'media-services'
    | 'developer';

export type MyServersViewMode = 'grid' | 'list';
export type MyServersSortBy = 'lastConnected' | 'name' | 'protocol';
export type MyServersFilterBy =
    | 'all'
    | 'ftp'
    | 's3'
    | 'webdav'
    | 'cloud'
    | 'media'
    | 'dev'
    | 'local-bridge'
    | 'peer'
    | 'favorites';

/** Filter chip definition for My Servers toolbar */
export interface FilterChip {
    id: MyServersFilterBy;
    labelKey: string;
    matchFn: (protocol: string, providerId?: string) => boolean;
}

const DEV_PROTOCOLS = ['github', 'gitlab'];
/** Provider IDs that are developer services even though they use a base protocol (e.g. SFTP) */
const DEV_PROVIDER_IDS = ['sourceforge'];

const MEDIA_PROTOCOLS = ['immich', 'googlephotos', 'imagekit', 'uploadcare', 'cloudinary'];

const isDevService = (protocol: string, providerId?: string): boolean =>
    DEV_PROTOCOLS.includes(protocol) || DEV_PROVIDER_IDS.includes(providerId || '');

const isMediaService = (protocol: string): boolean =>
    MEDIA_PROTOCOLS.includes(protocol);

export const FILTER_CHIPS: FilterChip[] = [
    { id: 'all', labelKey: 'introHub.filter.all', matchFn: () => true },
    { id: 'ftp', labelKey: 'introHub.filter.ftpSftp', matchFn: (p, pid) => ['ftp', 'ftps', 'sftp'].includes(p) && !isDevService(p, pid) },
    { id: 's3', labelKey: 'introHub.filter.s3', matchFn: (p) => p === 's3' || p === 'azure' },
    { id: 'webdav', labelKey: 'introHub.filter.webdav', matchFn: (p) => p === 'webdav' },
    { id: 'cloud', labelKey: 'introHub.filter.cloud', matchFn: (p, pid) => !['ftp', 'ftps', 'sftp', 'webdav', 's3', 'azure', 'peer', ...DEV_PROTOCOLS, ...MEDIA_PROTOCOLS].includes(p) && !isDevService(p, pid) && !isMediaService(p) },
    { id: 'media', labelKey: 'introHub.filter.media', matchFn: (p) => isMediaService(p) },
    { id: 'dev', labelKey: 'introHub.filter.dev', matchFn: (p, pid) => isDevService(p, pid) },
    { id: 'local-bridge', labelKey: 'introHub.filter.localBridge', matchFn: (_p, pid) => isLocalBridgeProvider(pid) },
    // AeroShare friends (protocol "peer"). REUSEs the aeroShare.feature label
    // ("AeroShare") so no new i18n key is needed. Flag-gated: the toolbar only
    // renders this chip when the AeroShare flag is on (the chip vanishes with
    // the rest of the friend surfaces when off).
    { id: 'peer', labelKey: 'aeroShare.feature', matchFn: (p) => p === 'peer' },
    { id: 'favorites', labelKey: 'introHub.filter.favorites', matchFn: () => true }, // Filtered by isFavorite in MyServersPanel
];
