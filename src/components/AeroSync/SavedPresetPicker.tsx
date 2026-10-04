// SPDX-License-Identifier: GPL-3.0-or-later

import * as React from 'react';
import { invoke } from '@tauri-apps/api/core';
import type { SyncProfile } from '../../types';
import { useTranslation } from '../../i18n';
import { SYNC_PROFILES_CHANGED_EVENT } from '../../utils/syncProfiles';

/** Read profiles without allowing an older request or an unmount to publish. */
export function useSavedSyncProfiles() {
    const [profiles, setProfiles] = React.useState<SyncProfile[]>([]);
    const [loading, setLoading] = React.useState(true);
    const [error, setError] = React.useState<string | null>(null);
    React.useEffect(() => {
        let generation = 0;
        const reload = async () => {
            const request = ++generation;
            setLoading(true);
            setError(null);
            try {
                const loaded = await invoke<SyncProfile[]>('load_sync_profiles_cmd');
                if (request === generation) setProfiles(loaded.filter(p => p.builtin !== true));
            } catch (err) {
                if (request === generation) {
                    setProfiles([]);
                    setError(String(err));
                }
            } finally {
                if (request === generation) setLoading(false);
            }
        };
        void reload();
        window.addEventListener(SYNC_PROFILES_CHANGED_EVENT, reload);
        return () => {
            ++generation;
            window.removeEventListener(SYNC_PROFILES_CHANGED_EVENT, reload);
        };
    }, []);
    return { profiles, loading, error };
}

export const SavedPresetPicker: React.FC<{
    profiles: SyncProfile[];
    loading: boolean;
    error: string | null;
    selectedId: string;
    onSelect: (profile: SyncProfile) => void;
}> = ({ profiles, loading, error, selectedId, onSelect }) => {
    const t = useTranslation();
    return (
        <div className="mt-3 space-y-1 text-xs">
            <label className="flex items-center gap-2">
                <span>{t('aerosync.savedPresets')}</span>
                <select
                    aria-label={t('aerosync.savedPresets')}
                    value={selectedId}
                    disabled={loading || !!error || profiles.length === 0}
                    onChange={event => {
                        const profile = profiles.find(p => p.id === event.target.value);
                        if (profile) onSelect(profile);
                    }}
                    className="min-w-0 rounded border border-gray-200 bg-white px-2 py-1 dark:border-gray-700 dark:bg-gray-900"
                >
                    <option value="" disabled>{loading ? t('common.loading') : t('aerosync.savedPresets')}</option>
                    {profiles.map(p => <option key={p.id} value={p.id}>{p.name}</option>)}
                </select>
            </label>
            {error && <p role="alert" className="text-red-600 dark:text-red-400">{t('aerosync.savedPresetsLoadFailed', { error })}</p>}
            {selectedId && <p className="text-gray-500 dark:text-gray-400">{t('aerosync.savedPresetLimits')}</p>}
        </div>
    );
};
