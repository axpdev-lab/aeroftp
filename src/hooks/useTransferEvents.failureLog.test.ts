// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import { act, createElement } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { useTransferEvents } from './useTransferEvents';
import en from '../i18n/locales/en.json';

const listeners = vi.hoisted(() => new Map<string, (event: { payload: unknown }) => void>());
vi.mock('@tauri-apps/api/event', () => ({
    listen: (name: string, handler: (event: { payload: unknown }) => void) => {
        listeners.set(name, handler);
        return Promise.resolve(() => undefined);
    },
}));
vi.mock('../components/Transfer/TransferToastContainer', () => ({
    dispatchTransferToast: () => undefined,
}));

type Options = Parameters<typeof useTransferEvents>[0];

interface LogRow { id: string; operation: string; status: string; message: string }

/** In-memory Activity Log + humanLog sharing one entry list. */
const makeLog = () => {
    const entries: LogRow[] = [];
    let n = 0;
    const log = (operation: string, message: string, status = 'running'): string => {
        const id = `log_${++n}`;
        entries.push({ id, operation, status, message });
        return id;
    };
    const updateEntry = (id: string, updates: Partial<Pick<LogRow, 'status' | 'message'>>) => {
        const e = entries.find(x => x.id === id);
        if (e) Object.assign(e, updates);
    };
    const t = (key: string, params?: Record<string, string | number>) =>
        params ? `${key} ${Object.values(params).join(' ')}` : key;
    const activityLog = { updateEntry } as unknown as Options['activityLog'];
    const humanLog = {
        logStart: (op: string, params?: Record<string, string>) => log(op, `${op} ${params?.filename ?? ''}`, 'running'),
        logRaw: (key: string, type: string, params?: Record<string, unknown>, status?: 'running' | 'success' | 'error') =>
            log(type, `${key} ${Object.values(params ?? {}).join(' ')}`, status ?? 'running'),
        logError: (op: string, params?: Record<string, unknown>, existingId?: string) => {
            const message = `${op}_error ${Object.values(params ?? {}).join(' ')}`;
            if (existingId) { updateEntry(existingId, { status: 'error', message }); return existingId; }
            return log('ERROR', message, 'error');
        },
        updateEntry,
    } as unknown as Options['humanLog'];
    return { entries, activityLog, humanLog, t };
};

let root: Root;
let host: HTMLDivElement;

const mount = async (currentRemotePath: string, currentLocalPath: string) => {
    const { entries, activityLog, humanLog, t } = makeLog();
    const transferQueue = {
        items: [],
        markAsFolder: vi.fn(),
        startTransfer: vi.fn(),
        setProgress: vi.fn(),
        failTransfer: vi.fn(),
        completeTransfer: vi.fn(),
        cancelTransfer: vi.fn(),
        getItemNow: vi.fn(),
    };
    const toastErrorOnly = vi.fn();
    const options = {
        t,
        activityLog,
        humanLog,
        transferQueue,
        notify: { success: () => null, error: () => null, info: () => null, warning: () => null },
        toastErrorOnly,
        setActiveTransfer: () => undefined,
        loadRemoteFiles: () => undefined,
        loadLocalFiles: () => undefined,
        currentLocalPath,
        currentRemotePath,
    } as unknown as Options;
    let controls!: ReturnType<typeof useTransferEvents>;
    const Probe = () => {
        controls = useTransferEvents(options);
        return null;
    };
    await act(async () => root.render(createElement(Probe)));
    return { entries, controls, toastErrorOnly, transferQueue };
};

const emit = async (payload: Record<string, unknown>) => {
    await act(async () => listeners.get('transfer_event')!({ payload }));
};

beforeEach(() => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    listeners.clear();
    host = document.createElement('div');
    document.body.append(host);
    root = createRoot(host);
});
afterEach(async () => {
    await act(async () => root.unmount());
    host.remove();
});

