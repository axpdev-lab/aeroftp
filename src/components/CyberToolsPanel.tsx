// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { useState } from 'react';
import { Hash, Lock, KeyRound } from 'lucide-react';
import { useTranslation } from '../i18n';
import { NATIVE_DROP_OWNER_ATTR } from '../utils/nativeDropOwner';
import { HashForgeTab } from './HashForgeTab';
import { CryptoLabTab } from './CryptoLabTab';
import { PasswordForgeTab } from './PasswordForgeTab';

interface CyberToolsPanelProps {
    /** Which OS file drops Hash Forge takes: every drop in the window when the
     *  panel is shown as a modal over the app, or only the drops that land on
     *  the panel when it sits next to other surfaces (AeroTools). */
    nativeDropScope: 'window' | 'panel';
}

type TabId = 'hash' | 'crypto' | 'password';

/**
 * The Security Tools body: tab bar and the three tools, with no window around
 * it. CyberToolsModal wraps it for the Cyber-theme titlebar easter egg;
 * AeroTools shows it as a column next to the editor and the terminal (#347).
 * It fills a flex-column parent and scrolls its own content.
 *
 * The root claims the keyboard (`data-keyboard-island`, honoured by the
 * app-wide shortcut hook) and is focusable, so a click anywhere inside keeps
 * focus here: Tab moves between the tool's controls and Delete, arrows or
 * Ctrl+A never act on the file selection behind it.
 */
export const CyberToolsPanel: React.FC<CyberToolsPanelProps> = ({ nativeDropScope }) => {
    const t = useTranslation();
    const [activeTab, setActiveTab] = useState<TabId>('hash');

    const tabs: { id: TabId; label: string; icon: React.ReactNode }[] = [
        { id: 'hash', label: t('cyberTools.hashForge'), icon: <Hash size={15} /> },
        { id: 'crypto', label: t('cyberTools.cryptoLab'), icon: <Lock size={15} /> },
        { id: 'password', label: t('cyberTools.passwordForge'), icon: <KeyRound size={15} /> },
    ];

    return (
        <div
            {...{ [NATIVE_DROP_OWNER_ATTR]: '' }}
            data-keyboard-island=""
            tabIndex={-1}
            className="flex-1 min-h-0 flex flex-col focus:outline-none"
        >
            {/* Tabs: scroll sideways rather than wrap when the column is narrow */}
            <div className="flex flex-shrink-0 overflow-x-auto border-b border-gray-200 dark:border-gray-700 px-2">
                {tabs.map(tab => (
                    <button
                        key={tab.id}
                        onClick={() => setActiveTab(tab.id)}
                        className={`flex flex-shrink-0 items-center gap-1.5 px-3 py-2 text-sm font-medium whitespace-nowrap transition-colors border-b-2 cursor-pointer ${
                            activeTab === tab.id
                                ? 'border-cyan-500 text-cyan-600 dark:text-cyan-400'
                                : 'border-transparent text-gray-500 hover:text-gray-700 dark:hover:text-gray-300'
                        }`}
                    >
                        {tab.icon}
                        {tab.label}
                    </button>
                ))}
            </div>

            {/* Content */}
            <div className="p-4 overflow-y-auto flex-1 min-h-0">
                {activeTab === 'hash' && <HashForgeTab nativeDropScope={nativeDropScope} />}
                {activeTab === 'crypto' && <CryptoLabTab />}
                {activeTab === 'password' && <PasswordForgeTab />}
            </div>
        </div>
    );
};
