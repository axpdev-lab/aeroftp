import { describe, expect, it } from 'vitest';
import cases from '../shared/redaction-cases.json';
import matrix from '../shared/redaction-matrix.json';
import { redactCredentials, redactSensitive, redactUrlUserinfo } from './redactSensitive';

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

describe('redactCredentials', () => {
    const M = '***REDACTED***';
    it.each([
        ['Bearer in Authorization', 'GET failed: Authorization: Bearer abcdefghijklmnop (401)', 'GET failed: authorization: ***REDACTED*** (401)'],
        ['Basic in Authorization', 'sent Authorization: Basic dXNlcjpwYXNz then 403', 'sent authorization: ***REDACTED*** then 403'],
        ['bare Bearer token', 'refused Bearer abcdefghijklmnop for /a.txt', `refused Bearer ${M} for /a.txt`],
        ['access_token', 'GET /a?access_token=tok123 failed', `GET /a?access_token=${M} failed`],
        ['refresh_token', 'refresh_token=rt123 expired', `refresh_token=${M} expired`],
        ['id_token', 'id_token: idt123 rejected', `id_token: ${M} rejected`],
        ['token', 'token=t123 invalid', `token=${M} invalid`],
        ['api_key', 'api_key=k123 invalid', `api_key=${M} invalid`],
        ['apikey', 'apikey=k123 invalid', `apikey=${M} invalid`],
        ['key query parameter', 'GET https://h/o?key=k123&alt=json 403', `GET https://h/o?key=${M}&alt=json 403`],
        ['secret', 'secret=s123 wrong', `secret=${M} wrong`],
        ['password', 'password=p123 wrong', `password=${M} wrong`],
        ['signature', 'signature=abc123 mismatch', `signature=${M} mismatch`],
        ['sig query parameter', 'GET https://h/b?sv=1&sig=abc123&se=2 403', `GET https://h/b?sv=1&sig=${M}&se=2 403`],
        ['X-Amz-Signature', 'GET https://s3/b?X-Amz-Date=1&X-Amz-Signature=abc123 403', `GET https://s3/b?X-Amz-Date=1&X-Amz-Signature=${M} 403`],
        ['X-Amz-Credential', 'GET https://s3/b?X-Amz-Credential=AKIA%2F1&X-Amz-Date=1 403', `GET https://s3/b?X-Amz-Credential=${M}&X-Amz-Date=1 403`],
        ['X-Amz-Security-Token', 'X-Amz-Security-Token=st123 expired', `X-Amz-Security-Token=${M} expired`],
        ['URL userinfo with a password', 'connect ftp://bob:hunter2@ftp.example.com:21/srv failed', `connect ftp://${M}@ftp.example.com:21/srv failed`],
        ['URL userinfo of another scheme', 'open s3://AKIA1:wJal@bucket/key.txt failed', `open s3://${M}@bucket/key.txt failed`],
    ])('masks %s and keeps the rest', (_name, input, expected) => {
        expect(redactCredentials(input)).toBe(expected);
    });

    it('leaves a message without credentials as it is', () => {
        const plain = 'Delete failed: docs/a.txt - 550 Permission denied on /home/u/work/a.txt (bob@example.org at 192.168.1.3, sha 0123456789abcdef0123456789abcdef, key docs/a.txt)';
        expect(redactCredentials(plain)).toBe(plain);
    });
});

describe('redactCredentials: URL userinfo', () => {
    it.each([
        ['ftp://bob:first@second@host/file', 'ftp://***REDACTED***@host/file'],
        ['sftp://bob:p@ss@w0rd@host:22/a', 'sftp://***REDACTED***@host:22/a'],
        ['https://user:pa@ss@example.com?x=1', 'https://***REDACTED***@example.com?x=1'],
    ])('masks a password that holds @: %s', (raw, masked) => {
        expect(redactCredentials(`Upload failed: ${raw}`)).toBe(`Upload failed: ${masked}`);
        expect(redactCredentials(raw)).not.toMatch(/first|second|ss@w0rd|pa@ss/);
    });

    it.each([
        ['an @ in the path', 'ftp://host/docs/me@home.txt'],
        ['an @ in the query', 'https://host/a?to=me@example.com'],
        ['an @ in the fragment', 'https://host/a#me@example.com'],
    ])('leaves a URL without userinfo alone, with %s', (_label, raw) => {
        expect(redactCredentials(raw)).toBe(raw);
    });

    it('stops at the authority when the path also holds an @', () => {
        expect(redactCredentials('ftp://bob:x@y@host/docs/me@home.txt'))
            .toBe('ftp://***REDACTED***@host/docs/me@home.txt');
    });
});

