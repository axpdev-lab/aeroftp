// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
import { act, createElement } from 'react';
import { createRoot } from 'react-dom/client';
import { expect, it, vi } from 'vitest';
import { useRemoteSearchRequest } from './useRemoteSearchRequest';

type Request = ReturnType<ReturnType<typeof useRemoteSearchRequest>['begin']>;

it('drops late errors and results after cancel, context change, a newer search or unmount', async () => {
  (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
  const root = createRoot(document.createElement('div'));
  let scope!: ReturnType<typeof useRemoteSearchRequest>;
  const Harness = ({ context }: { context: string }) => { scope = useRemoteSearchRequest(context); return null; };
  await act(async () => root.render(createElement(Harness, { context: 'session-a/open' })));
  const received = vi.fn();
  let reject!: (e: Error) => void;
  let request!: Request;
  act(() => { request = scope.begin(); });
  const pending = new Promise<never>((_, fail) => { reject = fail; })
    .catch((error) => { if (request.isCurrent()) received(error); }).finally(() => request.finish());
  scope.cancel();
  expect(scope.pendingCount).toBe(1); // The backend still owns its provider lock.
  await act(async () => { reject(new Error('old request failed')); await pending; });
  expect(received).not.toHaveBeenCalled();
  expect(scope.pendingCount).toBe(0);
  let oldContext!: Request;
  act(() => { oldContext = scope.begin(); });
  await act(async () => root.render(createElement(Harness, { context: 'session-b/open' })));
  expect(oldContext.isCurrent()).toBe(false);
  let earlier!: Request, latest!: Request;
  act(() => { earlier = scope.begin(); latest = scope.begin(); });
  expect(earlier.isCurrent()).toBe(false);
  expect(latest.isCurrent()).toBe(true);
  act(() => { oldContext.finish(); earlier.finish(); });
  await act(async () => root.unmount());
  expect(latest.isCurrent()).toBe(false);
  latest.finish(); // A late native completion must not update an unmounted panel.
});

it('stays busy until all native calls finish even when their results are invalidated', async () => {
  (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
  const root = createRoot(document.createElement('div'));
  let scope!: ReturnType<typeof useRemoteSearchRequest>;
  const Harness = () => { scope = useRemoteSearchRequest('session'); return null; };
  await act(async () => root.render(createElement(Harness)));
  let first!: Request, second!: Request;
  act(() => { first = scope.begin(); second = scope.begin(); });
  expect(scope.pendingCount).toBe(2);
  scope.cancel();
  expect(first.isCurrent()).toBe(false);
  expect(second.isCurrent()).toBe(false);
  expect(scope.pendingCount).toBe(2);
  act(() => first.finish());
  expect(scope.pendingCount).toBe(1);
  act(() => { first.finish(); second.finish(); second.finish(); });
  expect(scope.pendingCount).toBe(0);
  await act(async () => root.unmount());
});
