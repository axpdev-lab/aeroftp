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
vi.mock('./PlanTabContent', () => ({ PlanTabContent: () => {
    const store = useContext(TabStateStoreContext);
    return createElement('output', { 'data-testid': 'plan-settings' }, JSON.stringify({
        preset: store?.get('plan.preset', null), direction: store?.get('plan.direction', null),
        source: store?.get('sync.source', null), destination: store?.get('sync.destination', null),
    }));
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
