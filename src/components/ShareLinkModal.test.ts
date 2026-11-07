// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { act, createElement, StrictMode } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { expiresSoonerThanChosen, formatShareExpiry, ShareLinkModal } from './ShareLinkModal';

const invoke = vi.hoisted(() => vi.fn());
vi.mock('@tauri-apps/api/core', () => ({ invoke }));
vi.mock('../i18n', () => ({
    useTranslation: () => (key: string, params?: { count?: number }) => params?.count != null ? `${key}:${params.count}` : key,
}));
vi.mock('../hooks/useHumanizedLog', () => ({
    useHumanizedLog: () => ({ logRaw: () => 'log-id', updateEntry: () => {} }),
}));

const PLAIN_LINK_ONLY = {
    supports_expiration: false,
    supports_password: false,
    supports_permissions: false,
    available_permissions: [],
    supports_list_links: false,
    supports_revoke: false,
};

let root: Root;
let host: HTMLDivElement;
// App.tsx closes the modal by dropping `shareLinkDialog`, which unmounts it.
const modal = () => createElement(ShareLinkModal, {
    path: '/docs/report.pdf',
    fileName: 'report.pdf',
    providerName: 'Koofr',
    onClose: () => root.render(null),
});
const creates = () => invoke.mock.calls.filter(([command]) => command === 'provider_create_share_link');

beforeEach(() => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    invoke.mockReset();
    host = document.createElement('div'); document.body.append(host); root = createRoot(host);
});
afterEach(async () => { await act(async () => root.unmount()); host.remove(); });

const CAPPED_EXPIRY = {
    ...PLAIN_LINK_ONLY,
    supports_expiration: true,
    max_expiration_secs: 604800,
};

describe('Share Link modal on a provider with an expiry limit', () => {
    const s3Modal = () => createElement(ShareLinkModal, {
        path: '/bucket/report.pdf', fileName: 'report.pdf', providerName: 'Amazon S3', onClose: () => root.render(null),
    });
    const buttons = () => [...host.querySelectorAll('button')].map(b => b.textContent ?? '');

    // #1506 Requested 174 (Greptile on #1525): an S3 link without an expiry is
    // a one-hour link, so "never" is not offered and the longest choice starts selected.
    it('offers no "never", starts at the longest allowed choice and shows the real expiry', async () => {
        const expiresAt = new Date(Date.now() + 3600 * 1000).toISOString();
        invoke.mockImplementation(async (command: string) => command === 'provider_share_link_capabilities'
            ? CAPPED_EXPIRY
            : { url: 'https://bucket.example.invalid/report.pdf?X-Amz-Expires=3600', password: null, expires_at: expiresAt });
        await act(async () => root.render(s3Modal()));
        const advanced = [...host.querySelectorAll('button')].find(b => b.textContent === 'shareLinkModal.showAdvanced');
        await act(async () => advanced!.click());
        expect(buttons()).not.toContain('shareLinkModal.expirationNever');
        // The choices themselves, not only their number (CodeRabbit on #1525):
        // the seven-day cap keeps 1 hour, 24 hours and 7 days, never 30 days.
        expect(buttons().filter(b => b.startsWith('shareLinkModal.expiration'))).toEqual([
            'shareLinkModal.expirationHoursOne:1',
            'shareLinkModal.expirationHoursMany:24',
            'shareLinkModal.expirationDaysMany:7',
        ]);
        const create = [...host.querySelectorAll('button')].find(b => b.textContent === 'shareLinkModal.generateWithOptions');
        await act(async () => create!.click());
        expect(creates()[0][1]).toMatchObject({ expiresInSecs: 604800 });
        // The session that signed it ends in an hour: the line says so.
        const expiry = host.querySelector('[data-testid="share-link-expiry"]')?.textContent ?? '';
        expect(expiry).toContain('shareLinkModal.manageExpires');
        expect(expiry).toContain('shareLinkModal.expiresSoonerThanChosen');
    });

    // Greptile on #1525: Proton answers with a date only, which is not a
    // shorter link when it is the chosen day.
    it('compares a date-only expiry by day', () => {
        const now = Date.parse('2026-11-07T10:00:00Z');
        const week = 7 * 86400;
        expect(expiresSoonerThanChosen('2026-11-14', week, now)).toBe(false);
        expect(expiresSoonerThanChosen('2026-11-13', week, now)).toBe(true);
        expect(expiresSoonerThanChosen('2026-11-14T10:00:00Z', week, now)).toBe(false);
        expect(expiresSoonerThanChosen('2026-11-07T11:00:00Z', week, now)).toBe(true);
        expect(expiresSoonerThanChosen(null, week, now)).toBe(false);
        expect(expiresSoonerThanChosen('2026-11-14', null, now)).toBe(false);
    });
});

// Greptile and CodeRabbit on #1525: Proton's date-only expiry is a calendar
// day. Read as a timestamp it was UTC midnight, so New York showed the evening
// before, with a time Proton never gave.
describe('Share Link expiry line', () => {
    const tz = process.env.TZ;
    beforeEach(() => { process.env.TZ = 'America/New_York'; });
    afterEach(() => { process.env.TZ = tz; });

    it('shows a date-only expiry on its own day, with no time, west of UTC', () => {
        expect(formatShareExpiry('2026-11-14', 'en-US')).toBe('11/14/2026');
    });

    it('keeps the time of a full timestamp', () => {
        expect(formatShareExpiry('2026-11-14T15:30:00Z', 'en-US')).toBe(new Date('2026-11-14T15:30:00Z').toLocaleString('en-US'));
    });
});

describe('Share Link modal start-up', () => {
    it('creates no link when it is closed while the provider is still answering', async () => {
        let answer!: (caps: unknown) => void;
        invoke.mockImplementation((command: string) => command === 'provider_share_link_capabilities'
            ? new Promise(resolve => { answer = resolve; })
            : Promise.resolve({ url: 'https://example.invalid/s/abc', password: null, expires_at: null }));
        await act(async () => root.render(modal()));
        await act(async () => { window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' })); });
        expect(host.innerHTML).toBe('');
        await act(async () => answer(PLAIN_LINK_ONLY));
        expect(creates()).toHaveLength(0);
    });

    it('creates exactly one link under StrictMode for a provider with no options', async () => {
        invoke.mockImplementation(async (command: string) => command === 'provider_share_link_capabilities'
            ? PLAIN_LINK_ONLY
            : command === 'provider_create_share_link'
                ? { url: 'https://example.invalid/s/abc', password: null, expires_at: null }
                : undefined);
        await act(async () => root.render(createElement(StrictMode, null, modal())));
        expect(creates()).toHaveLength(1);
        expect(host.querySelector<HTMLInputElement>('input[readonly]')?.value).toBe('https://example.invalid/s/abc');
    });
});
