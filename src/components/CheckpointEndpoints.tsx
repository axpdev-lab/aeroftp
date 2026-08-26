// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { useCallback, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Trash2, RefreshCw } from 'lucide-react';
import { useTranslation } from '../i18n';
import { logger } from '../utils/logger';

interface CheckpointEndpoint {
    provider: string;
    protocol: string;
    host: string;
    account: string;
    records: number;
    updatedUnixSecs: number;
}

/**
 * The resume store, per destination, with a way to clear one.
 *
 * The store is capped, and the documented escape from the cap is forgetting a
 * destination. That escape existed only on `aeroftp checkpoints`, so a
 * GUI-only user could watch the cap drop their oldest resumable transfer with
 * no way to clear a server they had decommissioned instead.
 *
 * The listing is not decoration: the backend matches all four identity values
 * exactly, so showing them is the only way a person can supply them.
 */
export const CheckpointEndpoints: React.FC = () => {
    const t = useTranslation();
    const [rows, setRows] = useState<CheckpointEndpoint[] | null>(null);
    const [error, setError] = useState<string | null>(null);
    const [busy, setBusy] = useState<string | null>(null);
    const [confirming, setConfirming] = useState<string | null>(null);

    const identity = (r: CheckpointEndpoint) => `${r.provider}/${r.protocol}/${r.host}/${r.account}`;

    const load = useCallback(async () => {
        try {
            setError(null);
            setRows(await invoke<CheckpointEndpoint[]>('checkpoint_endpoints'));
        } catch (e) {
            logger.error('[checkpoints] listing failed:', e);
            setError(String(e));
            setRows([]);
        }
    }, []);

    useEffect(() => { void load(); }, [load]);

    const forget = async (row: CheckpointEndpoint) => {
        const id = identity(row);
        setBusy(id);
        setConfirming(null);
        try {
            const removed = await invoke<number>('checkpoint_forget_endpoint', {
                provider: row.provider,
                protocol: row.protocol,
                host: row.host,
                account: row.account,
            });
            // Zero removed is not a failure, it is the end state already being
            // true. Reloading rather than splicing keeps the view honest about
            // what the store actually holds.
            logger.info(`[checkpoints] forgot ${id}: ${removed} record(s)`);
            await load();
        } catch (e) {
            logger.error('[checkpoints] forget failed:', e);
            setError(String(e));
        } finally {
            setBusy(null);
        }
    };

    return (
        <div>
            <div className="flex items-center justify-between mb-1">
                <label className="block text-sm font-medium">{t('settings.resumeRecords')}</label>
                <button
                    type="button"
                    onClick={() => void load()}
                    className="p-1.5 text-gray-400 hover:text-gray-600 dark:hover:text-gray-200 rounded transition-colors"
                    title={t('common.refresh')}
                >
                    <RefreshCw size={14} />
                </button>
            </div>
            <p className="text-xs text-gray-500 dark:text-gray-400 mb-2">{t('settings.resumeRecordsDesc')}</p>

            {error && <p className="text-xs text-red-600 dark:text-red-400 mb-2">{error}</p>}

            {rows === null ? (
                <p className="text-xs text-gray-500">{t('common.loading')}</p>
            ) : rows.length === 0 ? (
                <p className="text-xs text-gray-500">{t('settings.resumeRecordsEmpty')}</p>
            ) : (
                <div className="overflow-x-auto rounded-lg border border-gray-200 dark:border-gray-700">
                    <table className="w-full text-xs">
                        <thead className="bg-gray-50 dark:bg-gray-800/60 text-gray-500 dark:text-gray-400">
                            <tr>
                                <th className="text-left font-medium px-3 py-2">{t('settings.resumeColumnDestination')}</th>
                                <th className="text-right font-medium px-3 py-2">{t('settings.resumeColumnRecords')}</th>
                                <th className="px-3 py-2" />
                            </tr>
                        </thead>
                        <tbody>
                            {rows.map((row) => {
                                const id = identity(row);
                                return (
                                    <tr key={id} className="border-t border-gray-100 dark:border-gray-700/60">
                                        <td className="px-3 py-2">
                                            <div className="font-medium text-gray-700 dark:text-gray-200">{row.host}</div>
                                            <div className="text-gray-500 dark:text-gray-400">
                                                {row.provider} · {row.protocol} · {row.account}
                                            </div>
                                        </td>
                                        <td className="px-3 py-2 text-right tabular-nums text-gray-600 dark:text-gray-300">{row.records}</td>
                                        <td className="px-3 py-2 text-right whitespace-nowrap">
                                            {confirming === id ? (
                                                <>
                                                    <button
                                                        type="button"
                                                        onClick={() => void forget(row)}
                                                        disabled={busy === id}
                                                        className="px-2 py-1 rounded bg-red-600 hover:bg-red-700 text-white disabled:opacity-50"
                                                    >
                                                        {t('settings.resumeForgetConfirm')}
                                                    </button>
                                                    <button
                                                        type="button"
                                                        onClick={() => setConfirming(null)}
                                                        className="ml-2 px-2 py-1 rounded text-gray-500 hover:text-gray-700 dark:hover:text-gray-200"
                                                    >
                                                        {t('common.cancel')}
                                                    </button>
                                                </>
                                            ) : (
                                                <button
                                                    type="button"
                                                    onClick={() => setConfirming(id)}
                                                    disabled={busy === id}
                                                    className="p-1.5 text-gray-400 hover:text-red-600 rounded transition-colors disabled:opacity-50"
                                                    title={t('settings.resumeForget')}
                                                >
                                                    <Trash2 size={14} />
                                                </button>
                                            )}
                                        </td>
                                    </tr>
                                );
                            })}
                        </tbody>
                    </table>
                </div>
            )}
        </div>
    );
};
