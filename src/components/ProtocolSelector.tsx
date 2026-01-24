/**
 * ProtocolSelector Component
 * Dropdown for selecting storage protocol (FTP, WebDAV, S3, etc.)
 */

import React from 'react';
import {
    Server,
    Cloud,
    Database,
    Lock,
    ShieldCheck,
    HardDrive,
    ChevronDown
} from 'lucide-react';
import { ProviderType } from '../types';

// Official brand logos as inline SVGs
const GoogleDriveLogo: React.FC<{ size?: number; className?: string }> = ({ size = 16, className = '' }) => (
    <svg width={size} height={size} viewBox="0 0 87.3 78" className={className}>
        <path fill="#0066da" d="M6.6 66.85l3.85 6.65c.8 1.4 1.95 2.5 3.3 3.3l13.75-23.8H0c0 1.55.4 3.1 1.2 4.5l5.4 9.35z" />
        <path fill="#00ac47" d="M43.65 25L29.9 1.2c-1.35.8-2.5 1.9-3.3 3.3L1.2 52.35c-.8 1.4-1.2 2.95-1.2 4.5h27.5L43.65 25z" />
        <path fill="#ea4335" d="M73.55 76.8c1.35-.8 2.5-1.9 3.3-3.3l1.6-2.75 7.65-13.25c.8-1.4 1.2-2.95 1.2-4.5H59.85L73.55 76.8z" />
        <path fill="#00832d" d="M43.65 25L57.4 1.2c-1.35-.8-2.9-1.2-4.5-1.2H34.35c-1.6 0-3.15.45-4.45 1.2L43.65 25z" />
        <path fill="#2684fc" d="M59.85 53H27.5L13.75 76.8c1.35.8 2.9 1.2 4.5 1.2h50.8c1.6 0 3.15-.45 4.5-1.2L59.85 53z" />
        <path fill="#ffba00" d="M73.4 26.5l-12.7-22c-.8-1.4-1.95-2.5-3.3-3.3L43.65 25l16.2 28h27.45c0-1.55-.4-3.1-1.2-4.5l-12.7-22z" />
    </svg>
);

const DropboxLogo: React.FC<{ size?: number; className?: string }> = ({ size = 16, className = '' }) => (
    <svg width={size} height={size} viewBox="0 0 43 40" className={className}>
        <path fill="#0061ff" d="M12.5 0L0 8.1l8.5 6.9 12.5-8.2L12.5 0zM0 22l12.5 8.1 8.5-6.8-12.5-8.2L0 22zm21 1.3l8.5 6.8L42 22l-8.5-6.9-12.5 8.2zm21-15.2L29.5 0 21 6.8l12.5 8.2L42 8.1zM21.1 24.4l-8.6 6.9-3.9-2.6v2.9l12.5 7.5 12.5-7.5v-2.9l-3.9 2.6-8.6-6.9z" />
    </svg>
);

const OneDriveLogo: React.FC<{ size?: number; className?: string }> = ({ size = 16, className = '' }) => (
    <svg width={size} height={size} viewBox="0 0 24 24" className={className}>
        <path fill="#0364b8" d="M14.5 15h6.78l.72-.53V14c0-2.48-1.77-4.6-4.17-5.05A5.5 5.5 0 0 0 7.5 10.5v.5H7c-2.21 0-4 1.79-4 4s1.79 4 4 4h7.5z" />
        <path fill="#0078d4" d="M9.5 10.5A5.5 5.5 0 0 1 17.83 8.95 5.5 5.5 0 0 0 14.5 15H7c-2.21 0-4-1.79-4-4s1.79-4 4-4h.5v.5c0 1.66.74 3.15 1.9 4.15.4-.08.8-.15 1.1-.15z" />
        <path fill="#1490df" d="M21.28 14.47l-.78.53H14.5 7c-2.21 0-4-1.79-4-4a3.99 3.99 0 0 1 2.4-3.67A4 4 0 0 1 9 6c.88 0 1.7.29 2.36.78A5.49 5.49 0 0 1 17.83 9a5 5 0 0 1 3.45 5.47z" />
        <path fill="#28a8ea" d="M21.28 14.47A5 5 0 0 0 17.83 9a5.49 5.49 0 0 0-6.47-1.22A4 4 0 0 0 5.4 10.33c-.35.11-.68.28-.98.5a4.49 4.49 0 0 0 2.08 4.67H14.5h6.78z" />
    </svg>
);

