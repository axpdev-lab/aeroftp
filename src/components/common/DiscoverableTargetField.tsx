// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import React, { useEffect, useId, useRef, useState } from 'react';
import { RefreshCw, Search } from 'lucide-react';
import { useTranslation } from '../../i18n';

export interface ConnectionTarget {
    value: string;
    label: string;
}

/** Non-secret change token for clearing results when credential inputs change. */
export function discoveryResetKey(...parts: Array<string | undefined>): string {
    let hash = 0x811c9dc5;
    for (const character of parts.join('\0')) {
        hash ^= character.charCodeAt(0);
        hash = Math.imul(hash, 0x01000193);
    }
    return (hash >>> 0).toString(16);
}

interface DiscoverableTargetFieldProps {
    label: string;
    value: string;
    onChange: (value: string) => void;
    onDiscover: () => Promise<ConnectionTarget[]>;
    canDiscover: boolean;
    disabled?: boolean;
    placeholder?: string;
    helpText?: string;
    required?: boolean;
    inputMode?: React.HTMLAttributes<HTMLInputElement>['inputMode'];
    maxLength?: number;
    resetKey?: string;
}

/**
 * Searchable provider target picker with a permanent manual fallback.
 * Discovery is user-triggered so credentials are never sent while typing;
 * restricted keys that cannot list account targets can still enter one.
 */
export const DiscoverableTargetField: React.FC<DiscoverableTargetFieldProps> = ({
    label,
    value,
    onChange,
    onDiscover,
    canDiscover,
    disabled = false,
    placeholder,
    helpText,
    required = false,
    inputMode,
    maxLength,
    resetKey,
}) => {
    const t = useTranslation();
    const listId = useId();
    const [targets, setTargets] = useState<ConnectionTarget[]>([]);
    const [loading, setLoading] = useState(false);
    const [error, setError] = useState('');
    const [hasFetched, setHasFetched] = useState(false);

    // Which set of credentials the results on screen belong to. Bumped on every
    // reset, and captured by each discovery so a reply that lands after the
    // credentials changed can be recognised as belonging to the old account.
    const epoch = useRef(0);

    useEffect(() => {
        epoch.current++;
        setTargets([]);
        setError('');
        setHasFetched(false);
        // Any request still in flight now belongs to the previous credentials
        // and will be dropped when it lands, so its spinner is released here
        // rather than by the reply, which no longer speaks for this form.
        setLoading(false);
    }, [resetKey]);

    const discover = async () => {
        const requested = epoch.current;
        setLoading(true);
        setError('');
        try {
            const discovered = await onDiscover();
            // A request is in flight for as long as the network takes, and the
            // user can edit the credentials meanwhile. Without this check the
            // late reply repopulated the list with the previous account's
            // targets, and a single result was written straight into the field
            // by the auto-select below: the form then held one account's
            // credentials pointing at another account's bucket.
            if (requested !== epoch.current) return;
            setTargets(discovered);
            setHasFetched(true);
            if (discovered.length === 1) onChange(discovered[0].value);
        } catch (reason) {
            if (requested !== epoch.current) return;
            setError(reason instanceof Error ? reason.message : String(reason));
        } finally {
            // The spinner belongs to the request that is current now: a stale
            // reply must not stop the spinner of the one that replaced it.
            if (requested === epoch.current) setLoading(false);
        }
    };

    return (
        <div>
            <label className="block text-sm font-medium mb-1.5">{label}</label>
            <div className="flex gap-2">
                <input
                    type="text"
                    list={targets.length > 0 ? listId : undefined}
                    value={value}
                    onChange={(event) => onChange(event.target.value)}
                    disabled={disabled}
                    className="min-w-0 flex-1 px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
                    placeholder={placeholder}
                    required={required}
                    inputMode={inputMode}
                    maxLength={maxLength}
                />
                <button
                    type="button"
                    onClick={discover}
                    disabled={disabled || loading || !canDiscover}
                    className="shrink-0 inline-flex items-center gap-1.5 px-3 py-2.5 bg-gray-100 dark:bg-gray-600 border border-gray-300 dark:border-gray-600 rounded-lg text-sm hover:bg-gray-200 dark:hover:bg-gray-500 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
                    title={t('connection.discovery.fetch')}
                >
                    {loading
                        ? <RefreshCw size={15} className="animate-spin" />
                        : <Search size={15} />}
                    {loading
                        ? t('connection.discovery.fetching')
                        : t(hasFetched ? 'connection.discovery.refresh' : 'connection.discovery.fetch')}
                </button>
            </div>
            {targets.length > 0 && (
                <datalist id={listId}>
                    {targets.map((target) => (
                        <option key={target.value} value={target.value}>
                            {target.label === target.value ? target.value : `${target.label} (${target.value})`}
                        </option>
                    ))}
                </datalist>
            )}
            {hasFetched && !error && (
                <p className="text-xs text-emerald-600 dark:text-emerald-400 mt-1">
                    {targets.length === 0
                        ? t('connection.discovery.none')
                        : t('connection.discovery.found', { count: targets.length })}
                </p>
            )}
            {error && <p className="text-xs text-red-600 dark:text-red-400 mt-1">{error}</p>}
            {helpText && <p className="text-xs text-gray-500 mt-1">{helpText}</p>}
        </div>
    );
};
