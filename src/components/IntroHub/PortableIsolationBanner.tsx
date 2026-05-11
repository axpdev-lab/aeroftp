// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { useEffect, useState, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Shield, AlertTriangle, X, FolderOpen } from 'lucide-react';
import { useTranslation } from '../../i18n';
import { secureGet, secureStore } from '../../utils/secureStorage';

interface PortableInfo {
    is_portable: boolean;
    data_root: string | null;
    webview_data_dir: string | null;
    credential_store_dir: string | null;
    shared_legacy_present: boolean;
}

const DISMISS_FLAG_KEY = 'portable_isolation_notice_v3_7_8';

export const PortableIsolationBanner: React.FC = () => {
    const t = useTranslation();
    const [info, setInfo] = useState<PortableInfo | null>(null);
    const [dismissed, setDismissed] = useState<boolean>(true);

    useEffect(() => {
        let cancelled = false;
        (async () => {
            try {
                const data = await invoke<PortableInfo>('portable_info');
                if (cancelled) return;
                if (!data.is_portable) return;
                const flag = await secureGet<boolean>(DISMISS_FLAG_KEY);
                if (cancelled) return;
                setInfo(data);
                setDismissed(flag === true);
            } catch {
                // Non-portable / older backend: stay hidden.
            }
        })();
        return () => {
            cancelled = true;
        };
    }, []);

    const handleDismiss = useCallback(async () => {
        setDismissed(true);
        try {
            await secureStore(DISMISS_FLAG_KEY, true);
        } catch {
            // Non-fatal: worst case the banner reappears next launch.
        }
    }, []);

    const openDataRoot = useCallback(async () => {
        if (!info?.data_root) return;
        try {
            await invoke('open_in_file_manager', { path: info.data_root });
        } catch {
            /* ignore */
        }
    }, [info]);

    if (!info || !info.is_portable || dismissed) return null;

    return (
        <div
            className="mx-4 mt-3 mb-2 rounded-lg border border-violet-300/60 bg-violet-50 dark:bg-violet-950/40 dark:border-violet-700/60 px-4 py-3 text-sm"
            role="status"
            aria-live="polite"
        >
            <div className="flex items-start gap-3">
                <div className="shrink-0 mt-0.5">
                    <Shield className="w-5 h-5 text-violet-600 dark:text-violet-300" />
                </div>
                <div className="flex-1 min-w-0">
                    <div className="font-semibold text-violet-900 dark:text-violet-100 mb-1">
                        {t('portable.banner.title')}
                    </div>
                    <div className="text-violet-800 dark:text-violet-200">
                        {t('portable.banner.body')}
                    </div>
                    {info.data_root && (
                        <div className="mt-2 font-mono text-xs text-violet-700 dark:text-violet-300 break-all">
                            {info.data_root}
                        </div>
                    )}
                    {info.shared_legacy_present && (
                        <div className="mt-3 flex items-start gap-2 rounded-md border border-amber-300/60 bg-amber-50 dark:bg-amber-950/30 dark:border-amber-700/60 px-3 py-2">
                            <AlertTriangle className="w-4 h-4 text-amber-600 dark:text-amber-400 shrink-0 mt-0.5" />
                            <div className="text-amber-900 dark:text-amber-200 text-xs">
                                {t('portable.banner.legacyWarning')}
                            </div>
                        </div>
                    )}
                    <div className="mt-3 flex flex-wrap items-center gap-2">
                        {info.data_root && (
                            <button
                                type="button"
                                onClick={openDataRoot}
                                className="inline-flex items-center gap-1.5 rounded-md bg-violet-600 hover:bg-violet-700 text-white px-3 py-1.5 text-xs font-medium transition-colors"
                            >
                                <FolderOpen className="w-3.5 h-3.5" />
                                {t('portable.banner.openFolder')}
                            </button>
                        )}
                        <button
                            type="button"
                            onClick={handleDismiss}
                            className="inline-flex items-center gap-1.5 rounded-md border border-violet-300 dark:border-violet-700 hover:bg-violet-100 dark:hover:bg-violet-900/40 text-violet-700 dark:text-violet-200 px-3 py-1.5 text-xs font-medium transition-colors"
                        >
                            {t('portable.banner.dismiss')}
                        </button>
                    </div>
                </div>
                <button
                    type="button"
                    onClick={handleDismiss}
                    aria-label={t('portable.banner.dismiss')}
                    className="shrink-0 -mt-1 -mr-1 p-1 rounded-md hover:bg-violet-100 dark:hover:bg-violet-900/40 text-violet-700 dark:text-violet-300 transition-colors"
                >
                    <X className="w-4 h-4" />
                </button>
            </div>
        </div>
    );
};

export default PortableIsolationBanner;
