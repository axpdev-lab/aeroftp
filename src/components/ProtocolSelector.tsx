/**
 * ProtocolSelector Component
 * Dropdown for selecting storage protocol (FTP, WebDAV, S3, etc.)
 */

import React, { useState } from 'react';
import {
    Server,
    Cloud,
    Database,
    Lock,
    ShieldCheck,
    ShieldAlert,
    HardDrive,
    ChevronDown,
    ExternalLink
} from 'lucide-react';
import { ProviderType, FtpTlsMode } from '../types';
import { useTranslation } from '../i18n';
import { getProviderById } from '../providers';
import { BoxLogo, PCloudLogo, AzureLogo, FilenLogo, FourSharedLogo } from './ProviderLogos';

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
    <svg width={size} height={size} viewBox="0 0 512 512" className={className}>
        <path d="M255.999 6C117.931 6 6 117.932 6 256c0 138.07 111.93 250 250 250s250-111.93 250-250C506 117.931 394.07 6 256 6z" fill="#db282e" />
        <path d="M385.808 344.461a7.693 7.693 0 01-7.693 7.693h-32.692a7.692 7.692 0 01-7.692-7.693V243.9c0-.858-1.036-1.287-1.642-.68l-69.21 69.21c-6.009 6.008-15.749 6.008-21.757 0l-69.21-69.21c-.607-.607-1.643-.178-1.643.68v100.562a7.692 7.692 0 01-7.691 7.693h-32.694a7.693 7.693 0 01-7.693-7.693V167.54a7.693 7.693 0 017.693-7.693h22.475a15.39 15.39 0 0110.878 4.506l86.044 86.043a3.844 3.844 0 005.438 0l86.044-86.043a15.39 15.39 0 0110.879-4.506h22.473a7.693 7.693 0 017.693 7.693v176.922z" fill="#fff" />
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

