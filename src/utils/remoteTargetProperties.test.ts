// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it, vi } from 'vitest';
import { transformWithOxc } from 'vite';
import app from '../App.tsx?raw';
import { QueuedTargetChangedError } from './queuedTarget';

const start = app.indexOf('  const applyOpenDrivePrivacyToPaths =');
const end = app.indexOf('  const [fileLuRemoteUploadDialog', start);
if (start < 0 || end < 0) throw new Error('Properties handler not found');
const { code } = await transformWithOxc(app.slice(start, end), 'opendrive-properties.ts');

describe('Properties access target', () => {
    it('refuses all writes when the target changes while its ownership is acquired', async () => {
        let current = 'A';
        let release!: (value: { epoch: number }) => void;
        const prepared = new Promise<{ epoch: number }>(resolve => { release = resolve; });
        const target = { profileId: 'A' };
        const owner = { target, prepared };
        const invoke = vi.fn(async () => undefined);
        const notify = { error: vi.fn(), warning: vi.fn(), success: vi.fn() };
        const context = {
            propertiesOwnerRef: { current: owner }, multiPropertiesOwnerRef: { current: null },
            queuedTargetGuard: (wanted: typeof target) => () => {
                if (wanted.profileId !== current) throw new QueuedTargetChangedError('changed');
            },
            resolveGuiTarget: async (pending: Promise<{ epoch: number }>, check: () => void) => {
                const result = await pending; check(); return result.epoch;
            },
            invoke, notify, t: (key: string) => key,
            humanLog: { logRaw: () => 'log', updateEntry: vi.fn() },
            setPropertiesDialog: vi.fn(), setMultiPropertiesDialog: vi.fn(), loadRemoteFiles: vi.fn(),
        };
        const apply = new Function(...Object.keys(context), `${code}; return applyOpenDrivePrivacyToPaths;`)(...Object.values(context));
        const pending = apply('public', [{ path: '/a', isDir: false }, { path: '/b', isDir: false }], owner);
        current = 'B';
        release({ epoch: 9 });
        await pending;
        expect(invoke).not.toHaveBeenCalled();
        expect(context.loadRemoteFiles).not.toHaveBeenCalled();
        expect(notify.error).toHaveBeenCalled();
    });

    it('passes the same captured epoch to each access change', async () => {
        const target = { profileId: 'A' };
        const invoke = vi.fn(async () => undefined);
        const context = {
            propertiesOwnerRef: { current: { target, prepared: Promise.resolve({ epoch: 9 }) } }, multiPropertiesOwnerRef: { current: null },
            queuedTargetGuard: () => () => undefined,
            resolveGuiTarget: async (pending: Promise<{ epoch: number }>) => (await pending).epoch,
            invoke, notify: { error: vi.fn(), warning: vi.fn(), success: vi.fn() }, t: (key: string) => key,
            humanLog: { logRaw: () => 'log', updateEntry: vi.fn() },
            setPropertiesDialog: vi.fn(), setMultiPropertiesDialog: vi.fn(), loadRemoteFiles: vi.fn(),
        };
        const apply = new Function(...Object.keys(context), `${code}; return applyOpenDrivePrivacyToPaths;`)(...Object.values(context));
        await apply('private', [{ path: '/a', isDir: false }, { path: '/b', isDir: true }], context.propertiesOwnerRef.current);
        expect(invoke.mock.calls).toEqual([
            ['opendrive_set_path_access', { path: '/a', accessLevel: 'private', isDir: false, expectedGuiTarget: 9 }],
            ['opendrive_set_path_access', { path: '/b', accessLevel: 'private', isDir: true, expectedGuiTarget: 9 }],
        ]);
    });
});


