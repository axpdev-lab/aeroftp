// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { act, createElement } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { useTransferEvents } from './useTransferEvents';
import { updateTransferStatus } from '../components/transferQueueActions';
import type { TransferItem } from '../components/TransferQueue';
import { fixture } from '../utils/testFixtures/appTransferWiring';

const listeners = vi.hoisted(() => new Map<string, (event: { payload: unknown }) => void>());
vi.mock('@tauri-apps/api/event', () => ({
    listen: (name: string, handler: (event: { payload: unknown }) => void) => {
        listeners.set(name, handler);
        return Promise.resolve(() => undefined);
    },
}));
// The toast states the hook dispatches, for the lane checks.
const toastStates = vi.hoisted(() => [] as Array<{ lanes?: Array<{ id: string; state: string }> } | null>);
vi.mock('../components/Transfer/TransferToastContainer', () => ({
    dispatchTransferToast: (state: { lanes?: Array<{ id: string; state: string }> } | null) => { toastStates.push(state); },
}));

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

// CodeRabbit on #1561: a folder child's own id (`<folder>-<n>`) holds
// `-folder-`, so its skip after its own file_start was read as the folder's:
// the row settled by its path key, but its bare mapping and its toast lane
// stayed active. A started folder's child is one file's skip.
it('ends a started folder child\'s mapping and lane when its file is skipped', async () => {
    let items = [
        { id: 'folder', filename: 'docs', path: '/docs', size: 0, type: 'upload', status: 'pending' },
        { id: 'child', filename: 'a.txt', path: '/docs/a.txt', size: 1, type: 'upload', status: 'pending' },
    ] as TransferItem[];
    const queue = Object.assign(queueWith({ id: 'folder', filename: 'docs', status: 'pending' }), {
        completeTransfer: (id: string) => { items = updateTransferStatus(items, id, 'completed'); },
        updateFolderProgress: vi.fn(), setProgress: vi.fn(),
    });
    Object.defineProperty(queue, 'items', { get: () => items });
    const { controls } = await mount(queue);
    const folderProgress = { transfer_id: 'ul-folder-9', filename: 'docs', total: 1, transferred: 0, percentage: 0, speed_bps: 0, eta_seconds: 0, total_files: 1 };
    await emit({ event_type: 'start', transfer_id: 'ul-folder-9', filename: 'docs', path: '/docs', direction: 'upload', progress: folderProgress });
    const child = { transfer_id: 'ul-folder-9-0', filename: 'a.txt', path: '/docs/a.txt', direction: 'upload' };
    await emit({ ...child, event_type: 'file_start' });
    await emit({ ...child, event_type: 'file_skip', message: 'Destination already exists' });
    expect(items.find(item => item.id === 'child')?.status).toBe('completed');
    expect(items.find(item => item.id === 'folder')?.status).not.toBe('completed');
    // Its lane ends with it.
    const withLanes = toastStates.filter(state => state?.lanes);
    const lane = withLanes[withLanes.length - 1]?.lanes?.find((entry: { id: string }) => entry.id === 'ul-folder-9-0');
    expect(lane?.state).toBe('completed');
    // Its mapping went with it: a late progress of that id reaches no row.
    await emit({ ...child, event_type: 'progress', progress: { transfer_id: 'ul-folder-9-0', filename: 'a.txt', total: 1, transferred: 1, percentage: 100, speed_bps: 1, eta_seconds: 0 } });
    expect(queue.setProgress).not.toHaveBeenCalledWith('child', expect.anything(), expect.anything());
    expect(controls.singleFileTransferIds.current.has('ul-folder-9-0')).toBe(false);
    // Nor does it make the skipped file's lane active again.
    const after = toastStates.filter(state => state?.lanes);
    expect(after[after.length - 1]?.lanes?.find((entry: { id: string }) => entry.id === 'ul-folder-9-0')?.state).toBe('completed');
});