// Helper to get protocols with translations
// We define this function outside the component to avoid re-creating the array on every render
const getProtocols = (t: (key: string, params?: Record<string, string>) => string): ProtocolInfo[] => [
    // Traditional Server Protocols
    {
        type: 'ftp',
        name: 'FTP',
        icon: <Server size={16} />,
        description: t('protocol.ftpDesc'),
        defaultPort: 21,
        badge: 'TLS',
        color: 'text-blue-500',
        tooltip: t('protocol.ftpTooltip'),
    },
    {
        type: 'sftp',
        name: 'SFTP',
        icon: <Lock size={16} />,
        description: t('protocol.sftpDesc'),
        defaultPort: 22,
        badge: 'SSH',
        color: 'text-emerald-500',
        tooltip: t('protocol.sftpTooltip'),
    },
    {
        type: 'webdav',
        name: 'WebDAV',
        icon: <Cloud size={16} />,
        description: t('protocol.webdavDesc'),
        defaultPort: 443,
        badge: 'TLS',
        color: 'text-orange-500',
        tooltip: t('protocol.webdavTooltip'),
    },
    {
        type: 's3',
        name: 'S3',
        icon: <AwsS3Logo size={18} />,
        description: t('protocol.s3Desc'),
        defaultPort: 443,
        badge: 'HMAC',
        color: 'text-amber-600',
        tooltip: t('protocol.s3Tooltip'),
    },
    // Cloud Storage Providers (AeroCloud FIRST!)
    {
        type: 'aerocloud',
        name: 'AeroCloud',
        icon: <Cloud size={18} className="text-sky-400" />,
        description: t('protocol.aerocloudDesc'),
        defaultPort: 21,
        badge: 'Sync',
        color: 'text-sky-500',
        isCloudStorage: true,
        tooltip: t('protocol.aerocloudTooltip'),
    },
    {
        type: 'googledrive',
        name: 'Google Drive',
        icon: <GoogleDriveLogo size={18} />,
        description: t('protocol.googledriveDesc'),
        defaultPort: 443,
        badge: 'OAuth',
        isOAuth: true,
        isCloudStorage: true,
        tooltip: t('protocol.googledriveTooltip'),
    },
    {
        type: 'dropbox',
        name: 'Dropbox',
        icon: <DropboxLogo size={18} />,
        description: t('protocol.dropboxDesc'),
        defaultPort: 443,
        badge: 'OAuth',
        isOAuth: true,
        isCloudStorage: true,
        tooltip: t('protocol.dropboxTooltip'),
    },
    {
        type: 'onedrive',
        name: 'OneDrive',
        icon: <OneDriveLogo size={18} />,
        description: t('protocol.onedriveDesc'),
        defaultPort: 443,
        badge: 'OAuth',
        isOAuth: true,
        isCloudStorage: true,
        tooltip: t('protocol.onedriveTooltip'),
    },
    {
        type: 'mega',
        name: 'MEGA',
        icon: <MegaLogo size={18} />,
        description: t('protocol.megaDesc'),
        defaultPort: 443,
        badge: 'E2E',
        color: 'text-red-600',
        isCloudStorage: true,
        tooltip: t('protocol.megaTooltip'),
    },
    {
        type: 'box',
        name: 'Box',
        icon: <BoxLogo size={18} />,
        description: t('protocol.boxDesc'),
        defaultPort: 443,
        badge: 'OAuth',
        isOAuth: true,
        isCloudStorage: true,
        tooltip: t('protocol.boxTooltip'),
    },
    {
        type: 'filen',
        name: 'Filen',
        icon: <FilenLogo size={18} />,
        description: t('protocol.filenDesc'),
        defaultPort: 443,
        badge: 'E2E',
        color: 'text-emerald-600',
        isCloudStorage: true,
        tooltip: t('protocol.filenTooltip'),
    },
    {
        type: 'pcloud',
        name: 'pCloud',
        icon: <PCloudLogo size={18} />,
        description: t('protocol.pcloudDesc'),
        defaultPort: 443,
        badge: 'OAuth',
        isOAuth: true,
        isCloudStorage: true,
        tooltip: t('protocol.pcloudTooltip'),
        disabled: !import.meta.env.DEV,
    },
    {
        type: 'fourshared',
        name: '4shared',
        icon: <FourSharedLogo size={18} />,
        description: t('protocol.foursharedDesc'),
        defaultPort: 443,
        badge: 'OAuth',
        isOAuth: true,
        isCloudStorage: true,
        tooltip: t('protocol.foursharedTooltip'),
    },
    {
        type: 'azure',
        name: 'Azure Blob',
        icon: <AzureLogo size={18} />,
        description: t('protocol.azureDesc'),
        defaultPort: 443,
        badge: 'HMAC',
        color: 'text-blue-500',
        isCloudStorage: true,
        tooltip: t('protocol.azureTooltip'),
        disabled: !import.meta.env.DEV,
    },
];

