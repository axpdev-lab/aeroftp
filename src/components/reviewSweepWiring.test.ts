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

    it('ignores stale search success, failure and completion and invalidates on close/session', () => {
        const start = appRaw.indexOf('const handleRemoteSearch = async');
        const body = appRaw.slice(start, appRaw.indexOf('// Check provider capabilities', start));
        expect(body).toContain('const isCurrent = remoteSearchRequest.begin()');
        expect(body).toMatch(/if \(!isCurrent\(\)\) return;\s*setRemoteSearchResults\(results\)/);
        expect(body).toMatch(/catch \(error\) \{\s*if \(!isCurrent\(\)\) return;/);
        expect(body).toContain('if (isCurrent()) setRemoteSearching(false)');
        expect(appRaw).toContain('activeSessionId, isConnected, showRemoteSearchBar, currentRemotePath');
        expect(appRaw).toContain("cancelRemoteSearch(); setShowRemoteSearchBar(false)");
    });

    it('says the recovery kit was not cached when the metadata write fails', () => {
        const start = cryptRaw.indexOf('const handleMigrateLegacyMarker');
        const body = cryptRaw.slice(start, cryptRaw.indexOf('setMarkerMigrating(false)', start));
        expect(body).toMatch(/await invoke\('store_overlay_metadata'[^\n]*\n\s*kitMetadataStaleRef\.current = false;\n\s*\} catch \{[^}]*setError\(t\('aerocryptNative\.overlayMetadataNotSaved'\)\);\s*return;/);
    });
});
