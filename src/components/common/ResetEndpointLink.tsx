// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { RotateCcw } from 'lucide-react';
import { useTranslation } from '../../i18n';
import type { ProviderConfig } from '../../providers/types';
import {
    isEndpointCustomized,
    presetEndpointDefault,
    type EndpointDefault,
    type EndpointParamsLike,
} from '../../providers/presetEndpoint';

interface ResetEndpointLinkProps {
    /** The selected preset; generic providers have no default and render nothing. */
    provider: ProviderConfig | null | undefined;
    /** The current connection params slice used to compare against the default. */
    params: EndpointParamsLike;
    /** Port used when neither the params nor the preset name one. */
    fallbackPort?: number;
    /** Applies the default to the connection params; does not touch anything else. */
    onReset: (def: EndpointDefault) => void;
}

/**
 * "Reset to default" link for an editable preset endpoint. Shown only when the
 * provider is a preset (not generic) and the current value differs from the
 * preset default (trailing slash not a difference; server and port both
 * compared), so it appears exactly when a reset would change something.
 */
export function ResetEndpointLink({ provider, params, fallbackPort, onReset }: ResetEndpointLinkProps) {
    const t = useTranslation();
    const def = presetEndpointDefault(provider, params);
    if (!def || !isEndpointCustomized(provider, params, fallbackPort)) return null;
    return (
        <button
            type="button"
            onClick={() => onReset(def)}
            className="inline-flex items-center gap-1 text-xs text-blue-500 hover:text-blue-600 dark:text-blue-400 dark:hover:text-blue-300"
        >
            <RotateCcw size={10} />
            {t('connection.resetEndpointToDefault')}
        </button>
    );
}
