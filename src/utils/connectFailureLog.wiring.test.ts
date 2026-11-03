// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { act, createElement } from 'react';
import { createRoot } from 'react-dom/client';
import { ActivityLogProvider, useActivityLog, type ActivityLogContextValue } from '../hooks/useActivityLog';
import APP from '../App.tsx?raw';
import { CONNECT_CANCELLED_MARKER, isConnectCancelledError } from './connectCancel';

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
    const starts = [...source.matchAll(/connectingLogId(?:\s*:\s*(\w+))?\s*\}\s*=\s*await[^;]*?logConnectionSteps\(/g)];
    return starts.flatMap((match, index) => {
        const name = match[1] ?? 'connectingLogId';
        const region = source.slice(match.index, starts[index + 1]?.index ?? source.length);
        // Accept only the expression-callback forms the current callers use.
        // This requires runConnect to be inside the wrapper; a leftover
        // wrapper around a no-op followed by an unwrapped connect must fail.
        const wrappedConnect = new RegExp(
            `connectUnderRow\\(\\s*${name}\\s*,\\s*\\(\\)\\s*=>\\s*(?:connectScope\\.step\\(\\s*\\(\\)\\s*=>\\s*)?runConnect\\(`,
        );
        return wrappedConnect.test(region) ? [] : [name];
    });
}

describe('connect failure log', () => {
    it('settles a user-cancelled connect row neutrally and rethrows the cancellation', async () => {
        const start = APP.indexOf('const connectUnderRow = async');
        const end = APP.indexOf('\n  };', start);
        const handler = APP.slice(start, end).replace(/:\s*(?:string \| null|\(\) => Promise<unknown>|Promise<void>)/g, '');
        const run = new Function('humanLog', 'isConnectCancelledError', `${handler}\n  }; return connectUnderRow;`);
        const entries: unknown[] = [];
        const connect = run({ updateEntry: (...args: unknown[]) => entries.push(args) }, isConnectCancelledError);
        const cancelled = new Error(CONNECT_CANCELLED_MARKER);
        await expect(connect('row', async () => { throw cancelled; })).rejects.toBe(cancelled);
        expect(entries).toEqual([['row', { status: 'cancelled' }]]);
        entries.length = 0;
        const failed = new Error('connection refused');
        await expect(connect('row', async () => { throw failed; })).rejects.toBe(failed);
        expect(entries).toEqual([['row', { status: 'error' }]]);
        entries.length = 0;
        const serverFailure = new Error('server refused CONNECT_CANCELLED credentials');
        await expect(connect('row', async () => { throw serverFailure; })).rejects.toBe(serverFailure);
        expect(entries).toEqual([['row', { status: 'error' }]]);
    });

    it('a cancelled connecting row stops contributing to the actual active badge', async () => {
        const host = document.createElement('div');
        const root = createRoot(host);
        let log!: ActivityLogContextValue;
        const Probe = () => { log = useActivityLog(); return null; };
        try {
            await act(async () => { root.render(createElement(ActivityLogProvider, { children: createElement(Probe) })); });
            let id = '';
            await act(async () => { id = log.log('CONNECT', 'connecting', 'running'); });
            expect(log.runningCount).toBe(1);
            const start = APP.indexOf('const connectUnderRow = async');
            const end = APP.indexOf('\n  };', start);
            const handler = APP.slice(start, end).replace(/:\s*(?:string \| null|\(\) => Promise<unknown>|Promise<void>)/g, '');
            const connect = new Function('humanLog', 'isConnectCancelledError', `${handler}\n  }; return connectUnderRow;`)(log, isConnectCancelledError);
            await act(async () => { await expect(connect(id, async () => { throw new Error(CONNECT_CANCELLED_MARKER); })).rejects.toThrow(CONNECT_CANCELLED_MARKER); });
            expect(log.entries[0].status).toBe('cancelled');
            expect(log.runningCount).toBe(0);
            expect(log.hasRunning).toBe(false);
        } finally { await act(async () => root.unmount()); }
    });

    it('rejects an unwrapped duplicate even when another row has the same identifier', () => {
        const source = `const { connectingLogId } = await logConnectionSteps(a, 22, 'sftp');
            await connectUnderRow(connectingLogId, () => runConnect('connect_ftp', {}));
            const { connectingLogId } = await logConnectionSteps(b, 22, 'scp');
            await runConnect('connect_ftp', {});`;
        expect(unclosedRows(source)).toEqual(['connectingLogId']);
    });

    it('rejects a leftover wrapper when the actual connect is outside it', () => {
        const source = `const { connectingLogId } = await logConnectionSteps(a, 22, 'scp');
            await connectUnderRow(connectingLogId, () => Promise.resolve());
            await runConnect('provider_connect', params);`;
        expect(unclosedRows(source)).toEqual(['connectingLogId']);
    });

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
