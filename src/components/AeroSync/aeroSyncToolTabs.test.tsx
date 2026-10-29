// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
import { act, createElement, useContext } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { pickFile } from '../../utils/pickPath';
import { readTextFile } from '@tauri-apps/plugin-fs';
import { TabStateStoreContext } from './tabStateStore';
import english from '../../i18n/locales/en.json';
import { AeroSyncDialog } from './AeroSyncDialog';
import { MultiPathEditor } from '../Sync/MultiPathEditor';
import type { AeroSyncContext, AeroSyncDialogProps } from './types';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('../../utils/pickPath', () => ({ pickFile: vi.fn(), pickSave: vi.fn() }));
vi.mock('@tauri-apps/plugin-fs', () => ({ readTextFile: vi.fn(), writeTextFile: vi.fn() }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock('../../hooks/useDraggableModal', () => ({
    useDraggableModal: () => ({ panelProps: {}, dragHandleProps: {} }),
}));
vi.mock('../../i18n', () => ({
    useTranslation: () => (key: string) => key.split('.').reduce<unknown>(
        (value, part) => (value as Record<string, unknown>)?.[part], english.translations,
    ),
}));
vi.mock('./CompareTabContent', () => ({ CompareTabContent: () => createElement('p', null, 'Compare content') }));
vi.mock('./PlanTabContent', () => ({ PlanTabContent: ({ canExecute, onExecute }: { canExecute: boolean; onExecute: () => void }) => {
    const store = useContext(TabStateStoreContext);
    return createElement('div', null, createElement('button', { disabled: !canExecute, onClick: onExecute }, 'Execute plan'), createElement('output', { 'data-testid': 'plan-settings' }, JSON.stringify({
        preset: store?.get('plan.preset', null), direction: store?.get('plan.direction', null),
        source: store?.get('sync.source', null), destination: store?.get('sync.destination', null),
    })));
} }));
vi.mock('./SyncTabContent', () => ({
    SyncTabContent: ({ onRunningChange }: { onRunningChange: (running: boolean) => void }) =>
        createElement('button', { onClick: () => onRunningChange(true) }, 'Start local run'),
}));

let root: Root;
let host: HTMLDivElement;
let props: AeroSyncDialogProps;
const context: AeroSyncContext = {
    pairKind: 'local-remote', leftLabel: 'Local', rightLabel: 'Server', compareResult: null,
    initialSource: '/local', initialDestination: '/remote', activeProfileId: 'server-a', activeProfileName: 'Server A',
};
const render = async (overrides: Partial<AeroSyncDialogProps> = {}) => {
    props = { ...props, ...overrides };
    await act(async () => root.render(createElement(AeroSyncDialog, props)));
};
const tab = (label: string) => {
    const button = [...host.querySelectorAll<HTMLButtonElement>('[role=tab]')].find(b => b.textContent?.trim() === label);
    expect(button, label).toBeDefined();
    return button!;
};
const choose = async (label: string) => { await act(async () => tab(label).click()); };
const panel = () => host.querySelector<HTMLElement>('[role=tabpanel]')!;
const visibleButtons = () => [...host.querySelectorAll<HTMLButtonElement>('button')].filter(b => !b.closest('[hidden]') && !b.hidden);

beforeEach(() => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    vi.mocked(invoke).mockReset().mockImplementation(async command => {
        if (command === 'get_multi_path_config') return {
            pairs: [{ id: 'pair', name: 'Original', local_path: '/local', remote_path: '/remote', enabled: true, exclude_overrides: [] }],
            parallel_pairs: false,
        };
        if (command === 'get_sync_schedule_cmd') return { enabled: false, interval_secs: 86400, time_window: null, paused: false, last_sync: null };
        return [];
    });
    host = document.createElement('div'); document.body.append(host); root = createRoot(host);
    props = { isOpen: true, context, onClose: vi.fn(), onApplyMirrorLeftToRight: vi.fn(), onApplyMirrorRightToLeft: vi.fn(),
        onExecutePreset: vi.fn(), onResumeJournal: vi.fn(), onDismissJournal: vi.fn(), onRescan: vi.fn() };
});
afterEach(async () => { await act(async () => root.unmount()); host.remove(); });

it('places all five named tools after Compare/Plan with one divider and mounts them without nested modals', async () => {
    await render();
    expect([...host.querySelectorAll('[role=tab]')].map(t => t.textContent?.trim())).toEqual([
        'Compare', 'Plan', 'Templates', 'Multi-Path', 'Rollback snapshots', 'Sync journals', 'Scheduler',
    ]);
    expect(host.querySelectorAll('[role=separator]')).toHaveLength(1);
    expect(invoke).not.toHaveBeenCalled(); // Tools load only when first visited.
    for (const label of ['Templates', 'Multi-Path', 'Rollback snapshots', 'Sync journals', 'Scheduler']) {
        await choose(label);
        expect(tab(label).getAttribute('aria-selected')).toBe('true');
        expect(tab(label).getAttribute('aria-controls')).toBe(panel().id);
        expect(panel().getAttribute('aria-labelledby')).toBe(tab(label).id);
        expect(host.querySelectorAll('[role=dialog]')).toHaveLength(1);
        expect(panel().querySelector('.fixed')).toBeNull();
    }
    expect(invoke).toHaveBeenCalledWith('get_multi_path_config', { localPath: '/local', remotePath: '/remote' });
    expect(invoke).toHaveBeenCalledWith('get_sync_schedule_cmd');
    // Embedded panes do not register the old child-dialog Escape callbacks.
    await act(async () => window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' })));
    expect(tab('Scheduler').getAttribute('aria-selected')).toBe('true');
    expect(props.onClose).not.toHaveBeenCalled();
});

it('preserves an unsaved Multi-Path draft across tabs, and saves the displayed pair', async () => {
    await render(); await choose('Multi-Path');
    const input = panel().querySelector<HTMLInputElement>('input')!;
    await act(async () => {
        Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(input, 'Draft name');
        input.dispatchEvent(new Event('input', { bubbles: true }));
    });
    await choose('Plan'); await choose('Multi-Path');
    expect(panel().querySelector<HTMLInputElement>('input')!.value).toBe('Draft name');
    expect(vi.mocked(invoke).mock.calls.filter(([cmd]) => cmd === 'get_multi_path_config')).toHaveLength(1);
    const save = visibleButtons().find(b => b.textContent?.trim() === 'Save')!;
    await act(async () => save.click());
    expect(invoke).toHaveBeenCalledWith('save_multi_path_config_cmd', expect.objectContaining({
        localPath: '/local', remotePath: '/remote', config: expect.objectContaining({ pairs: [expect.objectContaining({ name: 'Draft name' })] }),
    }));
    expect(tab('Plan').getAttribute('aria-selected')).toBe('true');
    expect(document.activeElement).toBe(tab('Plan'));
});

it('clears visited panes after closing and resets their data on a profile change', async () => {
    await render(); await choose('Multi-Path');
    await render({ context: { ...context, activeProfileId: 'server-b' } });
    expect(vi.mocked(invoke).mock.calls.filter(([cmd]) => cmd === 'get_multi_path_config')).toHaveLength(2);
    await render({ isOpen: false }); await render({ isOpen: true });
    expect(tab('Compare').getAttribute('aria-selected')).toBe('true');
    expect(panel().querySelector('input')).toBeNull();
});

it('retains drafts when opening directly on a tool tab', async () => {
    await render({ initialTab: 'templates' });
    const input = panel().querySelector<HTMLInputElement>('input')!;
    await act(async () => {
        Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(input, 'My export');
        input.dispatchEvent(new Event('input', { bubbles: true }));
    });
    await choose('Plan'); await choose('Templates');
    expect(panel().querySelector<HTMLInputElement>('input')!.value).toBe('My export');
});

it('supports arrow/Home/End navigation and keeps a single tab in the Tab order', async () => {
    await render();
    tab('Compare').focus();
    const key = async (key: string) => {
        await act(async () => document.activeElement!.dispatchEvent(new KeyboardEvent('keydown', { key, bubbles: true })));
    };
    await key('End'); expect(document.activeElement).toBe(tab('Scheduler'));
    await key('ArrowRight'); expect(document.activeElement).toBe(tab('Compare'));
    await key('ArrowLeft'); expect(document.activeElement).toBe(tab('Scheduler'));
    await key('Home'); expect(document.activeElement).toBe(tab('Compare'));
    expect([...host.querySelectorAll('[role=tab]')].filter(t => t.getAttribute('tabindex') === '0')).toHaveLength(1);
});

it.each(['local-local', null] as const)('keeps remote tools unavailable for %s pairs', async pairKind => {
    await render({ context: { ...context, pairKind }, initialTab: 'templates' });
    expect(host.querySelectorAll('[role=tab]')).toHaveLength(3);
    expect(tab('Compare').getAttribute('aria-selected')).toBe('true');
    expect(host.querySelector('[role=separator]')).toBeNull();
});

it('falls back to Compare on disconnect and preserves the running-local-sync lock', async () => {
    await render(); await choose('Scheduler');
    await render({ context: { ...context, activeProfileId: undefined } });
    expect(host.querySelectorAll('[role=tab]')).toHaveLength(2);
    expect(tab('Compare').getAttribute('aria-selected')).toBe('true');
    await render({ context: { ...context, pairKind: 'local-local' } });
    await choose('Local mirror');
    await act(async () => visibleButtons().find(b => b.textContent === 'Start local run')!.click());
    await render({ context });
    expect(tab('Local mirror').getAttribute('aria-selected')).toBe('true');
    expect(tab('Scheduler').disabled).toBe(true);
    await choose('Scheduler');
    expect(tab('Local mirror').getAttribute('aria-selected')).toBe('true');
});

it('retains standalone dialog behavior for other callers', async () => {
    const onClose = vi.fn();
    await act(async () => root.render(createElement(MultiPathEditor, { isOpen: true, onClose, localPath: '/local', remotePath: '/remote' })));
    expect(host.querySelector('[role=dialog][aria-modal=true]')).not.toBeNull();
    await act(async () => window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' })));
    expect(onClose).toHaveBeenCalledOnce();
});


it('applies a template from its embedded pane to the shared Plan store', async () => {
    const template = {
        schema_version: 1, name: 'Pull', description: '', created_by: 'test',
        path_patterns: [{ local: '/new-local', remote: '/new-remote' }],
        profile: { direction: 'remote_to_local', compare_timestamp: false, compare_size: true,
            compare_checksum: false, delete_orphans: false, parallel_streams: 1, compression_mode: 'auto' },
        exclude_patterns: [], schedule: null,
    };
    vi.mocked(pickFile).mockResolvedValue('/fixture.aerosync');
    vi.mocked(readTextFile).mockResolvedValue(JSON.stringify(template));
    vi.mocked(invoke).mockImplementation(async cmd => cmd === 'import_sync_template_cmd' ? template : []);
    await render(); await choose('Templates');
    const clickImport = async () => {
        const buttons = visibleButtons().filter(b => b.textContent?.trim() === 'Import Template');
        await act(async () => buttons[buttons.length - 1].click());
    };
    await clickImport(); // Select import mode.
    await clickImport(); // Select and parse the fixture.
    await act(async () => visibleButtons().find(b => b.textContent?.trim() === 'Apply')!.click());
    expect(tab('Plan').getAttribute('aria-selected')).toBe('true');
    expect(document.activeElement).toBe(tab('Plan'));
    expect(JSON.parse(panel().querySelector('output')!.textContent!)).toEqual({
        preset: 'backup', direction: 'right-to-left', source: '/new-local', destination: '/new-remote',
    });
    expect(props.onExecutePreset).not.toHaveBeenCalled();
});


const editFirstPairName = async (name: string) => {
    const input = panel().querySelector<HTMLInputElement>('input')!;
    await act(async () => {
        Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(input, name);
        input.dispatchEvent(new Event('input', { bubbles: true }));
    });
};
const clickVisible = async (name: string) => {
    const button = visibleButtons().find(b => b.textContent?.trim() === name);
    expect(button, name).toBeDefined();
    await act(async () => button!.click());
};

it('Cancel discards a Multi-Path draft without re-reading or saving it later', async () => {
    await render(); await choose('Multi-Path'); await editFirstPairName('Cancelled draft');
    await clickVisible('Cancel'); await choose('Multi-Path');
    expect(panel().querySelector<HTMLInputElement>('input')!.value).toBe('Original');
    expect(vi.mocked(invoke).mock.calls.filter(([cmd]) => cmd === 'get_multi_path_config')).toHaveLength(1);
    await clickVisible('Save');
    expect(invoke).toHaveBeenLastCalledWith('save_multi_path_config_cmd', expect.objectContaining({
        config: expect.objectContaining({ pairs: [expect.objectContaining({ name: 'Original' })] }),
    }));
});

it('Cancel restores the last successful save rather than the initial list', async () => {
    await render(); await choose('Multi-Path'); await editFirstPairName('Saved');
    await clickVisible('Save'); await choose('Multi-Path'); await editFirstPairName('Not saved');
    await clickVisible('Cancel'); await choose('Multi-Path');
    expect(panel().querySelector<HTMLInputElement>('input')!.value).toBe('Saved');
});

it('a failed Multi-Path save does not become the Cancel baseline', async () => {
    await render(); await choose('Multi-Path'); await editFirstPairName('Rejected');
    vi.mocked(invoke).mockRejectedValueOnce(new Error('fixture save refusal'));
    await clickVisible('Save'); await clickVisible('Cancel'); await choose('Multi-Path');
    expect(panel().querySelector<HTMLInputElement>('input')!.value).toBe('Original');
});

it('reloads History when it becomes active again after the backend journal list changes', async () => {
    const journal = { local_path: '/local', remote_path: '/remote', updated_at: '2026-10-29T19:00:00Z',
        completed: false, total_entries: 1, completed_entries: 0 };
    vi.mocked(invoke).mockResolvedValue([journal]);
    await render(); await choose('Sync journals');
    expect(panel().textContent).toContain('INCOMPLETE');
    await choose('Plan'); vi.mocked(invoke).mockResolvedValue([]); await choose('Sync journals');
    expect(panel().textContent).toContain('No sync journals to show.');
    expect(panel().textContent).not.toContain('INCOMPLETE');
});

it.each(['success', 'failure'])('blocks Plan during restore and unlocks on %s', async outcome => {
    let resolveRestore!: (result: unknown) => void;
    let rejectRestore!: (error: Error) => void;
    const restoring = new Promise((resolve, reject) => { resolveRestore = resolve; rejectRestore = reject; });
    vi.mocked(invoke).mockImplementation(async command => {
        if (command === 'list_sync_snapshots_cmd') return [{ id: 'snapshot-a', name: 'Snapshot A',
            created_at: '2026-10-29T19:00:00Z', files: {} }];
        if (command === 'restore_sync_snapshot_cmd') return restoring;
        return [];
    });
    await render(); await choose('Rollback snapshots');
    await act(async () => panel().querySelector<HTMLElement>('[class*=cursor-pointer]')!.click());
    await clickVisible('Restore');
    // Confirm restoration in the pane's confirmation overlay.
    const restoreButtons = visibleButtons().filter(b => b.textContent?.trim() === 'Restore');
    await act(async () => restoreButtons[restoreButtons.length - 1].click());
    expect(invoke).toHaveBeenCalledWith('restore_sync_snapshot_cmd', expect.anything());
    await choose('Plan');
    const execute = visibleButtons().find(b => b.textContent === 'Execute plan')!;
    expect(execute.disabled).toBe(true);
    await act(async () => execute.click()); expect(props.onExecutePreset).not.toHaveBeenCalled();
    await act(async () => {
        if (outcome === 'failure') rejectRestore(new Error('fixture restore failure'));
        else resolveRestore({ restored_from_remote: 1, restored_to_remote: 0, skipped: 0, failed: [] });
    });
    expect(execute.disabled).toBe(false);
});


it('refreshes an already-active History pane only after journal dismissal settles', async () => {
    const journal = { local_path: '/local', remote_path: '/remote', updated_at: '2026-10-29T19:00:00Z',
        completed: false, total_entries: 1, completed_entries: 0 };
    let finishDismiss!: () => void;
    const dismissed = new Promise<void>(resolve => { finishDismiss = resolve; });
    vi.mocked(invoke).mockResolvedValue([journal]);
    await render({ context: { ...context, pendingJournal: { ...journal, id: 'journal-a', created_at: journal.updated_at, entries: [],
            direction: 'local_to_remote', verify_policy: 'none',
            retry_policy: { max_retries: 0, base_delay_ms: 1, max_delay_ms: 1, timeout_ms: 1, backoff_multiplier: 1 } } },
        onDismissJournal: () => dismissed });
    await choose('Sync journals'); await clickVisible('Dismiss');
    expect(vi.mocked(invoke).mock.calls.filter(([cmd]) => cmd === 'list_sync_journals_cmd')).toHaveLength(1);
    vi.mocked(invoke).mockResolvedValue([]);
    await act(async () => finishDismiss());
    expect(vi.mocked(invoke).mock.calls.filter(([cmd]) => cmd === 'list_sync_journals_cmd')).toHaveLength(2);
    expect(panel().textContent).toContain('No sync journals to show.');
});


it('Cancel keeps an immediately persisted added pair but discards edits to an existing pair', async () => {
    await render(); await choose('Multi-Path'); await editFirstPairName('Unsaved');
    await clickVisible('Add Path Pair'); await clickVisible('Cancel'); await choose('Multi-Path');
    const names = panel().querySelectorAll<HTMLInputElement>('input[spellcheck=false]');
    expect(names[0].value).toBe('Original');
    expect(names[3].value).toBe('Path 2');
    expect(invoke).toHaveBeenCalledWith('add_path_pair', expect.anything());
});

it('Cancel uses the list confirmed by a successful toggle, not a later unsaved edit', async () => {
    await render(); await choose('Multi-Path');
    await act(async () => panel().querySelector<HTMLButtonElement>('button[title="Disable"]')!.click());
    await editFirstPairName('Unsaved'); await clickVisible('Cancel'); await choose('Multi-Path');
    expect(panel().querySelector<HTMLInputElement>('input')!.value).toBe('Original');
    expect(panel().querySelector('button[title="Enable"]')).not.toBeNull();
});

it('serializes backend writes and keeps Cancel disabled until the accepted write settles', async () => {
    let finishSave!: () => void;
    const saving = new Promise<void>(resolve => { finishSave = resolve; });
    await render(); await choose('Multi-Path'); await editFirstPairName('Saved in flight');
    vi.mocked(invoke).mockImplementation(async command => command === 'save_multi_path_config_cmd' ? saving : []);
    await clickVisible('Save');
    expect(visibleButtons().find(b => b.textContent?.trim() === 'Cancel')!.matches(':disabled')).toBe(true);
    await clickVisible('Add Path Pair');
    expect(vi.mocked(invoke).mock.calls.filter(([cmd]) => cmd === 'add_path_pair')).toHaveLength(0);
    await act(async () => finishSave()); await choose('Multi-Path');
    await editFirstPairName('Later draft'); await clickVisible('Cancel'); await choose('Multi-Path');
    expect(panel().querySelector<HTMLInputElement>('input')!.value).toBe('Saved in flight');
});

it('ignores a stale History listing that finishes after a newer activation reload', async () => {
    let finishOld!: (journals: unknown[]) => void;
    const oldListing = new Promise<unknown[]>(resolve => { finishOld = resolve; });
    vi.mocked(invoke).mockResolvedValueOnce(oldListing);
    await render(); await choose('Sync journals'); await choose('Plan');
    vi.mocked(invoke).mockResolvedValue([]); await choose('Sync journals');
    await act(async () => finishOld([{ local_path: '/local', remote_path: '/remote', completed: false,
        updated_at: '2026-10-29T19:00:00Z', total_entries: 1, completed_entries: 0 }]));
    expect(panel().textContent).toContain('No sync journals to show.');
    expect(panel().textContent).not.toContain('INCOMPLETE');
});


it('retains the saved Cancel baseline when a later read of the same pair fails', async () => {
    const editorProps = { isOpen: true, onClose: vi.fn(), localPath: '/local', remotePath: '/remote' };
    await act(async () => root.render(createElement(MultiPathEditor, editorProps)));
    await act(async () => root.render(createElement(MultiPathEditor, { ...editorProps, isOpen: false })));
    vi.mocked(invoke).mockRejectedValueOnce(new Error('fixture read refusal'));
    await act(async () => root.render(createElement(MultiPathEditor, editorProps)));
    expect(host.querySelector<HTMLInputElement>('input')!.value).toBe('Original');
});

it('does not offer writes with an unconfirmed initial config or reuse another pair after a failed read', async () => {
    const editorProps = { isOpen: true, onClose: vi.fn(), localPath: '/local', remotePath: '/remote' };
    vi.mocked(invoke).mockRejectedValueOnce(new Error('fixture initial read refusal'));
    await act(async () => root.render(createElement(MultiPathEditor, editorProps)));
    expect(visibleButtons().find(b => b.textContent?.trim() === 'Save')!.matches(':disabled')).toBe(true);
    await act(async () => root.render(createElement(MultiPathEditor, { ...editorProps, isOpen: false })));
    vi.mocked(invoke).mockResolvedValue({ pairs: [{ id: 'p', name: 'Old pair', local_path: '/local', remote_path: '/remote', enabled: true, exclude_overrides: [] }], parallel_pairs: false });
    await act(async () => root.render(createElement(MultiPathEditor, editorProps)));
    vi.mocked(invoke).mockRejectedValueOnce(new Error('fixture other pair read refusal'));
    await act(async () => root.render(createElement(MultiPathEditor, { ...editorProps, remotePath: '/other' })));
    expect(host.querySelector('input')).toBeNull();
    expect(visibleButtons().find(b => b.textContent?.trim() === 'Save')!.matches(':disabled')).toBe(true);
});
