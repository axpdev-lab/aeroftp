// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Z.4.5 R2 FileLu sub-tabs: unified surface that lets the user switch
 * between FileLu's five integration modes (Native API, Rsync, WebDAV,
 * S3, FTP) without having to leave the Connect screen.
 *
 * Why this exists: FileLu exposes the same account behind five
 * protocols; previously every mode lived in a different Discover
 * tile and the user had to backtrack to Discover to switch. This
 * component renders inline at the top of the FileLu form and lets the
 * operator hop between presets in one click, with the active mode
 * highlighted. Hidden when the active preset is not a `filelu-*` one.
 */
import React from 'react';
import { Key, FileText, Globe, Database, Server, Zap } from 'lucide-react';
import { useTranslation } from '../i18n';
import { getProviderById } from '../providers/registry';
import type { BaseProtocol } from '../providers/types';

export interface FileLuMode {
    /** registry providerId */
    providerId: string;
    /** BaseProtocol to switch the form to */
    protocol: BaseProtocol;
    /** lucide-react icon component */
    icon: React.ReactNode;
    /** color class for the icon when active */
    activeColor: string;
    /** short label */
    label: string;
    /** short tooltip / one-liner description */
    description: string;
    /** badge text (e.g. 'NEW', 'BETA') */
    badge?: string;
}

/** Ordered FileLu modes shown in the sub-tab strip. */
export const FILELU_MODES: FileLuMode[] = [
    {
        providerId: 'filelu',
        protocol: 'filelu',
        icon: <Key size={14} />,
        activeColor: 'text-sky-500',
        label: 'Native API',
        description: 'REST API with API key. Full feature set (trash, share links, password-protected files).',
    },
    {
        providerId: 'filelu-rsync',
        protocol: 'filelu-rsync',
        icon: <Zap size={14} />,
        activeColor: 'text-purple-500',
        label: 'Rsync',
        description: 'rsync-over-SSH on port 2222. Transfer-only, native aerorsync delta engine. Best for high-bandwidth syncs.',
        badge: 'BETA',
    },
    {
        providerId: 'filelu-webdav',
        protocol: 'webdav',
        icon: <Globe size={14} />,
        activeColor: 'text-emerald-500',
        label: 'WebDAV',
        description: 'WebDAV access on port 443. Wide client compatibility, mounts as a network drive.',
    },
    {
        providerId: 'filelu-s3',
        protocol: 's3',
        icon: <Database size={14} />,
        activeColor: 'text-amber-500',
        label: 'S3',
        description: 'S3-compatible API on port 443. Use rclone, AWS CLI, or any S3 SDK.',
    },
    {
        providerId: 'filelu-ftp',
        protocol: 'ftp',
        icon: <Server size={14} />,
        activeColor: 'text-blue-500',
        label: 'FTP',
        description: 'Classic FTP on port 21. Plaintext, useful for CCTV NVRs and legacy clients.',
    },
];

/** True when the given providerId is one of the FileLu presets. */
export function isFileLuMode(providerId: string | undefined | null): boolean {
    if (!providerId) return false;
    return FILELU_MODES.some(m => m.providerId === providerId);
}

interface FileLuSubTabsProps {
    /** Currently active providerId from the Connect form. */
    activeProviderId: string | null | undefined;
    /** Called when the operator clicks a sub-tab. Routes through the
     *  ConnectionScreen's `handleProtocolChange(newProtocol, providerId)`. */
    onSwitchMode: (protocol: BaseProtocol, providerId: string) => void;
    /** Optional className override for the outer container. */
    className?: string;
}

/**
 * Renders the FileLu mode chip strip. Hidden when no FileLu preset is
 * active. The active chip is filled with its brand color; inactive
 * chips show as outlined buttons with a hover tint.
 *
 * Below the chip strip we render a one-paragraph description of the
 * active mode so the user knows what they are about to connect to
 * before they enter credentials.
 */
export const FileLuSubTabs: React.FC<FileLuSubTabsProps> = ({
    activeProviderId,
    onSwitchMode,
    className,
}) => {
    const t = useTranslation();

    if (!isFileLuMode(activeProviderId)) {
        return null;
    }

    const active = FILELU_MODES.find(m => m.providerId === activeProviderId);

    return (
        <div
            className={
                className ||
                'mb-4 p-3 rounded-lg border border-gray-200 dark:border-gray-700 bg-gray-50 dark:bg-gray-800/60'
            }
            role="tablist"
            aria-label="FileLu connection modes"
        >
            <div className="flex items-center gap-1.5 mb-2 flex-wrap">
                <FileText size={12} className="text-gray-400 dark:text-gray-500" />
                <span className="text-[11px] font-semibold uppercase tracking-wider text-gray-500 dark:text-gray-400 mr-2">
                    FileLu Modes
                </span>
                {FILELU_MODES.map(mode => {
                    const isActive = mode.providerId === activeProviderId;
                    const registryEntry = getProviderById(mode.providerId);
                    const reachable = !!registryEntry; // future: health probe
                    return (
                        <button
                            key={mode.providerId}
                            type="button"
                            role="tab"
                            aria-selected={isActive}
                            disabled={!reachable}
                            onClick={() => onSwitchMode(mode.protocol, mode.providerId)}
                            title={mode.description}
                            className={
                                isActive
                                    ? `inline-flex items-center gap-1.5 px-2.5 py-1 rounded-md text-[12px] font-medium border bg-white dark:bg-gray-900 border-gray-300 dark:border-gray-600 ${mode.activeColor}`
                                    : 'inline-flex items-center gap-1.5 px-2.5 py-1 rounded-md text-[12px] font-medium border border-transparent text-gray-600 dark:text-gray-300 hover:bg-white hover:dark:bg-gray-900 hover:border-gray-200 hover:dark:border-gray-700 transition-colors'
                            }
                        >
                            <span className={isActive ? mode.activeColor : 'text-gray-400 dark:text-gray-500'}>
                                {mode.icon}
                            </span>
                            <span>{mode.label}</span>
                            {mode.badge && (
                                <span
                                    className={
                                        isActive
                                            ? 'ml-1 text-[9px] font-bold px-1 py-0.5 rounded bg-purple-100 dark:bg-purple-900/40 text-purple-700 dark:text-purple-300'
                                            : 'ml-1 text-[9px] font-bold px-1 py-0.5 rounded bg-gray-200 dark:bg-gray-700 text-gray-600 dark:text-gray-300'
                                    }
                                >
                                    {mode.badge}
                                </span>
                            )}
                        </button>
                    );
                })}
            </div>
            {active && (
                <div className="text-[11px] text-gray-600 dark:text-gray-400 leading-snug pl-1">
                    <span className="font-medium text-gray-700 dark:text-gray-300">{active.label}:</span>{' '}
                    {active.description}
                </div>
            )}
            {activeProviderId === 'filelu-rsync' && (
                <div className="mt-2 text-[11px] text-amber-700 dark:text-amber-300 bg-amber-50 dark:bg-amber-900/20 border border-amber-200 dark:border-amber-800/40 rounded px-2 py-1.5 leading-snug">
                    {t('filelu.rsyncTransferOnly') ||
                        'Transfer-only endpoint: browsing is not available on Rsync. Use Native API, WebDAV, S3 or FTP to navigate; switch to Rsync for high-bandwidth delta transfers.'}
                </div>
            )}
        </div>
    );
};

export default FileLuSubTabs;
