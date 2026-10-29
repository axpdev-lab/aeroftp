// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later

import * as React from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { AeroSyncDialog } from './AeroSyncDialog';
import type { AeroSyncDialogProps } from './types';

const mocks = vi.hoisted(() => ({
    invoke: vi.fn(),
    listen: vi.fn(),
    emitScan: vi.fn(),
}));

vi.mock('@tauri-apps/api/core', () => ({ invoke: mocks.invoke }));
vi.mock('@tauri-apps/api/event', () => ({ listen: mocks.listen }));
vi.mock('../../i18n', () => ({ useTranslation: () => (key: string) => key }));
vi.mock('../../hooks/useDraggableModal', () => ({
    useDraggableModal: () => ({ panelProps: {}, dragHandleProps: {} }),
}));
vi.mock('./CompareTabContent', async () => {
    const { CompareScanState } = await import('./CompareScanState');
    return {
        CompareTabContent: (props: Record<string, unknown>) => React.createElement(CompareScanState, {
            loading: props.loading,
            scanProgressId: props.scanProgressId,
            stopped: props.stopped,
            leftLabel: props.leftLabel,
            rightLabel: props.rightLabel,
            onStart: props.onStartCompare,
            onStop: props.onStopCompare,
        } as Parameters<typeof CompareScanState>[0]),
    };
});
vi.mock('./PlanTabContent', async () => {
    const { CompareScanState } = await import('./CompareScanState');
    return {
        PlanTabContent: (props: Record<string, unknown>) => React.createElement(CompareScanState, {
            loading: props.loading,
            scanProgressId: props.scanProgressId,
            stopped: props.compareStopped,
            leftLabel: props.leftLabel,
            rightLabel: props.rightLabel,
            onStart: props.onStartCompare,
            onStop: props.onStopCompare,
        } as Parameters<typeof CompareScanState>[0]),
    };
});

let root: Root;
let host: HTMLDivElement;
const noop = () => undefined;
const props = (): AeroSyncDialogProps => ({
    isOpen: true,
    onClose: noop,
    context: {
        pairKind: 'local-remote',
        leftLabel: '/local',
        rightLabel: '/remote',
        compareResult: null,
        compareLoading: true,
        scanProgressId: 'scan-1',
    },
    onApplyMirrorLeftToRight: noop,
    onApplyMirrorRightToLeft: noop,
    onExecutePreset: noop,
    onResumeJournal: noop,
    onDismissJournal: noop,
    onInvalidateComparison: noop,
    onStartCompare: noop,
    onStopCompare: noop,
});

beforeEach(() => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    host = document.createElement('div');
    document.body.append(host);
    root = createRoot(host);
    mocks.invoke.mockReset().mockResolvedValue(null);
    mocks.emitScan.mockReset();
    mocks.listen.mockReset().mockImplementation(async (_event, callback) => {
        mocks.emitScan.mockImplementation(callback);
        return vi.fn();
    });
});

afterEach(async () => {
    await React.act(async () => root.unmount());
    host.remove();
    vi.restoreAllMocks();
});

it('keeps the running scan counters when switching between Compare and Plan', async () => {
    await React.act(async () => root.render(React.createElement(AeroSyncDialog, props())));
    await React.act(async () => {
        mocks.emitScan({ payload: { progress_id: 'scan-1', phase: 'local', files_found: 12, dirs_found: 2, bytes_found: 120 } });
    });
    expect(host.textContent).toContain('12');

    const plan = Array.from(host.querySelectorAll('button')).find(button => button.textContent === 'aerosync.tabPlan');
    expect(plan).toBeTruthy();
    await React.act(async () => plan!.click());
    await React.act(async () => Promise.resolve());

    expect(host.textContent).toContain('12');
    expect(host.textContent).not.toContain('aerosync.planUnavailable');
});