const AwsS3Logo: React.FC<{ size?: number; className?: string }> = ({ size = 16, className = '' }) => (
    <svg width={size} height={size} viewBox="0 0 24 24" className={className}>
        <path fill="#e25444" d="M12 2L3 7v10l9 5 9-5V7l-9-5z" />
        <path fill="#7b1d13" d="M12 2v20l9-5V7l-9-5z" />
        <path fill="#58150d" d="M12 22l-9-5V7l9 5v10z" />
        <path fill="#ffffff" d="M12 12L3 7l9-5 9 5-9 5z" />
    </svg>
);

const MegaLogo: React.FC<{ size?: number; className?: string }> = ({ size = 16, className = '' }) => (
    <svg width={size} height={size} viewBox="0 0 24 24" className={className}>
        <circle cx="12" cy="12" r="12" fill="#D9272E" />
        <path fill="#ffffff" d="M6.5 16V8h1.8l2.2 4.5L12.7 8h1.8v8h-1.5v-5.2l-1.8 3.7h-1.4l-1.8-3.7V16H6.5z" />
    </svg>
);

interface ProtocolSelectorProps {
    value: ProviderType | '' | undefined;
    onChange: (protocol: ProviderType) => void;
    disabled?: boolean;
    className?: string;
    showLabel?: boolean;
    onOpenChange?: (isOpen: boolean) => void;
}

interface ProtocolInfo {
    type: ProviderType;
    name: string;
    icon: React.ReactNode;
    description: string;
    defaultPort: number;
    badge?: string;
    isOAuth?: boolean;
    color?: string;
    isCloudStorage?: boolean;  // For cloud providers (AeroCloud, OAuth, etc.)
    tooltip?: string;  // Tooltip on hover
    disabled?: boolean;  // If true, show as coming soon
}

