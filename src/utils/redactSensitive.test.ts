import { describe, expect, it } from 'vitest';
import cases from '../shared/redaction-cases.json';
import matrix from '../shared/redaction-matrix.json';
import { CREDENTIAL_RULES, REDACTION_PATTERNS, applyRules, redactCredentials, redactSensitive, redactUrlUserinfo, uncoveredIdentity, type Span } from './redactSensitive';

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

    // T3, the class: every rule reads the original text, so no rule can
    // consume another's delimiter and the order of the rules means nothing.
    // Any order of either list gives the same output on every shared case;
    // and every secret field and Authorization form of the matrix, put
    // inside a URL's userinfo, leaves nothing of it (a form holding `/`, `?`
    // or `#` ends the authority itself and is not one).
    it('gives the same output whatever the order of the rules', () => {
        // A fixed shuffle, so a failure can be replayed.
        let seed = 7;
        const shuffled = <T>(list: ReadonlyArray<T>): T[] => {
            const out = [...list];
            for (let i = out.length - 1; i > 0; i -= 1) {
                seed = (seed * 1103515245 + 12345) % 2147483648;
                const j = seed % (i + 1);
                [out[i], out[j]] = [out[j], out[i]];
            }
            return out;
        };
        for (let round = 0; round < 8; round += 1) {
            const sensitive = shuffled(REDACTION_PATTERNS);
            const credentials = shuffled(CREDENTIAL_RULES);
            for (const { name, input } of cases) {
                expect(applyRules(input, sensitive), name).toBe(redactSensitive(input));
                expect(applyRules(input, credentials), name).toBe(redactCredentials(input));
            }
        }
    });

    it('masks the whole userinfo of a URL that holds any secret field or Authorization form', () => {
        const userinfos: string[] = [];
        for (const key of keys) {
            for (const template of values) {
                const value = template.replace('{v}', secret);
                for (const sep of separators) {
                    for (const form of valueForms) userinfos.push(`${key}${sep}${form.replace('{v}', value)}`);
                }
            }
        }
        for (const { credential } of authorization) userinfos.push(`authorization: ${credential}`);
        let checked = 0;
        for (const userinfo of userinfos.filter((u) => !/[/?#]/.test(u))) {
            const input = `https://tok:${userinfo}@host/x`;
            for (const out of [redactSensitive(input), redactCredentials(input)]) {
                // The whole userinfo, from the token to the field's value, is
                // gone (the field's value may take the host with it: over-
                // masking), and a second pass changes nothing.
                expect(out.startsWith('https://***REDACTED***'), `${input} -> ${out}`).toBe(true);
                expect(out, input).not.toContain(secret);
                expect(out, input).not.toContain('tok:');
                expect(redactSensitive(out), out).toBe(out);
            }
            checked += 1;
        }
        expect(checked).toBeGreaterThan(100);
    });
});

