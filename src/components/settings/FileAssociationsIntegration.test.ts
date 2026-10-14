// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
import { act, createElement } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { FileAssociationsPanel } from './FileAssociationsPanel';
const invoke = vi.hoisted(() => vi.fn());
const translate = (key: string) => key;
vi.mock('@tauri-apps/api/core', () => ({ invoke }));
vi.mock('../../i18n', () => ({ useTranslation: () => translate }));
let host: HTMLDivElement;
let root: Root;
let state: { platform: string; items: unknown[]; windowsIntegrationError?: string; windowsIntegration: { ownedFormats: boolean; archiveFormats: boolean; canChange: boolean; managedBy: string } | null };
const toggles = () => Array.from(host.querySelectorAll<HTMLInputElement>('input[type=checkbox]'));
beforeEach(() => {
  (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
  host = document.createElement('div'); document.body.append(host); root = createRoot(host);
  state = { platform: 'windows', items: [], windowsIntegration: { ownedFormats: true, archiveFormats: true, canChange: true, managedBy: 'nsis' } };
  invoke.mockReset().mockImplementation(async (command: string, args: Record<string, boolean>) => {
    if (command === 'file_associations_status') return structuredClone(state);
    if (command === 'file_associations_set_integration') {
      state.windowsIntegration = { ...state.windowsIntegration!, ...args };
      return structuredClone(state.windowsIntegration);
    }
    return undefined;
  });
});
afterEach(async () => { await act(async () => root.unmount()); host.remove(); });
describe('Windows association choices', () => {
  it('turns off archives independently and reflects the backend receipt', async () => {
    await act(async () => root.render(createElement(FileAssociationsPanel)));
    await act(async () => toggles()[1].click());
    expect(invoke).toHaveBeenCalledWith('file_associations_set_integration', { ownedFormats: true, archiveFormats: false });
    expect(toggles().map(input => input.checked)).toEqual([true, false]);
  });
  it('does not claim success when Windows Installer is cancelled', async () => {
    const normal = invoke.getMockImplementation()!;
    invoke.mockImplementation((command: string, args: Record<string, boolean>) => command === 'file_associations_set_integration'
      ? Promise.reject(new Error('Windows Installer cancelled')) : normal(command, args));
    await act(async () => root.render(createElement(FileAssociationsPanel)));
    await act(async () => toggles()[0].click());
    expect(toggles().map(input => input.checked)).toEqual([true, true]);
    expect(host.textContent).toContain('Windows Installer cancelled');
  });
  it('disables both choices until the pending change settles', async () => {
    let answer!: (result: unknown) => void;
    const normal = invoke.getMockImplementation()!;
    invoke.mockImplementation((command: string, args: Record<string, boolean>) => command === 'file_associations_set_integration'
      ? new Promise(resolve => { answer = resolve; }) : normal(command, args));
    await act(async () => root.render(createElement(FileAssociationsPanel)));
    await act(async () => toggles()[0].click());
    expect(host.querySelector('fieldset')?.disabled).toBe(true);
    await act(async () => { state.windowsIntegration!.ownedFormats = false; answer(state.windowsIntegration); });
    expect(host.querySelector('fieldset')?.disabled).toBe(false);
    expect(toggles()[0].checked).toBe(false);
  });
  it('explains why portable builds cannot modify installer integration', async () => {
    state.windowsIntegration = { ownedFormats: false, archiveFormats: false, canChange: false, managedBy: 'portable' };
    await act(async () => root.render(createElement(FileAssociationsPanel)));
    expect(host.querySelector('fieldset')?.disabled).toBe(true);
    expect(host.textContent).toContain('settings.fileAssociationIntegrationPortable');
  });
  it('shows the elevation note for an MSI installation', async () => {
    state.windowsIntegration!.managedBy = 'msi';
    await act(async () => root.render(createElement(FileAssociationsPanel)));
    expect(host.textContent).toContain('settings.fileAssociationIntegrationAdmin');
  });
  it('keeps format rows and Default apps available when integration status fails', async () => {
    state.windowsIntegration = null;
    state.windowsIntegrationError = 'This MSI does not expose selectable file associations (state -1).';
    state.items = [{ key: 'profile', label: 'Server profile', extensions: ['aeroftp'], isDefault: false, isAvailable: true, action: 'os_confirmation' }];
    await act(async () => root.render(createElement(FileAssociationsPanel)));
    expect(toggles()).toHaveLength(0);
    expect(host.textContent).toContain(state.windowsIntegrationError);
    expect(host.textContent).toContain('Server profile');
    const openSettings = Array.from(host.querySelectorAll('button')).find(button => button.textContent?.includes('settings.fileAssociationOpenSettings'))!;
    await act(async () => openSettings.click());
    expect(invoke).toHaveBeenCalledWith('file_associations_open_settings');
  });
  it('does not expose Windows registration controls on Linux', async () => {
    state.platform = 'linux'; state.windowsIntegration = null;
    await act(async () => root.render(createElement(FileAssociationsPanel)));
    expect(toggles()).toHaveLength(0);
  });
});
