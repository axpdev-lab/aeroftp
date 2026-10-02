// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it, vi } from 'vitest';
import modalSource from '../components/ShareLinkModal.tsx?raw';
import {
    loadShareLinkCapabilities,
    NO_SHARE_LINK_CAPABILITIES,
    shareLinkCapabilitiesFromBackend,
} from './shareLinkCapabilities';

/**
 * The Share Link modal offers what the connected provider says it supports,
 * as `provider_share_link_capabilities` reports it. It used to read a table of
 * its own, keyed by protocol, that had drifted from the providers: Proton,
 * B2 and Immich honour expiry in `create_share_link` (Proton and Immich a
 * password too) and were missing from it, so the modal created a plain link
 * without asking. The backend flags are pinned against the methods each
 * provider defines by `share_link_capability_guard.rs`.
 */
describe('Share Link capabilities come from the provider', () => {
    it('maps the backend flags, Proton as the provider the old table left out', () => {
        const caps = shareLinkCapabilitiesFromBackend({
            supports_expiration: true,
            supports_password: true,
            supports_permissions: true,
            available_permissions: ['view', 'edit'],
            supports_list_links: false,
            supports_revoke: false,
        });
        expect(caps).toEqual({
            expiration: true,
            password: true,
            permissions: true,
            availablePermissions: ['view', 'edit'],
            hasAdvancedOptions: true,
            supportsList: false,
            supportsRevoke: false,
        });
    });

    it('counts expiry alone as an advanced option (B2, S3, Azure)', () => {
        const caps = shareLinkCapabilitiesFromBackend({
            supports_expiration: true,
            supports_password: false,
            supports_permissions: false,
            available_permissions: [],
            supports_list_links: false,
            supports_revoke: false,
        });
        expect(caps.hasAdvancedOptions).toBe(true);
    });

    it('asks the connected provider through provider_share_link_capabilities', async () => {
        const invoke = vi.fn(async () => ({
            supports_expiration: false,
            supports_password: false,
            supports_permissions: false,
            available_permissions: [],
            supports_list_links: true,
            supports_revoke: true,
        }));
        const caps = await loadShareLinkCapabilities(invoke);
        expect(invoke).toHaveBeenCalledWith('provider_share_link_capabilities');
        expect(caps.supportsList).toBe(true);
        expect(caps.hasAdvancedOptions).toBe(false);
    });

    it('offers nothing when the query fails, so the plain create reports the error', async () => {
        const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
        const caps = await loadShareLinkCapabilities(async () => {
            throw new Error('Not connected to any provider');
        });
        expect(caps).toEqual(NO_SHARE_LINK_CAPABILITIES);
        warn.mockRestore();
    });

    it('leaves the modal no per-provider table to drift from the backend', () => {
        expect(modalSource).toContain('loadShareLinkCapabilities(invoke)');
        // Any `case '<protocol>':` in the modal would be a second source of truth.
        expect(modalSource).not.toMatch(/case '(googledrive|dropbox|onedrive|box|pcloud|filen|proton|b2)'/);
        expect(modalSource).not.toMatch(/providerType/);
    });

    it('revokes a listed link by the shared item path plus that link id', () => {
        // Every provider's remove_share_link takes the item path; the id was
        // sent in its place, so Box resolved a file id as a file name and
        // pCloud, Koofr and Zoho could not tell two links on one item apart.
        expect(modalSource).toContain("invoke('provider_remove_share_link', { path, linkId })");
        expect(modalSource).not.toMatch(/provider_remove_share_link', \{ path: /);
    });
});
