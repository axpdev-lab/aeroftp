// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
import { act, createElement } from 'react';
import { createRoot } from 'react-dom/client';
import { expect, it, vi } from 'vitest';
import { useRemoteSearchRequest } from './useRemoteSearchRequest';

it('drops late errors and results after cancel, context change, a newer search or unmount', async () => {
  (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
  const host = document.createElement('div');
  const root = createRoot(host);
  let scope!: ReturnType<typeof useRemoteSearchRequest>;
  const Harness = ({ context }: { context: string }) => { scope = useRemoteSearchRequest(context); return null; };
  await act(async () => root.render(createElement(Harness, { context: 'session-a/open' })));
  const received = vi.fn();
  let reject!: (e: Error) => void;
  const isCurrent = scope.begin();
  const pending = new Promise<never>((_, fail) => { reject = fail; }).catch((error) => { if (isCurrent()) received(error); });
  scope.cancel(); // The close handler cancels synchronously, before a rerender.
  reject(new Error('old request failed'));
  await pending;
  expect(received).not.toHaveBeenCalled();
  const oldContext = scope.begin();
  await act(async () => root.render(createElement(Harness, { context: 'session-b/open' })));
  expect(oldContext()).toBe(false);
  const earlier = scope.begin();
  const latest = scope.begin();
  expect(earlier()).toBe(false);
  expect(latest()).toBe(true);
  await act(async () => root.unmount());
  expect(latest()).toBe(false);
});
