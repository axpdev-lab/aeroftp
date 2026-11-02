// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect, vi } from 'vitest';
import { transformWithOxc } from 'vite';
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
    it.each([
        { message: 'wrong factors', markerMayHaveChanged: false },
        { message: 'lost publication acknowledgement', markerMayHaveChanged: true },
        'plain migration failure',
        new Error('native failure'),
    ])('displays the real App migration rejection message: %s', async (err) => {
        const start = appRaw.indexOf("'aerocrypt_provider_migrate_legacy_marker'");
        const end = appRaw.indexOf('              })();', start);
        const section = appRaw.slice(start, end);
        const match = section.match(/} catch \(err\) \{\n(\s*const message[\s\S]*?)\n {16}\}/);
        if (!match) throw new Error('Missing App migration catch');
        const { code } = await transformWithOxc(match[1], 'migration-catch.ts');
        const notify = { error: vi.fn() };
        const humanLog = { updateEntry: vi.fn() };
        new Function('err', 'notify', 'humanLog', 'logId', code)(err, notify, humanLog, 'log');
        const message = typeof err === 'string' ? err : err.message;
        expect(notify.error).toHaveBeenCalledWith('AeroCrypt', message);
        expect(humanLog.updateEntry).toHaveBeenCalledWith('log', { status: 'error', message });
    });

    it('invalidates and clears a search before the real branch-switch handler starts reconnecting', async () => {
        const start = appRaw.indexOf('  const switchGitHubBranch =');
        const end = appRaw.indexOf('  const requestGitHubBatchCommitMessage', start);
        const { code } = await transformWithOxc(appRaw.slice(start, end), 'branch-switch.ts');
        const cancel = vi.fn(); const clear = vi.fn();
        const context = {
            useCallback: (fn: unknown) => fn, cancelRemoteSearch: cancel, setRemoteSearchResults: clear,
            normalizeProviderConnectionParams: () => { throw new Error('fixture stops before reconnect'); },
            activeSessionId: 'a', liveConnectionParams: {}, currentRemotePath: '/', notify: {},
            refreshGitHubContext: vi.fn(), runConnect: vi.fn(), t: (key: string) => key,
        };
        const handler = new Function(...Object.keys(context), `${code}\nreturn switchGitHubBranch;`)(...Object.values(context));
        await expect(handler('other')).rejects.toThrow('fixture stops before reconnect');
        expect(cancel).toHaveBeenCalledOnce();
        expect(clear).toHaveBeenCalledWith(null);
    });

    it('clears old matches when the query edit cancels result ownership', () => {
        const match = appRaw.match(/onChange=\{(e => \{ cancelRemoteSearch\(\);[^\n]+setRemoteSearchQuery\(e.target.value\); \})\}/);
        if (!match) throw new Error('Missing remote query handler');
        const cancel = vi.fn(), clear = vi.fn(), query = vi.fn();
        const handler = new Function('cancelRemoteSearch', 'setRemoteSearchResults', 'setRemoteSearchQuery', `return ${match[1]};`)(cancel, clear, query);
        handler({ target: { value: 'new text' } });
        expect(cancel).toHaveBeenCalledOnce();
        expect(clear).toHaveBeenCalledWith(null);
        expect(query).toHaveBeenCalledWith('new text');
    });

});
