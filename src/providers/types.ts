/**
 * Provider Types - Extensible architecture for cloud storage providers
 * 
 * Supports both pre-configured providers (Backblaze, Nextcloud, etc.)
 * and generic/custom connections for any WebDAV or S3-compatible service.
 */

// ============================================================================
// Protocol Types
// ============================================================================

export type BaseProtocol = 'ftp' | 'sftp' | 's3' | 'webdav' | 'googledrive' | 'dropbox' | 'onedrive' | 'mega';

export type ProviderCategory = 'ftp' | 'oauth' | 's3' | 'webdav' | 'mega';

// ============================================================================
// Provider Field Configuration
// ============================================================================

export interface ProviderField {
    /** Unique key for this field (maps to connection params) */
    key: string;

    /** Display label (i18n key or plain text) */
    label: string;

    /** Placeholder text */
    placeholder?: string;

    /** Field type */
    type: 'text' | 'password' | 'number' | 'select' | 'checkbox' | 'url' | 'email';

    /** Is this field required? */
    required: boolean;

    /** Help text shown below field */
    helpText?: string;

    /** Default value */
    defaultValue?: string | number | boolean;

    /** Options for select type */
    options?: Array<{ value: string; label: string }>;

    /** Validation pattern (regex) */
    pattern?: string;

    /** Group this field belongs to (for UI organization) */
    group?: 'credentials' | 'server' | 'advanced';
}

// ============================================================================
// Provider Configuration
// ============================================================================

export interface ProviderConfig {
    /** Unique provider ID (lowercase, no spaces) */
    id: string;

    /** Display name */
    name: string;

    /** Short description */
    description?: string;

    /** Base protocol used */
    protocol: BaseProtocol;

    /** Provider category for UI grouping */
    category: ProviderCategory;

    /** Icon name (from lucide-react or custom) */
    icon?: string;

    /** Brand color (hex) */
    color?: string;

    /** Logo URL or component name */
    logo?: string;

    /** Is this a generic/custom provider? */
    isGeneric?: boolean;

    /** Form fields for connection */
    fields: ProviderField[];

    /** Default values for connection params */
    defaults?: {
        server?: string;
        port?: number;
        pathStyle?: boolean;    // S3 path-style access
        region?: string;        // S3 region
        basePath?: string;      // WebDAV base path
        save_session?: boolean; // MEGA save session
    };

    /** API endpoints for provider-specific features */
    endpoints?: {
        /** Share link API endpoint */
        shareLink?: string;

        /** Auth endpoint (for custom OAuth) */
        auth?: string;

        /** WebDAV base path template */
        webdavPath?: string;
    };

    /** Feature flags */
    features?: {
        /** Supports share link generation */
        shareLink?: boolean;

        /** Supports folder sync */
        sync?: boolean;

        /** Supports versioning */
        versioning?: boolean;

        /** Supports trash/recycle bin */
        trash?: boolean;

        /** Native thumbnails support */
        thumbnails?: boolean;
    };

    /** Documentation/help URL */
    helpUrl?: string;

    /** Whether this provider is fully tested/stable */
    stable?: boolean;
}

// ============================================================================
// Connection Parameters (runtime)
// ============================================================================

export interface ProviderConnectionParams {
    /** Provider ID (or 'custom-webdav', 'custom-s3') */
    providerId: string;

    /** Base protocol */
    protocol: BaseProtocol;

    /** Server hostname */
    server: string;

    /** Port number */
    port?: number;

    /** Username or Access Key */
    username: string;

    /** Password or Secret Key */
    password: string;

    /** Display name for this connection */
    displayName?: string;

    /** S3-specific options */
    s3Options?: {
        bucket: string;
        region: string;
        endpoint?: string;
        pathStyle?: boolean;
    };

    /** WebDAV-specific options */
    webdavOptions?: {
        basePath?: string;
        useHttps?: boolean;
    };

    /** Initial path to navigate to */
    initialPath?: string;
}

// ============================================================================
// Provider Registry Interface
// ============================================================================

export interface ProviderRegistry {
    /** Get all registered providers */
    getAll(): ProviderConfig[];

    /** Get providers by category */
    getByCategory(category: ProviderCategory): ProviderConfig[];

    /** Get a specific provider by ID */
    getById(id: string): ProviderConfig | undefined;

    /** Get the generic/custom provider for a protocol */
    getGeneric(protocol: BaseProtocol): ProviderConfig | undefined;

    /** Check if share link is supported for a provider */
    supportsShareLink(providerId: string): boolean;
}
