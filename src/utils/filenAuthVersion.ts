// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import type { ServerProfile } from '../types';

const toPositiveInteger = (value: unknown): number | null => {
    if (typeof value === 'number' && Number.isFinite(value) && value > 0) {
        return Math.trunc(value);
    }
    if (typeof value === 'string' && /^\d+$/.test(value.trim())) {
        const parsed = Number(value.trim());
        return parsed > 0 ? parsed : null;
    }
    return null;
};

export const getFilenAuthVersion = (server: ServerProfile): number | null => {
    if (server.protocol !== 'filen') return null;
    const options = (server.options || {}) as Record<string, unknown>;
    return (
        toPositiveInteger(options.filen_auth_version)
        ?? toPositiveInteger(options.authVersion)
        ?? toPositiveInteger(options.filenAuthVersion)
    );
};
