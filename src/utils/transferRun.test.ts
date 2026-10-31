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

    it('ends the run when the work fails', async () => {
        await expect(withTransferRun(async () => { throw new Error('stopped'); })).rejects.toThrow('stopped');
        expect(currentTransferRun()).toBeNull();
    });
});
