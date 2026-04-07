/**
 * Unified data source for the Discover Services tab.
 * Merges provider registry (S3/WebDAV/FTP presets) with
 * protocol-level cloud services (OAuth providers).
 */
import { ProviderType } from '../../types';
import { getAllProviders, getProvidersByCategory } from '../../providers';
import type { ProviderConfig, ProviderCategory } from '../../providers/types';
import { CatalogCategoryId } from '../../types/catalog';

export interface DiscoverCategory {
    id: CatalogCategoryId;
    labelKey: string;
    icon: string; // Lucide icon name
    count: number;
    items: DiscoverItem[];
}

export interface DiscoverItem {
    id: string;
    name: string;
    description?: string;
    protocol: ProviderType;
    providerId?: string;
    badge?: string;
    isGeneric?: boolean;
    helpUrl?: string;
    signupUrl?: string;
    healthCheckUrl?: string;
    source: 'registry' | 'protocol';
    /** Pre-filled demo credentials (read-only test servers) */
    demo?: {
        server: string;
        port: number;
        username: string;
        password: string;
    };
}

/** Cloud services defined at protocol level (not in provider registry) */
const CLOUD_SERVICES: DiscoverItem[] = [
    { id: 'googledrive', name: 'Google Drive', description: 'Google cloud storage (15 GB free)', protocol: 'googledrive', badge: 'OAuth', signupUrl: 'https://drive.google.com', healthCheckUrl: 'https://www.googleapis.com', source: 'protocol' },
    { id: 'onedrive', name: 'OneDrive', description: 'Microsoft cloud storage (5 GB free)', protocol: 'onedrive', badge: 'OAuth', signupUrl: 'https://onedrive.live.com', healthCheckUrl: 'https://graph.microsoft.com', source: 'protocol' },
    { id: 'dropbox', name: 'Dropbox', description: 'File sync and sharing (2 GB free)', protocol: 'dropbox', badge: 'OAuth', signupUrl: 'https://www.dropbox.com', healthCheckUrl: 'https://api.dropboxapi.com', source: 'protocol' },
    { id: 'mega', name: 'MEGA', description: 'Secure cloud with client-side encryption (20 GB free)', protocol: 'mega', badge: 'E2E', signupUrl: 'https://mega.nz/register', healthCheckUrl: 'https://g.api.mega.co.nz', source: 'protocol' },
    { id: 'box', name: 'Box', description: 'Enterprise cloud content management (10 GB free)', protocol: 'box', badge: 'OAuth', signupUrl: 'https://www.box.com/pricing/individual', healthCheckUrl: 'https://api.box.com', source: 'protocol' },
    { id: 'pcloud', name: 'pCloud', description: 'Swiss cloud storage (10 GB free)', protocol: 'pcloud', badge: 'OAuth', signupUrl: 'https://www.pcloud.com', healthCheckUrl: 'https://api.pcloud.com', source: 'protocol' },
    { id: 'filen', name: 'Filen', description: 'Zero-knowledge encrypted cloud (10 GB free)', protocol: 'filen', badge: 'E2E', signupUrl: 'https://filen.io', healthCheckUrl: 'https://gateway.filen.io', source: 'protocol' },
    { id: 'internxt', name: 'Internxt', description: 'Privacy-focused encrypted cloud (1 GB free)', protocol: 'internxt', badge: 'E2E', signupUrl: 'https://internxt.com', healthCheckUrl: 'https://api.internxt.com', source: 'protocol' },
    { id: 'zohoworkdrive', name: 'Zoho WorkDrive', description: 'Team collaboration and storage (5 GB free)', protocol: 'zohoworkdrive', badge: 'OAuth', signupUrl: 'https://www.zoho.com/workdrive/', healthCheckUrl: 'https://www.zohoapis.com', source: 'protocol' },
    { id: 'kdrive', name: 'kDrive', description: 'Infomaniak Swiss cloud (15 GB free)', protocol: 'kdrive', badge: 'API', signupUrl: 'https://www.infomaniak.com/en/kdrive', healthCheckUrl: 'https://api.infomaniak.com', source: 'protocol' },
    { id: 'filelu', name: 'FileLu', description: 'Multi-protocol cloud storage (1 GB free)', protocol: 'filelu', badge: 'API', signupUrl: 'https://filelu.com', healthCheckUrl: 'https://filelu.com/api/', source: 'protocol' },
    { id: 'koofr-cloud', name: 'Koofr', description: 'EU-based privacy-friendly cloud (10 GB free)', protocol: 'koofr', badge: 'API', signupUrl: 'https://koofr.eu', healthCheckUrl: 'https://app.koofr.net', source: 'protocol' },
    { id: 'drime', name: 'Drime Cloud', description: 'Cloud storage with API access (20 GB free)', protocol: 'drime', badge: 'API', signupUrl: 'https://drime.cloud', healthCheckUrl: 'https://app.drime.cloud', source: 'protocol' },
    { id: 'jottacloud', name: 'Jottacloud', description: 'Norwegian cloud storage (5 GB free)', protocol: 'jottacloud', badge: 'API', signupUrl: 'https://www.jottacloud.com', healthCheckUrl: 'https://jottacloud.com', source: 'protocol' },
    { id: 'fourshared', name: '4shared', description: 'File sharing platform (15 GB free)', protocol: 'fourshared', badge: 'OAuth', signupUrl: 'https://www.4shared.com', healthCheckUrl: 'https://webdav.4shared.com', source: 'protocol' },
    { id: 'opendrive', name: 'OpenDrive', description: 'Cloud storage and backup (5 GB free)', protocol: 'opendrive', badge: 'API', signupUrl: 'https://www.opendrive.com', healthCheckUrl: 'https://dev.opendrive.com', source: 'protocol' },
    { id: 'yandexdisk', name: 'Yandex Disk', description: 'Russian cloud storage (5 GB free)', protocol: 'yandexdisk', badge: 'OAuth', signupUrl: 'https://disk.yandex.com', healthCheckUrl: 'https://cloud-api.yandex.net', source: 'protocol' },
];

