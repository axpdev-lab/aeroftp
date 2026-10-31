// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import librs from '../../src-tauri/src/lib.rs?raw';
import app from '../App.tsx?raw';
import unlock from './AeroCryptUnlock.tsx?raw';
import connection from './ConnectionScreen.tsx?raw';
import connect from './OAuthConnect.tsx?raw';
import settings from './SettingsPanel.tsx?raw';

describe('an OAuth app is saved as one unit', () => {
    it('never writes the id and the secret in separate calls', () => {
        for (const source of [connect, connection, settings]) {
            expect(source).toContain("invoke('store_oauth_app'");
            expect(source).not.toMatch(/store_credential[^;]*_client_(id|secret)/);
        }
    });

    it('goes through a registered command under the lock an import holds', () => {
        expect(librs).toMatch(/^\s+store_oauth_app,$/m);
        const command = librs.slice(librs.indexOf('async fn store_oauth_app('), librs.indexOf('fn write_oauth_app<'));
        expect(command).toContain('only_main_window(webview.label(), "store_oauth_app")');
        expect(command).toContain('ProfileListLock::acquire');
    });

    it('saves an overlay config with its salt, never apart', () => {
        for (const source of [app, unlock]) {
            expect(source).toContain("invoke('store_overlay_metadata'");
            expect(source).not.toMatch(/store_credential[^;]*aerocrypt_overlay_(config|salt)/);
        }
        expect(librs).toMatch(/^\s+store_overlay_metadata,$/m);
    });

    it('does not swallow a secret the connection form could not save', () => {
        expect(connection).toContain('.catch(reportSecretNotSaved)');
        expect(connection).not.toMatch(/github_store_pat[^;]*catch\(\(\) => \{\}\)/);
    });

    it('reports a failed save in Settings', () => {
        const save = settings.slice(settings.indexOf("invoke('store_oauth_app'"));
        expect(save.slice(0, 500)).toContain("title: t('toast.saveFailed')");
    });
});
