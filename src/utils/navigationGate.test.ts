import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { createNavigationGate } from './navigationGate';

beforeEach(() => { vi.useFakeTimers(); });
afterEach(() => { vi.useRealTimers(); });

it('lets one navigation in at a time', () => {
    const gate = createNavigationGate();
    expect(gate.enter()).toBe(true);
    expect(gate.enter()).toBe(false);
    gate.leave();
    expect(gate.enter()).toBe(true);
});

it('is free at once when nothing is loading', async () => {
    await expect(createNavigationGate().whenFree(1000)).resolves.toBe(true);
});

it('waits for the navigation in flight', async () => {
    const gate = createNavigationGate();
    gate.enter();
    let freed: boolean | undefined;
    void gate.whenFree(1000).then(value => { freed = value; });
    await vi.advanceTimersByTimeAsync(500);
    expect(freed).toBeUndefined();
    gate.leave();
    await vi.advanceTimersByTimeAsync(0);
    expect(freed).toBe(true);
    expect(gate.enter()).toBe(true);
});

it('waits for a navigation that took the gate in the meantime too', async () => {
    const gate = createNavigationGate();
    gate.enter();
    let freed: boolean | undefined;
    void gate.whenFree(1000).then(value => { freed = value; });
    gate.leave();
    gate.enter();
    await vi.advanceTimersByTimeAsync(0);
    expect(freed).toBeUndefined();
    gate.leave();
    await vi.advanceTimersByTimeAsync(0);
    expect(freed).toBe(true);
});

it('gives up when the navigation never answers', async () => {
    const gate = createNavigationGate();
    gate.enter();
    let freed: boolean | undefined;
    void gate.whenFree(1000).then(value => { freed = value; });
    await vi.advanceTimersByTimeAsync(999);
    expect(freed).toBeUndefined();
    await vi.advanceTimersByTimeAsync(1);
    expect(freed).toBe(false);
    expect(gate.enter()).toBe(false);
});