/** Media Services — photo/video platforms with file management.
 *  Google Photos: STANDBY — photoslibrary.readonly scope removed by Google on 2025-03-31.
 *  Browse/download no longer possible. Upload-only via appendonly scope still works.
 *  Re-enable when: Google provides a REST replacement or Picker API is integrated. */
const MEDIA_SERVICES: DiscoverItem[] = [
    { id: 'immich', name: 'Immich', description: 'Self-hosted photo management (open source)', protocol: 'immich' as ProviderType, badge: 'API', isGeneric: true, signupUrl: 'https://immich.app', helpUrl: 'https://immich.app/docs/overview/introduction', source: 'protocol' },
    { id: 'pixelunion', name: 'PixelUnion', description: 'EU-hosted Immich photo cloud', protocol: 'immich' as ProviderType, providerId: 'pixelunion', badge: 'EU', signupUrl: 'https://pixelunion.eu', healthCheckUrl: 'https://pixelunion.eu', source: 'protocol' },
    // Google Photos: STANDBY — photoslibrary.readonly scope removed by Google on 2025-03-31.
    // { id: 'googlephotos', name: 'Google Photos', ... },
];

const PROTOCOL_ITEMS: DiscoverItem[] = [
    { id: 'ftp-generic', name: 'FTP / FTPS', description: 'File Transfer Protocol (plain or TLS)', protocol: 'ftp', badge: 'TLS', isGeneric: true, source: 'protocol' },
    { id: 'sftp-generic', name: 'SFTP', description: 'SSH File Transfer', protocol: 'sftp', badge: 'SSH', isGeneric: true, source: 'protocol' },
    { id: 'rebex-ftp-demo', name: 'Rebex FTP Demo', description: 'Public read-only FTP test server', protocol: 'ftp', badge: 'DEMO', healthCheckUrl: 'https://test.rebex.net', source: 'protocol', demo: { server: 'test.rebex.net', port: 21, username: 'demo', password: 'password' } },
    { id: 'rebex-sftp-demo', name: 'Rebex SFTP Demo', description: 'Public read-only SFTP test server', protocol: 'sftp', badge: 'DEMO', healthCheckUrl: 'https://test.rebex.net', source: 'protocol', demo: { server: 'test.rebex.net', port: 22, username: 'demo', password: 'password' } },
];

/** Object storage items defined at protocol level (not in provider registry) */
const OBJECT_STORAGE_ITEMS: DiscoverItem[] = [
    { id: 'azure-generic', name: 'Azure Blob', description: 'Azure Blob Storage', protocol: 'azure', badge: 'HMAC', isGeneric: true, healthCheckUrl: 'https://login.microsoftonline.com/common/v2.0/.well-known/openid-configuration', source: 'protocol' },
];

const DEVELOPER_ITEMS: DiscoverItem[] = [
    { id: 'github', name: 'GitHub', description: 'Repository file system', protocol: 'github', badge: 'API', healthCheckUrl: 'https://api.github.com', source: 'protocol' },
    { id: 'gitlab', name: 'GitLab', description: 'Repository & CI/CD platform', protocol: 'gitlab', badge: 'API', healthCheckUrl: 'https://gitlab.com', source: 'protocol' },
];

