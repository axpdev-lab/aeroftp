// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createDeferredRefresh } from './deferredRefresh';

describe('createDeferredRefresh', () => {
    beforeEach(() => {
        vi.useFakeTimers();
        vi.setSystemTime(1_000_000);
    });
    afterEach(() => vi.useRealTimers());

    it('stands down when another refresh started after it was requested', () => {
        let lastStarted = 0;
        const run = vi.fn();
        const refresh = createDeferredRefresh(run, () => lastStarted);
        refresh.schedule();
        // The upload loop finishes and lists the folder itself.
        vi.advanceTimersByTime(50);
        lastStarted = Date.now();
        vi.advanceTimersByTime(1000);
        expect(run).not.toHaveBeenCalled();
    });

    it('runs when nothing else refreshed, once for several requests', () => {
        const run = vi.fn();
        const refresh = createDeferredRefresh(run, () => 0);
        refresh.schedule();
        vi.advanceTimersByTime(100);
        refresh.schedule();
        vi.advanceTimersByTime(1000);
        expect(run).toHaveBeenCalledTimes(1);
    });

    it('does not stand down for a refresh that started before the request', () => {
        const run = vi.fn();
        const startedEarlier = Date.now() - 10;
        const refresh = createDeferredRefresh(run, () => startedEarlier);
        refresh.schedule();
        vi.advanceTimersByTime(1000);
        expect(run).toHaveBeenCalledTimes(1);
    });
});
