// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import React from 'react';
import { useTranslation } from '../i18n';
import { MODAL_Z } from '../utils/modalLayers';
import type { ProtonPathChange, ProtonSwitchPlan } from '../utils/protonModeSwitch';

interface ProtonModeSwitchDialogProps {
    profileName: string;
    plan: ProtonSwitchPlan;
    onConfirm: () => void;
    onCancel: () => void;
}

/**
 * What a Proton Drive profile's switch between the API and the CLI moves,
 * shown before anything is written (Requested 79): the saved paths that move
 * to the other spelling of My Files, those kept for lack of an equivalent,
 * and the sync histories too old to be attributed.
 */
export const ProtonModeSwitchDialog: React.FC<ProtonModeSwitchDialogProps> = ({ profileName, plan, onConfirm, onCancel }) => {
    const t = useTranslation();
    const target = plan.to === 'native' ? 'API' : 'CLI';
    const holder = (change: ProtonPathChange): string => {
        switch (change.kind) {
            case 'initialPath': return t('connection.protonSwitch.initialPath');
            case 'aerocloud': return t('connection.protonSwitch.aerocloud');
            case 'aerocloudPair': return t('connection.protonSwitch.aerocloudPair', { name: change.label });
            case 'pendingSync': return t('connection.protonSwitch.pendingSync', { folder: change.label });
            default: return t('connection.protonSwitch.syncHistory', { folder: change.label });
        }
    };
    const moved = plan.changes.filter((change) => change.to !== null);
    const kept = plan.changes.filter((change) => change.to === null);
    const title = t('connection.protonSwitch.title', { name: profileName, mode: target });
    return (
        <div className={`fixed inset-0 bg-black/50 flex items-center justify-center ${MODAL_Z.modal}`} role="dialog" aria-modal="true" aria-label={title}>
            <div className="bg-white dark:bg-gray-800 rounded-lg p-6 shadow-2xl max-w-lg w-full mx-4 animate-scale-in">
                <h3 className="text-base font-semibold text-gray-900 dark:text-gray-100 mb-2">{title}</h3>
                <p className="text-sm text-gray-600 dark:text-gray-300 mb-3">{t('connection.protonSwitch.intro')}</p>
                {moved.length === 0 && kept.length === 0 ? (
                    <p className="text-sm text-gray-700 dark:text-gray-200 mb-3">{t('connection.protonSwitch.nothing')}</p>
                ) : null}
                {moved.length > 0 ? (
                    <ul className="text-sm text-gray-800 dark:text-gray-100 mb-3 space-y-1 max-h-48 overflow-y-auto">
                        {moved.map((change) => (
                            <li key={`${change.kind}:${change.label}:${change.from}`}>
                                <span className="text-gray-500 dark:text-gray-400">{holder(change)}: </span>
                                <code className="break-all">{change.from}</code>
                                {' → '}
                                <code className="break-all">{change.to}</code>
                            </li>
                        ))}
                    </ul>
                ) : null}
                {kept.length > 0 ? (
                    <div className="text-sm mb-3">
                        <p className="text-amber-700 dark:text-amber-400 mb-1">{t('connection.protonSwitch.noEquivalent', { mode: target })}</p>
                        <ul className="space-y-1 text-gray-800 dark:text-gray-100">
                            {kept.map((change) => (
                                <li key={`${change.kind}:${change.label}:${change.from}`}>
                                    <span className="text-gray-500 dark:text-gray-400">{holder(change)}: </span>
                                    <code className="break-all">{change.from}</code>
                                </li>
                            ))}
                        </ul>
                    </div>
                ) : null}
                {plan.legacyHistories > 0 ? (
                    <p className="text-xs text-gray-500 dark:text-gray-400 mb-3">
                        {t(plan.legacyHistories === 1 ? 'connection.protonSwitch.legacy_one' : 'connection.protonSwitch.legacy_many', { count: plan.legacyHistories })}
                    </p>
                ) : null}
                <div className="flex justify-end gap-2">
                    <button
                        onClick={onCancel}
                        className="px-4 py-2 text-gray-600 dark:text-gray-400 hover:bg-gray-100 dark:hover:bg-gray-700 rounded-lg"
                    >
                        {t('common.cancel')}
                    </button>
                    <button
                        onClick={onConfirm}
                        className="px-4 py-2 text-white rounded-lg bg-blue-500 hover:bg-blue-600"
                    >
                        {t('connection.protonSwitch.confirm')}
                    </button>
                </div>
            </div>
        </div>
    );
};
