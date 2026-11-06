// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
//
// Row 140: a row the user stopped is known by its `cancelled` marker, never by
// its label. The label is translated, so the old text checks ("cancelled",
// "Stopped by user", "cancel") missed it in Italian, German, French and more:
// "Retry all failed" re-ran stopped rows and the journal saved them as failed,
// so they came back after a restart. Checked here with every locale's label.

import { describe, expect, it } from 'vitest';
import type { TransferItem } from './TransferQueue';
import { cancelTransfer, retryAllFailed, stopItems, updateTransferStatus } from './transferQueueActions';
import {
    buildJournalEntries,
    isRestorableJournalEntry,
    type JournalDescriptorFields,
} from '../utils/transferQueueJournal';
import { translationsInEveryLanguage } from '../i18n/I18nContext';

const LOCALES = import.meta.glob<{ translations: { transfer: { cancelledByUser: string } } }>(
    '../i18n/locales/*.json',
    { eager: true, import: 'default' },
);

const LABELS = Object.entries(LOCALES).map(([path, locale]) => ({
    language: path.replace(/^.*\/(.+)\.json$/, '$1'),
    label: locale.translations.transfer.cancelledByUser,
}));

const row = (id: string): TransferItem => ({
    id,
    filename: `${id}.bin`,
    path: `/${id}.bin`,
    size: 1,
    type: 'upload',
    status: 'transferring',
});

const descriptor = (filename: string): JournalDescriptorFields => ({
    direction: 'upload',
    local_path: `/local/${filename}`,
    remote_path: `/remote/${filename}`,
    profile_id: 'p1',
    filename,
    size: 1,
    is_folder: false,
});

describe('a stopped row, in every language', () => {
    it('covers every locale file', () => {
        expect(LABELS.length).toBe(47);
        for (const { language, label } of LABELS) {
            expect(label, language).toBeTruthy();
        }
    });

    it.each(LABELS)('$language: Retry all failed leaves the stopped row alone', ({ label }) => {
        let items = [row('stopped'), row('failed')];
        items = cancelTransfer(items, 'stopped', label);
        items = updateTransferStatus(items, 'failed', 'error', undefined, 'Connection reset');
        const { next, ids } = retryAllFailed(items);
        expect(ids).toEqual(['failed']);
        expect(next.find(item => item.id === 'stopped')).toMatchObject({ status: 'error', error: label, cancelled: true });
    });

    it.each(LABELS)('$language: the journal saves the stopped row as cancelled', ({ label }) => {
        const items = cancelTransfer([row('stopped')], 'stopped', label);
        const [entry] = buildJournalEntries(items, new Map([['stopped', descriptor('stopped.bin')]]));
        expect(entry.status).toBe('cancelled');
        expect(isRestorableJournalEntry(entry)).toBe(false);
    });

    it.each(LABELS)('$language: an entry an older build saved as failed with the label stays out', ({ label }) => {
        const legacy = new Set(translationsInEveryLanguage('transfer.cancelledByUser'));
        expect(isRestorableJournalEntry({ status: 'failed', last_error: label }, legacy)).toBe(false);
        // A real failure of that older build still comes back.
        expect(isRestorableJournalEntry({ status: 'failed', last_error: 'Connection reset' }, legacy)).toBe(true);
    });
});

describe('Stop and Stop all', () => {
    it('mark the rows they stop, and Retry all failed skips them', () => {
        const pending = { ...row('p'), status: 'pending' as const };
        const done = { ...row('d'), status: 'completed' as const };
        const stopped = stopItems([pending, row('t'), done], ['pending', 'transferring'], 'Stopped by user');
        expect(stopped.map(item => item.cancelled)).toEqual([true, true, undefined]);
        expect(retryAllFailed(stopped).ids).toEqual([]);
    });

    it('a row that already settled keeps its outcome', () => {
        const failed = updateTransferStatus([row('x')], 'x', 'error', undefined, 'Disk full');
        expect(cancelTransfer(failed, 'x', 'Annullato dall\'utente')[0]).toMatchObject({ error: 'Disk full' });
        expect(cancelTransfer(failed, 'x', 'Annullato dall\'utente')[0].cancelled).toBeUndefined();
    });
});