const PROTOCOLS: ProtocolInfo[] = [
    // Traditional Server Protocols
    {
        type: 'ftp',
        name: 'FTP',
        icon: <Server size={16} />,
        description: 'File Transfer Protocol',
        defaultPort: 21,
        color: 'text-blue-500',
        tooltip: 'Standard FTP connection - unencrypted, port 21',
    },
    {
        type: 'ftps',
        name: 'FTPS',
        icon: <ShieldCheck size={16} />,
        description: 'FTP over TLS/SSL',
        defaultPort: 990,
        badge: 'Secure',
        color: 'text-green-500',
        tooltip: 'FTP with TLS/SSL encryption - secure connection',
    },
    {
        type: 'sftp',
        name: 'SFTP',
        icon: <Lock size={16} />,
        description: 'SSH File Transfer',
        defaultPort: 22,
        badge: 'Secure',
        color: 'text-emerald-500',
        tooltip: 'SFTP over SSH - secure shell file transfer, supports key authentication',
    },
    {
        type: 'webdav',
        name: 'WebDAV',
        icon: <Cloud size={16} />,
        description: 'Nextcloud, ownCloud, Synology',
        defaultPort: 443,
        badge: 'Beta',
        color: 'text-orange-500',
        tooltip: 'WebDAV protocol - compatible with Nextcloud, ownCloud, Synology NAS',
    },
    {
        type: 's3',
        name: 'S3',
        icon: <AwsS3Logo size={18} />,
        description: 'AWS S3, MinIO, R2, B2',
        defaultPort: 443,
        badge: 'Beta',
        color: 'text-amber-600',
        tooltip: 'S3-compatible storage - AWS S3, MinIO, Cloudflare R2, Backblaze B2',
    },
    // Cloud Storage Providers (AeroCloud FIRST!)
    {
        type: 'aerocloud',
        name: 'AeroCloud',
        icon: <Cloud size={18} className="text-sky-400" />,
        description: 'Personal FTP-based cloud',
        defaultPort: 21,
        badge: 'Sync',
        color: 'text-sky-500',
        isCloudStorage: true,
        tooltip: 'Turn any FTP server into your personal cloud with automatic sync',
    },
    {
        type: 'googledrive',
        name: 'Google Drive',
        icon: <GoogleDriveLogo size={18} />,
        description: 'Connect with Google Account',
        defaultPort: 443,
        badge: 'OAuth',
        isOAuth: true,
        isCloudStorage: true,
        tooltip: 'Google Drive - 15GB free storage, OAuth2 authentication',
    },
    {
        type: 'dropbox',
        name: 'Dropbox',
        icon: <DropboxLogo size={18} />,
        description: 'Connect with Dropbox Account',
        defaultPort: 443,
        badge: 'OAuth',
        isOAuth: true,
        isCloudStorage: true,
        tooltip: 'Dropbox - 2GB free storage, OAuth2 authentication',
    },
    {
        type: 'onedrive',
        name: 'OneDrive',
        icon: <OneDriveLogo size={18} />,
        description: 'Connect with Microsoft Account',
        defaultPort: 443,
        badge: 'OAuth',
        isOAuth: true,
        isCloudStorage: true,
        tooltip: 'Microsoft OneDrive - 5GB free storage, OAuth2 authentication',
    },
    {
        type: 'mega',
        name: 'MEGA',
        icon: <MegaLogo size={18} />,
        description: 'Secure Cloud Storage',
        defaultPort: 443,
        badge: 'Secure',
        color: 'text-red-600',
        isCloudStorage: true,
        tooltip: 'MEGA.nz - Client-side encryption, 20GB free',
    },
];

export const getProtocolInfo = (type: ProviderType | ''): ProtocolInfo | null => {
    if (!type) return null;
    return PROTOCOLS.find(p => p.type === type) || PROTOCOLS[0];
};

export const getDefaultPort = (type: ProviderType): number => {
    return getProtocolInfo(type)?.defaultPort || 21;
};

export const isOAuthProtocol = (type: ProviderType): boolean => {
    return getProtocolInfo(type)?.isOAuth ?? false;
};