// Temporary fallback for getProtocolInfo when called outside component (no t function available)
// This returns a protocol info with English fallbacks for description/tooltip
const PROTOCOLS_FALLBACK: ProtocolInfo[] = [
    { type: 'ftp', name: 'FTP', icon: <Server size={16} />, description: 'File Transfer Protocol', defaultPort: 21, badge: 'TLS', color: 'text-blue-500', tooltip: 'FTP with configurable TLS encryption' },
    { type: 'sftp', name: 'SFTP', icon: <Lock size={16} />, description: 'SSH File Transfer', defaultPort: 22, badge: 'SSH', color: 'text-emerald-500', tooltip: 'SFTP over SSH' },
    { type: 'webdav', name: 'WebDAV', icon: <Cloud size={16} />, description: 'Nextcloud, CloudMe, Koofr', defaultPort: 443, badge: 'TLS', color: 'text-orange-500', tooltip: 'WebDAV protocol' },
    { type: 's3', name: 'S3', icon: <AwsS3Logo size={18} />, description: 'AWS S3, MinIO, R2, B2', defaultPort: 443, badge: 'HMAC', color: 'text-amber-600', tooltip: 'S3-compatible storage' },
    { type: 'aerocloud', name: 'AeroCloud', icon: <Cloud size={18} />, description: 'Personal FTP-based cloud', defaultPort: 21, badge: 'Sync', color: 'text-sky-500', isCloudStorage: true, tooltip: 'Turn any FTP server into your personal cloud' },
    { type: 'googledrive', name: 'Google Drive', icon: <GoogleDriveLogo size={18} />, description: 'Connect with Google Account', defaultPort: 443, badge: 'OAuth', isOAuth: true, isCloudStorage: true, tooltip: 'Google Drive OAuth2' },
    { type: 'dropbox', name: 'Dropbox', icon: <DropboxLogo size={18} />, description: 'Connect with Dropbox Account', defaultPort: 443, badge: 'OAuth', isOAuth: true, isCloudStorage: true, tooltip: 'Dropbox OAuth2' },
    { type: 'onedrive', name: 'OneDrive', icon: <OneDriveLogo size={18} />, description: 'Connect with Microsoft Account', defaultPort: 443, badge: 'OAuth', isOAuth: true, isCloudStorage: true, tooltip: 'OneDrive OAuth2' },
    { type: 'mega', name: 'MEGA', icon: <MegaLogo size={18} />, description: 'Secure Cloud Storage', defaultPort: 443, badge: 'E2E', color: 'text-red-600', isCloudStorage: true, tooltip: 'MEGA E2E encryption' },
    { type: 'box', name: 'Box', icon: <BoxLogo size={18} />, description: 'Connect with Box Account', defaultPort: 443, badge: 'OAuth', isOAuth: true, isCloudStorage: true, tooltip: 'Box OAuth2' },
    { type: 'filen', name: 'Filen', icon: <FilenLogo size={18} />, description: 'E2E Encrypted Cloud', defaultPort: 443, badge: 'E2E', color: 'text-emerald-600', isCloudStorage: true, tooltip: 'Filen zero-knowledge encryption' },
    { type: 'pcloud', name: 'pCloud', icon: <PCloudLogo size={18} />, description: 'Connect with pCloud Account', defaultPort: 443, badge: 'OAuth', isOAuth: true, isCloudStorage: true, tooltip: 'pCloud OAuth2' },
    { type: 'fourshared', name: '4shared', icon: <FourSharedLogo size={18} />, description: '15 GB Free Cloud Storage', defaultPort: 443, badge: 'OAuth', isOAuth: true, isCloudStorage: true, tooltip: '4shared OAuth 1.0' },
    { type: 'azure', name: 'Azure Blob', icon: <AzureLogo size={18} />, description: 'Microsoft Azure Storage', defaultPort: 443, badge: 'HMAC', color: 'text-blue-500', isCloudStorage: true, tooltip: 'Azure Blob Storage' },
];