/** Maps provider/item IDs to i18n keys for translated descriptions */
export const DISCOVER_DESC_KEYS: Record<string, string> = {
    // S3 registry providers
    'custom-s3': 'protocol.discoverCustomS3',
    'amazon-s3': 'protocol.discoverAmazonS3',
    'google-cloud-storage': 'protocol.discoverGoogleCloudStorage',
    'backblaze': 'protocol.discoverBackblaze',
    'mega-s4': 'protocol.discoverMegaS4',
    'cloudflare-r2': 'protocol.discoverCloudflareR2',
    'idrive-e2': 'protocol.discoverIDriveE2',
    'wasabi': 'protocol.discoverWasabi',
    'storj': 'protocol.discoverStorj',
    'alibaba-oss': 'protocol.discoverAlibabaOSS',
    'tencent-cos': 'protocol.discoverTencentCOS',
    'filelu-s3': 'protocol.discoverFileLuS3',
    'yandex-storage': 'protocol.discoverYandexStorage',
    'digitalocean-spaces': 'protocol.discoverDigitalOceanSpaces',
    'oracle-cloud': 'protocol.discoverOracleCloud',
    'minio': 'protocol.discoverMinIO',
    'quotaless-s3': 'protocol.discoverQuotalessS3',
    // WebDAV registry providers
    'custom-webdav': 'protocol.discoverCustomWebdav',
    'fourshared-webdav': 'protocol.discover4shared',
    'cloudme': 'protocol.discoverCloudMe',
    'drivehq': 'protocol.discoverDriveHQ',
    'felicloud': 'protocol.discoverFelicloud',
    'jianguoyun': 'protocol.discoverJianguoyun',
    'infinicloud': 'protocol.discoverInfiniCloud',
    'seafile': 'protocol.discoverSeafile',
    'filelu-webdav': 'protocol.discoverFileLuWebdav',
    'quotaless-webdav': 'protocol.discoverQuotalessWebdav',
    'nextcloud': 'protocol.discoverNextcloud',
    // FTP registry providers
    'hetzner-storage-box': 'protocol.discoverHetzner',
    'sourceforge': 'protocol.discoverSourceForge',
    'filelu-ftp': 'protocol.discoverFileLuFtp',
    'filelu-ftps': 'protocol.discoverFileLuFtps',
    'blomp': 'protocol.discoverBlomp',
    'filen-cloud': 'protocol.discoverFilen',
    'filelu-cloud': 'protocol.discoverFileLu',
    // Cloud services (protocol-level)
    'googledrive': 'protocol.discoverGoogleDrive',
    'onedrive': 'protocol.discoverOneDrive',
    'dropbox': 'protocol.discoverDropbox',
    'mega': 'protocol.discoverMega',
    'box': 'protocol.discoverBox',
    'pcloud': 'protocol.discoverPCloud',
    'filen': 'protocol.discoverFilenCloud',
    'internxt': 'protocol.discoverInternxt',
    'zohoworkdrive': 'protocol.discoverZohoWorkDrive',
    'kdrive': 'protocol.discoverKDrive',
    'filelu': 'protocol.discoverFileLuCloud',
    'koofr-cloud': 'protocol.discoverKoofr',
    'drime': 'protocol.discoverDrimeCloud',
    'jottacloud': 'protocol.discoverJottacloud',
    'fourshared': 'protocol.discoverFourShared',
    'opendrive': 'protocol.discoverOpenDrive',
    'yandexdisk': 'protocol.discoverYandexDisk',
    // Protocol items
    'ftp-generic': 'protocol.discoverFtpFtps',
    'sftp-generic': 'protocol.discoverSftp',
    'rebex-ftp-demo': 'protocol.discoverRebexFtp',
    'rebex-sftp-demo': 'protocol.discoverRebexSftp',
    'azure-generic': 'protocol.discoverAzureBlob',
    'github': 'protocol.discoverGitHub',
    'gitlab': 'protocol.discoverGitLab',
    'googlephotos': 'protocol.discoverGooglePhotos',
    'immich': 'protocol.discoverImmich',
    'pixelunion': 'protocol.discoverPixelUnion',
};

/** Badge overrides for registry providers with distinctive features */
const BADGE_OVERRIDES: Record<string, string> = {
    'felicloud': 'API OCS',   // Nextcloud-based, OCS REST API for sharing
    'nextcloud': 'OCS',       // Self-hosted Nextcloud, OCS REST API
};

