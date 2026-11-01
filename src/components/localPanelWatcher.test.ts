// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { act, createElement, useCallback, useEffect, useLayoutEffect, useMemo, useRef } from 'react';
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
 * the event stream mocked: what it asks the backend to watch, in which
 * order, and which loader a change event runs, at which folder.
 */

const start = app.indexOf('\n  // AeroFile auto-refresh:');
const end = app.indexOf('\n  // Ehud #2:', start);
if (start < 0 || end < 0) throw new Error('Missing App section: AeroFile auto-refresh');
// `currentLocalPathRef` is App's, written the way App writes it, before the section.
const { code } = await transformWithOxc(`function Watcher(props) {
  const { currentLocalPath, currentLocalPath2, isConnected, showRemotePanel, showDualLocalPanel, showConnectionScreen, flattenSearchParse, localFlattenedFiles, loadLocalFiles, loadLocalFiles2 } = props;
  const currentLocalPathRef = useRef('');
  useEffect(() => { currentLocalPathRef.current = currentLocalPath; }, [currentLocalPath]);
${app.slice(start, end)}
  return null;
}`, 'local-panel-watcher.ts');

type Row = { name: string; path: string; is_dir: boolean };
type Props = {
  currentLocalPath: string;
  currentLocalPath2: string;
  isConnected: boolean;
  showRemotePanel: boolean;
  showDualLocalPanel: boolean;
  showConnectionScreen: boolean;
  flattenSearchParse: { active: boolean };
  localFlattenedFiles: Row[] | null;
  loadLocalFiles: (path: string) => Promise<void>;
  loadLocalFiles2: (path: string) => Promise<void>;
};
type Handler = (event: { payload: { panel?: 'local' | 'local2'; path: string } }) => void;

const invoke = vi.fn(async (_command: string, _args?: unknown): Promise<unknown> => undefined);
const handlers: Handler[] = [];
const unlistened = vi.fn();
const listen = vi.fn(async (_name: string, handler: Handler) => {
  handlers.push(handler);
  return unlistened;
});
const Watcher = new Function('useCallback', 'useEffect', 'useLayoutEffect', 'useMemo', 'useRef', 'invoke', 'listen', `${code}; return Watcher;`)(
  useCallback, useEffect, useLayoutEffect, useMemo, useRef, invoke, listen) as (props: Props) => null;

let root: Root | null = null;
const loaders = () => ({
  loadLocalFiles: vi.fn(async (_path: string) => undefined),
  loadLocalFiles2: vi.fn(async (_path: string) => undefined),
});
const base = (over: Partial<Props> = {}): Props => ({
  currentLocalPath: '/home/me',
  currentLocalPath2: '/home/me/other',
  isConnected: false,
  showRemotePanel: true,
  showDualLocalPanel: false,
  showConnectionScreen: false,
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
/** Panel 2's requests, in the order they reached the backend. */
const panel2Requests = () => invoke.mock.calls.filter(([, args]) => (args as { panel?: string } | undefined)?.panel === 'local2');

beforeEach(() => {
  vi.useFakeTimers();
  invoke.mockReset();
  invoke.mockImplementation(async () => undefined);
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
    await render({ ...props, showDualLocalPanel: true });
    expect(last(watches())).toEqual({ path: '/home/me/other', panel: 'local2' });
    await render({ ...props, showDualLocalPanel: false });
    expect(last(stops())).toEqual({ panel: 'local2' });
  });

  it('a change reloads the panel it is for, at the folder that panel shows', async () => {
    const props = base({ showDualLocalPanel: true });
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
    const props = base({ showDualLocalPanel: true });
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
    const props = base({ showDualLocalPanel: true });
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

  it('a change that waits while the panel moves on reloads nothing', async () => {
    // The panel's new folder was just listed; reloading it again cleared
    // what was selected there meanwhile.
    const props = base({ showDualLocalPanel: true });
    await render(props);
    await change('/home/me', 'local');
    await change('/home/me/other', 'local2');
    await render({ ...props, currentLocalPath: '/home/me/moved', currentLocalPath2: '/home/me/next' });
    await settle();
    expect(props.loadLocalFiles).not.toHaveBeenCalled();
    expect(props.loadLocalFiles2).not.toHaveBeenCalled();
  });

  it('sends a panel\'s requests one after another, in the order they were made', async () => {
    // The commands run concurrently: a stop that ran after the watch made
    // next ended it, and the panel stayed without one.
    const held: Array<() => void> = [];
    invoke.mockImplementation(async (_command, args) => {
      if ((args as { panel?: string } | undefined)?.panel === 'local2') await new Promise<void>(resolve => { held.push(resolve); });
      return undefined;
    });
    const props = base({ showDualLocalPanel: true });
    await render(props);
    await render({ ...props, showDualLocalPanel: false });
    await render({ ...props, currentLocalPath2: '/home/me/next' });
    const first = ['local_panel_watch', { path: '/home/me/other', panel: 'local2' }];
    const stop = ['local_panel_watch_stop', { panel: 'local2' }];
    const next = ['local_panel_watch', { path: '/home/me/next', panel: 'local2' }];
    expect(panel2Requests()).toEqual([first]);
    await act(async () => { held.shift()!(); });
    expect(panel2Requests()).toEqual([first, stop]);
    await act(async () => { held.shift()!(); });
    expect(panel2Requests()).toEqual([first, stop, next]);
  });

  it('a request that fails does not hold back the ones after it', async () => {
    invoke.mockImplementation(async (command, args) => {
      if (command === 'local_panel_watch' && (args as { path?: string }).path === '/home/me/other') throw new Error('watcher state poisoned');
      return undefined;
    });
    const props = base({ showDualLocalPanel: true });
    await render(props);
    await render({ ...props, currentLocalPath2: '/home/me/next' });
    expect(last(panel2Requests())).toEqual(['local_panel_watch', { path: '/home/me/next', panel: 'local2' }]);
  });

  it('panel 2 is not watched behind the connection screen, and reloads when it closes', async () => {
    const props = base({ showDualLocalPanel: true });
    await render(props);
    expect(last(panel2Requests())).toEqual(['local_panel_watch', { path: '/home/me/other', panel: 'local2' }]);
    await render({ ...props, showConnectionScreen: true });
    expect(last(panel2Requests())).toEqual(['local_panel_watch_stop', { panel: 'local2' }]);
    expect(props.loadLocalFiles2).not.toHaveBeenCalled();
    await render(props);
    expect(props.loadLocalFiles2).toHaveBeenCalledWith('/home/me/other');
    expect(last(panel2Requests())).toEqual(['local_panel_watch', { path: '/home/me/other', panel: 'local2' }]);
    // Panel 1 keeps its watch: not every way back lists its folder again.
    expect(stops().filter(args => (args as { panel?: string }).panel !== 'local2')).toEqual([]);
  });

  it('panel 2 is not watched behind the remote panel, and reloads when it comes back', async () => {
    // A download or a sync into its folder meanwhile reloaded panel 1 only:
    // it came back with the old rows.
    const props = base({ showDualLocalPanel: true, isConnected: true, showRemotePanel: true });
    await render(props);
    expect(panel2Requests()).toEqual([['local_panel_watch_stop', { panel: 'local2' }]]);
    expect(props.loadLocalFiles2).not.toHaveBeenCalled();
    await render({ ...props, showRemotePanel: false });
    expect(props.loadLocalFiles2).toHaveBeenCalledWith('/home/me/other');
    expect(last(panel2Requests())).toEqual(['local_panel_watch', { path: '/home/me/other', panel: 'local2' }]);
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
