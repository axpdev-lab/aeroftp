/**
 * Provider Registry - Known cloud storage providers and generic connections
 * 
 * This file contains configurations for:
 * - Pre-configured providers (Backblaze, Nextcloud, DriveHQ, etc.)
 * - Generic/custom connections for any S3 or WebDAV compatible service
 * 
 * Add new providers here as they are tested and validated.
 */

import { ProviderConfig, ProviderCategory, BaseProtocol, ProviderRegistry } from './types';

// ============================================================================
// Common Field Definitions (reusable)
// ============================================================================

const COMMON_FIELDS = {
    username: {
        key: 'username',
        label: 'Username',
        type: 'text' as const,
        required: true,
        group: 'credentials' as const,
    },
    password: {
        key: 'password',
        label: 'Password',
        type: 'password' as const,
        required: true,
        group: 'credentials' as const,
    },
    server: {
        key: 'server',
        label: 'Server',
        type: 'url' as const,
        required: true,
        placeholder: 'https://example.com',
        group: 'server' as const,
    },
    port: {
        key: 'port',
        label: 'Port',
        type: 'number' as const,
        required: false,
        group: 'server' as const,
    },
    bucket: {
        key: 'bucket',
        label: 'Bucket Name',
        type: 'text' as const,
        required: true,
        group: 'server' as const,
    },
    region: {
        key: 'region',
        label: 'Region',
        type: 'text' as const,
        required: false,
        defaultValue: 'us-east-1',
        group: 'server' as const,
    },
    endpoint: {
        key: 'endpoint',
        label: 'S3 Endpoint',
        type: 'url' as const,
        required: true,
        placeholder: 'https://s3.example.com',
        group: 'server' as const,
    },
    accessKeyId: {
        key: 'username',
        label: 'Access Key ID',
        type: 'text' as const,
        required: true,
        group: 'credentials' as const,
    },
    secretAccessKey: {
        key: 'password',
        label: 'Secret Access Key',
        type: 'password' as const,
        required: true,
        group: 'credentials' as const,
    },
};

// ============================================================================
// Provider Definitions
// ============================================================================

