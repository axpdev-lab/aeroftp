// SPDX-License-Identifier: GPL-3.0-or-later

import { describe, expect, it } from 'vitest';
import cases from '../../src-tauri/tests/fixtures/aerosync/cli-commands.json';
import { buildCliSyncCommand, CLI_EXCLUDES_MATCH_PLAN, quoteArg, type CliCommandInput } from './aeroSyncCliCommand';
import { AEROSYNC_DEFAULT_EXCLUDES } from './aeroSyncExcludes';

/**
 * POSIX word splitting for what `quoteArg` emits: bare words and single-quoted
 * runs, with `'\''` for a quote. Enough to prove the text a user pastes turns
 * back into the argv the Rust test hands to clap.
 */
function splitPosix(text: string): string[] {
    const words: string[] = [];
    let current = '';
    let inWord = false;
    for (let i = 0; i < text.length; i++) {
        const ch = text[i];
        if (ch === "'") {
            const end = text.indexOf("'", i + 1);
            if (end < 0) throw new Error(`unterminated quote in ${text}`);
            current += text.slice(i + 1, end);
            i = end;
            inWord = true;
        } else if (ch === '\\') {
            current += text[++i];
            inWord = true;
        } else if (ch === ' ') {
            if (inWord) words.push(current);
            current = '';
            inWord = false;
        } else {
            current += ch;
            inWord = true;
        }
    }
    if (inWord) words.push(current);
    return words;
}

/**
 * PowerShell's reading of one single-quoted argument. Its tokenizer takes
 * U+2018, U+2019, U+201A and U+201B as single quotes too, and two quote
 * characters in a row as one literal quote.
 */
const PS_QUOTES = new Set(["'", '\u2018', '\u2019', '\u201A', '\u201B']);
function readPowerShellSingleQuoted(text: string): { value: string; rest: string } {
    if (!PS_QUOTES.has(text[0])) throw new Error(`not quoted: ${text}`);
    let value = '';
    for (let i = 1; i < text.length; i++) {
        if (PS_QUOTES.has(text[i])) {
            if (PS_QUOTES.has(text[i + 1] ?? '')) {
                value += text[i];
                i++;
                continue;
            }
            return { value, rest: text.slice(i + 1) };
        }
        value += text[i];
    }
    throw new Error(`unterminated quote in ${text}`);
}

const mirrorUpload: CliCommandInput = {
    pairKind: 'local-remote',
    preset: 'mirror',
    direction: 'left-to-right',
    profileName: 'Srv',
    profileInitialPath: '/www',
    localPath: '/l',
    remotePath: '/www/r',
    excludes: [...AEROSYNC_DEFAULT_EXCLUDES],
    canary: false,
    transferBudgetBytes: 0,
    errorCorrectionPct: null,
    versionedBackup: false,
    shell: 'posix',
    excludesMatch: true,
};

describe('aeroftp-cli sync line for the Plan tab', () => {
    it('reads a non-empty fixture shared with the Rust clap test', () => {
        expect(cases.length).toBeGreaterThanOrEqual(2);
    });

    for (const c of cases) {
        it(`builds the fixture command: ${c.name}`, () => {
            for (const shell of ['posix', 'powershell'] as const) {
                const built = buildCliSyncCommand({ ...(c.input as Omit<CliCommandInput, 'shell'>), shell });
                expect(built.kind).toBe('command');
                if (built.kind !== 'command') return;
                expect(built.argv).toEqual(c.argv);
                expect(built.text).toBe(shell === 'posix' ? c.posix : c.powershell);
            }
            // What the user pastes into a POSIX shell is the argv clap parses.
            expect(splitPosix(c.posix)).toEqual(c.argv);
        });
    }

    it('offers no command while the CLI matches exclusions differently from the compare', () => {
        // The compare always excludes node_modules and friends per path
        // segment; the CLI's --exclude does not, so its Mirror would delete
        // destination files under node_modules/ that the Plan never listed.
        expect(CLI_EXCLUDES_MATCH_PLAN).toBe(false);
        const { excludesMatch: _unused, ...live } = mirrorUpload;
        expect(buildCliSyncCommand(live)).toEqual({ kind: 'none', reason: 'exclusions' });
    });

    it.each([
        [{ preset: 'backup' as const }, 'overwrites-newer'],
        [{ preset: 'update' as const }, 'overwrites-newer'],
        [{ preset: 'bisync' as const }, 'two-way'],
        [{ pairKind: 'local-local' as const }, 'local-pair'],
        [{ profileName: null }, 'unsaved-server'],
        [{ canary: true }, 'canary'],
        [{ versionedBackup: true }, 'versioned-backup'],
        [{ transferBudgetBytes: 1 }, 'transfer-budget'],
        // Not under the start folder: the CLI would join it under /www.
        [{ remotePath: '/elsewhere' }, 'remote-path'],
        // No start folder and a bare root: the CLI reads it as the server's
        // default folder, which may be a home directory.
        [{ profileInitialPath: '', remotePath: '/' }, 'remote-path'],
    ])('names the difference instead of a command: %o', (patch, reason) => {
        expect(buildCliSyncCommand({ ...mirrorUpload, ...patch })).toEqual({ kind: 'none', reason });
    });

    it.each(["it's", 'it\u2018s', 'it\u2019s', 'it\u201As', 'it\u201Bs', "a\u2019'\u2018b"])(
        'keeps a PowerShell argument whole when it contains a quote character: %s',
        (arg) => {
            // CWE-78: a typographic quote left single used to close the
            // argument early, and the rest of the path became shell input.
            const quoted = quoteArg(`/data/${arg}; Remove-Item x`, 'powershell');
            expect(readPowerShellSingleQuoted(quoted)).toEqual({
                value: `/data/${arg}; Remove-Item x`,
                rest: '',
            });
        },
    );

    it('adds --error-correction to an upload only', () => {
        const up = buildCliSyncCommand({ ...mirrorUpload, errorCorrectionPct: 25 });
        expect(up.kind === 'command' && up.argv[up.argv.length - 1]).toBe('--error-correction=25');
        const down = buildCliSyncCommand({ ...mirrorUpload, direction: 'right-to-left', errorCorrectionPct: 25 });
        expect(down.kind === 'command' && down.argv.some((arg) => arg.startsWith('--error-correction'))).toBe(false);
        expect(down.kind === 'command' && down.argv[down.argv.indexOf('--direction') + 1]).toBe('download');
    });
});
