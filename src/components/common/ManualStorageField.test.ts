import { describe, expect, it } from 'vitest';
import { manualStorageBytes, manualStorageInput } from './ManualStorageField';

describe('manual storage amount and unit', () => {
    it('accepts decimals and values above 1024', () => {
        expect(manualStorageBytes('0.5', 'GB')).toBe(512 * 1024 * 1024);
        expect(manualStorageBytes('2048', 'GB')).toBe(2 * 1024 ** 4);
    });

    it('rejects empty, non-positive and unsafe values', () => {
        expect(manualStorageBytes('', 'GB')).toBeUndefined();
        expect(manualStorageBytes('0', 'TB')).toBeUndefined();
        expect(manualStorageBytes('-1', 'TB')).toBeUndefined();
        expect(manualStorageBytes('999999999999', 'PB')).toBeUndefined();
    });

    it('round-trips stored byte values through a readable unit', () => {
        const input = manualStorageInput(1536 * 1024 ** 3);
        expect(input).toEqual({ amount: '1.5', unit: 'TB' });
        expect(manualStorageBytes(input.amount, input.unit)).toBe(1536 * 1024 ** 3);
    });
});
