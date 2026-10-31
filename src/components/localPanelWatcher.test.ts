// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { act, createElement, useEffect, useLayoutEffect, useMemo, useRef } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { transformWithOxc } from 'vite';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import app from '../App.tsx?raw';

/**
 * Each local panel reloads when what it shows changes on disk.
 *
 * The watcher (src-tauri/src/local_panel_watcher.rs, its logic and inotify
 * tests there) watched panel 1's folder only, non-recursively: a flattened
 * search showed the folders below it and kept their rows when one of them
 * changed, and panel 2 never reloaded. Panel 1 did not reload either: the
 * listener's cleanup stopped the watch, and the listener subscribed again on
 * every render of App.
 *
 * This renders App's own watcher section in a component, with the IPC and
 * the event stream mocked: what it asks the backend to watch, and which
 * loader a change event runs, at which folder.
 */

const start = app.indexOf('\n  // AeroFile auto-refresh:');
const end = app.indexOf('\n  // Ehud #2:', start);
if (start < 0 || end < 0) throw new Error('Missing App section: AeroFile auto-refresh');
// `currentLocalPathRef` is App's, written the way App writes it, before the section.
const { code } = await transformWithOxc(`function Watcher(props) {
  const { currentLocalPath, currentLocalPath2, localPanel2Visible, flattenSearchParse, localFlattenedFiles, loadLocalFiles, loadLocalFiles2 } = props;
  const currentLocalPathRef = useRef('');
  useEffect(() => { currentLocalPathRef.current = currentLocalPath; }, [currentLocalPath]);
${app.slice(start, end)}
  return null;
}`, 'local-panel-watcher.ts');

type Row = { name: string; path: string; is_dir: boolean };
type Props = {
  currentLocalPath: string;
  currentLocalPath2: string;
  localPanel2Visible: boolean;
  flattenSearchParse: { active: boolean };
  localFlattenedFiles: Row[] | null;
  loadLocalFiles: (path: string) => Promise<void>;
  loadLocalFiles2: (path: string) => Promise<void>;
};
type Handler = (event: { payload: { panel?: 'local' | 'local2'; path: string } }) => void;

const invoke = vi.fn(async (_command: string, _args?: unknown) => undefined);
const handlers: Handler[] = [];
const unlistened = vi.fn();
const listen = vi.fn(async (_name: string, handler: Handler) => {
  handlers.push(handler);
  return unlistened;
});
const Watcher = new Function('useEffect', 'useLayoutEffect', 'useMemo', 'useRef', 'invoke', 'listen', `${code}; return Watcher;`)(
  useEffect, useLayoutEffect, useMemo, useRef, invoke, listen) as (props: Props) => null;

