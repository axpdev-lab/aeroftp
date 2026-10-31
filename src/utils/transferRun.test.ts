// SPDX-License-Identifier: GPL-3.0-or-later

import { describe, expect, it } from 'vitest';
import { currentTransferRun, withTransferRun } from './transferRun';

describe('withTransferRun', () => {
    it('names the run while it lasts and a different one the next time', async () => {
        expect(currentTransferRun()).toBeNull();
        const first = await withTransferRun(currentTransferRun);
        expect(first).toBeTruthy();
        expect(currentTransferRun()).toBeNull();
        expect(await withTransferRun(currentTransferRun)).not.toBe(first);
    });

    it('takes an inner run into the one under way', async () => {
        await withTransferRun(async () => {
            const outer = currentTransferRun();
            await withTransferRun(async () => expect(currentTransferRun()).toBe(outer));
            expect(currentTransferRun()).toBe(outer);
        });
    });

    it('keeps the run until the last of two overlapping pieces of work ends', async () => {
        let endFirst = () => {};
        let endSecond = () => {};
        const first = withTransferRun(() => new Promise<void>((resolve) => { endFirst = resolve; }));
        const run = currentTransferRun();
        const second = withTransferRun(() => new Promise<void>((resolve) => { endSecond = resolve; }));
        expect(currentTransferRun()).toBe(run);
        endFirst();
        await first;
        expect(currentTransferRun()).toBe(run);
        endSecond();
        await second;
        expect(currentTransferRun()).toBeNull();
    });

    it('ends the run when the work fails', async () => {
        await expect(withTransferRun(async () => { throw new Error('stopped'); })).rejects.toThrow('stopped');
        expect(currentTransferRun()).toBeNull();
    });
});
