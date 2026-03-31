/**
 * Types for the IntroHub Service Catalog system.
 * Used by the redesigned intro page (Tab Hub layout).
 */

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
    | 'developer';

export type MyServersViewMode = 'grid' | 'list';
export type MyServersSortBy = 'lastConnected' | 'name' | 'protocol';
export type MyServersFilterBy =
    | 'all'
    | 'ftp'
    | 's3'
    | 'webdav'
    | 'cloud'
    | 'favorites';

/** Maps a ProviderType or providerId to a catalog category for display */
export const PROTOCOL_CATEGORY_MAP: Record<string, CatalogCategoryId> = {
    // Protocols
    ftp: 'protocols',
    ftps: 'protocols',
    sftp: 'protocols',
    azure: 'protocols',

    // Object Storage (S3)
    s3: 'object-storage',
    'amazon-s3': 'object-storage',
    'aws-s3': 'object-storage',
    backblaze: 'object-storage',
    'mega-s4': 'object-storage',
    'cloudflare-r2': 'object-storage',
    'idrive-e2': 'object-storage',
    wasabi: 'object-storage',
    storj: 'object-storage',
    'alibaba-oss': 'object-storage',
    'tencent-cos': 'object-storage',
    'filelu-s5': 'object-storage',
    'yandex-storage': 'object-storage',
    'digitalocean-spaces': 'object-storage',
    'oracle-cloud': 'object-storage',
    minio: 'object-storage',
    'custom-s3': 'object-storage',

    // WebDAV
    webdav: 'webdav',
    nextcloud: 'webdav',
    cloudme: 'webdav',
    drivehq: 'webdav',
    jianguoyun: 'webdav',
    'koofr-webdav': 'webdav',
    infinicloud: 'webdav',
    seafile: 'webdav',
    'filelu-webdav': 'webdav',
    'felicloud-webdav': 'webdav',
    'custom-webdav': 'webdav',

    // Cloud Storage
    googledrive: 'cloud-storage',
    onedrive: 'cloud-storage',
    dropbox: 'cloud-storage',
    mega: 'cloud-storage',
    box: 'cloud-storage',
    pcloud: 'cloud-storage',
    internxt: 'cloud-storage',
    filen: 'cloud-storage',
    kdrive: 'cloud-storage',
    jottacloud: 'cloud-storage',
    drime: 'cloud-storage',
    fourshared: 'cloud-storage',
    opendrive: 'cloud-storage',
    yandexdisk: 'cloud-storage',
    blomp: 'cloud-storage',
    koofr: 'cloud-storage',
    filelu: 'cloud-storage',

    // Developer
    github: 'developer',
    sourceforge: 'developer',

    // Cloud (ex-Enterprise)
    zohoworkdrive: 'cloud-storage',
    swift: 'cloud-storage',
};

/** Get catalog category for a given protocol or providerId */
export function getCatalogCategory(protocolOrProviderId: string): CatalogCategoryId {
    return PROTOCOL_CATEGORY_MAP[protocolOrProviderId] || 'cloud-storage';
}

/** Filter chip definition for My Servers toolbar */
export interface FilterChip {
    id: MyServersFilterBy;
    labelKey: string;
    matchFn: (protocol: string, providerId?: string) => boolean;
}

export const FILTER_CHIPS: FilterChip[] = [
    { id: 'all', labelKey: 'introHub.filter.all', matchFn: () => true },
    { id: 'ftp', labelKey: 'introHub.filter.ftpSftp', matchFn: (p) => ['ftp', 'ftps', 'sftp'].includes(p) },
    { id: 's3', labelKey: 'introHub.filter.s3', matchFn: (p) => p === 's3' },
    { id: 'webdav', labelKey: 'introHub.filter.webdav', matchFn: (p) => p === 'webdav' },
    { id: 'cloud', labelKey: 'introHub.filter.cloud', matchFn: (p) => !['ftp', 'ftps', 'sftp', 'webdav', 's3', 'azure'].includes(p) },
    { id: 'favorites', labelKey: 'introHub.filter.favorites', matchFn: () => true }, // Filtered by isFavorite in MyServersPanel
];