export const getProtocolInfo = (type: ProviderType | ''): ProtocolInfo | null => {
    if (!type) return null;
    return PROTOCOLS_FALLBACK.find(p => p.type === type) || PROTOCOLS_FALLBACK[0];
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
    const t = useTranslation();
    const PROTOCOLS = React.useMemo(() => getProtocols(t), [t]);
    const selectedProtocol = value ? PROTOCOLS.find(p => p.type === value) : null;
    const [isOpen, setIsOpen] = React.useState(false);

    // Close dropdown when value is set externally (e.g., Edit button)
    React.useEffect(() => {
        if (value) {
            setIsOpen(false);
        }
    }, [value]);

    // Notify parent when isOpen changes
    const handleOpenChange = (newIsOpen: boolean) => {
        setIsOpen(newIsOpen);
        onOpenChange?.(newIsOpen);
    };

    return (
        <div className={className}>
            {showLabel && (
                <label className="block text-sm font-medium mb-1.5">{t('connection.protocol')}</label>
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
                            ? `${selectedProtocol.name} - ${t(`protocol.${selectedProtocol.type}Desc`)}`
                            : t('protocol.selectProtocol')}
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
                        <p className="text-xs text-gray-500 uppercase tracking-wide mb-2">{t('protocol.servers')}</p>
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
                                <div className="font-medium text-sm whitespace-nowrap">{protocol.name}</div>
                                <div className="text-xs text-gray-500 truncate">{t(`protocol.${protocol.type}Desc`)}</div>
                            </div>
                            {protocol.badge && (
                                <span className={`text-[10px] px-1.5 py-0.5 rounded inline-flex items-center gap-0.5 flex-shrink-0 ${
                                    ['TLS', 'SSH', 'HMAC', 'E2E'].includes(protocol.badge)
                                        ? 'bg-green-100 text-green-700 dark:bg-green-900 dark:text-green-300'
                                        : protocol.badge === 'Soon'
                                            ? 'bg-yellow-100 text-yellow-700 dark:bg-yellow-900 dark:text-yellow-300'
                                            : 'bg-gray-100 text-gray-600 dark:bg-gray-700 dark:text-gray-400'
                                    }`}>
                                    {['TLS', 'SSH', 'HMAC', 'E2E'].includes(protocol.badge) && <ShieldCheck size={10} />}
                                    {protocol.badge === 'OAuth' && <Lock size={10} />}
                                    {protocol.badge}
                                </span>
                            )}
                        </button>
                    ))}

                    {/* Cloud Storage Providers (AeroCloud first!) */}
                    <div className="col-span-2 mt-2">
                        <p className="text-xs text-gray-500 uppercase tracking-wide mb-2">{t('protocol.cloudStorage')}</p>
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
                                <div className="font-medium text-sm whitespace-nowrap">{protocol.name}</div>
                                <div className="text-xs text-gray-500 truncate">{t(`protocol.${protocol.type}Desc`)}</div>
                            </div>
                            {protocol.badge && (
                                <span className={`text-[10px] px-1.5 py-0.5 rounded inline-flex items-center gap-0.5 flex-shrink-0 ${
                                    protocol.badge === 'Sync'
                                        ? 'bg-sky-100 text-sky-700 dark:bg-sky-900 dark:text-sky-300'
                                        : protocol.badge === 'OAuth'
                                            ? 'bg-purple-100 text-purple-700 dark:bg-purple-900 dark:text-purple-300'
                                            : ['TLS', 'SSH', 'HMAC', 'E2E'].includes(protocol.badge)
                                                ? 'bg-green-100 text-green-700 dark:bg-green-900 dark:text-green-300'
                                                : protocol.badge === 'Soon'
                                                    ? 'bg-yellow-100 text-yellow-700 dark:bg-yellow-900 dark:text-yellow-300'
                                                    : 'bg-gray-100 text-gray-600 dark:bg-gray-700 dark:text-gray-400'
                                    }`}>
                                    {['TLS', 'SSH', 'HMAC', 'E2E'].includes(protocol.badge) && <ShieldCheck size={10} />}
                                    {protocol.badge === 'OAuth' && <Lock size={10} />}
                                    {protocol.badge}
                                </span>
                            )}
                        </button>
                    ))}
                </div>
            )}

            {/* Badge for selected protocol */}
            {value && selectedProtocol?.badge && !isOpen && (
                <span className={`inline-flex items-center gap-1 mt-1.5 text-xs px-2 py-0.5 rounded-full ${
                    ['TLS', 'SSH', 'HMAC', 'E2E'].includes(selectedProtocol.badge)
                        ? 'bg-green-100 text-green-700 dark:bg-green-900 dark:text-green-300'
                        : selectedProtocol.badge === 'OAuth'
                            ? 'bg-purple-100 text-purple-700 dark:bg-purple-900 dark:text-purple-300'
                            : selectedProtocol.badge === 'Sync'
                                ? 'bg-sky-100 text-sky-700 dark:bg-sky-900 dark:text-sky-300'
                                : 'bg-gray-100 text-gray-600 dark:bg-gray-700 dark:text-gray-400'
                    }`}>
                    {['TLS', 'SSH', 'HMAC', 'E2E'].includes(selectedProtocol.badge) && <ShieldCheck size={12} />}
                    {selectedProtocol.badge === 'OAuth' && <Lock size={12} />}
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
        // SFTP-specific
        private_key_path?: string;
        key_passphrase?: string;
        timeout?: number;
        // FTP/FTPS-specific
        tlsMode?: FtpTlsMode;
        verifyCert?: boolean;
    };
    onChange: (options: ProtocolFieldsProps['options']) => void;
    disabled?: boolean;
    onBrowseKeyFile?: () => void;  // Callback for key file selection
    selectedProviderId?: string | null;  // Provider preset ID for customized hints/placeholders
    isEditing?: boolean;  // Hide help links when editing existing server
}

