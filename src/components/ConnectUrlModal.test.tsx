// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { act, createElement } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ConnectUrlModal } from './ConnectUrlModal';

const invoke = vi.hoisted(() => vi.fn());
const saveLaunchUrlProfile = vi.hoisted(() => vi.fn());
vi.mock('@tauri-apps/api/core', () => ({ invoke }));
vi.mock('../i18n', () => ({
    useTranslation: () => (key: string) => key,
}));
vi.mock('../utils/connectUrlSave', () => ({ saveLaunchUrlProfile }));

let root: Root;
let host: HTMLDivElement;

beforeEach(() => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    invoke.mockReset();
    host = document.createElement('div');
    document.body.append(host);
    root = createRoot(host);
});
afterEach(async () => {
    await act(async () => root.unmount());
    host.remove();
});

const render = async (onClose: () => void) => {
    await act(async () => root.render(createElement(ConnectUrlModal, { onClose })));
};
const input = () => {
    const el = host.querySelector('input');
    if (!el) throw new Error('no input');
    return el;
};
const typeInto = (el: HTMLInputElement, value: string) => act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(el, value);
    el.dispatchEvent(new Event('input', { bubbles: true }));
});
const key = (el: Element, k: string, opts: { shiftKey?: boolean } = {}) => {
    let event: KeyboardEvent | null = null;
    act(() => {
        event = new KeyboardEvent('keydown', { key: k, bubbles: true, cancelable: true, shiftKey: !!opts.shiftKey });
        el.dispatchEvent(event);
    });
    return event!;
};
// requestAnimationFrame in jsdom runs on a timer; flush it.
const flush = () => act(async () => { await new Promise((resolve) => setTimeout(resolve, 20)); });