// CodeRabbit CLI on the round: every skipped child is terminal now, so the
// terminal set must stay bounded across skip-heavy batches.
it('keeps the terminal ids bounded across skip-heavy batches', async () => {
    let rows = 0;
    const queue = Object.assign(queueWith({ id: 'q', filename: 'x', status: 'pending' }), {
        setProgress: vi.fn(), completeTransfer: vi.fn(), addItem: vi.fn(() => `row-${++rows}`),
    });
    const { controls } = await mount(queue);
    for (const run of [1, 2, 3]) {
        const folder = `ul-folder-b${run}`;
        await emit({ event_type: 'start', transfer_id: folder, filename: 'docs', path: '/docs', direction: 'upload' });
        for (let index = 0; index < 300; index++) {
            const child = { transfer_id: `${folder}-${index}`, filename: `f${index}`, path: `/docs/f${index}`, direction: 'upload' };
            await emit({ ...child, event_type: 'file_start' });
            await emit({ ...child, event_type: 'file_skip', message: 'Destination already exists' });
        }
        await emit({ event_type: 'complete', transfer_id: folder, filename: 'docs', path: '/docs', direction: 'upload' });
    }
    expect(controls.completedTransferIds.current.size).toBeLessThanOrEqual(500);
    // The latest skipped file is still terminal for a late progress.
    expect(controls.completedTransferIds.current.has('ul-folder-b3-299')).toBe(true);
});

// CodeRabbit CLI on the round: past the bound, an early skipped child of a
// folder still running leaves the terminal set; its late progress must still
// not show it transferring again.
it('keeps an evicted skipped child terminal while its folder runs', async () => {
    let rows = 0;
    const queue = Object.assign(queueWith({ id: 'q', filename: 'x', status: 'pending' }), {
        setProgress: vi.fn(), completeTransfer: vi.fn(), addItem: vi.fn(() => `row-${++rows}`),
    });
    const { controls } = await mount(queue);
    const folder = 'ul-folder-big';
    const folderProgress = { transfer_id: folder, filename: 'docs', total: 600, transferred: 0, percentage: 0, speed_bps: 0, eta_seconds: 0, total_files: 600 };
    await emit({ event_type: 'start', transfer_id: folder, filename: 'docs', path: '/docs', direction: 'upload', progress: folderProgress });
    for (let index = 0; index < 600; index++) {
        const child = { transfer_id: `${folder}-${index}`, filename: `f${index}`, path: `/docs/f${index}`, direction: 'upload' };
        await emit({ ...child, event_type: 'file_start' });
        await emit({ ...child, event_type: 'file_skip', message: 'Destination already exists' });
    }
    expect(controls.completedTransferIds.current.has(`${folder}-0`)).toBe(false);
    await emit({ event_type: 'progress', transfer_id: `${folder}-0`, filename: 'f0', path: '/docs/f0', direction: 'upload', progress: { transfer_id: `${folder}-0`, filename: 'f0', total: 1, transferred: 1, percentage: 100, speed_bps: 1, eta_seconds: 0 } });
    const withLanes = toastStates.filter(state => state?.lanes);
    const lane = withLanes[withLanes.length - 1]?.lanes?.find((entry: { id: string }) => entry.id === `${folder}-0`);
    expect(lane?.state ?? 'gone').not.toBe('active');
    expect(queue.setProgress).not.toHaveBeenCalled();
    // CodeRabbit CLI on the combined range: the folder's complete retired its
    // started id, and the evicted child's late progress then made an active
    // toast again. Complete, let the toast clear, then that progress.
    vi.useFakeTimers();
    try {
        await emit({ event_type: 'complete', transfer_id: folder, filename: 'docs', path: '/docs', direction: 'upload' });
        await act(async () => { vi.advanceTimersByTime(600); });
        expect(toastStates[toastStates.length - 1]).toBeNull();
        const before = toastStates.length;
        await emit({ event_type: 'progress', transfer_id: `${folder}-0`, filename: 'f0', path: '/docs/f0', direction: 'upload', progress: { transfer_id: `${folder}-0`, filename: 'f0', total: 1, transferred: 1, percentage: 100, speed_bps: 1, eta_seconds: 0 } });
        await act(async () => { vi.advanceTimersByTime(600); });
        const active = toastStates.slice(before).flatMap(state => state?.lanes ?? []).filter(entry => entry.state === 'active');
        expect(active).toEqual([]);
        expect(toastStates[toastStates.length - 1]).toBeNull();
        expect(queue.setProgress).not.toHaveBeenCalled();
    } finally { vi.useRealTimers(); }
});

