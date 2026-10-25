import { describe, expect, it } from 'vitest';
import cases from '../shared/redaction-cases.json';
import { redactSensitive } from './redactSensitive';

// The Rust table (`debug_tests.rs`, `redaction_patterns`) redacts the on-disk
// log tail with the same policy and reads this very file, so a rule that
// changes on one side only turns the other side's test red.
describe('redactSensitive', () => {
    for (const { name, input, expected } of cases) {
        it(`redacts: ${name}`, () => {
            expect(redactSensitive(input)).toBe(expected);
        });
    }

    it('leaves nothing for a second pass: a redacted line redacts to itself', () => {
        for (const { name, expected } of cases) {
            expect(redactSensitive(expected), name).toBe(expected);
        }
    });

    it('keeps an empty message as it is', () => {
        expect(redactSensitive('')).toBe('');
    });
});