describe('redactCredentials: one URL rule, any scheme, never past a field (T11)', () => {
    it('keeps the field after an authority without userinfo', () => {
        expect(redactCredentials('{"a":"https://host:443","b":"x@y"}')).toBe('{"a":"https://host:443","b":"x@y"}');
    });
    // D1: raw escaped JSON is not JSON as a whole: plain text, where no quote
    // ends an authority, so a path-less URL over-masks into the next field
    // by policy; nothing leaks.
    it('over-masks, never leaks, in JSON escaped without its outer string', () => {
        const escaped = '{\\"a\\":\\"https://host:443\\",\\"b\\":\\"x@y\\"}';
        expect(redactCredentials(escaped)).toBe('{\\"a\\":\\"https://***REDACTED***@y\\"}');
    });
    it('still masks the userinfo of other schemes', () => {
        expect(redactCredentials('get s3://AKIAEXAMPLE:secretkey@bucket/key failed'))
            .toBe('get s3://***REDACTED***@bucket/key failed');
    });
    // P9 (U10): an escaped quote and a comma inside a password, in plain
    // JSON, is the password's, not the end of the field.
    it('masks a password that holds an escaped quote and a comma', () => {
        const raw = String.raw`{"url":"https://bob:pa\",ss@host/path"}`;
        expect(redactCredentials(raw)).toBe(String.raw`{"url":"https://***REDACTED***@host/path"}`);
        const s3 = String.raw`{"src":"s3://AKIAEXAMPLE:se\",cret@bucket/key"}`;
        expect(redactCredentials(s3)).toBe(String.raw`{"src":"s3://***REDACTED***@bucket/key"}`);
    });
    // Q3: text before the URL in a plain JSON string, and `\":` in the
    // password: the string's level is the opening quote's, so it is masked.
    it('masks a password after text in a plain JSON string', () => {
        const raw = String.raw`{"message":"upload to ftp://bob:pa\":ss@host/file"}`;
        expect(redactCredentials(raw)).toBe(String.raw`{"message":"upload to ftp://***REDACTED***@host/file"}`);
        const s3 = String.raw`{"message":"copy s3://AKIAEXAMPLE:se\":cret@bucket/key"}`;
        expect(redactCredentials(s3)).toBe(String.raw`{"message":"copy s3://***REDACTED***@bucket/key"}`);
    });
});

describe('redactUrlUserinfo (H9: linear by construction)', () => {
    it('stays linear on long adversarial input', () => {
        const inputs = [
            'ftp://bob:' + '\\'.repeat(200_000) + 'x',
            'ftp://bob\\:' + '\\'.repeat(200_000),
            'ftp://' + ':'.repeat(200_000),
            'x'.repeat(100_000) + '://'.repeat(100_000),
            'ftp://ftp://'.repeat(50_000),
            // D1: long whitespace and many strings, valid JSON and not.
            '{"a":"' + ' '.repeat(200_000) + 'ftp://u:p@h"}',
            JSON.stringify(Array.from({ length: 20_000 }, (_, n) => `ftp://u${n}:p@h${n}/x`)),
            JSON.stringify(Array.from({ length: 20_000 }, () => 's')) + ' tail',
            '"'.repeat(200_000) + 'ftp://u:p@h',
            // D1: a large valid JSON document, URLs throughout.
            JSON.stringify({ rows: Array.from({ length: 20_000 }, (_, n) => ({ id: n, url: `https://user${n}:secret@host${n}.example/p`, note: 'x@y' })) }),
        ];
        for (const input of inputs) {
            const started = performance.now();
            redactUrlUserinfo(input);
            expect(performance.now() - started).toBeLessThan(1000);
        }
        // The input of the finding, through the whole pipeline.
        const finding = 'ftp://bob:' + '\\'.repeat(20_000) + 'x';
        const started = performance.now();
        expect(redactSensitive(finding)).toBe(finding);
        expect(performance.now() - started).toBeLessThan(1000);
    });
});

