// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import appRaw from '../App.tsx?raw';
import cryptRaw from './AeroCryptUnlock.tsx?raw';

describe('a failure that would otherwise read as success', () => {
    it('shows a failed remote search with notifications off', () => {
        const start = appRaw.indexOf('const handleRemoteSearch = async');
        const body = appRaw.slice(start, appRaw.indexOf('setRemoteSearching(false)', start));
        expect(body).toMatch(/notify\.error\(t\('common\.error'\), String\(error\)\);/);
        expect(body).toMatch(/if \(!showToastNotifications\) toastError\(t\('common\.error'\), String\(error\)\);/);
    });

    it('says the recovery kit was not cached when the metadata write fails', () => {
        expect(cryptRaw).toMatch(/await invoke\('store_overlay_metadata'[^\n]*\n\s*\} catch \{\n\s*setError\(t\('aerocryptNative\.overlayMetadataNotSaved'\)\);/);
    });
});
