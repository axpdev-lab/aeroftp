// SPDX-License-Identifier: GPL-3.0-or-later

/**
 * The `aeroftp-cli sync` command that does what the Plan tab is about to do,
 * or the reason there is none.
 *
 * A command is offered only when the CLI would do the same thing or less:
 * running it must never delete or overwrite anything the Plan would have left
 * alone. Each refusal names the difference, from the CLI source:
 *
 * - One-way, same size: the CLI transfers when the destination is older than
 *   the source by more than 2 s (`destination_is_current`); the Plan treats
 *   copies within 30 s as the same. Under Mirror that only rewrites the
 *   destination with the source, which Mirror does anyway past 30 s.
 * - One-way, different size: the CLI always overwrites the destination, also
 *   when it is newer. Mirror does too; Backup and Update never overwrite a
 *   newer destination, so they have no equivalent.
 * - `--direction both` treats two copies of the same size whose times differ
 *   at all as a conflict and resolves it (the older one is overwritten under
 *   `newer`); Two-way calls them the same within 30 s and leaves both.
 * - `--delete` removes files only; Mirror also removes emptied folders. The
 *   CLI keeps more, which is allowed.
 * - Local to local, the CLI runs its own mirror and ignores direction,
 *   `--delete` and `--conflict-mode` (`cmd_sync_local_to_local`).
 * - The compare always excludes AEROSYNC_DEFAULT_EXCLUDES per path segment;
 *   the CLI's `--exclude` matches a file's path or name as a glob, so
 *   `node_modules` there does not cover `node_modules/x`. Until both use one
 *   matcher (`CLI_EXCLUDES_MATCH_PLAN`), a Mirror without them would delete
 *   destination files the Plan never saw.
 */
import type { AeroSyncPairKind } from '../components/AeroSync/types';
import type { PresetDirection, SyncPreset } from './syncPresets';

/**
 * Whether `aeroftp-cli sync --exclude P` excludes exactly what the compare
 * excludes for P. False while the CLI matches globs on a file's path or name
 * and the compare matches path segments; the shared matcher makes it true.
 */
export const CLI_EXCLUDES_MATCH_PLAN = false;

export type CliNoEquivalentReason =
    | 'local-pair'
    | 'unsaved-server'
    | 'overwrites-newer'
    | 'two-way'
    | 'canary'
    | 'versioned-backup'
    | 'transfer-budget'
    | 'remote-path'
    | 'exclusions';

export type CliShell = 'posix' | 'powershell';

export interface CliCommandInput {
    pairKind: AeroSyncPairKind | null;
    preset: SyncPreset;
    direction: PresetDirection;
    /** Saved server name, when the connection came from a saved server. */
    profileName?: string | null;
    /** Start folder of that saved server. */
    profileInitialPath?: string | null;
    localPath: string;
    remotePath: string;
    /** Every pattern the compare applied: defaults first, then the user's. */
    excludes: readonly string[];
    canary: boolean;
    transferBudgetBytes: number;
    /** Percentage, or null when error correction is off. */
    errorCorrectionPct: number | null;
    versionedBackup: boolean;
    shell: CliShell;
    /** Tests only: pretend the CLI already shares the compare's matcher. */
    excludesMatch?: boolean;
}

export type CliCommandResult =
    | { kind: 'command'; argv: string[]; text: string }
    | { kind: 'none'; reason: CliNoEquivalentReason };

/**
 * How the CLI resolves a remote path under a profile
 * (`resolve_cli_remote_path_unchecked_with_note`): a path that does not start
 * with the start folder is joined under it, and with no start folder a bare
 * `/` means the server's default folder. Either way the CLI would reach a
 * different folder from the one the Plan compared.
 */
function cliReachesSameRemoteFolder(remotePath: string, initialPath: string | null | undefined): boolean {
    const base = (initialPath ?? '').trim();
    if (base === '' || base === '/') return remotePath.replace(/^\/+/, '') !== '';
    return remotePath.startsWith(base.replace(/\/+$/, ''));
}

export function buildCliSyncCommand(input: CliCommandInput): CliCommandResult {
    const none = (reason: CliNoEquivalentReason): CliCommandResult => ({ kind: 'none', reason });

    if (input.pairKind !== 'local-remote' && input.pairKind !== 'remote-local') return none('local-pair');
    if (!input.profileName) return none('unsaved-server');
    if (input.preset === 'backup' || input.preset === 'update') return none('overwrites-newer');
    if (input.preset === 'bisync') return none('two-way');
    if (input.canary) return none('canary');
    if (input.versionedBackup) return none('versioned-backup');
    if (input.transferBudgetBytes > 0) return none('transfer-budget');
    if (!cliReachesSameRemoteFolder(input.remotePath, input.profileInitialPath)) return none('remote-path');
    if (input.excludes.length > 0 && !(input.excludesMatch ?? CLI_EXCLUDES_MATCH_PLAN)) return none('exclusions');

    // Mirror. Left to right means towards the right panel; which side is
    // local decides whether that is an upload or a download.
    const localIsLeft = input.pairKind === 'local-remote';
    const towardsRemote = (input.direction === 'left-to-right') === localIsLeft;
    const argv = [
        'aeroftp-cli', '--profile', input.profileName,
        'sync', input.localPath, input.remotePath,
        '--direction', towardsRemote ? 'upload' : 'download',
        '--delete',
    ];
    for (const pattern of input.excludes) argv.push('--exclude', pattern);
    // The CLI writes sidecars after uploads only; on a download the Plan's
    // error correction verifies and repairs, which the CLI does not do. That
    // difference writes nothing to the destination the Plan would not.
    // One word with `=`: the flag takes an optional value and requires the
    // equals sign, so `--error-correction 15` reads as the default level and
    // passes 15 on as a positional path.
    if (towardsRemote && input.errorCorrectionPct != null) {
        argv.push(`--error-correction=${input.errorCorrectionPct}`);
    }
    return { kind: 'command', argv, text: argv.map((arg) => quoteArg(arg, input.shell)).join(' ') };
}

const SAFE_BARE = /^[A-Za-z0-9_\/.:=+-]+$/;

/**
 * Every character PowerShell's tokenizer reads as a single quote: the ASCII
 * apostrophe and U+2018, U+2019, U+201A, U+201B. Inside a single-quoted
 * string each one ends the string unless it is doubled.
 */
const POWERSHELL_SINGLE_QUOTES = /['\u2018\u2019\u201A\u201B]/g;

/**
 * Quote one argument for the shell the user will paste into. Single quotes
 * are literal in both shells; only the quote itself needs escaping, and the
 * two shells escape it differently. PowerShell also closes the string on the
 * typographic quotes, so each of those is doubled as well (CWE-78: a path
 * with a curly apostrophe would otherwise end the argument early and hand
 * the rest to the shell).
 */
export function quoteArg(arg: string, shell: CliShell): string {
    if (SAFE_BARE.test(arg)) return arg;
    return shell === 'powershell'
        ? `'${arg.replace(POWERSHELL_SINGLE_QUOTES, '$&$&')}'`
        : `'${arg.replace(/'/g, `'\\''`)}'`;
}
