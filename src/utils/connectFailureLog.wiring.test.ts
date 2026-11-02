// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import APP from '../App.tsx?raw';

/**
 * A connect that fails must close the rows it opened in the Activity Log, and
 * say why. `logConnectionSteps` opens a live "Connecting to ip:port" row that
 * only a success closed, so a refused connect left it live, "1 ACTIVE", until
 * the app was restarted; and the connect row read "Check credentials" for
 * every failure, a server without SFTP included.
 *
 * This test fails on a `logConnectionSteps` row whose connect does not run
 * under `connectUnderRow`, and on a connect failure logged with the generic
 * `logError('CONNECT')` instead of `reportConnectFailure`.
 */

/** The names the connect paths give to the row `logConnectionSteps` opens. */
function connectingRows(source: string): string[] {
    return [...source.matchAll(/connectingLogId(?:\s*:\s*(\w+))?\s*\}\s*=\s*await[^;]*?logConnectionSteps\(/g)]
        .map(match => match[1] ?? 'connectingLogId');
}

/** The rows of `connectingRows` whose connect does not run under `connectUnderRow`. */
function unclosedRows(source: string): string[] {
    return connectingRows(source).filter(name => !source.includes(`connectUnderRow(${name}, `));
}

describe('connect failure log', () => {
    it('finds every connect path that opens a "Connecting to" row', () => {
        expect(connectingRows(APP).length).toBeGreaterThanOrEqual(4);
    });

    it('closes the "Connecting to" row when the connect fails', () => {
        expect(unclosedRows(APP)).toEqual([]);
    });

    it('flags a connect that does not close its row', () => {
        const source = `const { resolvedIp: ip, connectingLogId: rowId } = await logConnectionSteps(host, 22, 'sftp');
            await runConnect('connect_ftp', params);`;
        expect(unclosedRows(source)).toEqual(['rowId']);
    });

    it('never blames the credentials for every failed connect', () => {
        expect(APP).not.toContain("logError('CONNECT'");
        expect(APP.split('reportConnectFailure(logId, error)').length - 1).toBeGreaterThanOrEqual(4);
    });
});
