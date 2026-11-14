// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import React, { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, expect, it } from 'vitest';
import { useInFlight, type InFlight } from './useInFlight';

let host: HTMLDivElement;
let root: Root;
let current: InFlight<string>;

function Probe() {
  current = useInFlight<string>();
  return null;
}

beforeEach(async () => {
  (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
  host = document.createElement('div');
  document.body.append(host);
  root = createRoot(host);
  await act(async () => root.render(React.createElement(Probe)));
});
afterEach(async () => { await act(async () => root.unmount()); host.remove(); });

it('keeps each key busy until its own operations settle', async () => {
  await act(async () => current.begin('a'));
  await act(async () => current.begin('b'));
  expect(current.has('a')).toBe(true);
  expect(current.has('b')).toBe(true);
  expect(current.count).toBe(2);
  await act(async () => current.end('a'));
  expect(current.has('a')).toBe(false);
  expect(current.has('b')).toBe(true);
  expect(current.count).toBe(1);
  await act(async () => current.end('b'));
  expect(current.count).toBe(0);
});

it('counts two operations on the same key', async () => {
  await act(async () => current.begin('a'));
  await act(async () => current.begin('a'));
  await act(async () => current.end('a'));
  expect(current.has('a')).toBe(true);
  expect(current.count).toBe(1);
  await act(async () => current.end('a'));
  expect(current.has('a')).toBe(false);
  expect(current.count).toBe(0);
});
