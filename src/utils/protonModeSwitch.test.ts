// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it, vi } from 'vitest';
import { runProtonModeSwitch, type ProtonSwitchPlan } from './protonModeSwitch';

const plan = (overrides: Partial<ProtonSwitchPlan> = {}): ProtonSwitchPlan => ({
    profileId: 'p1',
    from: 'cli',
    to: 'native',
    changes: [{ kind: 'initialPath', label: '', from: '/my-files/docs', to: '/docs' }],
    legacyHistories: 0,
    blockers: [],
    ...overrides,
});

const backend = (answer: ProtonSwitchPlan) => {
    const invoke = vi.fn(async (cmd: string, _payload?: Record<string, unknown>) => {
        if (cmd === 'proton_mode_switch_plan' || cmd === 'proton_mode_switch_apply') return answer;
        throw new Error(`unexpected ${cmd}`);
    });
    return invoke as unknown as (<T>(cmd: string, payload: Record<string, unknown>) => Promise<T>) & typeof invoke;
};

const base = {
    profileId: 'p1',
    storedOptions: {},
    storedInitialPath: '/my-files/docs',
    formOptions: { proton_mode: 'native' },
    formInitialPath: '/my-files/docs',
};

describe('runProtonModeSwitch', () => {
    it('writes nothing before the confirmation, and nothing at all on cancel', async () => {
        const invoke = backend(plan());
        const confirm = vi.fn(async () => {
            // Asked before any write: only the plan has been read so far.
            expect(invoke.mock.calls.map((call) => call[0])).toEqual(['proton_mode_switch_plan']);
            return false;
        });
        const outcome = await runProtonModeSwitch({ ...base, invoke, confirm });
        expect(outcome).toEqual({ kind: 'stop' });
        expect(confirm).toHaveBeenCalledOnce();
        expect(invoke.mock.calls.map((call) => call[0])).not.toContain('proton_mode_switch_apply');
    });

    it('applies after the yes and saves the translated initial folder', async () => {
        const invoke = backend(plan());
        const outcome = await runProtonModeSwitch({ ...base, invoke, confirm: async () => true });
        expect(invoke.mock.calls.map((call) => call[0])).toEqual(['proton_mode_switch_plan', 'proton_mode_switch_apply']);
        expect(invoke.mock.calls[1][1]).toEqual({ profileId: 'p1', mode: 'native' });
        expect(outcome).toMatchObject({ kind: 'switched', initialPath: '/docs' });
    });

    it('keeps a folder the user retyped in the form', async () => {
        const invoke = backend(plan());
        const outcome = await runProtonModeSwitch({
            ...base,
            formInitialPath: '/projects',
            invoke,
            confirm: async () => true,
        });
        expect(outcome).toMatchObject({ kind: 'switched', initialPath: '/projects' });
    });

    it('asks nothing and writes nothing when the switch is blocked', async () => {
        const invoke = backend(plan({ blockers: ['AeroCloud is syncing'] }));
        const confirm = vi.fn(async () => true);
        const outcome = await runProtonModeSwitch({ ...base, invoke, confirm });
        expect(outcome).toEqual({ kind: 'stop', blockers: ['AeroCloud is syncing'] });
        expect(confirm).not.toHaveBeenCalled();
        expect(invoke.mock.calls.map((call) => call[0])).toEqual(['proton_mode_switch_plan']);
    });

    it('is no switch when the tab did not change client', async () => {
        const invoke = backend(plan());
        const outcome = await runProtonModeSwitch({
            ...base,
            formOptions: {},
            invoke,
            confirm: async () => true,
        });
        expect(outcome).toEqual({ kind: 'none' });
        expect(invoke).not.toHaveBeenCalled();
    });
});
