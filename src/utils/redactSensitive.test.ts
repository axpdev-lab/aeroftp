import { describe, expect, it } from 'vitest';
import cases from '../shared/redaction-cases.json';
import matrix from '../shared/redaction-matrix.json';
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

// The matrix: every key form x separator x value form, and every Authorization
// scheme, in each way a log line can carry them. The Rust tables build the same
// inputs from the same file (src/shared/redaction-matrix.json): the secret
// never survives, the key does, and redacting twice changes nothing.
describe('redactSensitive matrix', () => {
    const { secret, keys, separators, valueForms, values, authorization, unchanged } = matrix;
    const escaped = (text: string): string => JSON.stringify(JSON.stringify(text)).slice(1, -1).slice(2, -2);
    const check = (input: string, leaked: string, kept: string): void => {
        const out = redactSensitive(input);
        expect(out, `${input} -> ${out}`).not.toContain(leaked);
        expect(out.toLowerCase(), `${input} -> ${out}`).toContain(kept.toLowerCase());
        expect(redactSensitive(out), `again: ${out}`).toBe(out);
    };

    // Each value is the secret alone and the secret after a backslash, which an
    // escaped value doubles once per level and must not end it early.
    it('redacts every secret field in every form', () => {
        for (const key of keys) {
            for (const template of values) {
                const value = template.replace('{v}', secret);
                for (const sep of separators) {
                    for (const form of valueForms) check(`${key}${sep}${form.replace('{v}', value)} next`, secret, key);
                }
                check(JSON.stringify({ [key]: value, next: 1 }), secret, key);
                check(`{"${key}": ${JSON.stringify(value)}, "next": 1}`, secret, key);
                check(JSON.stringify({ msg: JSON.stringify({ [key]: value, next: 1 }) }), secret, key);
                check(`Config { ${key}: \\"${escaped(value)}\\" }`, secret, key);
            }
        }
    });

    it('redacts every Authorization scheme in every form', () => {
        for (const { credential, secret: value } of authorization) {
            check(`Authorization: ${credential} next`, value, 'authorization');
            check(`authorization=${credential} next`, value, 'authorization');
            check(`proxy-authorization: ${credential}`, value, 'authorization');
            check(`authorization=${JSON.stringify(credential)} next`, value, 'authorization');
            check(`authorization=\\"${escaped(credential)}\\" next`, value, 'authorization');
            check(JSON.stringify({ authorization: credential, next: 1 }), value, 'authorization');
            check(JSON.stringify({ msg: JSON.stringify({ authorization: credential, next: 1 }) }), value, 'authorization');
        }
    });

    it('leaves what is not a secret field alone', () => {
        for (const line of unchanged) expect(redactSensitive(line)).toBe(line);
    });
});

describe('redactSensitive on a long line', () => {
    // Far above a linear scan even on a loaded machine, and far below the minutes
    // a quadratic one takes: a tighter bound failed under a parallel build.
    const LINEAR_BOUND_MS = 15_000;

    // A run of address characters with no "@" (a hex or base64url dump) once
    // cost a scan from every start position: quadratic, minutes for 200 KB.
    it('reads a long run of address characters in linear time', () => {
        const started = performance.now();
        for (const run of ['ab-cd_'.repeat(35000), 'a.'.repeat(100000), '0123456789abcdef'.repeat(12000)]) {
            redactSensitive(`dump ${run} end`);
        }
        expect(performance.now() - started).toBeLessThan(LINEAR_BOUND_MS);
    });

    // `-` is not a word character, so a name prefix that could repeat without
    // bound would restart after every hyphen: this is the line that would show it.
    it('reads a long kebab or snake run without a secret word in linear time', () => {
        const started = performance.now();
        for (const run of ['ab-'.repeat(100000), 'ab_'.repeat(100000), 'api-'.repeat(60000), 'token-'.repeat(50000), '1-'.repeat(150000)]) {
            redactSensitive(`line ${run} end`);
        }
        expect(performance.now() - started).toBeLessThan(LINEAR_BOUND_MS);
    });

    it('still redacts an address and keeps the words around it', () => {
        expect(redactSensitive('mail bob.smith+tag@example.org sent')).toBe('mail ***@*** sent');
    });
});
