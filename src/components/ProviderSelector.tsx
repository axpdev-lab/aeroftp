/**
 * ProviderSelector Component
 * 
 * Visual grid of cloud storage providers for quick selection.
 * Shows both pre-configured providers and generic/custom options.
 */

import React from 'react';
import {
    Cloud, Database, Globe, HardDrive, Flame, Server,
    ChevronRight, Sparkles, CheckCircle
} from 'lucide-react';
import { providerRegistry, ProviderConfig } from '../providers';

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

const getProviderIcon = (iconName?: string, color?: string): React.ReactNode => {
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

    // Render a single provider card
    const renderProviderCard = (provider: ProviderConfig) => {
        const isSelected = selectedProvider === provider.id;

        return (
            <button
                key={provider.id}
                onClick={() => onSelect(provider)}
                className={`
                    relative group flex flex-col items-center justify-center
                    ${compact ? 'p-3 gap-1.5' : 'p-4 gap-2'}
                    rounded-xl border-2 transition-all duration-200
                    ${isSelected
                        ? 'border-blue-500 bg-blue-500/10 shadow-lg shadow-blue-500/20'
                        : 'border-gray-700 hover:border-gray-500 hover:bg-gray-800/50'
                    }
                    ${!provider.stable ? 'opacity-70' : ''}
                `}
            >
                {/* Selected indicator */}
                {isSelected && (
                    <div className="absolute top-1.5 right-1.5">
                        <CheckCircle size={14} className="text-blue-400" />
                    </div>
                )}

                {/* Beta badge */}
                {!provider.stable && (
                    <div className="absolute top-1 left-1">
                        <span className="px-1.5 py-0.5 text-[9px] font-medium bg-yellow-500/20 text-yellow-400 rounded">
                            BETA
                        </span>
                    </div>
                )}

                {/* Icon */}
                <div className={`
                    flex items-center justify-center
                    ${compact ? 'w-8 h-8' : 'w-12 h-12'}
                    rounded-lg bg-gray-800 group-hover:bg-gray-700 transition-colors
                `}>
                    {getProviderIcon(provider.icon, provider.color)}
                </div>

                {/* Name */}
                <span className={`
                    font-medium text-center
                    ${compact ? 'text-xs' : 'text-sm'}
                    ${isSelected ? 'text-blue-300' : 'text-gray-300'}
                `}>
                    {provider.name}
                </span>

                {/* Description (only in non-compact mode) */}
                {!compact && provider.description && (
                    <span className="text-[10px] text-gray-500 text-center line-clamp-2">
                        {provider.description}
                    </span>
                )}

                {/* Custom/generic indicator */}
                {provider.isGeneric && (
                    <span className="text-[9px] text-gray-500 uppercase tracking-wider">
                        Custom
                    </span>
                )}
            </button>
        );
    };

    return (
        <div className="space-y-4">
            {/* Specific Providers */}
            {specificProviders.length > 0 && (
                <div>
                    <h4 className="text-xs font-medium text-gray-400 uppercase tracking-wider mb-2 flex items-center gap-2">
                        <Sparkles size={12} />
                        Pre-configured Providers
                    </h4>
                    <div className={`grid gap-2 ${compact ? 'grid-cols-4' : 'grid-cols-3'}`}>
                        {specificProviders.map(renderProviderCard)}
                    </div>
                </div>
            )}

            {/* Generic/Custom Providers */}
            {genericProviders.length > 0 && (
                <div>
                    <h4 className="text-xs font-medium text-gray-400 uppercase tracking-wider mb-2 flex items-center gap-2">
                        <Server size={12} />
                        Custom Connection
                    </h4>
                    <div className={`grid gap-2 ${compact ? 'grid-cols-4' : 'grid-cols-2'}`}>
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
        <div className="flex gap-1 p-1 bg-gray-800/50 rounded-lg">
            {tabs.map(tab => (
                <button
                    key={tab.id}
                    onClick={() => onChange(tab.id)}
                    className={`
                        flex items-center gap-1.5 px-3 py-1.5 rounded-md text-sm font-medium transition-all
                        ${selected === tab.id
                            ? 'bg-blue-600 text-white shadow-lg'
                            : 'text-gray-400 hover:text-white hover:bg-gray-700'
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