it('keeps a single Properties owner when another target opens multi Properties', async () => {
    const owner = { target: { profileId: 'A' }, prepared: Promise.resolve({ epoch: 1 }) };
    const other = { target: { profileId: 'B' }, prepared: Promise.resolve({ epoch: 2 }) };
    const context = {
        propertiesOwnerRef: { current: owner }, multiPropertiesOwnerRef: { current: other },
        queuedTargetGuard: (wanted: typeof owner.target) => () => {
            if (wanted.profileId !== 'B') throw new QueuedTargetChangedError('changed');
        },
        resolveGuiTarget: async (prepared: Promise<{epoch:number}>, check:()=>void) => { check(); return (await prepared).epoch; },
        invoke: vi.fn(), notify: { error:vi.fn(),success:vi.fn(),warning:vi.fn() }, t:(key:string)=>key,
        humanLog: {logRaw:()=> 'log', updateEntry:vi.fn()},setPropertiesDialog:vi.fn(),setMultiPropertiesDialog:vi.fn(),loadRemoteFiles:vi.fn(),
    };
    const apply = new Function(...Object.keys(context), `${code}; return applyOpenDrivePrivacyToPaths;`)(...Object.values(context));
    await apply('public', [{path:'/same',isDir:false}], owner);
    expect(context.invoke).not.toHaveBeenCalled();
    expect(context.notify.error).toHaveBeenCalled();
});

