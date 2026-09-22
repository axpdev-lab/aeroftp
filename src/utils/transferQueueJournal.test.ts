// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import type { TransferItem } from '../components/TransferQueue';
import {
    isPermanentTransferError,
    isRestorableJournalEntry,
    buildJournalEntries,
    displayPathForRestore,
    groupIdsByProfileId,
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

describe('groupIdsByProfileId', () => {
    it('groups by profile_id and keeps first-seen order', () => {
        const descriptors = new Map([
            ['a', { profile_id: 'p1' }],
            ['b', { profile_id: null }],
            ['c', { profile_id: 'p1' }],
            ['d', { profile_id: 'p2' }],
            ['e', { profile_id: '' }],
        ]);
        expect(groupIdsByProfileId(['a', 'b', 'c', 'd', 'e'], descriptors)).toEqual([
            { profileId: 'p1', ids: ['a', 'c'] },
            { profileId: null, ids: ['b', 'e'] },
            { profileId: 'p2', ids: ['d'] },
        ]);
    });

    it('treats missing descriptors as null profile', () => {
        expect(groupIdsByProfileId(['x'], new Map())).toEqual([
            { profileId: null, ids: ['x'] },
        ]);
    });
});

describe('isRestorableJournalEntry', () => {
    // The upload of 2026-09-22 into the Proton Drive root, as the queue stored it.
    const refused = 'Upload failed: Permission denied: The Proton Drive root only holds the account sections';

    it('keeps out a failure the next attempt cannot fix', () => {
        expect(isRestorableJournalEntry({ status: 'failed', last_error: refused })).toBe(false);
        for (const e of [
            'Upload failed: Read-only endpoint: share is view-only',
            'File too large: 100 MB limit',
            'Invalid path: name ends with a dot',
            'Operation not supported: download',
            'Restricted character : is not allowed by OneDrive',
        ]) {
            expect(isRestorableJournalEntry({ status: 'failed', last_error: e })).toBe(false);
        }
    });

    it('brings back a failure a later attempt can fix, and every pending transfer', () => {
        for (const e of [
            'Transfer failed: Connection lost: broken pipe',
            'Timeout',
            'Authentication failed: token expired',
            'Path not found: /a/b',
            'Path already exists: /a/b.txt',
            'IO error: Permission denied (os error 13)',
        ]) {
            expect(isRestorableJournalEntry({ status: 'failed', last_error: e })).toBe(true);
        }
        expect(isRestorableJournalEntry({ status: 'pending' })).toBe(true);
        expect(isRestorableJournalEntry({ status: 'in_progress', last_error: refused })).toBe(true);
    });

    it('never brings back completed or cancelled entries', () => {
        expect(isRestorableJournalEntry({ status: 'completed' })).toBe(false);
        expect(isRestorableJournalEntry({ status: 'cancelled' })).toBe(false);
    });

    it('matches the Display prefix, not a lowercase mention inside a server message', () => {
        expect(isPermanentTransferError('Server error: upstream said permission denied: retry later')).toBe(false);
        expect(isPermanentTransferError('Server error: upstream said Permission denied: retry later')).toBe(false);
        expect(isPermanentTransferError('Transfer failed: Upload failed: Permission denied: x')).toBe(true);
        expect(isPermanentTransferError(undefined)).toBe(false);
    });
});