export const ProtocolSelector: React.FC<ProtocolSelectorProps> = ({
    value,
    onChange,
    disabled = false,
    className = '',
    showLabel = true,
    onOpenChange,
}) => {
    const selectedProtocol = value ? getProtocolInfo(value) : null;
    const [isOpen, setIsOpen] = React.useState(false);

    // Notify parent when isOpen changes
    const handleOpenChange = (newIsOpen: boolean) => {
        setIsOpen(newIsOpen);
        onOpenChange?.(newIsOpen);
    };

    return (
        <div className={className}>
            {showLabel && (
                <label className="block text-sm font-medium mb-1.5">Protocol</label>
            )}

            {/* Custom dropdown button */}
            <div className="relative">
                <button
                    type="button"
                    onClick={() => !disabled && handleOpenChange(!isOpen)}
                    disabled={disabled}
                    className="w-full px-4 py-3 pl-10 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-xl text-left cursor-pointer focus:ring-2 focus:ring-blue-500 focus:border-transparent flex items-center justify-between"
                >
                    <span className={selectedProtocol ? '' : 'text-gray-400'}>
                        {selectedProtocol
                            ? `${selectedProtocol.name} - ${selectedProtocol.description}`
                            : 'Select protocol...'}
                    </span>
                    <ChevronDown size={16} className={`transition-transform ${isOpen ? 'rotate-180' : ''}`} />
                </button>
                <div className="absolute left-3 top-1/2 -translate-y-1/2 text-gray-500 dark:text-gray-400 pointer-events-none">
                    {selectedProtocol ? selectedProtocol.icon : <Server size={16} />}
                </div>
            </div>

            {/* Protocol Grid (always visible when no selection or dropdown open) */}
            {(isOpen || !value) && (
                <div className="mt-3 grid grid-cols-2 gap-2">
                    {/* Traditional Server Protocols */}
                    <div className="col-span-2">
                        <p className="text-xs text-gray-500 uppercase tracking-wide mb-2">Servers</p>
                    </div>
                    {PROTOCOLS.filter(p => !p.isCloudStorage).map((protocol) => (
                        <button
                            key={protocol.type}
                            type="button"
                            onClick={() => {
                                if (!protocol.disabled) {
                                    onChange(protocol.type);
                                    handleOpenChange(false);
                                }
                            }}
                            disabled={disabled || protocol.disabled}
                            title={protocol.tooltip}
                            className={`flex items-center gap-2 p-2.5 rounded-lg border transition-all text-left
                                ${protocol.disabled
                                    ? 'opacity-50 cursor-not-allowed border-gray-200 dark:border-gray-700 bg-gray-50 dark:bg-gray-800'
                                    : value === protocol.type
                                        ? 'border-blue-500 bg-blue-50 dark:bg-blue-900/30'
                                        : 'border-gray-200 dark:border-gray-600 hover:border-blue-300 hover:bg-gray-50 dark:hover:bg-gray-700'}
                            `}
                        >
                            <div className={`flex-shrink-0 ${protocol.disabled ? 'grayscale' : ''} ${protocol.color || ''}`}>
                                {protocol.icon}
                            </div>
                            <div className="min-w-0 flex-1">
                                <div className="font-medium text-sm">{protocol.name}</div>
                                <div className="text-xs text-gray-500 truncate">{protocol.description}</div>
                            </div>
                            {protocol.badge && (
                                <span className={`text-[10px] px-1.5 py-0.5 rounded ${protocol.badge === 'Secure'
                                    ? 'bg-green-100 text-green-700 dark:bg-green-900 dark:text-green-300'
                                    : protocol.badge === 'Beta'
                                        ? 'bg-orange-100 text-orange-700 dark:bg-orange-900 dark:text-orange-300'
                                        : protocol.badge === 'Soon'
                                            ? 'bg-yellow-100 text-yellow-700 dark:bg-yellow-900 dark:text-yellow-300'
                                            : 'bg-gray-100 text-gray-600 dark:bg-gray-700 dark:text-gray-400'
                                    }`}>
                                    {protocol.badge}
                                </span>
                            )}
                        </button>
                    ))}

                    {/* Cloud Storage Providers (AeroCloud first!) */}
                    <div className="col-span-2 mt-2">
                        <p className="text-xs text-gray-500 uppercase tracking-wide mb-2">Cloud Storage</p>
                    </div>
                    {PROTOCOLS.filter(p => p.isCloudStorage).map((protocol) => (
                        <button
                            key={protocol.type}
                            type="button"
                            onClick={() => {
                                if (!protocol.disabled) {
                                    onChange(protocol.type);
                                    handleOpenChange(false);
                                }
                            }}
                            disabled={disabled || protocol.disabled}
                            title={protocol.tooltip}
                            className={`flex items-center gap-2 p-2.5 rounded-lg border transition-all text-left
                                ${protocol.disabled
                                    ? 'opacity-50 cursor-not-allowed border-gray-200 dark:border-gray-700 bg-gray-50 dark:bg-gray-800'
                                    : value === protocol.type
                                        ? 'border-blue-500 bg-blue-50 dark:bg-blue-900/30'
                                        : protocol.type === 'aerocloud'
                                            ? 'border-sky-200 dark:border-sky-700 hover:border-sky-400 bg-sky-50/50 dark:bg-sky-900/20 hover:bg-sky-50 dark:hover:bg-sky-900/30'
                                            : 'border-gray-200 dark:border-gray-600 hover:border-blue-300 hover:bg-gray-50 dark:hover:bg-gray-700'}
                            `}
                        >
                            <div className={`flex-shrink-0 ${protocol.disabled ? 'grayscale' : ''} ${protocol.color || ''}`}>
                                {protocol.icon}
                            </div>
                            <div className="min-w-0 flex-1">
                                <div className="font-medium text-sm">{protocol.name}</div>
                                <div className="text-xs text-gray-500 truncate">{protocol.description}</div>
                            </div>
                            {protocol.badge && (
                                <span className={`text-[10px] px-1.5 py-0.5 rounded ${protocol.badge === 'Sync'
                                    ? 'bg-sky-100 text-sky-700 dark:bg-sky-900 dark:text-sky-300'
                                    : protocol.badge === 'OAuth'
                                        ? 'bg-purple-100 text-purple-700 dark:bg-purple-900 dark:text-purple-300'
                                        : protocol.badge === 'Soon'
                                            ? 'bg-yellow-100 text-yellow-700 dark:bg-yellow-900 dark:text-yellow-300'
                                            : 'bg-gray-100 text-gray-600 dark:bg-gray-700 dark:text-gray-400'
                                    }`}>
                                    {protocol.badge}
                                </span>
                            )}
                        </button>
                    ))}
                </div>
            )}

            {/* Badge for selected protocol */}
            {value && selectedProtocol?.badge && !isOpen && (
                <span className={`inline-block mt-1.5 text-xs px-2 py-0.5 rounded-full ${selectedProtocol.badge === 'New'
                    ? 'bg-green-100 text-green-700 dark:bg-green-900 dark:text-green-300'
                    : selectedProtocol.badge === 'Secure'
                        ? 'bg-blue-100 text-blue-700 dark:bg-blue-900 dark:text-blue-300'
                        : selectedProtocol.badge === 'OAuth'
                            ? 'bg-purple-100 text-purple-700 dark:bg-purple-900 dark:text-purple-300'
                            : 'bg-gray-100 text-gray-600 dark:bg-gray-700 dark:text-gray-400'
                    }`}>
                    {selectedProtocol.badge}
                </span>
            )}
        </div>
    );
};