export const ProtocolFields: React.FC<ProtocolFieldsProps> = ({
    protocol,
    options,
    onChange,
    disabled = false,
    onBrowseKeyFile,
    selectedProviderId,
    isEditing = false,
}) => {
    const t = useTranslation();
    const providerConfig = selectedProviderId ? getProviderById(selectedProviderId) : null;
    const [showInsecureCertModal, setShowInsecureCertModal] = useState(false);

    if (protocol === 'sftp') {
        return (
            <div className="space-y-3 pt-2 border-t border-gray-200 dark:border-gray-700 mt-3">
                <div className="text-sm font-medium text-gray-700 dark:text-gray-300 flex items-center gap-2">
                    <Lock size={14} />
                    {t('protocol.sshAuth')}
                </div>
                <p className="text-xs text-gray-500">
                    {t('protocol.sshAuthHelp')}
                </p>
                <div>
                    <label className="block text-sm font-medium mb-1.5">{t('protocol.privateKeyPath')}</label>
                    <div className="flex gap-2">
                        <input
                            type="text"
                            value={options.private_key_path || ''}
                            onChange={(e) => onChange({ ...options, private_key_path: e.target.value })}
                            disabled={disabled}
                            className="flex-1 px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-xl"
                            placeholder={t('protocol.privateKeyPlaceholder')}
                        />
                        {onBrowseKeyFile && (
                            <button
                                type="button"
                                onClick={onBrowseKeyFile}
                                disabled={disabled}
                                className="px-3 py-2.5 bg-gray-100 dark:bg-gray-600 border border-gray-300 dark:border-gray-600 rounded-xl hover:bg-gray-200 dark:hover:bg-gray-500 transition-colors"
                                title={t('protocol.browseKeyFile')}
                            >
                                <HardDrive size={16} />
                            </button>
                        )}
                    </div>
                </div>
                {options.private_key_path && (
                    <div>
                        <label className="block text-sm font-medium mb-1.5">{t('protocol.keyPassphrase')}</label>
                        <input
                            type="password"
                            value={options.key_passphrase || ''}
                            onChange={(e) => onChange({ ...options, key_passphrase: e.target.value })}
                            disabled={disabled}
                            className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-xl"
                            placeholder={t('protocol.keyPassphraseHelp')}
                        />
                    </div>
                )}
                <div>
                    <label className="block text-sm font-medium mb-1.5">{t('protocol.connectionTimeout')}</label>
                    <input
                        type="number"
                        value={options.timeout || 30}
                        onChange={(e) => onChange({ ...options, timeout: parseInt(e.target.value) || 30 })}
                        disabled={disabled}
                        min={5}
                        max={300}
                        className="w-24 px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-xl"
                    />
                </div>
            </div>
        );
    }

    if (protocol === 's3') {
        // Get provider-specific field configs from registry
        const bucketField = providerConfig?.fields?.find(f => f.key === 'bucket');
        const regionField = providerConfig?.fields?.find(f => f.key === 'region');
        const endpointField = providerConfig?.fields?.find(f => f.key === 'endpoint');
        const hasRegionSelect = regionField?.type === 'select' && regionField?.options;

        return (
            <div className="space-y-3 pt-2 border-t border-gray-200 dark:border-gray-700 mt-3">
                <div className="text-sm font-medium text-gray-700 dark:text-gray-300 flex items-center gap-2">
                    <Database size={14} />
                    {providerConfig ? `${providerConfig.name} — ${t('protocol.s3Config')}` : t('protocol.s3Config')}
                </div>
                <div>
                    <label className="block text-sm font-medium mb-1.5">
                        {bucketField?.label || t('protocol.bucketNameRequired')}
                    </label>
                    <input
                        type="text"
                        value={options.bucket || ''}
                        onChange={(e) => onChange({ ...options, bucket: e.target.value })}
                        disabled={disabled}
                        className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-xl"
                        placeholder={bucketField?.placeholder || t('protocol.bucketPlaceholder')}
                        required
                    />
                    {!isEditing && bucketField?.helpText && (
                        <p className="text-xs text-gray-500 mt-1">{bucketField.helpText}</p>
                    )}
                </div>
                <div className="grid grid-cols-2 gap-3">
                    <div>
                        <label className="block text-sm font-medium mb-1.5">
                            {regionField?.label || t('protocol.region')}
                        </label>
                        {hasRegionSelect ? (
                            <select
                                value={options.region || ''}
                                onChange={(e) => onChange({ ...options, region: e.target.value })}
                                disabled={disabled}
                                className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-xl"
                            >
                                <option value="">{t('protocol.selectRegion')}</option>
                                {regionField!.options!.map(opt => (
                                    <option key={opt.value} value={opt.value}>{opt.label}</option>
                                ))}
                            </select>
                        ) : (
                            <input
                                type="text"
                                value={options.region || ''}
                                onChange={(e) => onChange({ ...options, region: e.target.value })}
                                disabled={disabled}
                                className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-xl"
                                placeholder={providerConfig?.defaults?.region || t('protocol.regionPlaceholder')}
                            />
                        )}
                    </div>
                    <div>
                        <label className="block text-sm font-medium mb-1.5">
                            {endpointField?.label || t('protocol.customEndpoint')}
                        </label>
                        <input
                            type="text"
                            value={options.endpoint || ''}
                            onChange={(e) => onChange({ ...options, endpoint: e.target.value })}
                            disabled={disabled}
                            className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-xl"
                            placeholder={endpointField?.placeholder || t('protocol.endpointPlaceholder')}
                        />
                        {!isEditing && (
                            <p className="text-xs text-gray-500 mt-1">
                                {endpointField?.helpText || t('protocol.endpointHelp')}
                            </p>
                        )}
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
                    {t('protocol.pathStyle')}
                </label>
                {!isEditing && providerConfig?.helpUrl && (
                    <a
                        href={providerConfig.helpUrl}
                        target="_blank"
                        rel="noopener noreferrer"
                        className="inline-flex items-center gap-1.5 text-xs text-blue-500 hover:text-blue-600 dark:text-blue-400 dark:hover:text-blue-300 mt-1"
                    >
                        <ExternalLink size={12} />
                        {t('protocol.providerDocumentation', { name: providerConfig.name })}
                    </a>
                )}
            </div>
        );
    }

    if (protocol === 'webdav') {
        const isNextcloud = selectedProviderId === 'nextcloud';
        const isCustomOrGeneric = !selectedProviderId || selectedProviderId === 'custom-webdav';
        const showNextcloudHint = isNextcloud || isCustomOrGeneric;

        return (
            <div className="pt-2 border-t border-gray-200 dark:border-gray-700 mt-3 space-y-2">
                {showNextcloudHint && (
                    <>
                        <div className="text-sm text-gray-600 dark:text-gray-400 flex items-center gap-2">
                            <Cloud size={14} />
                            <span>{t('protocol.webdavNote')}</span>
                        </div>
                        <p className="text-xs text-gray-500">
                            {t('protocol.webdavExampleLabel')} <code className="bg-gray-100 dark:bg-gray-700 px-1.5 py-0.5 rounded">{t('protocol.webdavExample')}</code>
                        </p>
                    </>
                )}
                {providerConfig && !isCustomOrGeneric && (
                    <div className="text-sm text-gray-600 dark:text-gray-400 flex items-center gap-2">
                        <Cloud size={14} />
                        <span>
                            {providerConfig.defaults?.basePath
                                ? t('protocol.webdavBasePath', { path: providerConfig.defaults.basePath })
                                : t('protocol.webdavConnectVia', { name: providerConfig.name })}
                        </span>
                    </div>
                )}
                {!isEditing && providerConfig?.helpUrl && (
                    <a
                        href={providerConfig.helpUrl}
                        target="_blank"
                        rel="noopener noreferrer"
                        className="inline-flex items-center gap-1.5 text-xs text-blue-500 hover:text-blue-600 dark:text-blue-400 dark:hover:text-blue-300"
                    >
                        <ExternalLink size={12} />
                        {t('protocol.providerDocumentation', { name: providerConfig.name })}
                    </a>
                )}
            </div>
        );
    }

    if (protocol === 'azure') {
        return (
            <div className="space-y-3 pt-2 border-t border-gray-200 dark:border-gray-700 mt-3">
                <div className="text-sm font-medium text-gray-700 dark:text-gray-300 flex items-center gap-2">
                    <Database size={14} />
                    {t('protocol.azureBlobStorage')}
                </div>
                <div>
                    <label className="block text-sm font-medium mb-1.5">{t('protocol.azureContainerName')}</label>
                    <input
                        type="text"
                        value={options.bucket || ''}
                        onChange={(e) => onChange({ ...options, bucket: e.target.value })}
                        disabled={disabled}
                        className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-xl"
                        placeholder={t('protocol.azureContainerPlaceholder')}
                        required
                    />
                </div>
                <p className="text-xs text-gray-500">
                    {t('protocol.azureAuthHelp')}
                </p>
            </div>
        );
    }

    if (protocol === 'pcloud') {
        return (
            <div className="space-y-3 pt-2 border-t border-gray-200 dark:border-gray-700 mt-3">
                <div className="text-sm font-medium text-gray-700 dark:text-gray-300 flex items-center gap-2">
                    <Cloud size={14} />
                    {t('protocol.pcloudRegion')}
                </div>
                <div className="flex gap-3">
                    <label className="flex items-center gap-2 text-sm cursor-pointer">
                        <input
                            type="radio"
                            name="pcloud-region"
                            checked={(options.region || 'us') === 'us'}
                            onChange={() => onChange({ ...options, region: 'us' })}
                            disabled={disabled}
                            className="text-blue-500 focus:ring-blue-500"
                        />
                        {t('protocol.pcloudUS')}
                    </label>
                    <label className="flex items-center gap-2 text-sm cursor-pointer">
                        <input
                            type="radio"
                            name="pcloud-region"
                            checked={options.region === 'eu'}
                            onChange={() => onChange({ ...options, region: 'eu' })}
                            disabled={disabled}
                            className="text-blue-500 focus:ring-blue-500"
                        />
                        {t('protocol.pcloudEU')}
                    </label>
                </div>
                <p className="text-xs text-gray-500">
                    {t('protocol.pcloudRegionHelp')}
                </p>
            </div>
        );
    }

    if (protocol === 'ftp' || protocol === 'ftps') {
        const defaultTlsMode = protocol === 'ftps' ? 'implicit' : 'explicit';
        const currentTlsMode = options.tlsMode || defaultTlsMode;
        const showInsecureWarning = currentTlsMode === 'none';

        return (
            <div className="space-y-3 pt-2 border-t border-gray-200 dark:border-gray-700 mt-3">
                {/* Encryption mode */}
                <div>
                    <label className="block text-sm font-medium mb-1.5">{t('protocol.encryption')}</label>
                    <select
                        value={currentTlsMode}
                        onChange={(e) => onChange({ ...options, tlsMode: e.target.value as FtpTlsMode })}
                        disabled={disabled}
                        className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-xl"
                    >
                        {protocol === 'ftp' && (
                            <option value="explicit_if_available">{t('protocol.encryptionExplicitIfAvailable')}</option>
                        )}
                        <option value="explicit">{t('protocol.encryptionExplicit')}</option>
                        <option value="implicit">{t('protocol.encryptionImplicit')}</option>
                        {protocol === 'ftp' && (
                            <option value="none">{t('protocol.encryptionNone')}</option>
                        )}
                    </select>
                </div>

                {/* Accept invalid certificates (only when TLS is used) */}
                {currentTlsMode !== 'none' && (
                    <label className="flex items-center gap-2 text-sm cursor-pointer">
                        <input
                            type="checkbox"
                            checked={options.verifyCert === false}
                            onChange={(e) => {
                                if (e.target.checked) {
                                    setShowInsecureCertModal(true);
                                } else {
                                    onChange({ ...options, verifyCert: true });
                                }
                            }}
                            disabled={disabled}
                            className="rounded border-gray-300 text-blue-500 focus:ring-blue-500"
                        />
                        {t('protocol.acceptInvalidCerts')}
                    </label>
                )}

                {/* Insecure warning — only when user explicitly chooses plain FTP */}
                {showInsecureWarning && (
                    <div className="p-3 bg-amber-50 dark:bg-amber-900/20 border border-amber-200 dark:border-amber-800 rounded-xl">
                        <div className="flex items-start gap-2">
                            <ShieldCheck size={16} className="text-amber-500 mt-0.5 flex-shrink-0" />
                            <div>
                                <p className="text-sm font-medium text-amber-700 dark:text-amber-300">
                                    {t('protocol.ftpWarningTitle')}
                                </p>
                                <p className="text-xs text-amber-600 dark:text-amber-400 mt-1">
                                    {t('protocol.ftpWarningDesc')}
                                </p>
                            </div>
                        </div>
                    </div>
                )}

                {/* Insecure certificate confirmation modal */}
                {showInsecureCertModal && (
                    <div className="fixed inset-0 z-[9999] flex items-center justify-center bg-black/50 backdrop-blur-sm" onClick={() => setShowInsecureCertModal(false)}>
                        <div className="bg-white dark:bg-gray-800 rounded-2xl shadow-2xl border border-gray-200 dark:border-gray-700 max-w-md w-full mx-4 overflow-hidden" onClick={e => e.stopPropagation()}>
                            <div className="p-6">
                                <div className="flex items-center gap-3 mb-4">
                                    <div className="w-10 h-10 rounded-full bg-amber-100 dark:bg-amber-900/30 flex items-center justify-center flex-shrink-0">
                                        <ShieldAlert size={20} className="text-amber-500" />
                                    </div>
                                    <h3 className="text-lg font-semibold text-gray-900 dark:text-white">
                                        {t('protocol.insecureCertTitle')}
                                    </h3>
                                </div>
                                <p className="text-sm text-gray-600 dark:text-gray-400 leading-relaxed">
                                    {t('protocol.insecureCertConfirmFirst')}
                                </p>
                            </div>
                            <div className="flex border-t border-gray-200 dark:border-gray-700">
                                <button
                                    onClick={() => setShowInsecureCertModal(false)}
                                    className="flex-1 px-4 py-3 text-sm font-medium text-gray-700 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-700 transition-colors"
                                >
                                    {t('common.cancel')}
                                </button>
                                <button
                                    onClick={() => {
                                        setShowInsecureCertModal(false);
                                        onChange({ ...options, verifyCert: false });
                                    }}
                                    className="flex-1 px-4 py-3 text-sm font-medium text-amber-600 dark:text-amber-400 hover:bg-amber-50 dark:hover:bg-amber-900/20 transition-colors border-l border-gray-200 dark:border-gray-700"
                                >
                                    {t('protocol.insecureCertAccept')}
                                </button>
                            </div>
                        </div>
                    </div>
                )}
            </div>
        );
    }

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
        box: 'bg-blue-100 text-blue-700 dark:bg-blue-900 dark:text-blue-300',
        pcloud: 'bg-cyan-100 text-cyan-700 dark:bg-cyan-900 dark:text-cyan-300',
        azure: 'bg-blue-100 text-blue-700 dark:bg-blue-900 dark:text-blue-300',
        filen: 'bg-emerald-100 text-emerald-700 dark:bg-emerald-900 dark:text-emerald-300',
        fourshared: 'bg-blue-100 text-blue-700 dark:bg-blue-900 dark:text-blue-300',
    };

    return (
        <span className={`inline-flex items-center gap-1 text-xs px-2 py-0.5 rounded-full font-medium ${colorClass[protocol]} ${className}`}>
            {info.icon}
            {info.name}
        </span>
    );
};

export default ProtocolSelector;
