// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { act, createElement } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { useTransferEvents } from './useTransferEvents';
import { updateTransferStatus } from '../components/transferQueueActions';
import type { TransferItem } from '../components/TransferQueue';

const listeners = vi.hoisted(() => new Map<string, (event: { payload: unknown }) => void>());
vi.mock('@tauri-apps/api/event', () => ({
    listen: (name: string, handler: (event: { payload: unknown }) => void) => {
        listeners.set(name, handler);
        return Promise.resolve(() => undefined);
    },
}));
vi.mock('../components/Transfer/TransferToastContainer', () => ({ dispatchTransferToast: () => undefined }));

type Options = Parameters<typeof useTransferEvents>[0];

const queueWith = (row: { id: string; filename: string; status: string }) => ({
    items: [{ path: '', size: 0, type: 'upload', ...row }],
    markAsFolder: vi.fn(),
    startTransfer: vi.fn(),
    setProgress: vi.fn(),
});

let root: Root;
let host: HTMLDivElement;

const mount = async (transferQueue: ReturnType<typeof queueWith>) => {
    const options = {
        t: (key: string) => key,
        activityLog: { updateEntry: vi.fn() },
        humanLog: { logStart: () => 'log', logRaw: () => 'log' },
        transferQueue,
        notify: { success: () => null, error: () => null, info: () => null, warning: () => null },
        setActiveTransfer: () => undefined,
        loadRemoteFiles: () => undefined,
        loadLocalFiles: () => undefined,
        currentLocalPath: '/home/u/work',
        currentRemotePath: '/srv/data',
    } as unknown as Options;
    let controls!: ReturnType<typeof useTransferEvents>;
    const Probe = () => {
        controls = useTransferEvents(options);
        return null;
    };
    await act(async () => root.render(createElement(Probe)));
    return { ...options, controls };
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

describe('useTransferEvents start event', () => {
    it('attaches a single-file start to its row and keeps it a file row', async () => {
        // An upload, a download or an AeroSync file creates its row first,
        // then the backend start finds it by name (#364).
        const queue = queueWith({ id: 'q1', filename: 'b.txt', status: 'transferring' });
        await mount(queue);
        await emit({ event_type: 'start', transfer_id: 'pul-1', filename: 'b.txt', direction: 'upload' });
        await emit({
            event_type: 'progress',
            transfer_id: 'pul-1',
            filename: 'b.txt',
            direction: 'upload',
            progress: { transfer_id: 'pul-1', filename: 'b.txt', transferred: 400, total: 1000, percentage: 40, speed_bps: 1000, eta_seconds: 1, direction: 'upload' },
        });
        expect(queue.setProgress).toHaveBeenCalledWith('q1', 40, 1000);
        expect(queue.markAsFolder).not.toHaveBeenCalled();
    });

    it('still turns the row of a folder transfer into a folder row', async () => {
        const queue = queueWith({ id: 'q1', filename: 'docs', status: 'transferring' });
        await mount(queue);
        await emit({ event_type: 'start', transfer_id: 'ul-folder-1', filename: 'docs', direction: 'upload' });
        expect(queue.markAsFolder).toHaveBeenCalledWith('q1');
    });
});

describe('cross-profile terminal events', () => {
    it.each([
        ['error', 'error'],
        ['cancelled', 'error'],
        ['complete', 'success'],
    ])('closes the activity on %s and forgets the transfer mapping', async (eventType, status) => {
        const queue = queueWith({ id: 'q1', filename: 'docs', status: 'pending' });
        const options = await mount(queue);
        const transfer = { transfer_id: 'cross-1', filename: '2 files', direction: 'cross-profile' };
        await emit({ ...transfer, event_type: 'start', message: 'source -> destination' });
        expect(options.activityLog.updateEntry).not.toHaveBeenCalled();

        await emit({
            ...transfer, event_type: eventType, message: 'terminal outcome',
            progress: {
                ...transfer, filename: '', transferred: 2, total: 2,
                percentage: 100, speed_bps: 0, eta_seconds: 0, total_files: 2,
            },
        });
        expect(options.activityLog.updateEntry).toHaveBeenCalledExactlyOnceWith('log', {
            status, message: 'terminal outcome',
        });
        // A late duplicate must not update an activity belonging to a later run.
        await emit({ ...transfer, event_type: 'complete' });
        expect(options.activityLog.updateEntry).toHaveBeenCalledTimes(1);
        expect(queue.startTransfer).not.toHaveBeenCalled();
    });

    it.each([0, 1])('marks a completed batch with %i of 2 successful/skipped files as failed', async (transferred) => {
        const queue = queueWith({ id: 'q1', filename: 'docs', status: 'pending' });
        const options = await mount(queue);
        const transfer = { transfer_id: 'cross-partial', filename: '2 files', direction: 'cross-profile' };
        await emit({ ...transfer, event_type: 'start', message: 'source -> destination' });
        await emit({ ...transfer, event_type: 'file_error', message: 'file failed after retries' });
        expect(options.activityLog.updateEntry).not.toHaveBeenCalled();
        await emit({
            ...transfer, event_type: 'complete', filename: `${transferred} transferred, 0 skipped, ${2 - transferred} failed`,
            progress: {
                ...transfer, filename: '', transferred, total: 2,
                percentage: 100, speed_bps: 0, eta_seconds: 0, total_files: 2,
            },
        });
        expect(options.activityLog.updateEntry).toHaveBeenCalledExactlyOnceWith('log', { status: 'error', message: '' });
        await emit({ ...transfer, event_type: 'complete' });
        expect(options.activityLog.updateEntry).toHaveBeenCalledTimes(1);
    });
});

it('settles the active row and activity when the server skips an occupied upload', async () => {
    const queue = Object.assign(queueWith({ id: 'q1', filename: 'a.txt', status: 'pending' }), {
        addItem: vi.fn(() => 'duplicate'), completeTransfer: vi.fn(),
    });
    queue.items[0].path = '/target/a.txt';
    const options = await mount(queue);
    const transfer = { transfer_id: 'ul-folder-7-0', filename: 'a.txt', path: '/target/a.txt', direction: 'upload' };
    await emit({ ...transfer, event_type: 'file_start' });
    expect(queue.startTransfer).toHaveBeenCalledWith('q1');
    await emit({ ...transfer, event_type: 'file_skip', message: 'Destination already exists' });
    expect(queue.completeTransfer).toHaveBeenCalledExactlyOnceWith('q1');
    expect(queue.addItem).not.toHaveBeenCalled();
    expect(options.activityLog.updateEntry).toHaveBeenCalledWith('log', expect.objectContaining({ status: 'success' }));
});


describe('privacy completion ownership', () => {
    it('keeps file_complete unsettled so a rejected privacy write can fail the real queue row', async () => {
        let items = [{id:'q1',filename:'b.txt',path:'/b.txt',size:1,type:'upload',status:'transferring'}] as TransferItem[];
        const queue = Object.assign(queueWith({id:'q1',filename:'b.txt',status:'transferring'}), {
            completeTransfer: (id:string) => { items=updateTransferStatus(items,id,'completed'); },
            failTransfer: (id:string,error:string) => { items=updateTransferStatus(items,id,'error',undefined,error); },
        });
        Object.defineProperty(queue,'items',{get:()=>items});
        const {controls} = await mount(queue);
        controls.registerFileBatchRows('batch',['q1']);
        const release=controls.holdFileCompletions(['q1']);
        let rejectPrivacy!:(e:Error)=>void;
        const privacy=new Promise<void>((_,reject)=>{rejectPrivacy=reject;});
        const final=privacy.catch(error=>queue.failTransfer('q1',String(error)));
        await emit({event_type:'file_start',transfer_id:'batch-0',filename:'b.txt',path:'/b.txt',direction:'upload'});
        await emit({event_type:'file_complete',transfer_id:'batch-0',filename:'b.txt',path:'/b.txt',direction:'upload'});
        expect(items[0].status).toBe('transferring');
        rejectPrivacy(new Error('privacy refused'));
        await final;
        release();
        expect(items[0].status).toBe('error');
        expect(items[0].error).toContain('privacy refused');
        await emit({event_type:'file_complete',transfer_id:'batch-0',filename:'b.txt',path:'/b.txt',direction:'upload'});
        expect(items[0].status).toBe('error');
    });
});
