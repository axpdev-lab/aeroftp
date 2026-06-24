// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { Clock, LucideIcon } from 'lucide-react';
import { useTranslation } from '../../i18n';

export type VaultViewTab = 'files' | 'recent';

interface VaultViewTabsProps {
    activeTab: VaultViewTab;
    onSelect: (tab: VaultViewTab) => void;
    /** Label of the primary (left) tab. Standalone intro -> "Home", browser -> "Files". */
    primaryLabel: string;
    /** Icon of the primary tab (Home for the intro landing, Files for the browser). */
    primaryIcon: LucideIcon;
}

/**
 * Tab strip shared by the standalone intro modal AND the vault browser
 * (owner-locked unified layout, 2026-06-24). The primary (left) tab varies by
 * context - "Home" (the create/open landing) standalone, "Files" (the browser)
 * once a vault is open - while the `[Recent]` tab (the reusable cronologia) is
 * identical in both. Labels reuse existing translated keys (the caller passes
 * the primary one; Recent uses `sidebar.recent`), so no new i18n keys are added.
 */
export const VaultViewTabs: React.FC<VaultViewTabsProps> = ({ activeTab, onSelect, primaryLabel, primaryIcon }) => {
    const t = useTranslation();
    const tabs: { id: VaultViewTab; label: string; Icon: LucideIcon }[] = [
        { id: 'files', label: primaryLabel, Icon: primaryIcon },
        { id: 'recent', label: t('sidebar.recent'), Icon: Clock },
    ];
    return (
        <div className="flex items-center gap-1 px-3 pt-2 border-b border-gray-200 dark:border-gray-700" role="tablist">
            {tabs.map(({ id, label, Icon }) => {
                const active = activeTab === id;
                return (
                    <button
                        key={id}
                        role="tab"
                        aria-selected={active}
                        onClick={() => onSelect(id)}
                        className={`flex items-center gap-1.5 px-3 py-1.5 text-sm font-medium rounded-t border-b-2 -mb-px transition-colors ${active
                            ? 'border-emerald-500 text-emerald-600 dark:text-emerald-400'
                            : 'border-transparent text-gray-500 dark:text-gray-400 hover:text-gray-700 dark:hover:text-gray-200'
                            }`}
                    >
                        <Icon size={14} /> {label}
                    </button>
                );
            })}
        </div>
    );
};
