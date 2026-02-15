/**
 * Hook for loading and managing sync profiles
 */

import { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { SyncProfile, CompareOptions, RetryPolicy, VerifyPolicy, CompressionMode } from '../../types';

export interface ProfileApplyResult {
    options: CompareOptions;
    retryPolicy: RetryPolicy;
    verifyPolicy: VerifyPolicy;
    parallelStreams: number;
    compressionMode: CompressionMode;
}

export function useSyncProfiles(isOpen: boolean): {
    profiles: SyncProfile[];
    activeProfileId: string;
    setActiveProfileId: (id: string) => void;
    applyProfile: (profileId: string) => ProfileApplyResult | null;
} {
    const [profiles, setProfiles] = useState<SyncProfile[]>([]);
    const [activeProfileId, setActiveProfileId] = useState<string>('custom');

    useEffect(() => {
        if (isOpen) {
            invoke<SyncProfile[]>('load_sync_profiles_cmd')
                .then(p => setProfiles(p))
                .catch(() => {});
        }
    }, [isOpen]);

    const applyProfile = useCallback((profileId: string): ProfileApplyResult | null => {
        setActiveProfileId(profileId);
        if (profileId === 'custom') return null;
        const profile = profiles.find(p => p.id === profileId);
        if (!profile) return null;

        return {
            options: {
                compare_timestamp: profile.compare_timestamp,
                compare_size: profile.compare_size,
                compare_checksum: profile.compare_checksum,
                exclude_patterns: [...profile.exclude_patterns],
                direction: profile.direction,
            },
            retryPolicy: { ...profile.retry_policy },
            verifyPolicy: profile.verify_policy,
            parallelStreams: profile.parallel_streams || 1,
            compressionMode: profile.compression_mode || 'off',
        };
    }, [profiles]);

    return { profiles, activeProfileId, setActiveProfileId, applyProfile };
}
