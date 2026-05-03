// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
//
// Per-protocol-class breakdown table. Renders below the main My Servers
// table when the user has enough profiles or enough protocol diversity to
// benefit from the summary. Uses the same dedup logic the footer uses, so
// quotas are summed once per physical disk.

import * as React from 'react';
import { ServerProfile } from '../../types';
import { aggregateByDedupKey } from '../../utils/storageDedup';
import { formatBytes } from '../../utils/formatters';
import {
    getStorageTone,
    DEFAULT_THRESHOLDS,
    StorageThresholds,
    TONE_TEXT_CLASS,
} from '../../hooks/useStorageThresholds';
import { useTranslation } from '../../i18n';

interface MyServersProtocolBreakdownProps {
    servers: ServerProfile[];
    thresholds?: StorageThresholds;
}

const VISIBILITY_MIN_PROFILES = 5;
const VISIBILITY_MIN_DISTINCT_CLASSES = 2;

/**
 * Returns true when the dataset is interesting enough to render a per-protocol
 * breakdown table. Exposed so the footer toggle can hide the toggle affordance
 * for trivial setups (1-2 profiles of the same class).
 */
export const breakdownIsAvailable = (servers: ServerProfile[]): boolean => {
    if (servers.length === 0) return false;
    if (servers.length >= VISIBILITY_MIN_PROFILES) return true;
    const aggregate = aggregateByDedupKey(servers);
    const distinctClasses = aggregate.byProtocolClass.filter(row => row.profiles > 0).length;
    return distinctClasses >= VISIBILITY_MIN_DISTINCT_CLASSES;
};

export function MyServersProtocolBreakdown({
    servers,
    thresholds = DEFAULT_THRESHOLDS,
}: MyServersProtocolBreakdownProps) {
    const t = useTranslation();
    const aggregate = React.useMemo(() => aggregateByDedupKey(servers), [servers]);
    const distinctClasses = aggregate.byProtocolClass.filter(row => row.profiles > 0).length;

    if (servers.length < VISIBILITY_MIN_PROFILES && distinctClasses < VISIBILITY_MIN_DISTINCT_CLASSES) {
        return null;
    }
    if (aggregate.byProtocolClass.length === 0) {
        return null;
    }

    const meanPct = aggregate.totalTotal > 0
        ? (aggregate.totalUsed / aggregate.totalTotal) * 100
        : null;
    const totalTone = getStorageTone(aggregate.totalUsed, aggregate.totalTotal, thresholds);

    return (
        <section className="mt-6">
            <h3 className="text-sm font-semibold text-gray-700 dark:text-gray-300 mb-2">
                {t('introHub.breakdown.title')}
            </h3>
            <table className="w-full table-auto border-collapse text-left text-sm">
                <thead>
                    <tr className="border-b border-gray-200 dark:border-gray-700 text-xs uppercase tracking-wide text-gray-500 dark:text-gray-400">
                        <th className="px-3 py-1.5 font-medium">{t('introHub.breakdown.colProtocol')}</th>
                        <th className="px-3 py-1.5 font-medium text-right tabular-nums">
                            {t('introHub.breakdown.colProfiles')}
                        </th>
                        <th className="px-3 py-1.5 font-medium text-right tabular-nums">
                            {t('introHub.breakdown.colUnique')}
                        </th>
                        <th className="px-3 py-1.5 font-medium text-right tabular-nums">
                            {t('introHub.breakdown.colUsed')}
                        </th>
                        <th className="px-3 py-1.5 font-medium text-right tabular-nums">
                            {t('introHub.breakdown.colTotal')}
                        </th>
                        <th className="px-3 py-1.5 font-medium text-right tabular-nums">
                            {t('introHub.breakdown.colPct')}
                        </th>
                    </tr>
                </thead>
                <tbody>
                    {aggregate.byProtocolClass.map(row => {
                        const tone = getStorageTone(row.used, row.total, thresholds);
                        const pct = row.total > 0 ? (row.used / row.total) * 100 : null;
                        return (
                            <tr
                                key={row.protocolClass}
                                className="border-b border-gray-100 dark:border-gray-800"
                            >
                                <td className="px-3 py-1.5 font-medium text-gray-700 dark:text-gray-300">
                                    {row.protocolClass}
                                </td>
                                <td className="px-3 py-1.5 text-right tabular-nums text-gray-600 dark:text-gray-400">
                                    {row.profiles}
                                </td>
                                <td className="px-3 py-1.5 text-right tabular-nums text-gray-600 dark:text-gray-400">
                                    {row.unique}
                                </td>
                                <td className="px-3 py-1.5 text-right tabular-nums text-gray-600 dark:text-gray-400">
                                    {row.total > 0 ? formatBytes(row.used) : '—'}
                                </td>
                                <td className="px-3 py-1.5 text-right tabular-nums text-gray-600 dark:text-gray-400">
                                    {row.total > 0 ? formatBytes(row.total) : '—'}
                                </td>
                                <td
                                    className={`px-3 py-1.5 text-right tabular-nums ${TONE_TEXT_CLASS[tone.tone]}`}
                                >
                                    {pct !== null ? `${pct.toFixed(1)}%` : '—'}
                                </td>
                            </tr>
                        );
                    })}
                </tbody>
                <tfoot>
                    <tr className="font-semibold text-gray-800 dark:text-gray-200">
                        <td className="px-3 py-1.5">{t('introHub.breakdown.totalRow')}</td>
                        <td className="px-3 py-1.5 text-right tabular-nums">{aggregate.profiles}</td>
                        <td className="px-3 py-1.5 text-right tabular-nums">{aggregate.uniqueCount}</td>
                        <td className="px-3 py-1.5 text-right tabular-nums">
                            {aggregate.totalTotal > 0 ? formatBytes(aggregate.totalUsed) : '—'}
                        </td>
                        <td className="px-3 py-1.5 text-right tabular-nums">
                            {aggregate.totalTotal > 0 ? formatBytes(aggregate.totalTotal) : '—'}
                        </td>
                        <td
                            className={`px-3 py-1.5 text-right tabular-nums ${TONE_TEXT_CLASS[totalTone.tone]}`}
                        >
                            {meanPct !== null ? `${meanPct.toFixed(1)}%` : '—'}
                        </td>
                    </tr>
                </tfoot>
            </table>
        </section>
    );
}