// The Properties dialogs as App drives them: the real open, close and privacy
// callbacks over one dialog state, so a result is followed to the dialog it lands in.
describe('Properties privacy result and dialog identity', () => {
    type Dialog = { path: string; isRemote: boolean; permissions?: string | null; files?: Dialog[] } | null;
    const slice = (start: string, end: string, from = 0) => {
        const a = app.indexOf(start, from);
        const b = app.indexOf(end, a);
        if (a < 0 || b < 0) throw new Error(`Missing App source: ${start}`);
        return app.slice(a, b);
    };
    const jsxHandler = (component: string, prop: string, nextProp: string) => {
        const body = slice(`${prop}={`, nextProp, app.indexOf(component)).slice(`${prop}={`.length).trim();
        return body.slice(0, body.lastIndexOf('}'));
    };
    const sources = {
        closeSingle: jsxHandler('<PropertiesDialog', 'onClose', '            onCalculateChecksum='),
        closeMulti: jsxHandler('<MultiFilePropertiesDialog', 'onClose', '            folderSizes='),
        privacySingle: jsxHandler('<PropertiesDialog', 'onPrivacyChange', '            // Editable only for local files'),
        privacyMulti: jsxHandler('<MultiFilePropertiesDialog', 'onPrivacyChange', '          />'),
    };

    async function harness() {
        const openLocal = (await transformWithOxc(slice('  const openLocalProperties = async', '\n  // Duplicate Finder & Disk Usage dialogs'), 'open-local-properties.ts')).code;
        // App's own epoch read, which runs the target check once the epoch is known.
        const resolveTarget = (await transformWithOxc(slice('  const resolveGuiTarget = async', '\n  const getOverwriteDestination ='), 'resolve-gui-target.ts')).code;
        const openRemote = (await transformWithOxc(slice('    const openRemoteProperties = (tab', '\n    const items: ContextMenuItem[] = ['), 'open-remote-properties.ts')).code;
        const state = { single: null as Dialog, multi: null as Dialog };
        const set = (key: 'single' | 'multi') => (next: Dialog | ((prev: Dialog) => Dialog)) => {
            state[key] = typeof next === 'function' ? next(state[key]) : next;
        };
        // Each access change waits here until the test lets it through.
        const writes: Array<() => void> = [];
        const invoke = vi.fn(async (cmd: string, _args?: Record<string, unknown>) => {
            if (cmd === 'get_file_properties') return { permissions_text: 'rw-r--r--', permissions_mode: 0o644 };
            if (cmd === 'opendrive_set_path_access') await new Promise<void>(resolve => { writes.push(resolve); });
            return undefined;
        });
        const notify = { error: vi.fn(), warning: vi.fn(), success: vi.fn() };
        // The server the GUI shows now; a dialog's write checks its own target against it.
        const live = { current: 'A' };
        const context = {
            propertiesOwnerRef: { current: null as unknown }, multiPropertiesOwnerRef: { current: null as unknown },
            queuedTargetGuard: (wanted: { profileId: string }) => () => {
                if (wanted.profileId !== live.current) throw new QueuedTargetChangedError('CONNECTION_CHANGED: changed');
            },
            invoke, notify, t: (key: string) => key,
            humanLog: { logRaw: () => 'log', updateEntry: vi.fn() },
            setPropertiesDialog: set('single'), setMultiPropertiesDialog: set('multi'), loadRemoteFiles: vi.fn(),
            setPropertiesInitialTab: vi.fn(), localFoldersShownBy: () => [], cancelLocalFolderSizes: vi.fn(), stopRemoteFolderSize: vi.fn(),
        };
        const app = new Function(...Object.keys(context), `${resolveTarget}; ${code}; ${openLocal}; return {
            applyOpenDrivePrivacyToPaths, openLocalProperties,
            remote: (names, profile = 'A') => {
                const remoteFiles = ['/same', '/other'].map(path => ({ name: path.slice(1), path, size: 1, is_dir: false, modified: null, permissions: 'private' }));
                const selection = new Set(names), count = names.length, file = remoteFiles.find(f => f.name === names[0]);
                const menuTarget = { profileId: profile }, preparedMenuTarget = Promise.resolve({ epoch: profile === 'A' ? 9 : 10 }), shownProtocol = 'opendrive';
                ${openRemote}
                openRemoteProperties('permissions');
            },
            // The handlers of one render: they close over the dialogs shown at that time.
            render: (propertiesDialog, multiPropertiesDialog) => ({
                closeSingle: ${sources.closeSingle}, closeMulti: ${sources.closeMulti},
                privacySingle: propertiesDialog ? (${sources.privacySingle}) : undefined,
                privacyMulti: multiPropertiesDialog ? (${sources.privacyMulti}) : undefined,
            }),
        };`)(...Object.values(context));
        const rendered = () => app.render(state.single, state.multi);
        const local = (path: string): NonNullable<Dialog> => ({ path, isRemote: false, permissions: 'rw-r--r--' });
        return { state, app, rendered, local, writes, notify, context, invoke, live };
    }
    const settle = () => new Promise(resolve => setTimeout(resolve, 0));

    it.each(['closed first', 'replaced directly'] as const)('leaves a local dialog on the same path alone when the remote one was %s', async order => {
        const h = await harness();
        h.app.remote(['same']);
        expect(h.state.single).toMatchObject({ path: '/same', isRemote: true, permissions: 'private' });
        const pending = h.rendered().privacySingle('public');
        await vi.waitFor(() => expect(h.writes).toHaveLength(1));
        if (order === 'closed first') {
            h.rendered().closeSingle();
            expect(h.state.single).toBeNull();
        }
        await h.app.openLocalProperties([h.local('/same')]);
        h.writes[0]();
        await pending;
        expect(h.state.single).toMatchObject({ path: '/same', isRemote: false, permissions: 'rw-r--r--' });
        // The write itself succeeded on its own target, and is reported as such.
        expect(h.notify.success).toHaveBeenCalled();
        expect(h.notify.error).not.toHaveBeenCalled();
        expect(h.context.loadRemoteFiles).toHaveBeenCalled();
    });

    it('leaves a local multi dialog on the same paths alone after the remote multi dialog closed', async () => {
        const h = await harness();
        h.app.remote(['same', 'other']);
        expect(h.state.multi).toMatchObject({ isRemote: true });
        const pending = h.rendered().privacyMulti('public');
        await vi.waitFor(() => expect(h.writes).toHaveLength(2));
        h.rendered().closeMulti();
        await h.app.openLocalProperties([h.local('/same'), h.local('/other')]);
        h.writes.forEach(release => release());
        await pending;
        expect(h.state.multi).toMatchObject({ isRemote: false });
        expect(h.state.multi!.files!.map(f => f?.permissions)).toEqual(['rw-r--r--', 'rw-r--r--']);
        expect(h.notify.error).not.toHaveBeenCalled();
    });

    it('updates a remote dialog opened after a local one on the same path, and keeps local details out of it', async () => {
        const h = await harness();
        // The local dialog's own detail read is still pending when the remote dialog replaces it.
        const localOpen = h.app.openLocalProperties([h.local('/same')]);
        h.app.remote(['same']);
        await localOpen;
        expect(h.state.single).toMatchObject({ path: '/same', isRemote: true, permissions: 'private' });
        const pending = h.rendered().privacySingle('public');
        await vi.waitFor(() => expect(h.writes).toHaveLength(1));
        h.writes[0]();
        await pending;
        expect(h.state.single).toMatchObject({ path: '/same', isRemote: true, permissions: 'public' });
    });

    // Two remote dialogs on two targets: the one opened first stays on screen
    // while the user switches server and opens the other kind. Each write goes
    // through the callbacks App renders, and only the dialog of the live target
    // writes and shows the result.
    it.each(['single then multi', 'multi then single'] as const)('keeps each dialog on its own target when they open %s across servers', async order => {
        const h = await harness();
        const firstKind = order === 'single then multi' ? 'single' : 'multi';
        const secondKind = firstKind === 'single' ? 'multi' : 'single';
        const open = (kind: 'single' | 'multi', profile: string) => h.app.remote(kind === 'single' ? ['same'] : ['same', 'other'], profile);
        const privacy = (kind: 'single' | 'multi', level: string) => kind === 'single' ? h.rendered().privacySingle(level) : h.rendered().privacyMulti(level);
        const levels = (kind: 'single' | 'multi') => kind === 'single' ? [h.state.single!.permissions] : h.state.multi!.files!.map(f => f?.permissions);
        open(firstKind, 'A');
        h.live.current = 'B';
        open(secondKind, 'B');
        expect(h.state.single).toMatchObject({ isRemote: true });
        expect(h.state.multi).toMatchObject({ isRemote: true });
        // The dialog of the server left behind refuses, before any write.
        await privacy(firstKind, 'public');
        expect(h.invoke.mock.calls.filter(([cmd]) => cmd === 'opendrive_set_path_access')).toHaveLength(0);
        expect(h.notify.error).toHaveBeenCalledTimes(1);
        expect(levels(firstKind).every(level => level === 'private')).toBe(true);
        // The live server's dialog writes with its own epoch and shows the result.
        const pending = privacy(secondKind, 'hidden');
        const expected = secondKind === 'single' ? 1 : 2;
        await vi.waitFor(() => expect(h.writes).toHaveLength(expected));
        h.writes.forEach(release => release());
        await pending;
        const writes = h.invoke.mock.calls.filter(([cmd]) => cmd === 'opendrive_set_path_access');
        expect(writes.map(([, args]) => args?.expectedGuiTarget)).toEqual(Array(expected).fill(10));
        expect(levels(secondKind).every(level => level === 'hidden')).toBe(true);
        // The other dialog, on the same path, still shows its own server's level.
        expect(levels(firstKind).every(level => level === 'private')).toBe(true);
        expect(h.notify.error).toHaveBeenCalledTimes(1);
    });

    it('does not apply a replaced request to the remote dialog reopened on the same path', async () => {
        const h = await harness();
        h.app.remote(['same']);
        const first = h.rendered().privacySingle('public');
        await vi.waitFor(() => expect(h.writes).toHaveLength(1));
        h.rendered().closeSingle();
        h.app.remote(['same']);
        h.writes[0]();
        await first;
        await settle();
        expect(h.state.single).toMatchObject({ path: '/same', isRemote: true, permissions: 'private' });
        expect(h.notify.error).not.toHaveBeenCalled();
        // The reopened dialog's own request does reach it.
        const second = h.rendered().privacySingle('hidden');
        await vi.waitFor(() => expect(h.writes).toHaveLength(2));
        h.writes[1]();
        await second;
        expect(h.state.single).toMatchObject({ permissions: 'hidden' });
    });
});