// CodeRabbit CLI on the combined range of #1561: a late progress of an ended
// child cancelled the timer that clears the completed toast before the
// terminal check refused it, so the toast stayed on screen.
it('clears the completed toast when a skipped child\'s progress comes after its folder completed', async () => {
    let rows = 0;
    const queue = Object.assign(queueWith({ id: 'q', filename: 'x', status: 'pending' }), {
        setProgress: vi.fn(), completeTransfer: vi.fn(), addItem: vi.fn(() => `row-${++rows}`),
    });
    await mount(queue);
    vi.useFakeTimers();
    try {
        const folder = 'ul-folder-late';
        const child = { transfer_id: `${folder}-0`, filename: 'a.txt', path: '/docs/a.txt', direction: 'upload' };
        await emit({ event_type: 'start', transfer_id: folder, filename: 'docs', path: '/docs', direction: 'upload', progress: { transfer_id: folder, filename: 'docs', total: 1, transferred: 0, percentage: 0, speed_bps: 0, eta_seconds: 0, total_files: 1 } });
        await emit({ ...child, event_type: 'file_start' });
        await emit({ ...child, event_type: 'file_skip', message: 'Destination already exists' });
        await emit({ event_type: 'complete', transfer_id: folder, filename: 'docs', path: '/docs', direction: 'upload' });
        await emit({ ...child, event_type: 'progress', progress: { ...child, total: 1, transferred: 1, percentage: 100, speed_bps: 1, eta_seconds: 0 } });
        await act(async () => { vi.advanceTimersByTime(600); });
        expect(toastStates[toastStates.length - 1]).toBeNull();
        expect(queue.setProgress).not.toHaveBeenCalled();
    } finally { vi.useRealTimers(); }
});

// CodeRabbit on #1561: every registered batch child stayed classified as a
// single file for the App's lifetime, started or not.
it('keeps no single-file classification of settled batches, started or not', async () => {
    let items = [
        { id: 'q1', filename: 'a.txt', path: '/a.txt', size: 1, type: 'upload', status: 'pending' },
        { id: 'q2', filename: 'b.txt', path: '/b.txt', size: 1, type: 'upload', status: 'pending' },
    ] as TransferItem[];
    const queue = Object.assign(queueWith({ id: 'q1', filename: 'a.txt', status: 'pending' }), {
        completeTransfer: (id: string) => { items = updateTransferStatus(items, id, 'completed'); },
    });
    Object.defineProperty(queue, 'items', { get: () => items });
    const { controls } = await mount(queue);
    for (const run of [1, 2, 3]) {
        const batchId = `ul-files-${run}`;
        controls.registerFileBatchRows(batchId, ['q1', 'q2']);
        await emit({ event_type: 'file_start', transfer_id: `${batchId}-0`, filename: 'a.txt', path: '/a.txt', direction: 'upload' });
        await emit({ event_type: 'file_complete', transfer_id: `${batchId}-0`, filename: 'a.txt', path: '/a.txt', direction: 'upload' });
        // `${batchId}-1` never starts; the batch's end retires it too.
        await emit({ event_type: 'complete', transfer_id: batchId, filename: 'batch', direction: 'upload' });
    }
    expect(controls.singleFileTransferIds.current.size).toBe(0);
});

