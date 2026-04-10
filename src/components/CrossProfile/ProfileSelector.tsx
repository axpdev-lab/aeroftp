// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { ServerProfile } from '../../types';

interface ProfileSelectorProps {
    profiles: ServerProfile[];
    selected: ServerProfile | null;
    onSelect: (profile: ServerProfile | null) => void;
    excludeId?: string;
}

export const ProfileSelector: React.FC<ProfileSelectorProps> = ({
    profiles,
    selected,
    onSelect,
    excludeId,
}) => {
    const filtered = excludeId ? profiles.filter((p) => p.id !== excludeId) : profiles;

    return (
        <select
            value={selected?.id || ''}
            onChange={(e) => {
                const profile = filtered.find((p) => p.id === e.target.value) || null;
                onSelect(profile);
            }}
            className="w-full px-3 py-2 border border-gray-300 dark:border-gray-600 rounded bg-white dark:bg-gray-700 text-gray-900 dark:text-white text-sm"
        >
            <option value="">Select profile...</option>
            {filtered.map((p) => (
                <option key={p.id} value={p.id}>
                    {p.name} ({p.protocol || 'ftp'})
                </option>
            ))}
        </select>
    );
};
