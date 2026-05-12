// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { ChevronDown, FolderOpen, HardDrive, Server } from 'lucide-react';
import type { PanelEndpoint } from '../types/aerofile';
import type { ServerProfile } from '../types';
import { getEndpointLabel } from '../utils/panelEndpoints';
import { PROVIDER_LOGOS } from './ProviderLogos';

export interface PanelEndpointSelectorProps {
    endpoint: PanelEndpoint;
    savedProfiles: ServerProfile[];
    compactLabel?: string;
    onChooseLocalFolder: () => void;
    onChooseRemoteProfile: (profile: ServerProfile) => void;
}

const ProfileIcon: React.FC<{ profile: ServerProfile }> = ({ profile }) => {
    const Logo = PROVIDER_LOGOS[profile.providerId || profile.protocol || ''];
    if (Logo) return <Logo size={16} />;
    return <Server size={14} />;
};

export const PanelEndpointSelector: React.FC<PanelEndpointSelectorProps> = ({
    endpoint,
    savedProfiles,
    compactLabel,
    onChooseLocalFolder,
    onChooseRemoteProfile,
}) => {
    const [open, setOpen] = React.useState(false);
    const rootRef = React.useRef<HTMLDivElement>(null);
    const endpointLabel = getEndpointLabel(endpoint);

    React.useEffect(() => {
        if (!open) return;
        const onPointerDown = (event: PointerEvent) => {
            if (!rootRef.current?.contains(event.target as Node)) setOpen(false);
        };
        document.addEventListener('pointerdown', onPointerDown);
        return () => document.removeEventListener('pointerdown', onPointerDown);
    }, [open]);

    return (
        <div className="relative flex-shrink-0" ref={rootRef}>
            <button
                type="button"
                onClick={() => setOpen(prev => !prev)}
                className="h-7 max-w-[11rem] inline-flex items-center gap-1.5 rounded-md border border-gray-300 bg-white px-2 text-xs font-medium text-gray-700 shadow-sm transition-colors hover:bg-gray-50 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-200 dark:hover:bg-gray-700"
                title={endpointLabel}
                aria-haspopup="menu"
                aria-expanded={open}
            >
                {endpoint.kind === 'local' ? (
                    <HardDrive size={13} className="text-amber-500 shrink-0" />
                ) : (
                    <Server size={13} className="text-blue-500 shrink-0" />
                )}
                {compactLabel && (
                    <span className="font-semibold text-gray-500 dark:text-gray-400">{compactLabel}</span>
                )}
                <span className="truncate max-w-[6.5rem]">
                    {endpoint.kind === 'local' ? 'Local' : endpointLabel}
                </span>
                <ChevronDown size={12} className="text-gray-400 shrink-0" />
            </button>

            {open && (
                <div
                    role="menu"
                    className="absolute left-0 top-full z-40 mt-1 w-72 overflow-hidden rounded-lg border border-gray-200 bg-white shadow-xl dark:border-gray-700 dark:bg-gray-800"
                >
                    <div className="border-b border-gray-200 px-3 py-2 dark:border-gray-700">
                        <div className="text-xs font-semibold uppercase tracking-wide text-gray-500 dark:text-gray-400">
                            Endpoint
                        </div>
                        <div className="mt-0.5 truncate text-xs text-gray-500 dark:text-gray-400" title={endpointLabel}>
                            {endpointLabel}
                        </div>
                    </div>

                    <button
                        type="button"
                        role="menuitem"
                        onClick={() => {
                            setOpen(false);
                            onChooseLocalFolder();
                        }}
                        className="flex w-full items-center gap-3 px-3 py-2 text-left text-sm text-gray-800 transition-colors hover:bg-gray-50 dark:text-gray-100 dark:hover:bg-gray-700"
                    >
                        <span className="flex h-8 w-8 items-center justify-center rounded-md bg-amber-100 text-amber-700 dark:bg-amber-900/40 dark:text-amber-300">
                            <FolderOpen size={16} />
                        </span>
                        <span className="min-w-0">
                            <span className="block font-medium">Local folder</span>
                            <span className="block truncate text-xs text-gray-500 dark:text-gray-400">Choose a local path for this panel</span>
                        </span>
                    </button>

                    <div className="border-t border-gray-200 px-3 py-1.5 text-xs font-semibold uppercase tracking-wide text-gray-500 dark:border-gray-700 dark:text-gray-400">
                        Saved remote profiles
                    </div>
                    <div className="max-h-72 overflow-y-auto py-1">
                        {savedProfiles.length === 0 ? (
                            <div className="px-3 py-3 text-sm text-gray-500 dark:text-gray-400">
                                No saved profiles
                            </div>
                        ) : (
                            savedProfiles.map(profile => (
                                <button
                                    key={profile.id}
                                    type="button"
                                    role="menuitem"
                                    onClick={() => {
                                        setOpen(false);
                                        onChooseRemoteProfile(profile);
                                    }}
                                    className="flex w-full items-center gap-3 px-3 py-2 text-left transition-colors hover:bg-gray-50 dark:hover:bg-gray-700"
                                >
                                    <span className="flex h-8 w-8 items-center justify-center rounded-md bg-gray-100 text-gray-600 dark:bg-gray-700 dark:text-gray-200">
                                        <ProfileIcon profile={profile} />
                                    </span>
                                    <span className="min-w-0 flex-1">
                                        <span className="block truncate text-sm font-medium text-gray-900 dark:text-white">
                                            {profile.name}
                                        </span>
                                        <span className="block truncate text-xs text-gray-500 dark:text-gray-400">
                                            {(profile.protocol || 'ftp').toUpperCase()} {profile.initialPath || profile.host || '/'}
                                        </span>
                                    </span>
                                </button>
                            ))
                        )}
                    </div>
                </div>
            )}
        </div>
    );
};

export default PanelEndpointSelector;