function registryToDiscoverItem(p: ProviderConfig): DiscoverItem {
    const autoBadge = p.protocol === 'sftp' ? 'SSH'
        : p.protocol === 's3' ? 'HMAC'
        : p.protocol === 'webdav' ? 'TLS'
        : p.protocol === 'swift' ? 'Swift'
        : undefined;

    return {
        id: p.id,
        name: p.name,
        description: p.description,
        protocol: p.protocol as ProviderType,
        providerId: p.id,
        badge: BADGE_OVERRIDES[p.id] ?? autoBadge,
        isGeneric: p.isGeneric,
        helpUrl: p.helpUrl,
        signupUrl: p.signupUrl,
        healthCheckUrl: p.healthCheckUrl,
        source: 'registry',
    };
}

/** IDs to exclude from Discover (FileLu FTP/FTPS - redundant, already has S3+WebDAV+API) */
const EXCLUDED_IDS = new Set(['filelu-ftp', 'filelu-ftps']);

export function buildDiscoverCategories(): DiscoverCategory[] {
    const s3Providers = [...getProvidersByCategory('s3').map(registryToDiscoverItem), ...OBJECT_STORAGE_ITEMS]
        .sort((a, b) => {
            const priority: Record<string, number> = {
                // Row 1: generic + self-hosted + decentralized
                'custom-s3': 0, 'minio': 1, 'storj': 2,
                // Row 2: major cloud providers
                'amazon-s3': 3, 'google-cloud-storage': 4, 'cloudflare-r2': 5, 'azure-generic': 6,
                // Row 3: mid-tier providers
                'mega-s4': 7, 'backblaze': 8, 'idrive-e2': 9,
                // Row 4+: remaining
                'digitalocean-spaces': 10, 'filelu-s3': 11, 'wasabi': 12,
                'oracle-cloud': 13, 'quotaless-s3': 14,
                // Asian providers last
                'alibaba-oss': 20, 'tencent-cos': 21, 'yandex-storage': 22,
            };
            const pa = priority[a.id] ?? 15;
            const pb = priority[b.id] ?? 15;
            if (pa !== pb) return pa - pb;
            return a.name.localeCompare(b.name);
        });
    const webdavProviders = getProvidersByCategory('webdav').map(registryToDiscoverItem)
        .sort((a, b) => {
            // WebDAV Server (generic) first, then Nextcloud, Felicloud, then alphabetical
            const priority: Record<string, number> = { 'custom-webdav': 0, 'nextcloud': 1, 'felicloud': 2 };
            const pa = priority[a.id] ?? 10;
            const pb = priority[b.id] ?? 10;
            if (pa !== pb) return pa - pb;
            return a.name.localeCompare(b.name);
        });
    const ftpProviders = getProvidersByCategory('ftp')
        .filter(p => !EXCLUDED_IDS.has(p.id))
        .map(registryToDiscoverItem);

    // Add SourceForge FIRST in developer items (SSH-based, foundational)
    const sfInRegistry = ftpProviders.find(p => p.id === 'sourceforge');
    const devItems = sfInRegistry
        ? [{ ...sfInRegistry, description: 'Open source hosting' }, ...DEVELOPER_ITEMS]
        : [...DEVELOPER_ITEMS];

    // Build protocol items: generics first, then Hetzner, then demos last
    const hetzner = ftpProviders.find(p => p.id === 'hetzner-storage-box');
    const protoItems = [
        ...PROTOCOL_ITEMS.filter(p => !p.demo),
        ...(hetzner ? [hetzner] : []),
        ...PROTOCOL_ITEMS.filter(p => !!p.demo),
    ];

    const categories: DiscoverCategory[] = [
        {
            id: 'protocols',
            labelKey: 'introHub.category.protocols',
            icon: 'Server',
            count: protoItems.length,
            items: protoItems,
        },
        {
            id: 'object-storage',
            labelKey: 'introHub.category.objectStorage',
            icon: 'Database',
            count: s3Providers.length,
            items: s3Providers,
        },
        {
            id: 'webdav',
            labelKey: 'introHub.category.webdav',
            icon: 'Globe',
            count: webdavProviders.length,
            items: webdavProviders,
        },
        {
            id: 'cloud-storage',
            labelKey: 'introHub.category.cloudStorage',
            icon: 'Cloud',
            count: CLOUD_SERVICES.length,
            items: CLOUD_SERVICES,
        },
        {
            id: 'media-services',
            labelKey: 'introHub.category.mediaServices',
            icon: 'Camera',
            count: MEDIA_SERVICES.length,
            items: MEDIA_SERVICES,
        },
        {
            id: 'developer',
            labelKey: 'introHub.category.developer',
            icon: 'Code',
            count: devItems.length,
            items: devItems,
        },
    ];

    return categories;
}

/** Get total count of all services across all categories */
export function getTotalServiceCount(): number {
    return buildDiscoverCategories().reduce((sum, cat) => sum + cat.count, 0);
}
