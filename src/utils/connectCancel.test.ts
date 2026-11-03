// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { CONNECT_CANCELLED_MARKER, isConnectCancelledError } from './connectCancel';

describe('exact connect cancellation marker', () => {
    it('recognizes the exact IPC marker and the local cancellation Error', () => {
        expect(isConnectCancelledError(CONNECT_CANCELLED_MARKER)).toBe(true);
        expect(isConnectCancelledError(new Error(CONNECT_CANCELLED_MARKER))).toBe(true);
    });

    it.each([
        'server refused CONNECT_CANCELLED credentials',
        'CONNECT_CANCELLED: server-side failure',
        'CONNECT_CANCELLED_suffix',
        'prefix_CONNECT_CANCELLED',
        'CONNECT_CANCELLED\n',
        'CONNECT_CANCELLED ',
        '',
    ])('treats non-exact server error text as a failure: %s', text => {
        expect(isConnectCancelledError(text)).toBe(false);
        expect(isConnectCancelledError(new Error(text))).toBe(false);
    });

    it('does not treat absent or non-message values as cancellation', () => {
        for (const value of [undefined, null, 0, {}, { message: 'server CONNECT_CANCELLED' }]) {
            expect(isConnectCancelledError(value)).toBe(false);
        }
    });
});
