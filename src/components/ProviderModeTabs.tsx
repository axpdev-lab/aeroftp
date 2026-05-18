// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Z.4.5 R2 generalized provider mode tabs (originally FileLu-only,
 * extended to Filen and any other group declared in
 * `providerModeGroups.ts`).
 *
 * When the active form preset / protocol belongs to one of the
 * registered mode groups, this component renders an inline chip strip
 * above the form. Clicking a chip switches the form to that mode via
 * `onSwitchMode(protocol, providerId?)` — typically wired to
 * `ConnectionScreen.handleProtocolChange`.
 *
 * Renders nothing when no group is active, so it can be unconditionally
 * mounted in the form layout.
 */
import React from 'react';
import { FileText } from 'lucide-react';
import { useTranslation } from '../i18n';
import {
    findActiveMode,
    findActiveModeGroup,
} from './providerModeGroups';
import type { ProviderType } from '../types';

interface ProviderModeTabsProps {
    /** Active form providerId (may be undefined when the user picked a
     *  protocol directly via the ProtocolSelector and no preset is
     *  involved, e.g. Filen native). */
    activeProviderId: string | null | undefined;
    /** Active form protocol. Used as a fallback match key for
     *  preset-less modes. */
    activeProtocol: string | null | undefined;
    /** Called when the operator clicks a sub-tab. Should route through
     *  the form's own `handleProtocolChange(newProtocol, newProviderId?)`. */
    onSwitchMode: (protocol: ProviderType, providerId?: string) => void;
    /** When true the strip is shown but locked: it only indicates the
     *  current mode of a saved profile being edited and cannot switch
     *  surface. A profile keeps the mode chosen at creation time; to use
     *  a different mode the user creates a new connection. Switching mode
     *  in edit would silently rewrite the saved profile's
     *  providerId/server/credentials (issue #215). The FTP/FTPS/SFTP
     *  in-place switch is a separate, deliberately allowed path handled
     *  by the ProtocolSelector, not by this strip. */
    readOnly?: boolean;
    /** Optional className override for the outer container. */
    className?: string;
}

export const ProviderModeTabs: React.FC<ProviderModeTabsProps> = ({
    activeProviderId,
    activeProtocol,
    onSwitchMode,
    readOnly,
    className,
}) => {
    const t = useTranslation();
    const group = findActiveModeGroup(activeProviderId, activeProtocol);

    if (!group) {
        return null;
    }

    const active = findActiveMode(group, activeProviderId, activeProtocol);
    const warning = active && group.activeWarnings?.[active.label];

    // Per-group i18n key for the active warning. Falls back to the
    // hardcoded English string declared in providerModeGroups. Each
    // group can register its own translation under
    // `provider.modes.<group.id>.<modeLabelKebab>` if it wants.
    const resolveWarning = (g: typeof group, modeLabel: string, fallback: string): string => {
        const kebab = modeLabel.toLowerCase().replace(/\s+/g, '-');
        const key = `provider.modes.${g.id}.${kebab}`;
        const translated = t(key);
        return translated && translated !== key ? translated : fallback;
    };

    // Locked-in-edit hint. Follows the same translate-or-fallback pattern
    // as resolveWarning so it does not require an i18n key to exist yet.
    const lockedKey = 'provider.modes.lockedInEdit';
    const lockedTranslated = t(lockedKey);
    const lockedHint =
        lockedTranslated && lockedTranslated !== lockedKey
            ? lockedTranslated
            : 'Mode is chosen when the connection is created and stays fixed while editing a saved profile. To use a different mode, create a new connection.';

    return (
        <div
            className={
                className ||
                'mb-4 p-3 rounded-lg border border-gray-200 dark:border-gray-700 bg-gray-50 dark:bg-gray-800/60'
            }
            role="tablist"
            aria-label={`${group.headerLabel}`}
        >
            <div className="flex items-center gap-1.5 mb-2 flex-wrap">
                <FileText size={12} className="text-gray-400 dark:text-gray-500" />
                <span className="text-[11px] font-semibold uppercase tracking-wider text-gray-500 dark:text-gray-400 mr-2">
                    {group.headerLabel}
                </span>
                {group.modes.map(mode => {
                    const isActive = active === mode;
                    return (
                        <button
                            key={mode.providerId || `proto-${mode.protocol}`}
                            type="button"
                            role="tab"
                            aria-selected={isActive}
                            aria-disabled={!!readOnly && !isActive}
                            disabled={!!readOnly && !isActive}
                            onClick={() => { if (!readOnly) onSwitchMode(mode.protocol, mode.providerId); }}
                            title={readOnly && !isActive ? lockedHint : mode.description}
                            className={
                                isActive
                                    ? `inline-flex items-center gap-1.5 px-2.5 py-1 rounded-md text-[12px] font-medium border bg-white dark:bg-gray-900 border-gray-300 dark:border-gray-600 ${mode.activeColor}`
                                    : readOnly
                                    ? 'inline-flex items-center gap-1.5 px-2.5 py-1 rounded-md text-[12px] font-medium border border-transparent text-gray-600 dark:text-gray-300 opacity-50 cursor-not-allowed'
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
                                            ? `ml-1 text-[9px] font-bold px-1 py-0.5 rounded bg-purple-100 dark:bg-purple-900/40 text-purple-700 dark:text-purple-300`
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
            {readOnly && (
                <div className="mt-1.5 text-[11px] text-gray-500 dark:text-gray-500 leading-snug pl-1 italic">
                    {lockedHint}
                </div>
            )}
            {warning && (
                <div className="mt-2 text-[11px] text-amber-700 dark:text-amber-300 bg-amber-50 dark:bg-amber-900/20 border border-amber-200 dark:border-amber-800/40 rounded px-2 py-1.5 leading-snug">
                    {resolveWarning(group, active!.label, warning)}
                </div>
            )}
        </div>
    );
};

export default ProviderModeTabs;
