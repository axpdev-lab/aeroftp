// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { AlertTriangle } from 'lucide-react';
import { useTranslation } from '../i18n';
import type { ProviderConfig } from '../providers';

/**
 * Red warning banner shown inside the connection form when the selected
 * provider is flagged `stable: false`. The dev-only protocol grid already
 * hides these providers, but the SSOT-generated "Add Service" list view
 * (4.0.4) leaks them into production and opens a working form (see #308).
 * Until each provider is fully gated, this disclaimer makes the risk
 * explicit so users know failures are expected and not their fault.
 */
export const UnstableProviderNotice: React.FC<{ provider?: ProviderConfig | null }> = ({ provider }) => {
    const t = useTranslation();

    if (!provider || provider.stable !== false) return null;

    return (
        <div
            className="mb-3 rounded-lg border border-red-300/70 bg-red-50 dark:bg-red-950/40 dark:border-red-700/60 px-3 py-2.5 text-sm"
            role="alert"
            aria-live="polite"
        >
            <div className="flex items-start gap-2.5">
                <AlertTriangle className="w-5 h-5 text-red-600 dark:text-red-400 shrink-0 mt-0.5" />
                <div className="min-w-0">
                    <div className="font-semibold text-red-800 dark:text-red-200">
                        {t('connection.unstableTitle')}
                    </div>
                    <div className="text-red-700 dark:text-red-300 mt-0.5">
                        {t('connection.unstableBody', { provider: provider.name })}
                    </div>
                </div>
            </div>
        </div>
    );
};

export default UnstableProviderNotice;