it('keeps no queue mapping of a batch child that never started once its batch ends', async () => {
    let items = [
        { id: 'q1', filename: 'a.txt', path: '/a.txt', size: 1, type: 'upload', status: 'pending' },
        { id: 'q2', filename: 'b.txt', path: '/b.txt', size: 1, type: 'upload', status: 'pending' },
    ] as TransferItem[];
    const queue = Object.assign(queueWith({ id: 'q1', filename: 'a.txt', status: 'pending' }), {
        completeTransfer: (id: string) => { items = updateTransferStatus(items, id, 'completed'); },
        failTransfer: (id: string, error: string) => { items = updateTransferStatus(items, id, 'error', undefined, error); },
    });
    Object.defineProperty(queue, 'items', { get: () => items });
    const { controls } = await mount(queue);
    const ends = ['complete', 'error', 'cancelled'] as const;
    for (const [run, end] of ends.entries()) {
        const batchId = `ul-files-${run}`;
        controls.registerFileBatchRows(batchId, ['q1', 'q2']);
        await emit({ event_type: 'file_start', transfer_id: `${batchId}-0`, filename: 'a.txt', path: '/a.txt', direction: 'upload' });
        await emit({ event_type: 'file_complete', transfer_id: `${batchId}-0`, filename: 'a.txt', path: '/a.txt', direction: 'upload' });
        // `${batchId}-1` never starts: its bare mapping ends with the batch.
        await emit({ event_type: end, transfer_id: batchId, filename: 'batch', direction: 'upload', message: 'batch ended' });
    }
    expect([...controls.transferIdToQueueId.current.keys()]).toEqual([]);
    // Its row is left to the batch's command result: the batch's end does not settle it.
    expect(items.find(item => item.id === 'q2')?.status).toBe('pending');
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

    // A single-file upload: `start` registers the row under the bare transfer
    // id, then the server skips the file. That row is settled; no second row.
    it('settles a single-file upload skipped after its start, without a duplicate row', async () => {
        let items = [{ id: 'q1', filename: 'c.txt', path: '/c.txt', size: 1, type: 'upload', status: 'transferring' }] as TransferItem[];
        const queue = Object.assign(queueWith({ id: 'q1', filename: 'c.txt', status: 'transferring' }), {
            completeTransfer: (id: string) => { items = updateTransferStatus(items, id, 'completed'); },
            addItem: vi.fn((filename: string, path: string, size: number, type: string) => {
                items = [...items, { id: `added-${items.length}`, filename, path, size, type, status: 'pending' } as TransferItem];
                return `added-${items.length - 1}`;
            }),
        });
        Object.defineProperty(queue, 'items', { get: () => items });
        await mount(queue);
        await emit({ event_type: 'start', transfer_id: 'ul-single-1', filename: 'c.txt', path: '/c.txt', direction: 'upload' });
        await emit({ event_type: 'file_skip', transfer_id: 'ul-single-1', filename: 'c.txt', path: '/c.txt', direction: 'upload', message: 'Destination already exists' });
        // No completed duplicate was added for the skip, and every row is settled.
        expect(items.filter(item => item.filename === 'c.txt' && item.status === 'transferring')).toHaveLength(0);
        expect(queue.addItem.mock.calls.filter(call => call[2] === 0)).toHaveLength(0);
    });

    // A file batch registers its child rows before it starts; a preflight
    // can skip one before any file_start. The registered row completes, and
    // no duplicate row is added.
    it('settles a registered batch row skipped before its file_start', async () => {
        let items = [
            { id: 'q1', filename: 'a.txt', path: '/a.txt', size: 1, type: 'upload', status: 'pending' },
            { id: 'q2', filename: 'b.txt', path: '/b.txt', size: 1, type: 'upload', status: 'pending' },
        ] as TransferItem[];
        const queue = Object.assign(queueWith({ id: 'q1', filename: 'a.txt', status: 'pending' }), {
            completeTransfer: (id: string) => { items = updateTransferStatus(items, id, 'completed'); },
            addItem: vi.fn(() => 'duplicate'),
        });
        Object.defineProperty(queue, 'items', { get: () => items });
        const { controls } = await mount(queue);
        controls.registerFileBatchRows('batch-7', ['q1', 'q2']);
        await emit({ event_type: 'file_skip', transfer_id: 'batch-7-1', filename: 'b.txt', path: '/b.txt', direction: 'upload', message: 'Destination already exists' });
        expect(items.find(item => item.id === 'q2')?.status).toBe('completed');
        expect(items.find(item => item.id === 'q1')?.status).toBe('pending');
        expect(queue.addItem).not.toHaveBeenCalled();
    });

    // U8(b): the same with the id App.tsx makes (`ul-files-<time>-<rand>`),
    // whose `-files-` once sent the child's skip to the aggregate lookup.
    it('settles a production-id batch row skipped before its file_start', async () => {
        let items = [
            { id: 'q1', filename: 'a.txt', path: '/a.txt', size: 1, type: 'upload', status: 'pending' },
            { id: 'q2', filename: 'b.txt', path: '/b.txt', size: 1, type: 'upload', status: 'pending' },
        ] as TransferItem[];
        const queue = Object.assign(queueWith({ id: 'q1', filename: 'a.txt', status: 'pending' }), {
            completeTransfer: (id: string) => { items = updateTransferStatus(items, id, 'completed'); },
            addItem: vi.fn(() => 'duplicate'),
        });
        Object.defineProperty(queue, 'items', { get: () => items });
        const { controls } = await mount(queue);
        const batchId = 'ul-files-1731542400000-k3x9qa';
        controls.registerFileBatchRows(batchId, ['q1', 'q2']);
        await emit({ event_type: 'file_skip', transfer_id: `${batchId}-1`, filename: 'b.txt', path: '/b.txt', direction: 'upload', message: 'Destination already exists' });
        expect(items.find(item => item.id === 'q2')?.status).toBe('completed');
        expect(items.find(item => item.id === 'q1')?.status).toBe('pending');
        expect(queue.addItem).not.toHaveBeenCalled();
        // The other child still starts and completes on its own row.
        await emit({ event_type: 'file_start', transfer_id: `${batchId}-0`, filename: 'a.txt', path: '/a.txt', direction: 'upload' });
        await emit({ event_type: 'file_complete', transfer_id: `${batchId}-0`, filename: 'a.txt', path: '/a.txt', direction: 'upload' });
        expect(items.find(item => item.id === 'q1')?.status).toBe('completed');
        expect(queue.addItem).not.toHaveBeenCalled();
    });

    // A folder's start registers the folder row under the bare id. A child
    // skipped before any file_start must not complete that row: the bare id
    // is found only for a single-file transfer.
    it('keeps a folder row running when its first child is skipped before any file starts', async () => {
        let items = [{ id: 'folder', filename: 'docs', path: '/docs', size: 0, type: 'upload', status: 'transferring' }] as TransferItem[];
        const queue = Object.assign(queueWith({ id: 'folder', filename: 'docs', status: 'transferring' }), {
            completeTransfer: (id: string) => { items = updateTransferStatus(items, id, 'completed'); },
            updateFolderProgress: vi.fn(),
            addItem: vi.fn((filename: string, path: string, size: number, type: string) => {
                items = [...items, { id: `added-${items.length}`, filename, path, size, type, status: 'pending' } as TransferItem];
                return `added-${items.length - 1}`;
            }),
        });
        Object.defineProperty(queue, 'items', { get: () => items });
        await mount(queue);
        await emit({ event_type: 'start', transfer_id: 'ul-folder-7', filename: 'docs', path: '/docs', direction: 'upload' });
        await emit({ event_type: 'file_skip', transfer_id: 'ul-folder-7', filename: 'a.txt', path: '/docs/a.txt', direction: 'upload', message: 'Destination already exists' });
        expect(items.find(item => item.id === 'folder')?.status).toBe('transferring');
        // The next child transfers and the folder row still follows it.
        await emit({ event_type: 'file_start', transfer_id: 'ul-folder-7', filename: 'b.txt', path: '/docs/b.txt', direction: 'upload' });
        expect(items.find(item => item.id === 'folder')?.status).toBe('transferring');
        // T10: a symlink skipped inside the folder carries the folder's id;
        // its progress and its end still reach the folder row.
        await emit({ event_type: 'file_skip', transfer_id: 'ul-folder-7', filename: 'link', path: '/docs/link', direction: 'upload', message: 'Symlink skipped' });
        await emit({ event_type: 'progress', transfer_id: 'ul-folder-7', filename: 'docs', path: '/docs', direction: 'upload', progress: { transfer_id: 'ul-folder-7', filename: 'docs', total: 3, transferred: 2, percentage: 66, speed_bps: 0, eta_seconds: 0, total_files: 3 } });
        // U8(a): the progress reaches the folder row itself.
        expect(queue.updateFolderProgress).toHaveBeenLastCalledWith('folder', 3, 2);
        expect(items.find(item => item.id === 'folder')?.status).toBe('transferring');
        await emit({ event_type: 'complete', transfer_id: 'ul-folder-7', filename: 'docs', path: '/docs', direction: 'upload' });
        expect(items.find(item => item.id === 'folder')?.status).toBe('completed');
    });

    // A file the server skipped wrote nothing, so no privacy update is owed:
    // its row completes even while completions are held.
    it('completes a skipped row while completions are held', async () => {
        let items = [{ id: 'q1', filename: 'b.txt', path: '/b.txt', size: 1, type: 'upload', status: 'transferring' }] as TransferItem[];
        const queue = Object.assign(queueWith({ id: 'q1', filename: 'b.txt', status: 'transferring' }), {
            completeTransfer: (id: string) => { items = updateTransferStatus(items, id, 'completed'); },
            failTransfer: (id: string, error: string) => { items = updateTransferStatus(items, id, 'error', undefined, error); },
        });
        Object.defineProperty(queue, 'items', { get: () => items });
        const { controls } = await mount(queue);
        controls.registerFileBatchRows('batch', ['q1']);
        const release = controls.holdFileCompletions(['q1']);
        await emit({ event_type: 'file_start', transfer_id: 'batch-0', filename: 'b.txt', path: '/b.txt', direction: 'upload' });
        await emit({ event_type: 'file_skip', transfer_id: 'batch-0', filename: 'b.txt', path: '/b.txt', direction: 'upload', message: 'Destination already exists' });
        expect(items[0].status).toBe('completed');
        release();
    });

    it.each(['opendrive', 'fourshared'] as const)('lets the App %s upload batch settle its own rows through the mounted hook', async protocol => {
        // The real App batch closure, the real hook and the real row guard:
        // the backend reports both files complete before privacy is known.
        let items: TransferItem[] = [];
        const queue = Object.assign(queueWith({ id: 'unused', filename: 'unused', status: 'completed' }), {
            startTransfer: (id: string) => { items = updateTransferStatus(items, id, 'transferring'); },
            completeTransfer: (id: string) => { items = updateTransferStatus(items, id, 'completed'); },
            failTransfer: (id: string, error: string) => { items = updateTransferStatus(items, id, 'error', undefined, error); },
            // A file_start the hook cannot match to a live row adds its own.
            addItem: (filename: string, path: string, size: number, type: string) => {
                const id = `late-${items.length}`;
                items = [...items, { id, filename, path, size, type, status: 'pending' } as TransferItem];
                return id;
            },
        });
        Object.defineProperty(queue, 'items', { get: () => items });
        const { controls } = await mount(queue);
        const f = fixture({ direction: 'upload', protocol, destination: [], policy: 'overwrite', hold: controls,
            queue: { startTransfer: queue.startTransfer, completeTransfer: queue.completeTransfer, failTransfer: queue.failTransfer } });
        const addRow = f.transferQueue.addItem.getMockImplementation()! as (...args: unknown[]) => string;
        f.transferQueue.addItem.mockImplementation(((...args: unknown[]) => {
            const id = addRow(...args);
            items = [...items, { id, filename: args[0], path: args[1], size: args[2], type: 'upload', status: 'pending' } as TransferItem];
            return id;
        }) as never);
        const backend = f.invoke.getMockImplementation()!;
        const beforePrivacy: string[][] = [];
        let batchId = '';
        f.invoke.mockImplementation(async (command, args) => {
            if (command === 'provider_upload_files_batch') {
                const { batch_id, entries } = args!.params as { batch_id: string; entries: { display_name: string; remote_path: string }[] };
                batchId = batch_id;
                for (const [index, entry] of entries.entries()) {
                    const event = { transfer_id: `${batch_id}-${index}`, filename: entry.display_name, path: entry.remote_path, direction: 'upload' };
                    await emit({ ...event, event_type: 'file_start' });
                    await emit({ ...event, event_type: 'file_complete' });
                }
                return backend(command, args);
            }
            if (!command.endsWith('_set_path_privacy')) return backend(command, args);
            beforePrivacy.push(items.map(item => item.status));
            if (String(args?.path).endsWith('/b.bin')) throw new Error('privacy refused');
            return undefined;
        });
        const sources = ['a.bin', 'b.bin'].map(name => ({ name, sourcePath: `/source/${name}`, size: 100, modified: null }));
        await f.runtime.runFileBatch('upload', sources, '/actual');
        // file_complete arrived for both rows, and neither was settled by it.
        expect(beforePrivacy).toEqual([['transferring', 'transferring'], ['transferring', 'transferring']]);
        expect(items.map(item => [item.filename, item.status, item.error])).toEqual([
            ['a.bin', 'completed', undefined],
            ['b.bin', 'error', 'activity.set_private_error'],
        ]);
        // The hold is released. A late completion of the SAME file of the
        // batch, its id registered again by a file_start: the settled (failed)
        // row is not a live row, so the late events cannot touch it, and the
        // failure stays a failure.
        const late = { transfer_id: `${batchId}-1`, filename: 'b.bin', path: '/actual/b.bin', direction: 'upload' };
        expect(batchId).not.toBe('');
        await emit({ ...late, event_type: 'file_complete' });
        expect(items[1].status).toBe('error');
        await emit({ ...late, event_type: 'file_start' });
        await emit({ ...late, event_type: 'file_complete' });
        expect(items[1].status).toBe('error');
        expect(items[1].error).toBe('activity.set_private_error');
        expect(items[0].status).toBe('completed');
    });
});
