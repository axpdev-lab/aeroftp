import React, { useState, useEffect, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen, UnlistenFn } from '@tauri-apps/api/event';
import { WatcherStatus as WatcherStatusType } from '../types';
import { useTranslation } from '../i18n';
import { Eye, EyeOff, AlertTriangle } from 'lucide-react';

interface WatcherStatusProps {
    watchPath?: string;
}

export const WatcherStatus: React.FC<WatcherStatusProps> = ({ watchPath }) => {
    const t = useTranslation();
    const [active, setActive] = useState(false);
    const [mode, setMode] = useState<string | null>(null);
    const [status, setStatus] = useState<WatcherStatusType | null>(null);
    const unlistenRef = useRef<UnlistenFn | null>(null);

    useEffect(() => {
        let mounted = true;

        // Get watcher capabilities
        invoke<WatcherStatusType>('get_watcher_status_cmd', { watchPath: watchPath || null })
            .then(s => { if (mounted) setStatus(s); })
            .catch(() => {});

        // Listen for watcher status events from backend
        listen<{ active: boolean; mode?: string; path?: string }>(
            'cloud-watcher-status',
            (event) => {
                setActive(event.payload.active);
                if (event.payload.mode) setMode(event.payload.mode);
            }
        ).then(fn => {
            if (mounted) {
                unlistenRef.current = fn;
            } else {
                fn(); // Already unmounted, clean up immediately
            }
        });

        return () => {
            mounted = false;
            unlistenRef.current?.();
        };
    }, [watchPath]);

    const inotifyWarning = status?.inotify_capacity?.should_warn;
    const inotifyFallback = status?.inotify_capacity?.should_fallback_to_poll;

    return (
        <div className="flex items-center gap-2 text-xs">
            {active ? (
                <>
                    <Eye size={12} className="text-green-400" />
                    <span className="text-green-400">{t('syncPanel.watcherActive')}</span>
                    {mode && (
                        <span className="text-gray-500">({mode})</span>
                    )}
                </>
            ) : (
                <>
                    <EyeOff size={12} className="text-gray-500" />
                    <span className="text-gray-500">{t('syncPanel.watcherInactive')}</span>
                </>
            )}
            {status?.native_backend && (
                <span className="text-gray-600 text-[10px]">
                    {t('syncPanel.watcherBackend')}: {status.native_backend}
                </span>
            )}
            {inotifyWarning && !inotifyFallback && (
                <span className="text-yellow-400 flex items-center gap-0.5" title={t('syncPanel.watcherInotifyWarning')}>
                    <AlertTriangle size={10} />
                </span>
            )}
            {inotifyFallback && (
                <span className="text-orange-400 text-[10px]" title={t('syncPanel.watcherPolling')}>
                    ({t('syncPanel.watcherModePoll')})
                </span>
            )}
        </div>
    );
};
