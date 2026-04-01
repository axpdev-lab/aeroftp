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
    source: 'registry' | 'protocol';
}

/** Cloud services defined at protocol level (not in provider registry) */
const CLOUD_SERVICES: DiscoverItem[] = [
    { id: 'googledrive', name: 'Google Drive', description: 'Google cloud storage (15 GB free)', protocol: 'googledrive', badge: 'OAuth', signupUrl: 'https://drive.google.com', source: 'protocol' },
    { id: 'onedrive', name: 'OneDrive', description: 'Microsoft cloud storage (5 GB free)', protocol: 'onedrive', badge: 'OAuth', signupUrl: 'https://onedrive.live.com', source: 'protocol' },
    { id: 'dropbox', name: 'Dropbox', description: 'File sync and sharing (2 GB free)', protocol: 'dropbox', badge: 'OAuth', signupUrl: 'https://www.dropbox.com', source: 'protocol' },
    { id: 'mega', name: 'MEGA', description: 'Secure cloud with client-side encryption (20 GB free)', protocol: 'mega', badge: 'E2E', signupUrl: 'https://mega.nz/register', source: 'protocol' },
    { id: 'box', name: 'Box', description: 'Enterprise cloud content management (10 GB free)', protocol: 'box', badge: 'OAuth', signupUrl: 'https://www.box.com/pricing/individual', source: 'protocol' },
    { id: 'pcloud', name: 'pCloud', description: 'Swiss cloud storage (10 GB free)', protocol: 'pcloud', badge: 'OAuth', signupUrl: 'https://www.pcloud.com', source: 'protocol' },
    { id: 'filen', name: 'Filen', description: 'Zero-knowledge encrypted cloud (10 GB free)', protocol: 'filen', badge: 'E2E', signupUrl: 'https://filen.io', source: 'protocol' },
    { id: 'internxt', name: 'Internxt', description: 'Privacy-focused encrypted cloud (1 GB free)', protocol: 'internxt', badge: 'E2E', signupUrl: 'https://internxt.com', source: 'protocol' },
    { id: 'kdrive', name: 'kDrive', description: 'Infomaniak Swiss cloud (15 GB free)', protocol: 'kdrive', badge: 'API', signupUrl: 'https://www.infomaniak.com/en/kdrive', source: 'protocol' },
    { id: 'filelu', name: 'FileLu', description: 'Multi-protocol cloud storage (1 GB free)', protocol: 'filelu', badge: 'API', signupUrl: 'https://filelu.com', source: 'protocol' },
    { id: 'jottacloud', name: 'Jottacloud', description: 'Norwegian cloud storage (5 GB free)', protocol: 'jottacloud', badge: 'API', signupUrl: 'https://www.jottacloud.com', source: 'protocol' },
    { id: 'drime', name: 'Drime Cloud', description: 'Cloud storage with API access (20 GB free)', protocol: 'drime', badge: 'API', signupUrl: 'https://drime.cloud', source: 'protocol' },
    { id: 'fourshared', name: '4shared', description: 'File sharing platform (15 GB free)', protocol: 'fourshared', badge: 'OAuth', signupUrl: 'https://www.4shared.com', source: 'protocol' },
    { id: 'opendrive', name: 'OpenDrive', description: 'Cloud storage and backup (5 GB free)', protocol: 'opendrive', badge: 'API', signupUrl: 'https://www.opendrive.com', source: 'protocol' },
    { id: 'yandexdisk', name: 'Yandex Disk', description: 'Russian cloud storage (5 GB free)', protocol: 'yandexdisk', badge: 'OAuth', signupUrl: 'https://disk.yandex.com', source: 'protocol' },
    { id: 'koofr-cloud', name: 'Koofr', description: 'EU-based privacy-friendly cloud (10 GB free)', protocol: 'koofr', badge: 'API', signupUrl: 'https://koofr.eu', source: 'protocol' },
    { id: 'zohoworkdrive', name: 'Zoho WorkDrive', description: 'Team collaboration and storage (5 GB free)', protocol: 'zohoworkdrive', badge: 'OAuth', signupUrl: 'https://www.zoho.com/workdrive/', source: 'protocol' },
];

const PROTOCOL_ITEMS: DiscoverItem[] = [
    { id: 'ftp-generic', name: 'FTP / FTPS', description: 'File Transfer Protocol (plain or TLS)', protocol: 'ftp', badge: 'TLS', isGeneric: true, source: 'protocol' },
    { id: 'sftp-generic', name: 'SFTP', description: 'SSH File Transfer', protocol: 'sftp', badge: 'SSH', isGeneric: true, source: 'protocol' },
    { id: 'azure-generic', name: 'Azure Blob', description: 'Azure Blob Storage', protocol: 'azure', badge: 'HMAC', isGeneric: true, source: 'protocol' },
];

const DEVELOPER_ITEMS: DiscoverItem[] = [
    { id: 'github', name: 'GitHub', description: 'Repository file system', protocol: 'github', badge: 'API', source: 'protocol' },
    { id: 'gitlab', name: 'GitLab', description: 'Repository & CI/CD platform', protocol: 'gitlab', badge: 'API', source: 'protocol' },
];

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
        source: 'registry',
    };
}

/** IDs to exclude from Discover (FileLu FTP/FTPS - redundant, already has S3+WebDAV+API) */
const EXCLUDED_IDS = new Set(['filelu-ftp', 'filelu-ftps']);

export function buildDiscoverCategories(): DiscoverCategory[] {
    const s3Providers = getProvidersByCategory('s3').map(registryToDiscoverItem);
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

    // Add Hetzner to protocol items if present
    const hetzner = ftpProviders.find(p => p.id === 'hetzner-storage-box');
    const protoItems = [...PROTOCOL_ITEMS];
    if (hetzner) {
        protoItems.push(hetzner);
    }

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