describe('one failure line per failed transfer, with the real path', () => {
    it('resumed upload targeted at /Test-Quota while the panel shows /: one error entry naming the real target', async () => {
        const { entries, controls, toastErrorOnly } = await mount('/', '/home/u');
        // The caller (journal descriptor on resume) opened the entry and knows
        // the real target, which is NOT under the current folder.
        const logId = 'log_resume_1';
        entries.push({ id: logId, operation: 'UPLOAD', status: 'running', message: 'UPLOAD x.bin' });
        controls.pendingFileLogIds.current.set('x.bin', logId);
        controls.pendingFileTargetPaths.current.set('x.bin', '/Test-Quota/x.bin');

        await emit({ event_type: 'start', transfer_id: 't-resume', filename: 'x.bin', direction: 'upload' });
        await emit({ event_type: 'error', transfer_id: 't-resume', filename: 'x.bin', direction: 'upload', message: 'Storage quota exceeded' });

        const errorEntries = entries.filter(e => e.status === 'error');
        expect(errorEntries).toHaveLength(1);
        expect(errorEntries[0].id).toBe(logId); // updated in place, no second line
        expect(errorEntries[0].message).toContain('/Test-Quota/x.bin');
        expect(errorEntries[0].message).not.toContain(' /x.bin');
        // R1: the single line also carries the reason
        expect(errorEntries[0].message).toContain('Storage quota exceeded');
        expect(toastErrorOnly).toHaveBeenCalledTimes(1);
        expect(toastErrorOnly.mock.calls[0][1]).toContain('Storage quota exceeded');
    });

    it('direct upload failure: exactly one error entry, real remote path, error level', async () => {
        const { entries, controls, toastErrorOnly } = await mount('/', '/home/u');
        const logId = 'log_direct_1';
        entries.push({ id: logId, operation: 'UPLOAD', status: 'running', message: 'UPLOAD quota-test-110mib.bin' });
        controls.pendingFileLogIds.current.set('quota-test-110mib.bin', logId);
        controls.pendingFileTargetPaths.current.set('quota-test-110mib.bin', '/quota-test-110mib.bin');

        await emit({ event_type: 'start', transfer_id: 't-direct', filename: 'quota-test-110mib.bin', direction: 'upload' });
        await emit({ event_type: 'error', transfer_id: 't-direct', filename: 'quota-test-110mib.bin', direction: 'upload', message: 'Storage quota exceeded' });

        const errorEntries = entries.filter(e => e.status === 'error');
        expect(errorEntries).toHaveLength(1);
        expect(errorEntries[0].message).toContain('/quota-test-110mib.bin');
        expect(errorEntries[0].message).toContain('Storage quota exceeded');
        expect(toastErrorOnly).toHaveBeenCalledTimes(1);
    });

    it('download failure names the real local target, one error entry', async () => {
        const { entries, controls } = await mount('/srv/data', '/home/u');
        const logId = 'log_dl_1';
        entries.push({ id: logId, operation: 'DOWNLOAD', status: 'running', message: 'DOWNLOAD x.bin' });
        controls.pendingFileLogIds.current.set('x.bin', logId);
        controls.pendingFileTargetPaths.current.set('x.bin', '/home/u/Downloads/x.bin');

        await emit({ event_type: 'start', transfer_id: 't-dl', filename: 'x.bin', direction: 'download' });
        await emit({ event_type: 'error', transfer_id: 't-dl', filename: 'x.bin', direction: 'download', message: 'read failed' });

        const errorEntries = entries.filter(e => e.status === 'error');
        expect(errorEntries).toHaveLength(1);
        expect(errorEntries[0].message).toContain('activity.download_error_reason');
        expect(errorEntries[0].message).toContain('/home/u/Downloads/x.bin');
        expect(errorEntries[0].message).toContain('read failed');
    });

    it('start event with an explicit path keeps that path on the error line', async () => {
        const { entries, controls } = await mount('/', '/home/u');
        const logId = 'log_path_1';
        entries.push({ id: logId, operation: 'UPLOAD', status: 'running', message: 'UPLOAD y.bin' });
        controls.pendingFileLogIds.current.set('y.bin', logId);

        await emit({ event_type: 'start', transfer_id: 't-path', filename: 'y.bin', direction: 'upload', path: '/Elsewhere/y.bin' });
        await emit({ event_type: 'error', transfer_id: 't-path', filename: 'y.bin', direction: 'upload', message: 'boom' });

        const errorEntries = entries.filter(e => e.status === 'error');
        expect(errorEntries).toHaveLength(1);
        expect(errorEntries[0].message).toContain('/Elsewhere/y.bin');
    });
});

describe('transfer.resumeConnectConfirm plural', () => {
    const tr = (en.translations as unknown as Record<string, Record<string, string>>).transfer;
    it('count 1 and 2 render different strings in en', () => {
        expect(tr.resumeConnectConfirm_one).toBeDefined();
        expect(tr.resumeConnectConfirm_many).toBeDefined();
        expect(tr.resumeConnectConfirm_one).not.toBe(tr.resumeConnectConfirm_many);
        expect(tr.resumeConnectConfirm_one).toContain('{count}');
        expect(tr.resumeConnectConfirm_many).toContain('{count}');
    });
});
