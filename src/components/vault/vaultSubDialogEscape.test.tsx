// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
import { act, createElement } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import english from '../../i18n/locales/en.json';
import { useGuardedClose } from '../../hooks/useGuardedClose';
import { VaultBrowse } from './VaultBrowse';
import { VaultReceipt } from './VaultReceipt';
import type { VaultReport, VaultState } from './useVaultState';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(async () => []) }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => () => undefined) }));
vi.mock('@tauri-apps/plugin-fs', () => ({ readTextFile: vi.fn(), writeTextFile: vi.fn() }));
vi.mock('../../utils/pickPath', () => ({ pickFile: vi.fn(async () => null), pickSave: vi.fn(async () => null) }));
vi.mock('../../hooks/useDraggableModal', () => ({
    useDraggableModal: () => ({ panelProps: {}, dragHandleProps: {} }),
}));
vi.mock('../../i18n', () => ({
    useTranslation: () => (key: string) => key.split('.').reduce<unknown>(
        (value, part) => (value as Record<string, unknown>)?.[part], english.translations,
    ),
}));
vi.mock('../VaultSyncDialog', () => ({
    default: ({ onClose }: { onClose: () => void }) =>
        createElement('div', { 'data-testid': 'vault-sync-dialog' },
            createElement('button', { onClick: onClose }, 'sync-close')),
}));

let root: Root;
let host: HTMLDivElement;

// Stands in for VaultPanel: it stays in the ESC stack but suspended while a
// sub-dialog is open, exactly like the panel's escapeSuspended flag.
const Parent = ({ onClose, suspended = false }: { onClose: () => void; suspended?: boolean }) => {
    useGuardedClose({ guard: null, onClose, escape: true, escapeSuspended: suspended });
    return null;
};

const vaultState = (over: Record<string, unknown> = {}) => ({
    mode: 'browse', loading: false, entries: [], currentDir: '', isPlaintextZip: false,
    vaultSecurity: null, vaultPath: '/vaults/sample.aerovault', password: 'pw',
    showNewDirDialog: false, newDirName: '', setNewDirName: vi.fn(), handleCreateDirectory: vi.fn(),
    showSyncDialog: false, showScrubDialog: false, scrubResult: null,
    showRepairDialog: false, isRepairing: false, repairDryRun: false, setRepairDryRun: vi.fn(),
    repairResult: null, handleRepair: vi.fn(),
    setShowNewDirDialog: vi.fn(), setShowScrubDialog: vi.fn(), setShowSyncDialog: vi.fn(),
    setShowRepairDialog: vi.fn(), refreshVaultEntries: vi.fn(),
    handleAddFiles: vi.fn(), handleAddFolder: vi.fn(), handleExtract: vi.fn(), handleRemove: vi.fn(),
    handleScrub: vi.fn(), setCurrentDir: vi.fn(),
    ...over,
}) as unknown as VaultState;

const report = {
    operation: 'create', vault_format: 3, profile: 'balanced', files: 1, packed_files: 1,
    time_elapsed: 0.1, logical_chunks: 1, new_physical_chunks: 1, dedup_hits: 0,
    plaintext_bytes: 10, encrypted_bytes: 12, compressed_bytes: 11, compression_ratio_pct: 110,
    algorithms: ['a'], steps: [], attribution: '',
} as unknown as VaultReport;

const render = async (el: React.ReactElement) => {
    await act(async () => root.render(el));
};
const escape = async () => {
    await act(async () => {
        window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }));
    });
};

beforeEach(() => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    host = document.createElement('div'); document.body.append(host); root = createRoot(host);
});
afterEach(async () => { await act(async () => root.unmount()); host.remove(); });

it.each([
    ['new-folder bar', { showNewDirDialog: true }, 'setShowNewDirDialog'],
    ['scrub result', { showScrubDialog: true, scrubResult: { count: 0, checked: 3, damaged: [] } }, 'setShowScrubDialog'],
    ['repair dialog', { showRepairDialog: true }, 'setShowRepairDialog'],
])('ESC closes only the vault %s sub-dialog and leaves the panel open', async (_label, over, closer) => {
    const onClose = vi.fn();
    const state = vaultState(over);
    await render(<><Parent onClose={onClose} suspended />{createElement(VaultBrowse, { state })}</>);
    await escape();
    expect(state[closer as keyof VaultState]).toHaveBeenCalledWith(false);
    expect(onClose).not.toHaveBeenCalled();
    // The parent still answers ESC once the sub-dialog is gone.
    await render(<><Parent onClose={onClose} />{createElement(VaultBrowse, { state: vaultState() })}</>);
    await escape();
    expect(onClose).toHaveBeenCalledTimes(1);
});

it('a running repair keeps its dialog: backdrop inert, X and Close ask, Wait keeps it', async () => {
    const onClose = vi.fn();
    const state = vaultState({ showRepairDialog: true, isRepairing: true });
    await render(<><Parent onClose={onClose} suspended />{createElement(VaultBrowse, { state })}</>);
    const backdrop = host.querySelector<HTMLElement>('div.fixed.inset-0')!;
    const alertOpen = () => host.querySelector('[role=alertdialog]') !== null;
    // The backdrop is inert while the repair runs.
    await act(async () => { backdrop.dispatchEvent(new MouseEvent('click', { bubbles: true })); });
    expect(state.setShowRepairDialog).not.toHaveBeenCalled();
    expect(alertOpen()).toBe(false);
    // Both the X and the footer Close route through the busy confirm.
    const x = [...host.querySelectorAll('button')].find(b => b.textContent?.includes('✕'))!;
    await act(async () => { x.click(); });
    expect(alertOpen()).toBe(true);
    expect(state.setShowRepairDialog).not.toHaveBeenCalled();
    await act(async () => { [...host.querySelectorAll<HTMLButtonElement>('[role=alertdialog] button')].find(b => /keep running/i.test(b.textContent ?? ''))!.click(); });
    expect(alertOpen()).toBe(false);
    const footerClose = [...host.querySelectorAll('button')].find(b => b.textContent?.trim() === 'Close')!;
    await act(async () => { footerClose.click(); });
    expect(alertOpen()).toBe(true);
    expect(state.setShowRepairDialog).not.toHaveBeenCalled();
    // Wait keeps the dialog; no second repair was ever started.
    await act(async () => { [...host.querySelectorAll<HTMLButtonElement>('[role=alertdialog] button')].find(b => /keep running/i.test(b.textContent ?? ''))!.click(); });
    expect(alertOpen()).toBe(false);
    expect(state.setShowRepairDialog).not.toHaveBeenCalled();
    expect(state.handleRepair).not.toHaveBeenCalled();
    expect(onClose).not.toHaveBeenCalled();
});

it('ESC closes the vault receipt sub-dialog and leaves the panel open', async () => {
    const onClose = vi.fn();
    const onReceiptClose = vi.fn();
    await render(<><Parent onClose={onClose} suspended />
        {createElement(VaultReceipt, { report, t: (key: string) => key, onClose: onReceiptClose })}</>);
    await escape();
    expect(onReceiptClose).toHaveBeenCalledTimes(1);
    expect(onClose).not.toHaveBeenCalled();
});
