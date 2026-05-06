// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

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
        placeholder: 'es. https://example.com',
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
        placeholder: 'es. https://s3.example.com',
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
            { ...COMMON_FIELDS.endpoint, placeholder: 'es. https://s3.region.provider.com' },
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
            {
                key: 'storage_class',
                label: 'Storage Class',
                type: 'select' as const,
                required: false,
                group: 'advanced' as const,
                options: [
                    { value: '', label: 'Default (Standard)' },
                    { value: 'STANDARD', label: 'Standard' },
                    { value: 'STANDARD_IA', label: 'Standard-IA (Infrequent Access)' },
                    { value: 'ONEZONE_IA', label: 'One Zone-IA' },
                    { value: 'INTELLIGENT_TIERING', label: 'Intelligent-Tiering' },
                    { value: 'GLACIER_IR', label: 'Glacier Instant Retrieval' },
                    { value: 'GLACIER', label: 'Glacier Flexible Retrieval' },
                    { value: 'DEEP_ARCHIVE', label: 'Glacier Deep Archive' },
                    { value: 'REDUCED_REDUNDANCY', label: 'Reduced Redundancy' },
                ],
                helpText: 'Default storage class for uploaded objects',
            },
            {
                key: 'sse_mode',
                label: 'Server-Side Encryption',
                type: 'select' as const,
                required: false,
                group: 'advanced' as const,
                options: [
                    { value: '', label: 'None (provider default)' },
                    { value: 'AES256', label: 'SSE-S3 (AES-256)' },
                    { value: 'aws:kms', label: 'SSE-KMS (AWS KMS)' },
                ],
                helpText: 'Encryption applied to uploaded objects',
            },
            {
                key: 'sse_kms_key_id',
                label: 'KMS Key ID',
                type: 'text' as const,
                required: false,
                group: 'advanced' as const,
                placeholder: 'arn:aws:kms:region:account:key/id',
                helpText: 'Optional KMS key ARN (uses default AWS-managed key if empty)',
            },
        ],
        features: {
            shareLink: true, // Presigned URLs
            sync: true,
        },
    },
    {
        id: 'uploadcare',
        name: 'Uploadcare',
        description: 'EU media management with CDN and file APIs',
        protocol: 'uploadcare',
        category: 'object',
        icon: 'Image',
        color: '#20d47d',
        stable: true,
        fields: [
            {
                key: 'username',
                label: 'Public API Key',
                type: 'text',
                required: true,
                placeholder: 'demopublickey...',
                helpText: 'Dashboard -> API Keys -> Public key',
                group: 'credentials',
            },
            {
                key: 'password',
                label: 'Secret API Key',
                type: 'password',
                required: true,
                placeholder: 'secret_...',
                helpText: 'Required for REST file management',
                group: 'credentials',
            },
        ],
        defaults: {
            server: 'api.uploadcare.com',
            port: 443,
        },
        features: {
            shareLink: false,
            sync: true,
            thumbnails: true,
        },
        healthCheckUrl: 'https://api.uploadcare.com',
        helpUrl: 'https://uploadcare.com/docs/start/settings/',
        signupUrl: 'https://uploadcare.com/accounts/signup/',
    },
    {
        id: 'cloudinary',
        name: 'Cloudinary',
        description: 'Media management CDN with REST API (25 credits/month free)',
        protocol: 'cloudinary',
        category: 'object',
        icon: 'Image',
        color: '#3448C5',
        stable: true,
        fields: [
            {
                key: 'cloud_name',
                label: 'Cloud Name',
                type: 'text',
                required: true,
                placeholder: 'dxz9abc12',
                helpText: 'Your Cloudinary cloud name (Dashboard -> Account Details)',
                group: 'credentials',
            },
            {
                key: 'username',
                label: 'API Key',
                type: 'text',
                required: true,
                placeholder: 'API key',
                helpText: 'Dashboard -> API Keys -> API Key',
                group: 'credentials',
            },
            {
                key: 'password',
                label: 'API Secret',
                type: 'password',
                required: true,
                placeholder: 'API secret',
                helpText: 'Dashboard -> API Keys -> API Secret',
                group: 'credentials',
            },
        ],
        defaults: {
            server: 'api.cloudinary.com',
            port: 443,
        },
        features: {
            shareLink: false,
            sync: true,
            thumbnails: true,
        },
        healthCheckUrl: 'https://api.cloudinary.com',
        helpUrl: 'https://cloudinary.com/documentation',
        signupUrl: 'https://cloudinary.com/users/register/free',
    },
    {
        id: 'imagekit',
        name: 'ImageKit',
        description: 'Media CDN + DAM storage with native folder operations',
        protocol: 'imagekit',
        category: 'object',
        icon: 'Image',
        color: '#1f6bff',
        stable: true,
        fields: [
            {
                key: 'username',
                label: 'URL Endpoint ID',
                type: 'text',
                required: true,
                placeholder: 'your_imagekit_id',
                helpText: 'Dashboard -> Developer Options -> URL endpoint',
                group: 'credentials',
            },
            {
                key: 'password',
                label: 'Private API Key',
                type: 'password',
                required: true,
                placeholder: 'private_...',
                helpText: 'Used server-side only via Basic Auth',
                group: 'credentials',
            },
        ],
        defaults: {
            server: 'api.imagekit.io',
            port: 443,
        },
        features: {
            shareLink: false,
            sync: true,
            thumbnails: true,
        },
        healthCheckUrl: 'https://api.imagekit.io',
        helpUrl: 'https://imagekit.io/docs/api-overview',
        signupUrl: 'https://imagekit.io/registration',
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
            { ...COMMON_FIELDS.server, placeholder: 'es. https://webdav.yourserver.com/', helpText: 'Full WebDAV URL with https://' },
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
            { ...COMMON_FIELDS.accessKeyId, helpText: 'IAM Console → Users → Security Credentials → Access Keys' },
            { ...COMMON_FIELDS.secretAccessKey, helpText: 'Shown only once at key creation' },
            { ...COMMON_FIELDS.bucket, placeholder: 'my-bucket', helpText: 'S3 Console → Buckets → Bucket name' },
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
        healthCheckUrl: 'https://s3.amazonaws.com',
        helpUrl: 'https://docs.aws.amazon.com/s3/',
        signupUrl: 'https://aws.amazon.com/free/',
    },
    {
        id: 'backblaze-native',
        name: 'Backblaze B2 (native)',
        description: 'Native B2 v4 API: large-file workflow, server-side copy, version history',
        protocol: 'backblaze',
        category: 'object',
        icon: 'Flame',
        color: '#E31C1C',
        stable: true,
        fields: [
            {
                key: 'username',
                label: 'Application Key ID',
                type: 'text',
                required: true,
                placeholder: '003d90ca9d33900000000001',
                helpText: 'B2 Application Key ID (starts with 003...): App Keys page',
            },
            {
                key: 'password',
                label: 'Application Key',
                type: 'password',
                required: true,
                helpText: 'B2 Application Key (shown only once at creation)',
            },
            {
                key: 'bucket',
                label: 'Bucket Name',
                type: 'text',
                required: true,
                placeholder: 'my-b2-bucket',
                helpText: 'Exact bucket name (case-sensitive)',
            },
        ],
        defaults: {
            // Native API discovers apiUrl/downloadUrl during b2_authorize_account.
            // No endpoint field needed.
        },
        features: {
            shareLink: false,
            sync: true,
        },
        healthCheckUrl: 'https://api.backblazeb2.com/b2api/v4/b2_authorize_account',
        helpUrl: 'https://www.backblaze.com/apidocs/',
        signupUrl: 'https://www.backblaze.com/sign-up/cloud-storage',
    },
    {
        id: 'backblaze',
        name: 'Backblaze B2 (S3-compat)',
        description: 'B2 via S3-compatible endpoint (legacy; prefer the native option above)',
        protocol: 's3',
        category: 's3',
        icon: 'Flame',
        color: '#E31C1C',
        stable: true,
        fields: [
            {
                ...COMMON_FIELDS.accessKeyId,
                label: 'Key ID',
                placeholder: '003d90ca9d33900000000001',
                helpText: 'Your B2 Application Key ID (starts with 003...)',
            },
            {
                ...COMMON_FIELDS.secretAccessKey,
                label: 'Application Key',
                helpText: 'Your B2 Application Key (hidden after creation)',
            },
            {
                ...COMMON_FIELDS.bucket,
                label: 'Bucket Name',
                placeholder: 'my-b2-bucket',
                helpText: 'The exact name of your B2 bucket',
            },
            {
                key: 'endpoint',
                label: 'Endpoint',
                type: 'url',
                required: true,
                placeholder: 'es. s3.eu-central-003.backblazeb2.com',
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
        healthCheckUrl: 'https://s3.us-west-004.backblazeb2.com',
        helpUrl: 'https://www.backblaze.com/b2/docs/',
        signupUrl: 'https://www.backblaze.com/sign-up/cloud-storage',
    },
    {
        id: 'mega-s4',
        name: 'MEGA S4 Object Storage',
        description: 'S3-compatible, Pro plan required, 4 regions (EU/CA)',
        protocol: 's3',
        category: 's3',
        icon: 'Cloud',
        color: '#D9272E',
        stable: true,
        contactVerified: true,
        fields: [
            { ...COMMON_FIELDS.accessKeyId, helpText: 'S4 Dashboard → Access Keys → Access Key ID' },
            { ...COMMON_FIELDS.secretAccessKey, helpText: 'S4 Dashboard → Access Keys → Secret Access Key' },
            { ...COMMON_FIELDS.bucket, placeholder: 'my-s4-bucket', helpText: 'S4 Dashboard → Buckets → Bucket name' },
            {
                key: 'region',
                label: 'Region',
                type: 'select',
                required: true,
                options: [
                    { value: 'eu-central-1', label: 'EU Central 1 (Amsterdam)' },
                    { value: 'eu-central-2', label: 'EU Central 2 (Luxembourg)' },
                    { value: 'ca-central-1', label: 'CA Central 1 (Montreal)' },
                    { value: 'ca-west-1', label: 'CA West 1 (Vancouver)' },
                ],
                group: 'server',
            },
        ],
        defaults: {
            pathStyle: false,
            endpointTemplate: 's3.{region}.s4.mega.io',
        },
        features: {
            shareLink: true,
            sync: true,
        },
        healthCheckUrl: 'https://s3.eu-central-1.s4.mega.io',
        helpUrl: 'https://help.mega.io/megas4/setup-guides/s3-browser-setup-guide-for-mega-s4',
        signupUrl: 'https://mega.nz/register',
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
        healthCheckUrl: 'https://www.cloudflare.com',
        helpUrl: 'https://developers.cloudflare.com/r2/',
        signupUrl: 'https://dash.cloudflare.com/sign-up',
    },
    {
        id: 'google-cloud-storage',
        name: 'Google Cloud Storage',
        description: 'Google Cloud S3-compatible object storage (5 GB free)',
        protocol: 's3',
        category: 's3',
        icon: 'Cloud',
        color: '#4285F4',
        stable: true,
        fields: [
            { ...COMMON_FIELDS.accessKeyId, label: 'Access Key', helpText: 'Cloud Storage → Settings → Interoperability → Access Key' },
            { ...COMMON_FIELDS.secretAccessKey, label: 'Secret', helpText: 'Cloud Storage → Settings → Interoperability → Secret' },
            { ...COMMON_FIELDS.bucket, placeholder: 'my-gcs-bucket', helpText: 'Cloud Storage → Buckets → Bucket name' },
            {
                key: 'region',
                label: 'Location',
                type: 'select' as const,
                required: true,
                options: [
                    { value: 'auto', label: 'Auto (recommended)' },
                    { value: 'us', label: 'US (multi-region)' },
                    { value: 'eu', label: 'EU (multi-region)' },
                    { value: 'asia', label: 'Asia (multi-region)' },
                    { value: 'us-central1', label: 'Iowa (us-central1)' },
                    { value: 'us-east1', label: 'South Carolina (us-east1)' },
                    { value: 'us-west1', label: 'Oregon (us-west1)' },
                    { value: 'europe-west1', label: 'Belgium (europe-west1)' },
                    { value: 'europe-west3', label: 'Frankfurt (europe-west3)' },
                    { value: 'asia-east1', label: 'Taiwan (asia-east1)' },
                    { value: 'asia-northeast1', label: 'Tokyo (asia-northeast1)' },
                ],
                group: 'server',
            },
        ],
        defaults: {
            pathStyle: true,
            region: 'auto',
            endpointTemplate: 'https://storage.googleapis.com',
        },
        features: {
            shareLink: true,
            sync: true,
        },
        healthCheckUrl: 'https://storage.googleapis.com',
        helpUrl: 'https://cloud.google.com/storage/docs/interoperability',
        signupUrl: 'https://cloud.google.com/free',
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
        contactVerified: true,
        fields: [
            { ...COMMON_FIELDS.accessKeyId, helpText: 'e2 Dashboard → Access Keys → Access Key ID' },
            { ...COMMON_FIELDS.secretAccessKey, helpText: 'e2 Dashboard → Access Keys → Secret Access Key' },
            { ...COMMON_FIELDS.bucket, placeholder: 'my-e2-bucket', helpText: 'e2 Dashboard → Buckets → Bucket name' },
            {
                key: 'endpoint',
                label: 'Region Endpoint',
                type: 'url',
                required: true,
                placeholder: 'es. l4g4.ch11.idrivee2-2.com',
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
        healthCheckUrl: 'https://www.idrive.com',
        helpUrl: 'https://www.idrive.com/s3-storage-e2/',
        signupUrl: 'https://console.idrivee2.com/signup',
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
            { ...COMMON_FIELDS.accessKeyId, helpText: 'Console → Access Keys → Access Key ID' },
            { ...COMMON_FIELDS.secretAccessKey, helpText: 'Console → Access Keys → Secret Access Key' },
            { ...COMMON_FIELDS.bucket, helpText: 'Console → Buckets → Bucket name' },
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
        healthCheckUrl: 'https://s3.wasabisys.com',
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
        contactVerified: true,
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
                    { value: 'https://gateway.storjshare.io', label: 'US1: North America' },
                    { value: 'https://gateway.eu1.storjshare.io', label: 'EU1: Europe' },
                    { value: 'https://gateway.ap1.storjshare.io', label: 'AP1: Asia-Pacific' },
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
        healthCheckUrl: 'https://gateway.storjshare.io',
        helpUrl: 'https://storj.dev/dcs/api/s3/s3-compatible-gateway',
        signupUrl: 'https://www.storj.io/signup',
    },
    {
        id: 'alibaba-oss',
        name: 'Alibaba Cloud OSS',
        description: 'S3-compatible object storage (China & global)',
        protocol: 's3',
        category: 's3',
        icon: 'Cloud',
        color: '#FF6A00',
        stable: true,
        fields: [
            { ...COMMON_FIELDS.accessKeyId, label: 'AccessKey ID', helpText: 'RAM Console → AccessKey Management → AccessKeyId' },
            { ...COMMON_FIELDS.secretAccessKey, label: 'AccessKey Secret', helpText: 'RAM Console → AccessKey Management → AccessKeySecret' },
            { ...COMMON_FIELDS.bucket, placeholder: 'my-oss-bucket', helpText: 'Bucket name from OSS Console → Bucket List' },
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
        healthCheckUrl: 'https://oss-us-west-1.aliyuncs.com',
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
        stable: true,
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
        healthCheckUrl: 'https://cos.ap-guangzhou.myqcloud.com',
        helpUrl: 'https://www.tencentcloud.com/document/product/436/32537',
        signupUrl: 'https://www.tencentcloud.com/account/register',
    },
    {
        id: 'filelu-s3',
        name: 'FileLu S5 (S3)',
        description: 'FileLu S3-compatible object storage (enable in Account Settings)',
        protocol: 's3',
        category: 's3',
        icon: 'Database',
        color: '#8B5CF6',
        stable: true,
        contactVerified: true,
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
        healthCheckUrl: 'https://filelu.com/api/',
        helpUrl: 'https://filelu.com/pages/faq/',
        signupUrl: 'https://filelu.com/register/',
    },
    {
        id: 'yandex-storage',
        name: 'Yandex Object Storage',
        description: 'S3-compatible cloud storage by Yandex Cloud (Russia)',
        protocol: 's3',
        category: 's3',
        icon: 'Cloud',
        color: '#FF6600',
        stable: true,
        fields: [
            { ...COMMON_FIELDS.accessKeyId, helpText: 'Yandex Cloud Console → Service Accounts → Static Access Keys → Key ID' },
            { ...COMMON_FIELDS.secretAccessKey, helpText: 'Yandex Cloud Console → Service Accounts → Static Access Keys → Secret Key' },
            { ...COMMON_FIELDS.bucket, helpText: 'Yandex Cloud Console → Object Storage → Bucket name' },
        ],
        defaults: {
            pathStyle: false,
            region: 'ru-central1',
            endpoint: 'https://storage.yandexcloud.net',
        },
        features: {
            shareLink: true,
            sync: true,
        },
        healthCheckUrl: 'https://storage.yandexcloud.net',
        helpUrl: 'https://yandex.cloud/en/docs/storage/',
        signupUrl: 'https://console.yandex.cloud/',
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
            { ...COMMON_FIELDS.accessKeyId, label: 'Spaces Key', helpText: 'API → Spaces Keys → Key' },
            { ...COMMON_FIELDS.secretAccessKey, label: 'Spaces Secret', helpText: 'API → Spaces Keys → Secret' },
            { ...COMMON_FIELDS.bucket, placeholder: 'my-space-name', label: 'Space Name', helpText: 'Spaces Object Storage → Create → Space name' },
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
        healthCheckUrl: 'https://nyc3.digitaloceanspaces.com',
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
            { ...COMMON_FIELDS.accessKeyId, label: 'Access Key', helpText: 'Identity → Users → Customer Secret Keys → Access Key' },
            { ...COMMON_FIELDS.secretAccessKey, label: 'Secret Key', helpText: 'Identity → Users → Customer Secret Keys → Secret Key' },
            { ...COMMON_FIELDS.bucket, placeholder: 'my-oci-bucket', helpText: 'Object Storage → Buckets → Bucket name' },
            {
                key: 'endpoint',
                label: 'S3 Endpoint',
                type: 'url',
                required: true,
                placeholder: 'es. <namespace>.compat.objectstorage.<region>.oraclecloud.com',
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
        healthCheckUrl: 'https://objectstorage.us-ashburn-1.oraclecloud.com',
        helpUrl: 'https://docs.oracle.com/en-us/iaas/Content/Object/Tasks/s3compatibleapi.htm',
        signupUrl: 'https://signup.cloud.oracle.com/',
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
            { ...COMMON_FIELDS.accessKeyId, placeholder: 'minioadmin', helpText: 'MinIO Console → Access Keys → Access Key' },
            { ...COMMON_FIELDS.secretAccessKey, helpText: 'MinIO Console → Access Keys → Secret Key' },
            { ...COMMON_FIELDS.bucket, placeholder: 'my-bucket', helpText: 'MinIO Console → Buckets → Bucket name' },
            {
                ...COMMON_FIELDS.endpoint,
                label: 'MinIO Endpoint',
                placeholder: 'es. minio.example.com:9000',
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
    {
        id: 'quotaless-s3',
        name: 'Quotaless S3',
        description: 'Quotaless cloud storage via S3-compatible API (MinIO)',
        protocol: 's3',
        category: 's3',
        icon: 'Database',
        color: '#2563EB',
        stable: true,
        fields: [
            {
                ...COMMON_FIELDS.accessKeyId,
                helpText: 'Client Area → S3 Account Details → Access Key ID',
            },
            {
                ...COMMON_FIELDS.secretAccessKey,
                helpText: 'Client Area → S3 Account Details → Secret Access Key',
            },
            {
                ...COMMON_FIELDS.bucket,
                placeholder: 'personal-files',
                helpText: 'Your Quotaless bucket name',
            },
        ],
        defaults: {
            pathStyle: true,
            region: 'us-east-1',
            endpoint: 'https://io.quotaless.cloud:8000',
            basePath: '/personal-files',
        },
        features: {
            shareLink: false,
            sync: true,
        },
        healthCheckUrl: 'https://io.quotaless.cloud:8000',
        helpUrl: 'https://quotaless.cloud/',
        signupUrl: 'https://quotaless.cloud/clientarea/index.php?rp=/login',
    },
    {
        id: 'filen-desktop-s3',
        name: 'Filen Desktop (local S3)',
        description: 'Local S3-compatible bridge to a logged-in Filen Desktop instance. Default port 1700, runs on 127.0.0.1 via local.s3.filen.io. Requires path-style addressing and bucket "filen".',
        protocol: 's3',
        category: 's3',
        icon: 'HardDrive',
        color: '#0033FF',
        stable: true,
        fields: [
            {
                ...COMMON_FIELDS.accessKeyId,
                label: 'Access key',
                placeholder: 'admin',
                helpText: 'Set in Filen Desktop > Network Drive > S3. Not your Filen account email.',
            },
            {
                ...COMMON_FIELDS.secretAccessKey,
                label: 'Secret key',
                helpText: 'Set in Filen Desktop > Network Drive > S3. Not your Filen account password.',
            },
        ],
        defaults: {
            endpoint: 'http://local.s3.filen.io:1700',
            region: 'filen',
            bucket: 'filen',
            pathStyle: true,
        },
        features: { shareLink: false, sync: true },
        healthCheckUrl: 'http://local.s3.filen.io:1700',
        helpUrl: 'https://docs.filen.io/docs/desktop/network-drive',
        signupUrl: 'https://filen.io',
        setupInstructions: [
            'Open Filen Desktop and sign in with your Filen account',
            'Go to Settings > Network Drive > S3',
            'Choose an access key and secret key (different from your account)',
            'Toggle "Enabled" and pick port 1700 (default) or a custom one',
            'Region must stay "filen" and bucket must stay "filen"',
            'Keep Filen Desktop running while you connect from AeroFTP',
        ],
    },
    {
        id: 's3drive',
        name: 'S3Drive',
        description: 'S3-compatible cloud built on Storj (12 GB free). Storage quota is not exposed by the standard S3 API.',
        protocol: 's3',
        category: 's3',
        icon: 'HardDrive',
        color: '#0E7490',
        stable: false,
        fields: [
            { ...COMMON_FIELDS.accessKeyId, placeholder: 'AKIA...', helpText: 'From S3Drive: open the "Setup with Rclone" page and copy the access_key_id value.' },
            { ...COMMON_FIELDS.secretAccessKey, helpText: 'From S3Drive: same "Setup with Rclone" page, copy the secret_access_key value.' },
            { ...COMMON_FIELDS.bucket, placeholder: 'your-s3drive-bucket', helpText: 'The bucket created for your S3Drive account (visible in the S3Drive desktop app or in your generated rclone.conf).' },
        ],
        defaults: {
            endpoint: 'https://storage.kapsa.io',
            region: 'us-east-1',
            pathStyle: true,
        },
        features: {
            shareLink: false,
            sync: true,
        },
        healthCheckUrl: 'https://storage.kapsa.io',
        helpUrl: 'https://docs.s3drive.app/Advanced/Setup-rclone/',
        signupUrl: 'https://s3drive.app',
        setupInstructions: [
            'Sign in to S3Drive (free plan includes 12 GB on Storj)',
            'In the S3Drive desktop app open Settings, then "Setup with Rclone" (or visit docs.s3drive.app/Advanced/Setup-rclone)',
            'Generate the rclone configuration to reveal the S3 credentials issued for your account',
            'Copy access_key_id, secret_access_key and the bucket name into the fields here',
            'Endpoint and Region come from the same rclone snippet: open "Advanced" below if your values differ from the defaults (storage.kapsa.io / us-east-1)',
        ],
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
        healthCheckUrl: 'https://webdav.4shared.com',
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
        healthCheckUrl: 'https://webdav.cloudme.com',
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
        healthCheckUrl: 'https://webdav.drivehq.com',
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
        contactVerified: true,
        fields: [
            { ...COMMON_FIELDS.username, label: 'Email', placeholder: 'email@example.com' },
            {
                ...COMMON_FIELDS.password,
                label: 'App Password',
                helpText: 'Koofr → Preferences → Password → App Passwords (not your login password)',
            },
        ],
        defaults: {
            // The server URL already contains `/dav/Koofr` in its path; the
            // remote base path must therefore be `/` so the joined request
            // URL stays `https://app.koofr.net/dav/Koofr/...` instead of
            // doubling up to `https://app.koofr.net/dav/Koofr/dav/Koofr/...`,
            // which Koofr rejects with "Invalid credentials". Issue #126.
            server: 'https://app.koofr.net/dav/Koofr',
            port: 443,
            basePath: '/',
        },
        features: {
            shareLink: false,
            sync: true,
        },
        healthCheckUrl: 'https://app.koofr.net/dav/Koofr',
        passwordGenUrl: 'https://app.koofr.net/app/admin/preferences/password',
        helpUrl: 'https://app.koofr.net/help/webdav',
        signupUrl: 'https://app.koofr.net/signup',
    },
    {
        id: 'megacmd-webdav',
        name: 'MEGAcmd (local WebDAV)',
        description: 'Local WebDAV bridge to MEGA via the official MEGAcmd CLI. Anonymous, runs on 127.0.0.1.',
        protocol: 'webdav',
        category: 'webdav',
        icon: 'Server',
        color: '#D9272E',
        stable: true,
        fields: [],
        defaults: {
            server: 'http://127.0.0.1:4443/',
            port: 4443,
            basePath: '/',
            anonymous: true,
        },
        features: {
            shareLink: false,
            sync: true,
        },
        healthCheckUrl: 'http://127.0.0.1:4443/',
        helpUrl: 'https://mega.io/cmd',
        signupUrl: 'https://mega.io/cmd',
        setupInstructions: [
            'Install MEGAcmd from https://mega.io/cmd',
            'Open the MEGAcmd terminal',
            'Run: login your-email@example.com',
            'Run: webdav /',
            'Close the terminal; the WebDAV server keeps running in the background',
        ],
    },
    {
        id: 'filen-desktop-webdav',
        name: 'Filen Desktop (local WebDAV)',
        description: 'Local WebDAV bridge to a logged-in Filen Desktop instance. Default port 1900, runs on 127.0.0.1 via local.webdav.filen.io.',
        protocol: 'webdav',
        category: 'webdav',
        icon: 'Server',
        color: '#0033FF',
        stable: true,
        fields: [
            {
                ...COMMON_FIELDS.username,
                label: 'WebDAV username',
                placeholder: 'admin',
                helpText: 'Set in Filen Desktop > Network Drive > WebDAV. Not your Filen account email.',
            },
            {
                ...COMMON_FIELDS.password,
                label: 'WebDAV password',
                helpText: 'Set in Filen Desktop > Network Drive > WebDAV. Not your Filen account password.',
            },
        ],
        defaults: {
            server: 'local.webdav.filen.io',
            port: 1900,
            basePath: '/',
            webdavScheme: 'http',
        },
        features: { shareLink: false, sync: true },
        healthCheckUrl: 'http://local.webdav.filen.io:1900',
        helpUrl: 'https://docs.filen.io/docs/desktop/network-drive',
        signupUrl: 'https://filen.io',
        setupInstructions: [
            'Open Filen Desktop and sign in with your Filen account',
            'Go to Settings > Network Drive > WebDAV',
            'Choose a username and password (different from your account)',
            'Toggle "Enabled" and pick port 1900 (default) or a custom one',
            'Keep Filen Desktop running while you connect from AeroFTP',
        ],
    },
    {
        id: 'opendrive-webdav',
        name: 'OpenDrive (WebDAV)',
        description: 'OpenDrive cloud storage via WebDAV',
        protocol: 'webdav',
        category: 'webdav',
        icon: 'Cloud',
        color: '#0099CC',
        stable: true,
        fields: [
            { ...COMMON_FIELDS.username, label: 'Email', placeholder: 'email@example.com' },
            { ...COMMON_FIELDS.password, label: 'Password', helpText: 'Your regular OpenDrive login password' },
        ],
        defaults: {
            server: 'https://webdav.opendrive.com',
            port: 443,
            basePath: '/',
        },
        features: { shareLink: false, sync: true },
        healthCheckUrl: 'https://webdav.opendrive.com',
        helpUrl: 'https://www.opendrive.com/webdav',
        signupUrl: 'https://www.opendrive.com',
    },
    {
        id: 'yandexdisk-webdav',
        name: 'Yandex Disk (WebDAV)',
        description: 'Yandex Disk cloud storage via WebDAV (5 GB free)',
        protocol: 'webdav',
        category: 'webdav',
        icon: 'Cloud',
        color: '#FFCC00',
        stable: true,
        fields: [
            { ...COMMON_FIELDS.username, label: 'Email', placeholder: 'your@yandex.com' },
            { ...COMMON_FIELDS.password, label: 'App Password', helpText: 'Generate an app password at id.yandex.com (not your login password)' },
        ],
        defaults: {
            server: 'https://webdav.yandex.ru',
            port: 443,
            basePath: '/',
        },
        features: { shareLink: false, sync: true },
        healthCheckUrl: 'https://webdav.yandex.ru',
        helpUrl: 'https://yandex.com/support/disk/',
        signupUrl: 'https://passport.yandex.com/registration',
        passwordGenUrl: 'https://id.yandex.com/security/app-passwords',
    },
    {
        id: 'jianguoyun',
        name: 'Jianguoyun',
        description: 'Popular Chinese cloud storage with WebDAV (3 GB free)',
        protocol: 'webdav',
        category: 'webdav',
        contactVerified: true,
        icon: 'Cloud',
        color: '#3A9BDC',
        stable: true,
        fields: [
            {
                ...COMMON_FIELDS.username,
                label: 'Email',
                placeholder: 'email@example.com',
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
        healthCheckUrl: 'https://dav.jianguoyun.com',
        helpUrl: 'https://help.jianguoyun.com/?p=2064',
        signupUrl: 'https://www.jianguoyun.com/d/signup',
    },
    {
        id: 'infinicloud',
        name: 'InfiniCLOUD',
        description: 'Japanese cloud storage with WebDAV (25 GB free)',
        protocol: 'webdav',
        category: 'webdav',
        contactVerified: true,
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
                placeholder: 'email@example.com',
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
        healthCheckUrl: 'https://infini-cloud.net',
        helpUrl: 'https://infini-cloud.net/en/developer_webdav.html',
        signupUrl: 'https://account.teracloud.jp/RegistForm.php/index/',
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
            { ...COMMON_FIELDS.server, placeholder: 'es. https://your-server.com/seafdav/' },
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
        healthCheckUrl: 'https://cloud.seafile.com',
        helpUrl: 'https://manual.seafile.com/latest/',
        signupUrl: 'https://cloud.seafile.com/accounts/register/',
    },
    {
        id: 'hetzner-storage-box',
        name: 'Hetzner Storage Box',
        description: 'Hetzner online storage with SFTP access (port 23)',
        protocol: 'sftp',
        category: 'ftp',
        icon: 'HardDrive',
        color: '#D50C2D',
        stable: true,
        fields: [
            {
                ...COMMON_FIELDS.username,
                placeholder: 'u123456',
                helpText: 'Your Storage Box username (e.g. u123456)',
            },
            { ...COMMON_FIELDS.password },
            {
                key: 'server',
                label: 'Server',
                type: 'text' as const,
                required: true,
                placeholder: 'es. u123456.your-storagebox.de',
                helpText: 'Format: {username}.your-storagebox.de',
                group: 'server' as const,
            },
        ],
        defaults: {
            port: 23,
        },
        features: {
            sync: true,
        },
        helpUrl: 'https://www.hetzner.com/storage/storage-box/',
        signupUrl: 'https://accounts.hetzner.com/login',
    },
    {
        id: 'sourceforge',
        name: 'SourceForge',
        description: 'Upload releases to SourceForge File Release System via SFTP',
        protocol: 'sftp',
        category: 'ftp',
        icon: 'Package',
        color: '#FF6600',
        stable: true,
        contactVerified: true,
        fields: [
            {
                ...COMMON_FIELDS.username,
                placeholder: 'your-sf-username',
                helpText: 'Your SourceForge username',
            },
            {
                ...COMMON_FIELDS.password,
                helpText: 'Password or use SSH key authentication',
            },
        ],
        defaults: {
            server: 'frs.sourceforge.net',
            port: 22,
            basePath: '/home/frs/project/',
        },
        features: {
            sync: false,
        },
        healthCheckUrl: 'https://sourceforge.net',
        helpUrl: 'https://docs.aeroftp.app/providers/sourceforge',
        signupUrl: 'https://sourceforge.net/user/registration',
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
        contactVerified: true,
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
        healthCheckUrl: 'https://filelu.com/api/',
        helpUrl: 'https://filelu.com/pages/faq/',
        signupUrl: 'https://filelu.com/register/',
    },
    {
        id: 'quotaless-webdav',
        name: 'Quotaless WebDAV',
        description: 'Quotaless cloud storage via WebDAV (ownCloud)',
        protocol: 'webdav',
        category: 'webdav',
        icon: 'Globe',
        color: '#2563EB',
        stable: true,
        fields: [
            { ...COMMON_FIELDS.username, placeholder: 'Your Quotaless username' },
            { ...COMMON_FIELDS.password },
        ],
        defaults: {
            server: 'https://io.quotaless.cloud:8080/webdav',
            port: 8080,
        },
        features: {
            shareLink: false,
            sync: true,
        },
        healthCheckUrl: 'https://io.quotaless.cloud:8000',
        helpUrl: 'https://quotaless.cloud/',
        signupUrl: 'https://quotaless.cloud/clientarea/index.php?rp=/login',
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
                placeholder: 'es. https://cloud.example.com'
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
        id: 'felicloud',
        name: 'Felicloud',
        description: 'Felicloud (10 GB free, Nextcloud-based, EU/GDPR)',
        protocol: 'webdav',
        category: 'webdav',
        icon: 'Cloud',
        color: '#2196F3',
        stable: true,
        contactVerified: true,
        fields: [
            {
                ...COMMON_FIELDS.username,
                label: 'Email',
                placeholder: 'email@example.com',
            },
            {
                ...COMMON_FIELDS.password,
                label: 'Password or App Token',
                helpText: 'Use an App Token from Settings → Security for better security',
            },
        ],
        defaults: {
            server: 'https://cloud.felicloud.com/remote.php/dav/files/{username}/',
            port: 443,
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
        healthCheckUrl: 'https://cloud.felicloud.com',
        helpUrl: 'https://felicloud.com/en/help/',
        signupUrl: 'https://felicloud.com/en/signup/',
    },
    {
        id: 'blomp',
        name: 'Blomp',
        description: 'Blomp (40 GB free, OpenStack Swift)',
        protocol: 'swift',
        category: 'swift',
        icon: 'Cloud',
        color: '#7C3AED',
        stable: false, // Waiting for Blomp support: storage proxy returns 403
        fields: [
            {
                key: 'username',
                label: 'Email',
                type: 'email',
                required: true,
                placeholder: 'your@blomp.com',
                group: 'credentials',
            },
            {
                key: 'password',
                label: 'Password',
                type: 'password',
                required: true,
                group: 'credentials',
            },
        ],
        defaults: {
            server: 'https://authenticate.blomp.com',
            port: 443,
        },
        features: {
            shareLink: false,
            sync: true,
            versioning: false,
            trash: false,
        },
        healthCheckUrl: 'https://authenticate.blomp.com',
        helpUrl: 'https://www.blomp.com/faq/',
        signupUrl: 'https://www.blomp.com/sign-up/',
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
        contactVerified: true,
        fields: [
            {
                key: 'username',
                label: 'Email',
                type: 'email',
                required: true,
                placeholder: 'email@example.com',
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
            mega_mode: 'native',
        },
        features: {
            shareLink: true, // Assuming MEGA supports it
            sync: true,
            thumbnails: true, // Special feature
        },
        healthCheckUrl: 'https://g.api.mega.co.nz',
        helpUrl: 'https://mega.io/help',
        signupUrl: 'https://mega.nz/register',
    },

    // =========================================================================
    // FILELU: Native REST API + FTP/FTPS/WebDAV/S3 presets
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
        contactVerified: true,
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
        healthCheckUrl: 'https://filelu.com/api/',
        helpUrl: 'https://filelu.com/pages/api',
        signupUrl: 'https://filelu.com/register/',
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
        contactVerified: true,
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
        healthCheckUrl: 'https://filelu.com/api/',
        helpUrl: 'https://filelu.com/pages/faq/',
        signupUrl: 'https://filelu.com/register/',
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
        contactVerified: true,
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
        healthCheckUrl: 'https://filelu.com/api/',
        helpUrl: 'https://filelu.com/pages/faq/',
        signupUrl: 'https://filelu.com/register/',
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
            swift: [],
            object: [],
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
