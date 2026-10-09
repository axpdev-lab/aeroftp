// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * The SITE command report as `provider_site_command` returns it: the same
 * document `aeroftp-cli site --json` prints (snake_case on purpose, one
 * contract for both surfaces). `command` is `SITE <VERB>`, never the
 * arguments.
 */
export interface SiteCommandReport {
    outcome: 'replied' | 'not_sent' | 'unknown';
    command: string;
    verb_kind: 'read_only' | 'session_state' | 'other';
    code: number | null;
    lines: string[];
    encoding: 'utf8' | 'latin1' | null;
    elapsed_ms: number | null;
    session_reset: boolean;
    session_reconnected: boolean | null;
    reason: string | null;
}

/** Default and bounds of the reply wait, mirrored from `providers/ftp_site.rs`. */
export const SITE_REPLY_TIMEOUT_SECS = 60;

/**
 * Argument positions that carry a password, per verb (0 = the verb). glFTPd
 * `ADDUSER <user> <pass>`, `GADDUSER <group> <user> <pass>`, `CHPASS <user>
 * <pass>`, `PASSWD <pass>`; Serv-U `PSWD <old> <new>`.
 */
const SECRET_POSITIONS: Record<string, number[]> = {
    ADDUSER: [2],
    GADDUSER: [3],
    CHPASS: [2],
    PASSWD: [1],
    PSWD: [1, 2],
};

/** The words of a typed line, the optional leading `SITE` removed. */
function siteWords(line: string): string[] {
    const words = line.trim().split(/\s+/).filter(Boolean);
    return words.length > 0 && words[0].toUpperCase() === 'SITE' ? words.slice(1) : words;
}

/** Whether the typed line is a command known to carry a password. */
export function carriesSecret(line: string): boolean {
    const words = siteWords(line);
    return words.length > 0 && words[0].toUpperCase() in SECRET_POSITIONS;
}

/**
 * The line as the transcript echoes it: `SITE` prefixed, the password
 * positions of known verbs replaced by a mask. Display only: what is sent is
 * the line as typed.
 */
export function maskedEcho(line: string): string {
    const words = siteWords(line);
    const positions = words.length > 0 ? SECRET_POSITIONS[words[0].toUpperCase()] ?? [] : [];
    const shown = words.map((word, index) => (positions.includes(index) ? '••••••' : word));
    return ['SITE', ...shown].join(' ');
}

/** A reply line without its `NNN-` / `NNN ` prefix; free-text lines unchanged. */
export function withoutReplyCode(line: string): string {
    if (/^\d{3}$/.test(line)) return '';
    if (/^\d{3}[- ]/.test(line)) return line.slice(4);
    return line;
}

// ESC [ ... final byte, or ESC + one character.
// eslint-disable-next-line no-control-regex
const ANSI_SEQUENCE = /\u001b\[[0-?]*[ -/]*[@-~]|\u001b./g;

/** A reply line without ANSI colour sequences (glFTPd `SITE COLOR`). */
export function stripAnsi(line: string): string {
    return line.replace(ANSI_SEQUENCE, '');
}

/** The reply as display text, honouring the "hide reply codes" toggle. */
export function replyText(lines: string[], hideCodes: boolean): string {
    return lines.map(line => stripAnsi(hideCodes ? withoutReplyCode(line) : line)).join('\n');
}

/** Whether a command may have changed files, so the remote list should be refreshed. */
export function mayChangeFiles(report: SiteCommandReport): boolean {
    return report.outcome !== 'not_sent' && report.verb_kind !== 'read_only';
}