describe('redactSensitive on a long line', () => {
    // Far above a linear scan even on a loaded machine, and far below the minutes
    // a quadratic one takes: a tighter bound failed under a parallel build.
    const LINEAR_BOUND_MS = 15_000;

    // T3: the identity spans a credential covers are dropped in one walk.
    // 20k URLs whose `secret@host` is also an e-mail, and 20k e-mails beside
    // them; and the walk's comparisons, counted: linear in the spans.
    it('drops the identity spans a credential covers in linear time', () => {
        const n = 20_000;
        const urls = Array.from({ length: n }, (_, k) => `ftp://user${k}:secret@host${k}.example/x`);
        const mails = Array.from({ length: n }, (_, k) => `mail${k}@x${k}.example`);
        const input = [...urls, ...mails].join(' ');
        const started = performance.now();
        const out = redactSensitive(input);
        expect(performance.now() - started).toBeLessThan(LINEAR_BOUND_MS);
        expect(out).not.toMatch(/secret|user\d+:/);
        expect(out.match(/ftp:\/\/\*\*\*REDACTED\*\*\*@host\d+\.example\/x/g)).toHaveLength(n);
        expect(out.match(/(^| )\*\*\*@\*\*\*/g)).toHaveLength(n);

        const span = (start: number, end: number): Span => ({ start, end, replacement: 'x' });
        const credentials = Array.from({ length: n }, (_, k) => span(10 * k, 10 * k + 5));
        const identity = Array.from({ length: 2 * n }, (_, k) => span(5 * k, 5 * k + 3));
        const steps = { count: 0 };
        expect(uncoveredIdentity(identity, credentials, steps)).toHaveLength(n);
        expect(steps.count).toBeLessThanOrEqual(2 * (credentials.length + identity.length));
    });

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

describe('S1: both entry points, every scheme and every escape form', () => {
    it('masks each S1 shared case the same way through redactCredentials', () => {
        for (const { name, input, expected } of cases.filter(({ name }) => name.startsWith('S1 '))) {
            expect(redactCredentials(input), name).toBe(expected);
        }
    });

    // X2 (CodeRabbit 4574777429): escaped URL delimiters in text that is not
    // JSON as a whole (a prefix, a suffix, a trailing comma, single quotes).
    it('masks each X2 shared case the same way through redactCredentials', () => {
        const x2 = cases.filter(({ name }) => name.startsWith('X2 '));
        expect(x2.length).toBeGreaterThanOrEqual(7);
        for (const { name, input, expected } of x2) {
            expect(redactCredentials(input), name).toBe(expected);
            expect(redactCredentials(input), name).not.toContain('hunter2');
        }
    });

    it('masks every URL of a 20k escaped-token document, each at its own token', () => {
        const input = '[' + Array.from({ length: 20_000 }, (_, n) => `"https:\\/\\/u${n}:p\\u0040h${n}\\/x"`).join(',') + ']';
        const masked = redactUrlUserinfo(input);
        expect(masked.match(/\*\*\*REDACTED\*\*\*/g)).toHaveLength(20_000);
        expect(masked).not.toMatch(/u\d+:p/);
        expect(redactUrlUserinfo(masked)).toBe(masked);
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
            // S1: 20k escaped tokens, each decoded once; a document of 20k
            // URLs serialized inside one string; twelve levels of nesting.
            '[' + Array.from({ length: 20_000 }, (_, n) => `"https:\\/\\/u${n}:p\\u0040h${n}\\/x"`).join(',') + ']',
            JSON.stringify({ log: JSON.stringify(Array.from({ length: 20_000 }, (_, n) => `ftp://u${n}:p@h${n}/x`)) }),
            Array.from({ length: 12 }).reduce<string>((inner) => JSON.stringify({ v: inner }), 'ftp://u:p@h/' + 'x'.repeat(1000)),
            // X2: long lines that are not JSON, full of backslashes, decoded
            // as a string body at every level: a run halves at each one.
            'log: ' + '\\'.repeat(400_000) + ' https:\\/\\/bob:hunter2@h/p',
            'log: ' + 'x\\y'.repeat(100_000) + ' https:\\/\\/bob:hunter2@h/p',
            'log: ' + '\\/'.repeat(200_000),
            'log: ' + '\\u00'.repeat(100_000) + '\\u0040',
            'log: {"a":"' + 'x'.repeat(200_000) + '\\\\\\\\\\\\\\\\' + '"',
        ];
        for (const input of inputs) {
            const started = performance.now();
            redactUrlUserinfo(input);
            expect(performance.now() - started).toBeLessThan(1000);
        }
        // X2: masked at its level, whatever the backslashes before it.
        for (const run of ['\\'.repeat(400_000), 'x\\y'.repeat(100_000)]) {
            const line = `log: ${run} https:\\/\\/bob:hunter2@h/p`;
            const started = performance.now();
            expect(redactCredentials(line)).not.toContain('hunter2');
            expect(performance.now() - started).toBeLessThan(2000);
        }
        // The input of the finding, through the whole pipeline.
        const finding = 'ftp://bob:' + '\\'.repeat(20_000) + 'x';
        const started = performance.now();
        expect(redactSensitive(finding)).toBe(finding);
        expect(performance.now() - started).toBeLessThan(1000);
    });
});