// Protocol-specific fields component
interface ProtocolFieldsProps {
    protocol: ProviderType;
    options: {
        bucket?: string;
        region?: string;
        endpoint?: string;
        pathStyle?: boolean;
    };
    onChange: (options: ProtocolFieldsProps['options']) => void;
    disabled?: boolean;
}

export const ProtocolFields: React.FC<ProtocolFieldsProps> = ({
    protocol,
    options,
    onChange,
    disabled = false,
}) => {
    if (protocol === 's3') {
        return (
            <div className="space-y-3 pt-2 border-t border-gray-200 dark:border-gray-700 mt-3">
                <div className="text-sm font-medium text-gray-700 dark:text-gray-300 flex items-center gap-2">
                    <Database size={14} />
                    S3 Configuration
                </div>
                <div>
                    <label className="block text-sm font-medium mb-1.5">Bucket Name *</label>
                    <input
                        type="text"
                        value={options.bucket || ''}
                        onChange={(e) => onChange({ ...options, bucket: e.target.value })}
                        disabled={disabled}
                        className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-xl"
                        placeholder="my-bucket"
                        required
                    />
                </div>
                <div className="grid grid-cols-2 gap-3">
                    <div>
                        <label className="block text-sm font-medium mb-1.5">Region</label>
                        <input
                            type="text"
                            value={options.region || ''}
                            onChange={(e) => onChange({ ...options, region: e.target.value })}
                            disabled={disabled}
                            className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-xl"
                            placeholder="us-east-1"
                        />
                    </div>
                    <div>
                        <label className="block text-sm font-medium mb-1.5">Custom Endpoint</label>
                        <input
                            type="text"
                            value={options.endpoint || ''}
                            onChange={(e) => onChange({ ...options, endpoint: e.target.value })}
                            disabled={disabled}
                            className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-xl"
                            placeholder="s3.example.com"
                        />
                        <p className="text-xs text-gray-500 mt-1">For MinIO, Wasabi, R2, etc.</p>
                    </div>
                </div>
                <label className="flex items-center gap-2 text-sm cursor-pointer">
                    <input
                        type="checkbox"
                        checked={options.pathStyle || false}
                        onChange={(e) => onChange({ ...options, pathStyle: e.target.checked })}
                        disabled={disabled}
                        className="rounded border-gray-300 text-blue-500 focus:ring-blue-500"
                    />
                    Path-style URLs (for self-hosted S3)
                </label>
            </div>
        );
    }

    if (protocol === 'webdav') {
        return (
            <div className="pt-2 border-t border-gray-200 dark:border-gray-700 mt-3">
                <div className="text-sm text-gray-600 dark:text-gray-400 flex items-center gap-2">
                    <Cloud size={14} />
                    <span>
                        WebDAV works with Nextcloud, ownCloud, Synology, QNAP, and more.
                        Enter the full WebDAV URL as the server address.
                    </span>
                </div>
                <p className="text-xs text-gray-500 mt-2">
                    Example: <code className="bg-gray-100 dark:bg-gray-700 px-1.5 py-0.5 rounded">cloud.example.com/remote.php/dav/files/user/</code>
                </p>
            </div>
        );
    }

    // FTP/FTPS don't need extra fields
    return null;
};

