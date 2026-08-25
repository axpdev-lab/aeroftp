import { describe, expect, it } from 'vitest';
import { manualStorageBytes, manualStorageInput } from './ManualStorageField';
import componentSource from './ManualStorageField.tsx?raw';
import { withoutComments } from '../../utils/jsxTag';

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

describe('resync guard', () => {
    it('never parses to 0, so an emptiness test on the parsed value is safe', () => {
        // Half of an audit finding rested on `current` being able to hold 0.
        // It cannot: everything <= 0 comes back undefined. Pinned so the claim
        // is answered by the suite instead of being re-argued.
        for (const amount of ['0', '0.0', '-0', '', '   ', '-5']) {
            expect(manualStorageBytes(amount, 'GB'), `"${amount}" parsed to 0`).toBeUndefined();
        }
    });

    it('does not conflate an incoming 0 with an unset quota', () => {
        // The other half did hold: `!valueBytes` is true for 0 as well as for
        // undefined, so a quota arriving as 0 skipped the resync and left the
        // inputs showing the previous value. No DOM here (environment: node),
        // so the guard itself is pinned on the source. Comments are stripped
        // first: the component quotes the old guard while explaining why it
        // went, and a raw substring search would match that explanation.
        const code = withoutComments(componentSource);
        expect(code).not.toContain('!current && !valueBytes');
        expect(code).toContain('if (current === valueBytes) return;');
    });
});