export const PROVIDERS: ProviderConfig[] = [
    // =========================================================================
    // GENERIC / CUSTOM PROVIDERS (always available)
    // =========================================================================
    {
        id: 'custom-s3',
        name: 'S3 Compatible',
        description: 'Connect to any S3-compatible storage service',
        protocol: 's3',
        category: 's3',
        icon: 'Database',
        isGeneric: true,
        stable: true,
        fields: [
            { ...COMMON_FIELDS.accessKeyId, placeholder: 'Your Access Key ID' },
            { ...COMMON_FIELDS.secretAccessKey },
            { ...COMMON_FIELDS.bucket, placeholder: 'my-bucket' },
            { ...COMMON_FIELDS.endpoint, placeholder: 'https://s3.region.provider.com' },
            { ...COMMON_FIELDS.region, defaultValue: 'auto' },
            {
                key: 'pathStyle',
                label: 'Path-Style Access',
                type: 'checkbox',
                required: false,
                defaultValue: false,
                helpText: 'Enable for MinIO and some S3-compatible services',
                group: 'advanced',
            },
        ],
        features: {
            shareLink: true, // Presigned URLs
            sync: true,
        },
    },
    {
        id: 'custom-webdav',
        name: 'WebDAV Server',
        description: 'Connect to any WebDAV-compatible server',
        protocol: 'webdav',
        category: 'webdav',
        icon: 'Globe',
        isGeneric: true,
        stable: true,
        fields: [
            { ...COMMON_FIELDS.server, placeholder: 'https://webdav.yourserver.com/', helpText: 'Full WebDAV URL with https://' },
            { ...COMMON_FIELDS.username },
            { ...COMMON_FIELDS.password },
            {
                key: 'basePath',
                label: 'Base Path',
                type: 'text',
                required: false,
                placeholder: '/remote.php/dav/files/username/',
                helpText: 'Optional path prefix for WebDAV requests',
                group: 'advanced',
            },
        ],
        features: {
            shareLink: false, // Depends on specific server
            sync: true,
        },
    },

    // =========================================================================
    // S3 PROVIDERS
    // =========================================================================
    {
        id: 'backblaze',
        name: 'Backblaze B2',
        description: 'Affordable cloud storage with S3 compatibility',
        protocol: 's3',
        category: 's3',
        icon: 'Flame',
        color: '#E31C1C',
        stable: true,
        fields: [
            {
                ...COMMON_FIELDS.accessKeyId,
                label: 'keyID',
                placeholder: '003d90ca9d33900000000001',
                helpText: 'Your B2 Application Key ID (starts with 003...)',
            },
            {
                ...COMMON_FIELDS.secretAccessKey,
                label: 'applicationKey',
                helpText: 'Your B2 Application Key (hidden after creation)',
            },
            {
                ...COMMON_FIELDS.bucket,
                label: 'bucketName',
                placeholder: 'my-b2-bucket',
                helpText: 'The exact name of your B2 bucket',
            },
            {
                key: 'endpoint',
                label: 'Endpoint',
                type: 'url',
                required: true,
                placeholder: 's3.eu-central-003.backblazeb2.com',
                helpText: 'Bucket Settings → S3 Endpoint (without https://)',
                group: 'server',
            },
        ],
        defaults: {
            pathStyle: true,
            region: 'auto',
        },
        features: {
            shareLink: true,
            sync: true,
        },
        helpUrl: 'https://www.backblaze.com/b2/docs/',
    },
    {
        id: 'wasabi',
        name: 'Wasabi',
        description: 'Hot cloud storage with no egress fees',
        protocol: 's3',
        category: 's3',
        icon: 'Cloud',
        color: '#00C853',
        stable: false, // Not tested yet
        fields: [
            { ...COMMON_FIELDS.accessKeyId },
            { ...COMMON_FIELDS.secretAccessKey },
            { ...COMMON_FIELDS.bucket },
            {
                key: 'region',
                label: 'Region',
                type: 'select',
                required: true,
                options: [
                    { value: 'us-east-1', label: 'US East 1 (N. Virginia)' },
                    { value: 'us-east-2', label: 'US East 2 (N. Virginia)' },
                    { value: 'us-west-1', label: 'US West 1 (Oregon)' },
                    { value: 'eu-central-1', label: 'EU Central 1 (Amsterdam)' },
                    { value: 'eu-west-1', label: 'EU West 1 (London)' },
                    { value: 'ap-northeast-1', label: 'AP Northeast 1 (Tokyo)' },
                ],
                group: 'server',
            },
        ],
        defaults: {
            pathStyle: false,
        },
        features: {
            shareLink: true,
            sync: true,
        },
        helpUrl: 'https://wasabi.com/help/',
    },

    // =========================================================================
    // WEBDAV PROVIDERS
    // =========================================================================
    {
        id: 'drivehq',
        name: 'DriveHQ',
        description: 'Enterprise cloud storage and file sharing',
        protocol: 'webdav',
        category: 'webdav',
        icon: 'HardDrive',
        color: '#0066CC',
        stable: true,
        fields: [
            { ...COMMON_FIELDS.username, placeholder: 'Your DriveHQ username' },
            { ...COMMON_FIELDS.password },
        ],
        defaults: {
            server: 'https://webdav.drivehq.com',
            port: 443,
        },
        features: {
            shareLink: false, // DriveHQ has separate API for sharing
            sync: true,
        },
        helpUrl: 'https://www.drivehq.com/help/',
    },
    {
        id: 'nextcloud',
        name: 'Nextcloud',
        description: 'Self-hosted cloud storage platform',
        protocol: 'webdav',
        category: 'webdav',
        icon: 'Cloud',
        color: '#0082C9',
        stable: false, // Not tested yet
        fields: [
            {
                ...COMMON_FIELDS.server,
                label: 'Nextcloud URL',
                placeholder: 'https://cloud.example.com'
            },
            { ...COMMON_FIELDS.username },
            {
                ...COMMON_FIELDS.password,
                label: 'Password or App Token',
                helpText: 'Use an App Token for better security'
            },
        ],
        defaults: {
            basePath: '/remote.php/dav/files/{username}/',
        },
        endpoints: {
            webdavPath: '/remote.php/dav/files/{username}/',
            shareLink: '/ocs/v2.php/apps/files_sharing/api/v1/shares',
        },
        features: {
            shareLink: true,
            sync: true,
            versioning: true,
            trash: true,
        },
        helpUrl: 'https://docs.nextcloud.com/',
    },
    {
        id: 'owncloud',
        name: 'ownCloud',
        description: 'Open-source file sync and share',
        protocol: 'webdav',
        category: 'webdav',
        icon: 'Cloud',
        color: '#1D2D44',
        stable: false, // Not tested yet
        fields: [
            {
                ...COMMON_FIELDS.server,
                label: 'ownCloud URL',
                placeholder: 'https://cloud.example.com'
            },
            { ...COMMON_FIELDS.username },
            { ...COMMON_FIELDS.password },
        ],
        defaults: {
            basePath: '/remote.php/webdav/',
        },
        endpoints: {
            webdavPath: '/remote.php/webdav/',
            shareLink: '/ocs/v1.php/apps/files_sharing/api/v1/shares',
        },
        features: {
            shareLink: true,
            sync: true,
        },
        helpUrl: 'https://doc.owncloud.com/',
    },
    {
        id: 'mega',
        name: 'MEGA',
        description: 'Secure cloud storage with client-side encryption',
        protocol: 'mega',
        category: 'mega', // New category if needed, or 's3'/'webdav'. But Type says 'mega'
        icon: 'Cloud', // Or specific icon if available
        color: '#D9231E', // MEGA Red
        stable: false, // Beta
        fields: [
            {
                key: 'username',
                label: 'Email',
                type: 'email',
                required: true,
                placeholder: 'your@email.com',
                group: 'credentials',
            },
            {
                key: 'password',
                label: 'Password',
                type: 'password',
                required: true,
                group: 'credentials',
            },
            {
                key: 'save_session',
                label: 'Remember me (24h)',
                type: 'checkbox',
                required: false,
                defaultValue: true,
                group: 'advanced',
            }
        ],
        defaults: {
            save_session: true,
        },
        features: {
            shareLink: true, // Assuming MEGA supports it
            sync: true,
            thumbnails: true, // Special feature
        },
        helpUrl: 'https://mega.io/help',
    },
];

