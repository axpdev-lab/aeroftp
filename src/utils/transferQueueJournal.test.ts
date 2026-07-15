// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import type { TransferItem } from '../components/TransferQueue';
import {
    buildJournalEntries,
    displayPathForRestore,
    isCancelledTransferError,
    isRestorableJournalStatus,
    joinRemotePath,
    mapUiStatusToJournalStatus,
    parentDir,
    type JournalDescriptorFields,
} from './transferQueueJournal';

const desc = (
    partial?: Partial<JournalDescriptorFields>,
): JournalDescriptorFields => ({
    direction: 'download',
    local_path: '/home/u/a.txt',
    remote_path: '/remote/a.txt',
    profile_id: 'srv_1',
    filename: 'a.txt',
    size: 42,
    is_folder: false,
    ...partial,
});

const item = (
    id: string,
    status: TransferItem['status'],
    extra?: Partial<TransferItem>,
): TransferItem => ({
    id,
    filename: extra?.filename ?? `${id}.txt`,
    path: extra?.path ?? `/${id}.txt`,
    size: extra?.size ?? 100,
    type: extra?.type ?? 'download',
    status,
    error: extra?.error,
    restored: extra?.restored,
});

describe('mapUiStatusToJournalStatus', () => {
    it('maps staged and pending to pending', () => {
        expect(mapUiStatusToJournalStatus('staged')).toBe('pending');
        expect(mapUiStatusToJournalStatus('pending')).toBe('pending');
    });

    it('maps transferring to in_progress', () => {
        expect(mapUiStatusToJournalStatus('transferring')).toBe('in_progress');
    });

    it('maps completed to completed', () => {
        expect(mapUiStatusToJournalStatus('completed')).toBe('completed');
    });

    it('maps cancelled-looking errors to cancelled', () => {
        expect(mapUiStatusToJournalStatus('error', 'Stopped by user')).toBe('cancelled');
        expect(mapUiStatusToJournalStatus('error', 'Transfer cancelled')).toBe('cancelled');
        expect(mapUiStatusToJournalStatus('error', 'Cancelled by user')).toBe('cancelled');
    });

    it('maps other errors to failed', () => {
        expect(mapUiStatusToJournalStatus('error', 'Permission denied')).toBe('failed');
        expect(mapUiStatusToJournalStatus('error')).toBe('failed');
    });
});

describe('isCancelledTransferError', () => {
    it('detects stop/cancel phrases case-insensitively', () => {
        expect(isCancelledTransferError('Stopped by user')).toBe(true);
        expect(isCancelledTransferError('CANCELLED')).toBe(true);
        expect(isCancelledTransferError('network timeout')).toBe(false);
        expect(isCancelledTransferError(undefined)).toBe(false);
    });
});

describe('isRestorableJournalStatus', () => {
    it('excludes completed and cancelled only', () => {
        expect(isRestorableJournalStatus('pending')).toBe(true);
        expect(isRestorableJournalStatus('in_progress')).toBe(true);
        expect(isRestorableJournalStatus('failed')).toBe(true);
        expect(isRestorableJournalStatus('completed')).toBe(false);
        expect(isRestorableJournalStatus('cancelled')).toBe(false);
    });
});

describe('buildJournalEntries', () => {
    it('joins items with descriptors and maps status', () => {
        const descriptors = new Map<string, JournalDescriptorFields>([
            ['t1', desc({ direction: 'upload', local_path: '/L/a', remote_path: '/R/a', filename: 'a' })],
            ['t2', desc({ direction: 'download', filename: 'b.txt' })],
        ]);
        const items = [
            item('t1', 'transferring', { type: 'upload', filename: 'a' }),
            item('t2', 'error', { error: 'Permission denied', filename: 'b.txt' }),
        ];
        const entries = buildJournalEntries(items, descriptors);
        expect(entries).toHaveLength(2);
        expect(entries[0]).toMatchObject({
            id: 't1',
            direction: 'upload',
            local_path: '/L/a',
            remote_path: '/R/a',
            status: 'in_progress',
            attempts: 0,
        });
        expect(entries[1]).toMatchObject({
            id: 't2',
            status: 'failed',
            last_error: 'Permission denied',
        });
    });

    it('skips items without a descriptor', () => {
        const descriptors = new Map<string, JournalDescriptorFields>();
        const entries = buildJournalEntries([item('orphan', 'pending')], descriptors);
        expect(entries).toHaveLength(0);
    });

    it('prunes missing descriptor keys when requested', () => {
        const descriptors = new Map<string, JournalDescriptorFields>([
            ['keep', desc()],
            ['gone', desc({ filename: 'gone.txt' })],
        ]);
        buildJournalEntries([item('keep', 'pending')], descriptors, { pruneMissing: true });
        expect(descriptors.has('keep')).toBe(true);
        expect(descriptors.has('gone')).toBe(false);
    });

    it('maps cancelled stop message on error items', () => {
        const descriptors = new Map([['c1', desc()]]);
        const entries = buildJournalEntries(
            [item('c1', 'error', { error: 'Stopped by user' })],
            descriptors,
        );
        expect(entries[0].status).toBe('cancelled');
        expect(entries[0].last_error).toBe('Stopped by user');
    });
});

describe('parentDir', () => {
    it('handles unix and windows separators', () => {
        expect(parentDir('/remote/dir/file.txt')).toBe('/remote/dir');
        expect(parentDir('C:\\Users\\a\\file.txt')).toBe('C:/Users/a');
        expect(parentDir('/file.txt')).toBe('/');
        expect(parentDir('file.txt')).toBe('');
        expect(parentDir('/remote/dir/')).toBe('/remote');
    });
});

describe('joinRemotePath', () => {
    it('joins without double slashes', () => {
        expect(joinRemotePath('/remote/dir', 'a.txt')).toBe('/remote/dir/a.txt');
        expect(joinRemotePath('/remote/dir/', 'a.txt')).toBe('/remote/dir/a.txt');
        expect(joinRemotePath('/', 'a.txt')).toBe('/a.txt');
    });
});

describe('displayPathForRestore', () => {
    it('picks local for upload and remote for download', () => {
        expect(displayPathForRestore('upload', '/L/x', '/R/x')).toBe('/L/x');
        expect(displayPathForRestore('download', '/L/x', '/R/x')).toBe('/R/x');
    });
});
