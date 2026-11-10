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

// Discussion #1527: a click outside AeroSync closed it during a compare scan
// (the scan was cancelled silently) and after one (the comparison was lost).
const backdrop = () => host.querySelector('[data-gui-owned="aerosync"]') as HTMLElement;
const clickBackdrop = () => React.act(async () => { backdrop().dispatchEvent(new MouseEvent('click', { bubbles: true })); });
const button = (label: string) => Array.from(host.querySelectorAll('button')).find(b => b.textContent === label || b.getAttribute('aria-label') === label)!;
const withContext = (context: Partial<AeroSyncDialogProps['context']>, extra: Partial<AeroSyncDialogProps> = {}) => {
    const base = props();
    return { ...base, ...extra, context: { ...base.context, ...context } } as AeroSyncDialogProps;
};

it('keeps a compare scan running when the backdrop is clicked, and stops it only through the X and its confirm', async () => {
    const onClose = vi.fn();
    const onStopCompare = vi.fn();
    await React.act(async () => root.render(React.createElement(AeroSyncDialog, withContext({ compareLoading: true }, { onClose, onStopCompare }))));
    await clickBackdrop();
    expect(onClose).not.toHaveBeenCalled();
    expect(onStopCompare).not.toHaveBeenCalled();
    await React.act(async () => button('common.close').click());
    expect(onClose).not.toHaveBeenCalled();
    await React.act(async () => button('modalGuard.stopAndClose').click());
    expect(onStopCompare).toHaveBeenCalledTimes(1);
    expect(onClose).toHaveBeenCalledTimes(1);
});

it('keeps a finished comparison when the backdrop is clicked, and closes from the X at once', async () => {
    const onClose = vi.fn();
    const compareResult = { differences: [], identical: 0 } as unknown as NonNullable<AeroSyncDialogProps['context']['compareResult']>;
    await React.act(async () => root.render(React.createElement(AeroSyncDialog, withContext({ compareLoading: false, compareResult }, { onClose }))));
    await clickBackdrop();
    expect(onClose).not.toHaveBeenCalled();
    await React.act(async () => button('common.close').click());
    expect(onClose).toHaveBeenCalledTimes(1);
});

it('closes from the backdrop when it holds no comparison and runs nothing', async () => {
    const onClose = vi.fn();
    await React.act(async () => root.render(React.createElement(AeroSyncDialog, withContext({ compareLoading: false, compareResult: null, compareIdle: true }, { onClose }))));
    await clickBackdrop();
    expect(onClose).toHaveBeenCalledTimes(1);
});