// ============================================================================
// Provider Registry Implementation
// ============================================================================

class ProviderRegistryImpl implements ProviderRegistry {
    private providers: Map<string, ProviderConfig>;

    constructor(configs: ProviderConfig[]) {
        this.providers = new Map();
        configs.forEach(p => this.providers.set(p.id, p));
    }

    getAll(): ProviderConfig[] {
        return Array.from(this.providers.values());
    }

    getByCategory(category: ProviderCategory): ProviderConfig[] {
        return this.getAll().filter(p => p.category === category);
    }

    getById(id: string): ProviderConfig | undefined {
        return this.providers.get(id);
    }

    getGeneric(protocol: BaseProtocol): ProviderConfig | undefined {
        return this.getAll().find(p => p.protocol === protocol && p.isGeneric);
    }

    supportsShareLink(providerId: string): boolean {
        const provider = this.getById(providerId);
        return provider?.features?.shareLink ?? false;
    }

    /**
     * Get stable providers only (tested and working)
     */
    getStable(): ProviderConfig[] {
        return this.getAll().filter(p => p.stable);
    }

    /**
     * Get providers grouped by category
     */
    getGrouped(): Record<ProviderCategory, ProviderConfig[]> {
        const grouped: Record<ProviderCategory, ProviderConfig[]> = {
            ftp: [],
            oauth: [],
            s3: [],
            webdav: [],
            mega: [],
        };

        this.getAll().forEach(p => {
            grouped[p.category].push(p);
        });

        return grouped;
    }
}

// ============================================================================
// Export Singleton Registry
// ============================================================================

export const providerRegistry = new ProviderRegistryImpl(PROVIDERS);

// Helper functions for common operations
export const getProviderById = (id: string) => providerRegistry.getById(id);
export const getProvidersByCategory = (cat: ProviderCategory) => providerRegistry.getByCategory(cat);
export const getAllProviders = () => providerRegistry.getAll();
export const getStableProviders = () => providerRegistry.getStable();