let root: Root | null = null;
const loaders = () => ({
  loadLocalFiles: vi.fn(async (_path: string) => undefined),
  loadLocalFiles2: vi.fn(async (_path: string) => undefined),
});
const base = (over: Partial<Props> = {}): Props => ({
  currentLocalPath: '/home/me',
  currentLocalPath2: '/home/me/other',
  localPanel2Visible: false,
  flattenSearchParse: { active: false },
  localFlattenedFiles: null,
  ...loaders(),
  ...over,
});
const render = async (props: Props) => {
  (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
  root ??= createRoot(document.createElement('div'));
  await act(async () => root!.render(createElement(Watcher, props)));
};
/** A change event from the backend, then the debounce it waits for. */
const change = async (path: string, panel?: 'local' | 'local2') => {
  await act(async () => {
    for (const handler of handlers) handler({ payload: panel ? { panel, path } : { path } });
  });
};
const settle = async () => {
  await act(async () => { vi.advanceTimersByTime(1000); });
};
const watches = () => invoke.mock.calls.filter(([command]) => command === 'local_panel_watch').map(([, args]) => args);
const last = <T,>(items: T[]): T | undefined => items[items.length - 1];
const stops = () => invoke.mock.calls.filter(([command]) => command === 'local_panel_watch_stop').map(([, args]) => args);

beforeEach(() => {
  vi.useFakeTimers();
  invoke.mockClear();
  listen.mockClear();
  unlistened.mockClear();
  handlers.length = 0;
});
afterEach(async () => {
  await act(async () => root?.unmount());
  root = null;
  vi.useRealTimers();
});

describe('each local panel reloads when what it shows changes on disk', () => {
  it('watches panel 1\'s folder, and the folders a flattened search lists below it', async () => {
    await render(base());
    expect(watches()).toEqual([{ path: '/home/me', panel: 'local', dirs: [] }]);
    await render(base({
      flattenSearchParse: { active: true },
      localFlattenedFiles: [
        { name: 'a', path: '/home/me/a', is_dir: true },
        { name: 'a/x.txt', path: '/home/me/a/x.txt', is_dir: false },
        { name: 'a/b', path: '/home/me/a/b', is_dir: true },
      ],
    }));
    expect(last(watches())).toEqual({ path: '/home/me', panel: 'local', dirs: ['/home/me/a', '/home/me/a/b'] });
  });

  it('watches the folders of search results that arrive after the search started', async () => {
    const props = base({ flattenSearchParse: { active: true }, localFlattenedFiles: [] });
    await render(props);
    expect(last(watches())).toEqual({ path: '/home/me', panel: 'local', dirs: [] });
    await render({ ...props, localFlattenedFiles: [{ name: 'a', path: '/home/me/a', is_dir: true }] });
    expect(last(watches())).toEqual({ path: '/home/me', panel: 'local', dirs: ['/home/me/a'] });
  });

  it('watches panel 2 while it is on screen, and stops when it goes', async () => {
    const props = base();
    await render(props);
    expect(watches().filter(w => (w as { panel: string }).panel === 'local2')).toEqual([]);
    await render({ ...props, localPanel2Visible: true });
    expect(last(watches())).toEqual({ path: '/home/me/other', panel: 'local2' });
    await render({ ...props, localPanel2Visible: false });
    expect(last(stops())).toEqual({ panel: 'local2' });
  });

  it('a change reloads the panel it is for, at the folder that panel shows', async () => {
    const props = base({ localPanel2Visible: true });
    await render(props);
    await change('/home/me/other', 'local2');
    await settle();
    expect(props.loadLocalFiles2).toHaveBeenCalledWith('/home/me/other');
    expect(props.loadLocalFiles).not.toHaveBeenCalled();
    await change('/home/me', 'local');
    await settle();
    expect(props.loadLocalFiles).toHaveBeenCalledWith('/home/me');
  });

  it('a change for a folder the panel no longer shows reloads nothing', async () => {
    const props = base();
    await render(props);
    await render({ ...props, currentLocalPath: '/home/me/moved' });
    await change('/home/me', 'local');
    await settle();
    expect(props.loadLocalFiles).not.toHaveBeenCalled();
  });

  it('panel 2 moved to another folder reloads there, and no longer for the folder it left', async () => {
    const props = base({ localPanel2Visible: true });
    await render(props);
    await render({ ...props, currentLocalPath2: '/home/me/next' });
    expect(last(watches())).toEqual({ path: '/home/me/next', panel: 'local2' });
    await change('/home/me/other', 'local2');
    await settle();
    expect(props.loadLocalFiles2).not.toHaveBeenCalled();
    await change('/home/me/next', 'local2');
    await settle();
    expect(props.loadLocalFiles2).toHaveBeenCalledWith('/home/me/next');
  });

  it('changes close together reload once, and one panel\'s change does not cancel the other\'s', async () => {
    const props = base({ localPanel2Visible: true });
    await render(props);
    await change('/home/me', 'local');
    await change('/home/me', 'local');
    await change('/home/me/other', 'local2');
    await change('/home/me/other', 'local2');
    await settle();
    expect(props.loadLocalFiles).toHaveBeenCalledTimes(1);
    expect(props.loadLocalFiles2).toHaveBeenCalledTimes(1);
  });

  it('subscribes once while App renders new loaders, runs the latest one, and never stops the watch', async () => {
    // App builds its loaders again on every render.
    let props = base();
    await render(props);
    for (let i = 0; i < 5; i++) {
      props = { ...props, ...loaders() };
      await render(props);
    }
    expect(listen).toHaveBeenCalledTimes(1);
    expect(unlistened).not.toHaveBeenCalled();
    expect(stops()).toEqual([{ panel: 'local2' }]);
    await change('/home/me', 'local');
    await settle();
    expect(props.loadLocalFiles).toHaveBeenCalledWith('/home/me');
  });

  it('a re-render while a reload waits keeps it', async () => {
    let props = base();
    await render(props);
    await change('/home/me', 'local');
    props = { ...props, ...loaders() };
    await render(props);
    await settle();
    expect(props.loadLocalFiles).toHaveBeenCalledWith('/home/me');
  });
});
