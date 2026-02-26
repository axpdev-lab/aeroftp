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
        id: 'amazon-s3',
        name: 'Amazon S3',
        description: 'Amazon Web Services S3 cloud object storage',
        protocol: 's3',
        category: 's3',
        icon: 'Cloud',
        color: '#FF9900',
        stable: true,
        fields: [
            { ...COMMON_FIELDS.accessKeyId },
            { ...COMMON_FIELDS.secretAccessKey },
            { ...COMMON_FIELDS.bucket, placeholder: 'my-bucket' },
            {
                key: 'region',
                label: 'AWS Region',
                type: 'select',
                required: true,
                options: [
                    { value: 'us-east-1', label: 'US East (N. Virginia)' },
                    { value: 'us-east-2', label: 'US East (Ohio)' },
                    { value: 'us-west-1', label: 'US West (N. California)' },
                    { value: 'us-west-2', label: 'US West (Oregon)' },
                    { value: 'ca-central-1', label: 'Canada (Central)' },
                    { value: 'sa-east-1', label: 'South America (São Paulo)' },
                    { value: 'eu-west-1', label: 'EU (Ireland)' },
                    { value: 'eu-west-2', label: 'EU (London)' },
                    { value: 'eu-west-3', label: 'EU (Paris)' },
                    { value: 'eu-central-1', label: 'EU (Frankfurt)' },
                    { value: 'eu-central-2', label: 'EU (Zurich)' },
                    { value: 'eu-north-1', label: 'EU (Stockholm)' },
                    { value: 'eu-south-1', label: 'EU (Milan)' },
                    { value: 'eu-south-2', label: 'EU (Spain)' },
                    { value: 'me-south-1', label: 'Middle East (Bahrain)' },
                    { value: 'me-central-1', label: 'Middle East (UAE)' },
                    { value: 'il-central-1', label: 'Israel (Tel Aviv)' },
                    { value: 'af-south-1', label: 'Africa (Cape Town)' },
                    { value: 'ap-east-1', label: 'Asia Pacific (Hong Kong)' },
                    { value: 'ap-southeast-1', label: 'Asia Pacific (Singapore)' },
                    { value: 'ap-southeast-2', label: 'Asia Pacific (Sydney)' },
                    { value: 'ap-southeast-3', label: 'Asia Pacific (Jakarta)' },
                    { value: 'ap-southeast-4', label: 'Asia Pacific (Melbourne)' },
                    { value: 'ap-northeast-1', label: 'Asia Pacific (Tokyo)' },
                    { value: 'ap-northeast-2', label: 'Asia Pacific (Seoul)' },
                    { value: 'ap-northeast-3', label: 'Asia Pacific (Osaka)' },
                    { value: 'ap-south-1', label: 'Asia Pacific (Mumbai)' },
                    { value: 'ap-south-2', label: 'Asia Pacific (Hyderabad)' },
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
        helpUrl: 'https://docs.aws.amazon.com/s3/',
        signupUrl: 'https://aws.amazon.com/free/',
    },
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
        signupUrl: 'https://www.backblaze.com/sign-up/cloud-storage',
    },
    {
        id: 'cloudflare-r2',
        name: 'Cloudflare R2',
        description: 'Zero-egress S3-compatible storage (10 GB free)',
        protocol: 's3',
        category: 's3',
        icon: 'Cloud',
        color: '#F6821F',
        stable: true,
        fields: [
            { ...COMMON_FIELDS.accessKeyId, helpText: 'R2 API Token → Access Key ID' },
            { ...COMMON_FIELDS.secretAccessKey, helpText: 'R2 API Token → Secret Access Key' },
            { ...COMMON_FIELDS.bucket, placeholder: 'my-r2-bucket' },
            {
                key: 'accountId',
                label: 'Account ID',
                type: 'text',
                required: true,
                placeholder: 'a1b2c3d4e5f6...',
                helpText: 'Cloudflare Dashboard → R2 → Overview → Account ID',
                group: 'server',
            },
        ],
        defaults: {
            pathStyle: true,
            region: 'auto',
            endpointTemplate: '{accountId}.r2.cloudflarestorage.com',
        },
        features: {
            shareLink: true,
            sync: true,
        },
        helpUrl: 'https://developers.cloudflare.com/r2/',
        signupUrl: 'https://dash.cloudflare.com/sign-up',
    },
    {
        id: 'idrive-e2',
        name: 'IDrive e2',
        description: 'S3-compatible hot storage (10 GB free)',
        protocol: 's3',
        category: 's3',
        icon: 'Cloud',
        color: '#1A73E8',
        stable: true,
        fields: [
            { ...COMMON_FIELDS.accessKeyId, helpText: 'e2 Dashboard → Access Keys → Access Key ID' },
            { ...COMMON_FIELDS.secretAccessKey },
            { ...COMMON_FIELDS.bucket, placeholder: 'my-e2-bucket' },
            {
                key: 'endpoint',
                label: 'Region Endpoint',
                type: 'url',
                required: true,
                placeholder: 'l4g4.ch11.idrivee2-2.com',
                helpText: 'e2 Dashboard → Regions → your region endpoint (without https://)',
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
        helpUrl: 'https://www.idrive.com/s3-storage-e2/',
        signupUrl: 'https://www.idrive.com/e2/sign-up',
    },
    {
        id: 'wasabi',
        name: 'Wasabi',
        description: 'Hot cloud storage, no egress fees',
        protocol: 's3',
        category: 's3',
        icon: 'Cloud',
        color: '#00C853',
        stable: true,
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
                    { value: 'us-west-2', label: 'US West 2 (San Jose)' },
                    { value: 'us-central-1', label: 'US Central 1 (Texas)' },
                    { value: 'ca-central-1', label: 'CA Central 1 (Toronto)' },
                    { value: 'eu-central-1', label: 'EU Central 1 (Amsterdam)' },
                    { value: 'eu-central-2', label: 'EU Central 2 (Frankfurt)' },
                    { value: 'eu-south-1', label: 'EU South 1 (Milan)' },
                    { value: 'eu-west-1', label: 'EU West 1 (London)' },
                    { value: 'eu-west-2', label: 'EU West 2 (Paris)' },
                    { value: 'ap-northeast-1', label: 'AP Northeast 1 (Tokyo)' },
                    { value: 'ap-northeast-2', label: 'AP Northeast 2 (Osaka)' },
                    { value: 'ap-southeast-1', label: 'AP Southeast 1 (Singapore)' },
                    { value: 'ap-southeast-2', label: 'AP Southeast 2 (Sydney)' },
                ],
                group: 'server',
            },
        ],
        defaults: {
            pathStyle: false,
            endpointTemplate: 'https://s3.{region}.wasabisys.com',
        },
        features: {
            shareLink: true,
            sync: true,
        },
        helpUrl: 'https://docs.wasabi.com/',
        signupUrl: 'https://console.wasabisys.com/signup',
    },
    {
        id: 'storj',
        name: 'Storj',
        description: 'Decentralized S3-compatible cloud storage',
        protocol: 's3',
        category: 's3',
        icon: 'Cloud',
        color: '#2683FF',
        stable: true,
        fields: [
            { ...COMMON_FIELDS.accessKeyId, helpText: 'S3 Gateway access grant → Access Key' },
            { ...COMMON_FIELDS.secretAccessKey, helpText: 'S3 Gateway access grant → Secret Key' },
            { ...COMMON_FIELDS.bucket, placeholder: 'my-storj-bucket' },
            {
                key: 'endpoint',
                label: 'Satellite Gateway',
                type: 'select',
                required: true,
                options: [
                    { value: 'https://gateway.storjshare.io', label: 'US1 — North America' },
                    { value: 'https://gateway.eu1.storjshare.io', label: 'EU1 — Europe' },
                    { value: 'https://gateway.ap1.storjshare.io', label: 'AP1 — Asia-Pacific' },
                ],
                group: 'server',
            },
        ],
        defaults: {
            pathStyle: true,
            region: 'global',
        },
        features: {
            shareLink: true,
            sync: true,
        },
        helpUrl: 'https://storj.dev/dcs/api/s3/s3-compatible-gateway',
        signupUrl: 'https://www.storj.io/signup',
    },
    {
        id: 'digitalocean-spaces',
        name: 'DigitalOcean Spaces',
        description: 'S3-compatible object storage with built-in CDN',
        protocol: 's3',
        category: 's3',
        icon: 'Cloud',
        color: '#0069FF',
        stable: false,
        fields: [
            { ...COMMON_FIELDS.accessKeyId, helpText: 'API → Spaces Keys → Key' },
            { ...COMMON_FIELDS.secretAccessKey, helpText: 'API → Spaces Keys → Secret' },
            { ...COMMON_FIELDS.bucket, placeholder: 'my-space-name', label: 'Space Name' },
            {
                key: 'region',
                label: 'Region',
                type: 'select',
                required: true,
                options: [
                    { value: 'nyc3', label: 'New York 3' },
                    { value: 'sfo3', label: 'San Francisco 3' },
                    { value: 'ams3', label: 'Amsterdam 3' },
                    { value: 'sgp1', label: 'Singapore 1' },
                    { value: 'fra1', label: 'Frankfurt 1' },
                    { value: 'syd1', label: 'Sydney 1' },
                ],
                group: 'server',
            },
        ],
        defaults: {
            pathStyle: false,
            endpointTemplate: 'https://{region}.digitaloceanspaces.com',
        },
        features: {
            shareLink: true,
            sync: true,
        },
        helpUrl: 'https://docs.digitalocean.com/products/spaces/',
        signupUrl: 'https://cloud.digitalocean.com/registrations/new',
    },
    {
        id: 'oracle-cloud',
        name: 'Oracle Cloud',
        description: 'S3-compatible object storage (20 GB always free)',
        protocol: 's3',
        category: 's3',
        icon: 'Cloud',
        color: '#C74634',
        stable: false,
        fields: [
            { ...COMMON_FIELDS.accessKeyId, helpText: 'Identity → Users → Customer Secret Keys → Access Key' },
            { ...COMMON_FIELDS.secretAccessKey },
            { ...COMMON_FIELDS.bucket, placeholder: 'my-oci-bucket' },
            {
                key: 'endpoint',
                label: 'S3 Endpoint',
                type: 'url',
                required: true,
                placeholder: '<namespace>.compat.objectstorage.<region>.oraclecloud.com',
                helpText: 'Format: namespace.compat.objectstorage.region.oraclecloud.com',
                group: 'server',
            },
        ],
        defaults: {
            pathStyle: true,
            region: 'us-east-1',
        },
        features: {
            shareLink: true,
            sync: true,
        },
        helpUrl: 'https://docs.oracle.com/en-us/iaas/Content/Object/Tasks/s3compatibleapi.htm',
        signupUrl: 'https://signup.cloud.oracle.com/',
    },
    {
        id: 'alibaba-oss',
        name: 'Alibaba Cloud OSS',
        description: 'S3-compatible object storage (China & global)',
        protocol: 's3',
        category: 's3',
        icon: 'Cloud',
        color: '#FF6A00',
        stable: false,
        fields: [
            { ...COMMON_FIELDS.accessKeyId, helpText: 'RAM Console → AccessKey Management → AccessKeyId' },
            { ...COMMON_FIELDS.secretAccessKey },
            { ...COMMON_FIELDS.bucket, placeholder: 'my-oss-bucket' },
            {
                key: 'region',
                label: 'Region',
                type: 'select',
                required: true,
                options: [
                    { value: 'cn-hangzhou', label: 'Hangzhou (China East)' },
                    { value: 'cn-shanghai', label: 'Shanghai (China East)' },
                    { value: 'cn-beijing', label: 'Beijing (China North)' },
                    { value: 'cn-shenzhen', label: 'Shenzhen (China South)' },
                    { value: 'ap-southeast-1', label: 'Singapore (SE Asia)' },
                    { value: 'us-west-1', label: 'Silicon Valley (US)' },
                    { value: 'eu-central-1', label: 'Frankfurt (Europe)' },
                ],
                group: 'server',
            },
        ],
        defaults: {
            pathStyle: false,
            endpointTemplate: 'https://oss-{region}.aliyuncs.com',
        },
        features: {
            shareLink: true,
            sync: true,
        },
        helpUrl: 'https://www.alibabacloud.com/help/en/oss/developer-reference/use-aws-sdks-to-access-oss',
        signupUrl: 'https://account.alibabacloud.com/register/intl_register.htm',
    },
    {
        id: 'tencent-cos',
        name: 'Tencent Cloud COS',
        description: 'S3-compatible object storage (China & global)',
        protocol: 's3',
        category: 's3',
        icon: 'Cloud',
        color: '#006EFF',
        stable: false,
        fields: [
            { ...COMMON_FIELDS.accessKeyId, label: 'SecretId', helpText: 'CAM Console → API Keys → SecretId' },
            { ...COMMON_FIELDS.secretAccessKey, label: 'SecretKey' },
            {
                ...COMMON_FIELDS.bucket,
                placeholder: 'mybucket-1250000000',
                helpText: 'Bucket name must include APPID suffix (e.g. mybucket-1250000000)',
            },
            {
                key: 'region',
                label: 'Region',
                type: 'select',
                required: true,
                options: [
                    { value: 'ap-guangzhou', label: 'Guangzhou (China South)' },
                    { value: 'ap-beijing', label: 'Beijing (China North)' },
                    { value: 'ap-shanghai', label: 'Shanghai (China East)' },
                    { value: 'ap-chengdu', label: 'Chengdu (China West)' },
                    { value: 'ap-singapore', label: 'Singapore (SE Asia)' },
                    { value: 'na-siliconvalley', label: 'Silicon Valley (US)' },
                    { value: 'eu-frankfurt', label: 'Frankfurt (Europe)' },
                ],
                group: 'server',
            },
        ],
        defaults: {
            pathStyle: false,
            endpointTemplate: 'https://cos.{region}.myqcloud.com',
        },
        features: {
            shareLink: true,
            sync: true,
        },
        helpUrl: 'https://www.tencentcloud.com/document/product/436/32537',
        signupUrl: 'https://www.tencentcloud.com/account/register',
    },

    {
        id: 'minio',
        name: 'MinIO',
        description: 'High-performance self-hosted S3-compatible object storage',
        protocol: 's3',
        category: 's3',
        icon: 'Database',
        color: '#C72C48',
        stable: true,
        fields: [
            { ...COMMON_FIELDS.accessKeyId, placeholder: 'minioadmin' },
            { ...COMMON_FIELDS.secretAccessKey },
            { ...COMMON_FIELDS.bucket, placeholder: 'my-bucket' },
            {
                ...COMMON_FIELDS.endpoint,
                label: 'MinIO Endpoint',
                placeholder: 'minio.example.com:9000',
                helpText: 'Your MinIO server address (without https://)',
            },
        ],
        defaults: {
            pathStyle: true,
            region: 'us-east-1',
        },
        features: {
            shareLink: true,
            sync: true,
        },
        helpUrl: 'https://min.io/docs/minio/linux/index.html',
        signupUrl: 'https://min.io/download',
    },

    // =========================================================================
    // WEBDAV PROVIDERS
    // =========================================================================
    {
        id: '4shared',
        name: '4shared',
        description: 'File hosting with 15 GB free storage (OAuth 1.0)',
        protocol: 'fourshared',
        category: 'oauth',
        icon: 'Cloud',
        color: '#008BF6',
        stable: true,
        fields: [],
        defaults: {},
        features: {
            shareLink: false,
            sync: true,
        },
        helpUrl: 'https://www.4shared.com/developer/docs/index.jsp',
        signupUrl: 'https://www.4shared.com/reg0.jsp',
    },
    {
        id: 'cloudme',
        name: 'CloudMe',
        description: 'Swedish cloud storage with WebDAV (3 GB free)',
        protocol: 'webdav',
        category: 'webdav',
        icon: 'Cloud',
        color: '#00AEEF',
        stable: true,
        fields: [
            { ...COMMON_FIELDS.username, placeholder: 'Your CloudMe username' },
            { ...COMMON_FIELDS.password },
        ],
        defaults: {
            server: 'https://webdav.cloudme.com/{username}',
            port: 443,
        },
        features: {
            shareLink: false,
            sync: true,
        },
        helpUrl: 'https://www.cloudme.com/en/webdav',
        signupUrl: 'https://www.cloudme.com/signup',
    },
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
        signupUrl: 'https://www.drivehq.com/secure/SignUp.aspx',
    },
    {
        id: 'koofr',
        name: 'Koofr',
        description: 'EU-based privacy-friendly cloud (10 GB free)',
        protocol: 'webdav',
        category: 'webdav',
        icon: 'Cloud',
        color: '#00B4A0',
        stable: true,
        fields: [
            { ...COMMON_FIELDS.username, label: 'Email', placeholder: 'your@email.com' },
            {
                ...COMMON_FIELDS.password,
                label: 'App Password',
                helpText: 'Koofr → Preferences → Password → App Passwords (not your login password)',
            },
        ],
        defaults: {
            server: 'https://app.koofr.net/dav/Koofr',
            port: 443,
            basePath: '/dav/Koofr/',
        },
        features: {
            shareLink: false,
            sync: true,
        },
        helpUrl: 'https://app.koofr.net/help/webdav',
        signupUrl: 'https://app.koofr.net/registrations/new',
    },
    {
        id: 'jianguoyun',
        name: 'Jianguoyun',
        description: 'Popular Chinese cloud storage with WebDAV (3 GB free)',
        protocol: 'webdav',
        category: 'webdav',
        icon: 'Cloud',
        color: '#3A9BDC',
        stable: true,
        fields: [
            {
                ...COMMON_FIELDS.username,
                label: 'Email',
                placeholder: 'your@email.com',
            },
            {
                ...COMMON_FIELDS.password,
                label: 'App Password',
                helpText: 'Account Settings → Security Options → App Passwords (not your login password)',
            },
        ],
        defaults: {
            server: 'https://dav.jianguoyun.com/dav',
            port: 443,
            basePath: '/dav/',
        },
        features: {
            shareLink: false,
            sync: true,
        },
        helpUrl: 'https://help.jianguoyun.com/?p=2064',
        signupUrl: 'https://www.jianguoyun.com/d/signup',
    },
    {
        id: 'infinicloud',
        name: 'InfiniCLOUD',
        description: 'Japanese cloud storage with WebDAV (25 GB free)',
        protocol: 'webdav',
        category: 'webdav',
        icon: 'Cloud',
        color: '#00A0E9',
        stable: true,
        fields: [
            {
                ...COMMON_FIELDS.server,
                label: 'WebDAV URL',
                placeholder: 'https://<node>.teracloud.jp',
                helpText: 'My Page → Apps Connection → your personal WebDAV URL',
            },
            {
                ...COMMON_FIELDS.username,
                label: 'User ID (Email)',
                placeholder: 'your@email.com',
            },
            {
                ...COMMON_FIELDS.password,
                label: 'Apps Password',
                helpText: 'My Page → Apps Connection → Generate Apps Password (not your login password)',
            },
        ],
        defaults: {
            port: 443,
            basePath: '/dav/',
        },
        features: {
            shareLink: false,
            sync: true,
        },
        helpUrl: 'https://infini-cloud.net/en/developer_webdav.html',
        signupUrl: 'https://infini-cloud.net/en/entry.html',
    },
    {
        id: 'seafile',
        name: 'Seafile',
        description: 'Open-source self-hosted cloud storage with WebDAV',
        protocol: 'webdav',
        category: 'webdav',
        icon: 'Cloud',
        color: '#E86C00',
        stable: true,
        fields: [
            { ...COMMON_FIELDS.server, placeholder: 'https://your-server.com/seafdav/' },
            { ...COMMON_FIELDS.username, placeholder: 'Your Seafile email' },
            { ...COMMON_FIELDS.password },
        ],
        defaults: {
            server: 'https://plus.seafile.com/seafdav/',
            port: 443,
        },
        features: {
            shareLink: false,
            sync: true,
        },
        helpUrl: 'https://manual.seafile.com/extension/webdav/',
        signupUrl: 'https://cloud.seafile.com/accounts/register/',
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
        signupUrl: 'https://nextcloud.com/sign-up/',
    },
    {
        id: 'mega',
        name: 'MEGA',
        description: 'Secure cloud storage with client-side encryption',
        protocol: 'mega',
        category: 'mega', // New category if needed, or 's3'/'webdav'. But Type says 'mega'
        icon: 'Cloud', // Or specific icon if available
        color: '#D9231E', // MEGA Red
        stable: true,
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
        signupUrl: 'https://mega.nz/register',
    },

    // =========================================================================
    // FILELU — Native REST API + FTP/FTPS/WebDAV/S3 presets
    // =========================================================================
    {
        id: 'filelu',
        name: 'FileLu',
        description: 'Cloud storage with FTP, WebDAV, S3 and native API (1 GB free)',
        protocol: 'filelu',
        category: 'oauth',
        icon: 'Cloud',
        color: '#8B5CF6',
        stable: true,
        fields: [
            {
                key: 'password',
                label: 'API Key',
                type: 'password',
                required: true,
                placeholder: 'Your FileLu API key',
                helpText: 'Account Settings → Developer API Key → switch ON to generate',
                group: 'credentials',
            },
        ],
        defaults: {},
        features: {
            shareLink: true,
            sync: true,
        },
        helpUrl: 'https://filelu.com/pages/api',
        signupUrl: 'https://filelu.com/5253515355.html',
    },
    {
        id: 'filelu-ftp',
        name: 'FileLu FTP',
        description: 'FileLu via FTP (port 21)',
        protocol: 'ftp',
        category: 'ftp',
        icon: 'Server',
        color: '#8B5CF6',
        stable: true,
        fields: [
            {
                key: 'username',
                label: 'FTP Login',
                type: 'text',
                required: true,
                placeholder: 'Your FileLu username',
                helpText: 'Account Settings → FTP Login',
                group: 'credentials',
            },
            {
                key: 'password',
                label: 'FTP Password',
                type: 'password',
                required: true,
                helpText: 'Account Settings → FTP Password (Account password by default)',
                group: 'credentials',
            },
        ],
        defaults: {
            server: 'ftp.filelu.com',
            port: 21,
        },
        features: {
            shareLink: false,
            sync: true,
        },
        helpUrl: 'https://filelu.com/account/',
        signupUrl: 'https://filelu.com/5253515355.html',
    },
    {
        id: 'filelu-ftps',
        name: 'FileLu FTPS',
        description: 'FileLu via secure FTPS Implicit (port 990)',
        protocol: 'ftps',
        category: 'ftp',
        icon: 'Lock',
        color: '#8B5CF6',
        stable: true,
        fields: [
            {
                key: 'username',
                label: 'FTP Login',
                type: 'text',
                required: true,
                placeholder: 'Your FileLu username',
                helpText: 'Account Settings → FTP Login',
                group: 'credentials',
            },
            {
                key: 'password',
                label: 'FTP Password',
                type: 'password',
                required: true,
                helpText: 'Account Settings → FTP Password',
                group: 'credentials',
            },
        ],
        defaults: {
            server: 'ftp.filelu.com',
            port: 990,
            tls_mode: 'implicit',
        },
        features: {
            shareLink: false,
            sync: true,
        },
        helpUrl: 'https://filelu.com/account/',
        signupUrl: 'https://filelu.com/5253515355.html',
    },
    {
        id: 'filelu-webdav',
        name: 'FileLu WebDAV',
        description: 'FileLu via WebDAV (enable in Account Settings)',
        protocol: 'webdav',
        category: 'webdav',
        icon: 'Globe',
        color: '#8B5CF6',
        stable: true,
        fields: [
            {
                key: 'username',
                label: 'Username',
                type: 'text',
                required: true,
                placeholder: 'Your FileLu username',
                group: 'credentials',
            },
            {
                key: 'password',
                label: 'Password',
                type: 'password',
                required: true,
                helpText: 'Your FileLu account password',
                group: 'credentials',
            },
        ],
        defaults: {
            server: 'https://webdav.filelu.com',
            port: 443,
        },
        features: {
            shareLink: false,
            sync: true,
        },
        helpUrl: 'https://filelu.com/account/',
        signupUrl: 'https://filelu.com/5253515355.html',
    },
    {
        id: 'filelu-s3',
        name: 'FileLu S5 (S3)',
        description: 'FileLu S3-compatible object storage (enable in Account Settings)',
        protocol: 's3',
        category: 's3',
        icon: 'Database',
        color: '#8B5CF6',
        stable: false,
        fields: [
            {
                ...COMMON_FIELDS.accessKeyId,
                helpText: 'Account Settings → FileLu S5 Object Storage → Access Key ID',
            },
            {
                ...COMMON_FIELDS.secretAccessKey,
                helpText: 'Account Settings → FileLu S5 Object Storage → Secret Access Key',
            },
            {
                ...COMMON_FIELDS.bucket,
                placeholder: 'my-filelu-bucket',
                helpText: 'Your FileLu S5 bucket name',
            },
            {
                key: 'region',
                label: 'Region',
                type: 'select',
                required: true,
                options: [
                    { value: 'global', label: 'Global (default)' },
                    { value: 'us-east', label: 'US East' },
                    { value: 'eu-central', label: 'EU Central' },
                    { value: 'ap-southeast', label: 'AP Southeast' },
                    { value: 'me-central', label: 'ME Central' },
                ],
                group: 'server',
            },
        ],
        defaults: {
            pathStyle: true,
            region: 'global',
            endpoint: 's5lu.com',
        },
        features: {
            shareLink: true,
            sync: true,
        },
        helpUrl: 'https://filelu.com/account/',
        signupUrl: 'https://filelu.com/5253515355.html',
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

/**
 * Resolve the S3 endpoint for a provider based on its endpointTemplate.
 * Supports {region} and {accountId} (and any other) template variables.
 * Returns null for providers without a template (e.g. Amazon S3 uses default AWS endpoint).
 */
export const resolveS3Endpoint = (providerId: string | undefined, region?: string, extraParams?: Record<string, string>): string | null => {
    if (!providerId) return null;
    const provider = providerRegistry.getById(providerId);
    if (!provider) return null;

    if (provider.defaults?.endpoint) {
        return provider.defaults.endpoint;
    }

    const template = provider?.defaults?.endpointTemplate;
    if (!template) return null;

    let result = template;
    if (region) result = result.replace('{region}', region);
    if (extraParams) {
        for (const [key, value] of Object.entries(extraParams)) {
            result = result.replace(`{${key}}`, value);
        }
    }
    // If still has unreplaced placeholders, return null
    if (result.includes('{')) return null;
    return result;
};