describe('ConnectUrlModal', () => {
    it('Tab completes the typed fragment into a template and selects the first placeholder', async () => {
        await render(() => {});
        const el = input();
        await typeInto(el, 'sft');
        await key(el, 'Tab');
        await flush();
        expect(el.value).toBe('sftp://{user}@{host}/{path}');
        expect(el.selectionStart).toBe(8);
        expect(el.selectionEnd).toBe(12);
        expect(el.value.slice(el.selectionStart ?? 0, el.selectionEnd ?? 0)).toBe('user');
    });

    it('Tab jumps through the placeholders user -> host -> path', async () => {
        await render(() => {});
        const el = input();
        await typeInto(el, 'sft');
        await key(el, 'Tab');
        await flush();
        await key(el, 'Tab');
        expect(el.value.slice(el.selectionStart ?? 0, el.selectionEnd ?? 0)).toBe('host');
        await key(el, 'Tab');
        expect(el.value.slice(el.selectionStart ?? 0, el.selectionEnd ?? 0)).toBe('path');
        // Past the last placeholder the caret moves to the end.
        await key(el, 'Tab');
        expect(el.selectionStart).toBe(el.value.length);
    });

    it('Enter submits the URL to open_launch_url and closes on success', async () => {
        const onClose = vi.fn();
        invoke.mockResolvedValue(undefined);
        await render(onClose);
        const el = input();
        await typeInto(el, 'sftp://bob@example.com/reports');
        await key(el, 'Enter');
        await flush();
        expect(invoke).toHaveBeenCalledWith('open_launch_url', { url: 'sftp://bob@example.com/reports', password: null });
        expect(onClose).toHaveBeenCalled();
    });

    it('shows the backend parse error and stays open when the URL is rejected', async () => {
        const onClose = vi.fn();
        invoke.mockRejectedValue('Unsupported URL scheme. Supported: ftp, ftps, sftp');
        await render(onClose);
        const el = input();
        await typeInto(el, 'gopher://host');
        await key(el, 'Enter');
        await flush();
        expect(onClose).not.toHaveBeenCalled();
        expect(host.textContent).toContain('Unsupported URL scheme');
    });

    it('Tab on a complete URL is not captured, so focus can move on; Shift+Tab never is', async () => {
        await render(() => {});
        const el = input();
        await typeInto(el, 'sftp://bob@example.com/reports');
        const tab = key(el, 'Tab');
        expect(tab.defaultPrevented).toBe(false);
        const shiftTab = key(el, 'Tab', { shiftKey: true });
        expect(shiftTab.defaultPrevented).toBe(false);
        // But on a template with placeholders Tab still jumps.
        await typeInto(el, 'sft');
        key(el, 'Tab');
        await flush();
        const tab2 = key(el, 'Tab');
        expect(tab2.defaultPrevented).toBe(true);
        expect(el.value.slice(el.selectionStart ?? 0, el.selectionEnd ?? 0)).toBe('host');
    });

    it('Escape closes the modal', async () => {
        const onClose = vi.fn();
        await render(onClose);
        await key(input(), 'Escape');
        expect(onClose).toHaveBeenCalled();
    });

    it('shows the sign-in hint for an account scheme and disables Save to My Servers', async () => {
        await render(() => {});
        const el = input();
        await typeInto(el, 'gdrive://');
        await flush();
        expect(host.textContent).toContain('connectUrl.signInHint');
        const checkbox = host.querySelector('#connect-url-save') as HTMLInputElement;
        expect(checkbox.disabled).toBe(true);
        expect(host.querySelector('input[type="password"]')).toBeNull();
    });

    it('warns when the URL carries an inline password', async () => {
        await render(() => {});
        const el = input();
        await typeInto(el, 'sftp://bob:secret@example.com/');
        await flush();
        expect(host.textContent).toContain('connectUrl.inlinePasswordWarning');
    });

    it('forwards the masked password and clears all state on successful submit', async () => {
        const onClose = vi.fn();
        invoke.mockResolvedValue(undefined);
        await render(onClose);
        const el = input();
        await typeInto(el, 'sftp://bob@example.com/reports');
        const pw = host.querySelector('input[type="password"]') as HTMLInputElement;
        expect(pw).not.toBeNull();
        await typeInto(pw, 'hunter2');
        await key(el, 'Enter');
        await flush();
        expect(invoke).toHaveBeenCalledWith('open_launch_url', { url: 'sftp://bob@example.com/reports', password: 'hunter2' });
        expect(onClose).toHaveBeenCalled();
    });

    it('saves to My Servers via the vault path before connecting, with the smart default name', async () => {
        const onClose = vi.fn();
        invoke.mockImplementation(async (cmd: string) => cmd === 'parse_launch_url'
            ? { kind: 'remoteUrl', protocol: 'sftp', server: 'example.com', username: 'bob', port: 22 }
            : undefined);
        saveLaunchUrlProfile.mockResolvedValue({ id: 'srv_1' });
        await render(onClose);
        const el = input();
        await typeInto(el, 'sftp://bob@example.com/reports');
        const checkbox = host.querySelector('#connect-url-save') as HTMLInputElement;
        await act(async () => { checkbox.click(); });
        const nameInput = [...host.querySelectorAll('input[type="text"]')].find((i) => (i as HTMLInputElement).placeholder === 'connectUrl.profileName') as HTMLInputElement;
        expect(nameInput.value).toBe('bob@example.com');
        await key(el, 'Enter');
        await flush();
        expect(invoke).toHaveBeenCalledWith('parse_launch_url', { url: 'sftp://bob@example.com/reports', password: null });
        expect(saveLaunchUrlProfile).toHaveBeenCalledWith(
            expect.objectContaining({ server: 'example.com' }), 'bob@example.com', undefined,
        );
        expect(invoke).toHaveBeenCalledWith('open_launch_url', { url: 'sftp://bob@example.com/reports', password: null });
        expect(onClose).toHaveBeenCalled();
    });

    it('fails closed when the vault save refuses: no connect, modal stays open', async () => {
        const onClose = vi.fn();
        invoke.mockImplementation(async (cmd: string) => cmd === 'parse_launch_url'
            ? { kind: 'remoteUrl', protocol: 'sftp', server: 'example.com' }
            : undefined);
        saveLaunchUrlProfile.mockRejectedValue('STORE_NOT_READY: vault locked');
        await render(onClose);
        const el = input();
        await typeInto(el, 'sftp://bob@example.com/');
        const checkbox = host.querySelector('#connect-url-save') as HTMLInputElement;
        await act(async () => { checkbox.click(); });
        await key(el, 'Enter');
        await flush();
        expect(saveLaunchUrlProfile).toHaveBeenCalled();
        expect(invoke).not.toHaveBeenCalledWith('open_launch_url', expect.anything());
        expect(onClose).not.toHaveBeenCalled();
        expect(host.textContent).toContain('STORE_NOT_READY');
    });
});
