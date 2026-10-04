// @vitest-environment jsdom
import { act, createElement } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { useCancelStaleCompare } from './useCancelStaleCompare';

const invoke = vi.hoisted(() => vi.fn(async () => true));
vi.mock('@tauri-apps/api/core', () => ({ invoke }));

const Probe = ({ id }: { id: string | undefined }) => {
    useCancelStaleCompare(id);
    return null;
};
let root: Root;
let host: HTMLDivElement;
const render = async (id: string | undefined) => {
    await act(async () => root.render(createElement(Probe, { id })));
};
beforeEach(() => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    invoke.mockClear();
    host = document.createElement('div'); document.body.append(host); root = createRoot(host);
});
afterEach(async () => { await act(async () => root.unmount()); host.remove(); });

it('stops the running compare when a new open replaces it', async () => {
    await render('aerosync-1');
    await render('aerosync-2');
    expect(invoke).toHaveBeenCalledExactlyOnceWith('cancel_compare', { progressId: 'aerosync-1' });
});

it('stops the running compare when the dialog stops waiting for it', async () => {
    await render('aerosync-1');
    await render(undefined);
    expect(invoke).toHaveBeenCalledExactlyOnceWith('cancel_compare', { progressId: 'aerosync-1' });
});

it('stops the running compare when the app unmounts', async () => {
    await render('aerosync-1');
    await act(async () => root.unmount());
    root = createRoot(host);
    expect(invoke).toHaveBeenCalledExactlyOnceWith('cancel_compare', { progressId: 'aerosync-1' });
});

it('does not cancel a compare that is still awaited across renders', async () => {
    await render('aerosync-1');
    await render('aerosync-1');
    expect(invoke).not.toHaveBeenCalled();
});

it('cancels nothing when no compare runs', async () => {
    await render(undefined);
    await render(undefined);
    expect(invoke).not.toHaveBeenCalled();
});
