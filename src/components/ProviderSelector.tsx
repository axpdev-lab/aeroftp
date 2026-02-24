/**
 * ProviderSelector Component
 * 
 * Visual grid of cloud storage providers for quick selection.
 * Shows both pre-configured providers and generic/custom options.
 */

import React from 'react';
import {
    Cloud, Database, Globe, HardDrive, Flame, Server,
    ChevronRight, Sparkles, CheckCircle, Info
} from 'lucide-react';
import { useTranslation } from '../i18n';
import { providerRegistry, ProviderConfig } from '../providers';
import { PROVIDER_LOGOS } from './ProviderLogos';

// ============================================================================
// Props
// ============================================================================

interface ProviderSelectorProps {
    /** Currently selected provider ID */
    selectedProvider?: string;

    /** Callback when a provider is selected */
    onSelect: (provider: ProviderConfig) => void;

    /** Category filter (null = show all) */
    category?: 's3' | 'webdav' | null;

    /** Show only stable/tested providers */
    stableOnly?: boolean;

    /** Compact mode (smaller cards) */
    compact?: boolean;
}

// ============================================================================
// Icon Mapping
// ============================================================================

const PROVIDER_ICONS: Record<string, React.ReactNode> = {
    'Database': <Database size={24} />,
    'Globe': <Globe size={24} />,
    'HardDrive': <HardDrive size={24} />,
    'Flame': <Flame size={24} />,
    'Cloud': <Cloud size={24} />,
    'Server': <Server size={24} />,
};

const getProviderIcon = (iconName?: string, color?: string, providerId?: string): React.ReactNode => {
    // Use official logo if available for this provider
    if (providerId && PROVIDER_LOGOS[providerId]) {
        const LogoComponent = PROVIDER_LOGOS[providerId];
        return <LogoComponent size={24} />;
    }
    const icon = PROVIDER_ICONS[iconName || 'Cloud'] || <Cloud size={24} />;
    return <span style={{ color: color || 'currentColor' }}>{icon}</span>;
};

// ============================================================================
// Component
// ============================================================================

export const ProviderSelector: React.FC<ProviderSelectorProps> = ({
    selectedProvider,
    onSelect,
    category = null,
    stableOnly = false,
    compact = false,
}) => {
    const t = useTranslation();

    // Get providers based on filters
    const allProviders = providerRegistry.getAll();

    const filteredProviders = allProviders.filter(p => {
        if (category && p.category !== category) return false;
        if (stableOnly && !p.stable) return false;
        return true;
    });

    // Separate generic from specific providers
    const genericProviders = filteredProviders.filter(p => p.isGeneric);
    const specificProviders = filteredProviders.filter(p => !p.isGeneric);

    // Render a single provider row (horizontal style matching ProtocolSelector)
    const renderProviderCard = (provider: ProviderConfig) => {
        const isSelected = selectedProvider === provider.id;

        return (
            <button
                key={provider.id}
                onClick={() => onSelect(provider)}
                title={provider.description}
                className={`
                    relative flex items-center gap-2 p-2.5 rounded-lg border transition-all text-left
                    ${isSelected
                        ? 'border-blue-500 bg-blue-50 dark:bg-blue-900/30'
                        : 'border-gray-200 dark:border-gray-600 hover:border-blue-300 hover:bg-gray-50 dark:hover:bg-gray-700'
                    }
                    ${!provider.stable ? 'opacity-70' : ''}
                `}
            >
                {/* Beta badge */}
                {!provider.stable && (
                    <div className="absolute top-0.5 left-1">
                        <span className="px-1 py-px text-[8px] font-medium bg-yellow-500/20 text-yellow-600 dark:text-yellow-400 rounded">
                            BETA
                        </span>
                    </div>
                )}

                {/* Icon */}
                <div className="flex-shrink-0">
                    {getProviderIcon(provider.icon, provider.color, provider.id)}
                </div>

                {/* Name + Description */}
                <div className="min-w-0 flex-1">
                    <div className="font-medium text-sm whitespace-nowrap">{provider.name}</div>
                    <div className="text-xs text-gray-500 truncate">{provider.description}</div>
                </div>

            </button>
        );
    };

    return (
        <div className="space-y-4">
            {/* Info card for S3/WebDAV */}
            {category && (
                <div className="flex items-center gap-2 p-2.5 bg-blue-50/50 dark:bg-blue-900/15 border border-blue-200/50 dark:border-blue-800/40 rounded-lg text-xs text-blue-700 dark:text-blue-300">
                    <Info size={14} className="flex-shrink-0" />
                    <p>{t(`protocol.${category}InfoLine1`)} {t(`protocol.${category}InfoLine2`)}</p>
                </div>
            )}

            {/* Specific Providers */}
            {specificProviders.length > 0 && (
                <div>
                    <h4 className="text-xs font-medium text-gray-500 dark:text-gray-400 uppercase tracking-wider mb-2 flex items-center gap-2">
                        <Sparkles size={12} />
                        {t('protocol.preconfiguredProviders')}
                    </h4>
                    <div className="grid grid-cols-2 gap-2">
                        {specificProviders.map(renderProviderCard)}
                    </div>
                </div>
            )}

            {/* Generic/Custom Providers */}
            {genericProviders.length > 0 && (
                <div>
                    <h4 className="text-xs font-medium text-gray-500 dark:text-gray-400 uppercase tracking-wider mb-2 flex items-center gap-2">
                        <Server size={12} />
                        {t('protocol.customConnection')}
                    </h4>
                    <div className="grid grid-cols-2 gap-2">
                        {genericProviders.map(renderProviderCard)}
                    </div>
                </div>
            )}
        </div>
    );
};

// ============================================================================
// Quick Provider Tabs (for use in QuickConnect header)
// ============================================================================

interface ProviderTabsProps {
    selected: 's3' | 'webdav' | 'ftp' | 'oauth';
    onChange: (tab: 's3' | 'webdav' | 'ftp' | 'oauth') => void;
}

export const ProviderTabs: React.FC<ProviderTabsProps> = ({ selected, onChange }) => {
    const tabs = [
        { id: 'ftp' as const, label: 'FTP/SFTP', icon: <Server size={14} /> },
        { id: 'oauth' as const, label: 'Cloud', icon: <Cloud size={14} /> },
        { id: 's3' as const, label: 'S3', icon: <Database size={14} /> },
        { id: 'webdav' as const, label: 'WebDAV', icon: <Globe size={14} /> },
    ];

    return (
        <div className="flex gap-1 p-1 bg-gray-100 dark:bg-gray-800/50 rounded-lg">
            {tabs.map(tab => (
                <button
                    key={tab.id}
                    onClick={() => onChange(tab.id)}
                    className={`
                        flex items-center gap-1.5 px-3 py-1.5 rounded-md text-sm font-medium transition-all
                        ${selected === tab.id
                            ? 'bg-blue-600 text-white shadow-lg'
                            : 'text-gray-500 dark:text-gray-400 hover:text-gray-900 dark:hover:text-white hover:bg-gray-200 dark:hover:bg-gray-700'
                        }
                    `}
                >
                    {tab.icon}
                    {tab.label}
                </button>
            ))}
        </div>
    );
};

export default ProviderSelector;