// Protocol icons for quick display
export const ProtocolIcon: React.FC<{ protocol?: ProviderType; size?: number; className?: string }> = ({
    protocol = 'ftp',
    size = 16,
    className = '',
}) => {
    const info = getProtocolInfo(protocol);
    if (!info) return null;
    return (
        <span className={`inline-flex items-center ${className}`} title={info.description}>
            {React.cloneElement(info.icon as React.ReactElement, { size })}
        </span>
    );
};

// Protocol badge with name
export const ProtocolBadge: React.FC<{ protocol?: ProviderType; className?: string }> = ({
    protocol = 'ftp',
    className = '',
}) => {
    const info = getProtocolInfo(protocol);
    if (!info) return null;

    const colorClass: Record<ProviderType, string> = {
        ftp: 'bg-blue-100 text-blue-700 dark:bg-blue-900 dark:text-blue-300',
        ftps: 'bg-green-100 text-green-700 dark:bg-green-900 dark:text-green-300',
        sftp: 'bg-purple-100 text-purple-700 dark:bg-purple-900 dark:text-purple-300',
        webdav: 'bg-orange-100 text-orange-700 dark:bg-orange-900 dark:text-orange-300',
        s3: 'bg-amber-100 text-amber-700 dark:bg-amber-900 dark:text-amber-300',
        aerocloud: 'bg-sky-100 text-sky-700 dark:bg-sky-900 dark:text-sky-300',
        googledrive: 'bg-red-100 text-red-700 dark:bg-red-900 dark:text-red-300',
        dropbox: 'bg-blue-100 text-blue-700 dark:bg-blue-900 dark:text-blue-300',
        onedrive: 'bg-sky-100 text-sky-700 dark:bg-sky-900 dark:text-sky-300',
        mega: 'bg-red-100 text-red-700 dark:bg-red-900 dark:text-red-300',
    };

    return (
        <span className={`inline-flex items-center gap-1 text-xs px-2 py-0.5 rounded-full font-medium ${colorClass[protocol]} ${className}`}>
            {info.icon}
            {info.name}
        </span>
    );
};

export default ProtocolSelector;
