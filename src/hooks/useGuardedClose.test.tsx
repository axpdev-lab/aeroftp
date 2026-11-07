// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import React, { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { useGuardedClose, type BackdropPolicy, type GuardKind, type GuardedClose } from './useGuardedClose';

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

/**
 * Discussion #1527: a click on the backdrop closed a modal that held a
 * comparison, a plan or a question. The hook decides when the backdrop may
 * close (never while guarded; never at all with `backdrop: 'never'`) and
 * routes ESC like the X.
 */
describe('useGuardedClose', () => {
    let host: HTMLDivElement;
    let root: Root;
    let api: GuardedClose;
    const onClose = vi.fn();
    const onAbort = vi.fn();
    const Harness = (props: { guard: GuardKind | null; backdrop?: BackdropPolicy; escape?: boolean; abort?: boolean }) => {
        api = useGuardedClose({ guard: props.guard, onClose, onAbort: props.abort ? onAbort : undefined, backdrop: props.backdrop, escape: props.escape });
        return <div data-testid="backdrop" onClick={api.onBackdropClick}><div data-testid="dialog" /></div>;
    };
    const render = (props: React.ComponentProps<typeof Harness>) => act(() => root.render(<Harness {...props} />));
    const click = (id: string) => act(() => { host.querySelector(`[data-testid="${id}"]`)!.dispatchEvent(new MouseEvent('click', { bubbles: true })); });
    const escape = () => act(() => { window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true })); });

    beforeEach(() => {
        host = document.createElement('div');
        document.body.appendChild(host);
        root = createRoot(host);
    });
    afterEach(() => {
        act(() => root.unmount());
        host.remove();
        vi.clearAllMocks();
    });

    it.each([
        ['idle, no guard', null, 'idle', true],
        ['idle, busy', 'busy', 'idle', false],
        ['idle, dirty', 'dirty', 'idle', false],
        ['never, no guard', null, 'never', false],
        ['never, busy', 'busy', 'never', false],
        ['never, dirty', 'dirty', 'never', false],
    ] as const)('a backdrop click with %s %s', (_name, guard, backdrop, closes) => {
        render({ guard, backdrop });
        click('backdrop');
        expect(onClose).toHaveBeenCalledTimes(closes ? 1 : 0);
        // A click inside the dialog never closes.
        click('dialog');
        expect(onClose).toHaveBeenCalledTimes(closes ? 1 : 0);
    });

    it('closes from the X at once when nothing is guarded, whatever the backdrop policy', () => {
        render({ guard: null, backdrop: 'never' });
        act(() => api.requestClose());
        expect(onClose).toHaveBeenCalledTimes(1);
    });

    it.each(['busy', 'dirty'] as const)('asks before closing a %s modal, then keeps or closes', (guard) => {
        render({ guard, abort: true });
        act(() => api.requestClose());
        expect(onClose).not.toHaveBeenCalled();
        expect(api.confirmKind).toBe(guard);
        act(() => api.cancelConfirm());
        expect(api.confirmOpen).toBe(false);
        act(() => api.requestClose());
        act(() => api.confirmAndClose());
        expect(onClose).toHaveBeenCalledTimes(1);
        // Only a busy close stops the operation.
        expect(onAbort).toHaveBeenCalledTimes(guard === 'busy' ? 1 : 0);
    });

    it('says whether closing stops the operation', () => {
        render({ guard: 'busy', abort: false });
        expect(api.canAbort).toBe(false);
        render({ guard: 'busy', abort: true });
        expect(api.canAbort).toBe(true);
    });

    it('routes ESC like the X when asked to, and not otherwise', () => {
        render({ guard: null, escape: false });
        escape();
        expect(onClose).not.toHaveBeenCalled();
        render({ guard: null, escape: true });
        escape();
        expect(onClose).toHaveBeenCalledTimes(1);
        render({ guard: 'busy', escape: true });
        escape();
        expect(onClose).toHaveBeenCalledTimes(1);
        expect(api.confirmKind).toBe('busy');
        // With the confirm open, ESC belongs to the confirm, not to the modal.
        escape();
        expect(onClose).toHaveBeenCalledTimes(1);
    });

    it('leaves ESC to an inner handler that took it', () => {
        render({ guard: null, escape: true });
        act(() => {
            const event = new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true });
            event.preventDefault();
            window.dispatchEvent(event);
        });
        expect(onClose).not.toHaveBeenCalled();
    });
});
