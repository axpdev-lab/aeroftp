// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
import { act, createElement } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import english from '../../i18n/locales/en.json';
import { SyncTabContent } from './SyncTabContent';
import { JournalHistoryDialog } from './JournalHistoryDialog';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock('../../utils/pickPath', () => ({ pickFile: vi.fn() }));
vi.mock('../../hooks/useDraggableModal', () => ({
    useDraggableModal: () => ({ panelProps: {}, dragHandleProps: {} }),
}));
vi.mock('../../i18n', () => ({
    useTranslation: () => (key: string) => key.split('.').reduce<unknown>(
        (value, part) => (value as Record<string, unknown>)?.[part], english.translations,
    ),
}));

let root: Root;
let host: HTMLDivElement;
const click = async (selector: string) => {
    const target = host.querySelector<HTMLElement>(selector);
    expect(target, selector).not.toBeNull();
    await act(async () => target!.click());
};
const button = async (label: string) => {
    const target = [...host.querySelectorAll('button')].find(b => b.textContent?.trim() === label);
    expect(target, label).toBeDefined();
    await act(async () => target!.click());
};

beforeEach(() => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    vi.mocked(invoke).mockReset();
    localStorage.clear();
    host = document.createElement('div');
    document.body.append(host);
    root = createRoot(host);
});
afterEach(async () => { await act(async () => root.unmount()); host.remove(); });

it.each([
    ['ok', true, 'Dry run completed'],
    ['partial', true, 'Dry run completed with errors'],
    ['cancelled', true, 'Dry run cancelled'],
    ['ok', false, 'Sync completed'],
    ['partial', false, 'Sync completed with errors'],
    ['cancelled', false, 'Sync cancelled'],
] as const)('reports %s with dry-run=%s using the executed request', async (status, dryRun, heading) => {
    vi.mocked(invoke).mockResolvedValue({
        status, uploaded: 6, skipped: 0, errors: status === 'partial' ? 1 : 0,
        elapsed_ms: 1, total_payload_bytes: 0, bytes_on_wire: 0,
        savings_ratio: 1, error_messages: [], entries: [],
    });
    await act(async () => root.render(createElement(SyncTabContent, {
        initialSource: '/source', initialDestination: '/destination',
    })));
    await button('Advanced');
    if (dryRun) await click('input[type=checkbox]');
    await button('Start sync');
    expect(invoke).toHaveBeenCalledWith('local_sync_run', expect.objectContaining({
        request: expect.objectContaining({ dry_run: dryRun }),
    }));
    const assertResult = () => {
        expect(host.querySelector('span.font-semibold.text-sm')?.textContent).toBe(heading);
        const countLabel = host.querySelector('dt');
        expect(countLabel?.textContent).toBe(dryRun ? 'Planned copies' : 'Copied');
        expect(countLabel?.nextElementSibling?.textContent).toBe('6');
        expect(host.textContent).not.toContain('Uploaded');
        expect(host.textContent?.includes('Dry run preview only.')).toBe(dryRun);
    };
    assertResult();
    // Changing the next run's mode must not rewrite a previous result,
    // including its empty per-file report.
    await click('input[type=checkbox]');
    assertResult();
});

it('explains retained journals even when a successful run left the list empty', async () => {
    vi.mocked(invoke).mockResolvedValue([]);
    await act(async () => root.render(createElement(JournalHistoryDialog, { isOpen: true, onClose: vi.fn() })));
    expect(invoke).toHaveBeenCalledWith('list_sync_journals_cmd');
    expect(host.textContent).toContain('Successful runs remove their journal');
    expect(host.textContent).toContain('not a history of completed runs');
    expect(host.textContent).toContain('No sync journals to show.');
    expect(host.textContent).not.toContain('first preset execution');
});

it('does not infer that an incomplete journal is still running', async () => {
    vi.mocked(invoke).mockResolvedValue([false, true].map((completed, i) => ({
        local_path: `/source${i}`, remote_path: '/destination',
        updated_at: '2026-10-16T12:00:00Z', completed,
        total_entries: 6, completed_entries: completed ? 6 : 2,
    })));
    await act(async () => root.render(createElement(JournalHistoryDialog, { isOpen: true, onClose: vi.fn() })));
    expect(host.textContent).toContain('INCOMPLETE');
    expect(host.textContent).toContain('COMPLETED');
    expect(host.textContent).not.toContain('IN PROGRESS');
    expect(host.textContent).toContain('Successful runs remove their journal');
});
