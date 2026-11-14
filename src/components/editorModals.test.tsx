// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import React, { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const tauri = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: tauri.invoke }));
vi.mock('@tauri-apps/plugin-shell', () => ({ open: vi.fn() }));
vi.mock('../i18n', () => ({ useTranslation: () => (key: string) => key }));
vi.mock('../hooks/useHumanizedLog', () => ({
    useHumanizedLog: () => ({ logRaw: () => 'log-1', updateEntry: () => undefined }),
}));
vi.mock('../utils/pickPath', () => ({ pickFile: vi.fn(), pickSave: vi.fn() }));

import { BoxTagsDialog } from './BoxTagsDialog';
import { GitHubActionsBrowser } from './GitHubActionsBrowser';
import { GitHubReleaseBrowser } from './GitHubReleaseBrowser';
import { ObjectTierDialog } from './ObjectTierDialog';
import { S3TagsDialog } from './S3TagsDialog';
import { SharePermissionsDialog } from './SharePermissionsDialog';

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

let host: HTMLDivElement;
let root: Root;
let release: (() => void) | null;

const flush = () => act(async () => {
    for (let i = 0; i < 6; i++) await Promise.resolve();
});
const click = (el: Element) => act(() => { el.dispatchEvent(new MouseEvent('click', { bubbles: true })); });
const button = (label: string) => [...document.querySelectorAll('button')].find(b => b.textContent?.includes(label) || b.title === label) as HTMLButtonElement | undefined;
const typeInto = (el: HTMLInputElement | HTMLTextAreaElement, value: string) => act(() => {
    const proto = el instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
    Object.getOwnPropertyDescriptor(proto, 'value')!.set!.call(el, value);
    el.dispatchEvent(new Event('input', { bubbles: true }));
});
/** The X: last button of the drag-handle header every one of these modals has. */
const closeX = () => {
    const buttons = host.querySelector('.cursor-grab')!.querySelectorAll('button');
    return buttons[buttons.length - 1] as HTMLButtonElement;
};
const outerBackdrop = () => host.firstElementChild as HTMLElement;
const siblingBackdrop = () => host.firstElementChild!.firstElementChild as HTMLElement;
const input = (selector: string) => host.querySelector(selector) as HTMLInputElement;

interface Case {
    name: string;
    render: (onClose: () => void) => React.ReactElement;
    /** Command answers; the `held` command stays in flight until the test releases it. */
    answers: Record<string, unknown>;
    held: string;
    backdrop: () => HTMLElement;
    /** Start the operation the modal owns (after any input it needs). */
    start: () => Promise<void>;
    /** Type unsaved input (class C), when the modal has an editor. */
    type?: () => Promise<void>;
}

const RUN = {
    id: 4242, name: 'sentinel-ci', display_title: 'sentinel-ci', branch: 'sentinel-branch', sha: 'abcdef0123456789',
    status: 'completed', conclusion: 'failure', event: 'push', run_number: 7, actor_login: 'sentinel-user',
    created_at: '2026-01-01T00:00:00Z', duration_seconds: 0, html_url: 'https://example.com/run/4242',
};

const CASES: Case[] = [
    {
        name: 'ObjectTierDialog (restore, class A only)',
        render: onClose => <ObjectTierDialog mode="s3-restore" path="/sentinel/a.bin" name="a.bin" onClose={onClose} onDone={() => undefined} />,
        answers: {},
        held: 's3_glacier_restore',
        backdrop: siblingBackdrop,
        start: async () => { click(button('common.apply')!); await flush(); },
    },
    {
        name: 'GitHubActionsBrowser (re-run, class A only)',
        render: onClose => <GitHubActionsBrowser isOpen onClose={onClose} />,
        answers: { github_list_actions_runs: [RUN] },
        held: 'github_rerun_failed_jobs',
        backdrop: siblingBackdrop,
        start: async () => { click(button('github.rerunFailedJobs')!); await flush(); },
    },
    {
        name: 'BoxTagsDialog (tags, class A and C)',
        render: onClose => <BoxTagsDialog filePath="/sentinel/a.txt" currentTags={[]} onClose={onClose} onUpdated={() => undefined} />,
        answers: {},
        held: 'box_set_tags',
        backdrop: outerBackdrop,
        start: async () => {
            await typeInto(input('input[type="text"]'), 'sentinel-tag');
            click(button('box.addTag')!);
            click(button('common.save')!);
            await flush();
        },
        type: async () => { await typeInto(input('input[type="text"]'), 'sentinel-tag'); },
    },
    {
        name: 'SharePermissionsDialog (share, class A and C)',
        render: onClose => <SharePermissionsDialog filePath="/sentinel/a.txt" fileName="a.txt" onClose={onClose} />,
        answers: { provider_list_permissions: [] },
        held: 'provider_add_permission',
        backdrop: outerBackdrop,
        start: async () => {
            await typeInto(input('input[type="email"]'), 'sentinel@example.com');
            click(button('sharing.add')!);
            await flush();
        },
        type: async () => { await typeInto(input('input[type="email"]'), 'sentinel@example.com'); },
    },
    {
        name: 'S3TagsDialog (object tags, class A and C)',
        render: onClose => <S3TagsDialog path="/sentinel/a.txt" name="a.txt" onClose={onClose} onSaved={() => undefined} />,
        answers: { s3_get_object_tags: { 'sentinel-key': 'sentinel-value' } },
        held: 's3_set_object_tags',
        backdrop: siblingBackdrop,
        start: async () => {
            await typeInto(input('input[aria-label="s3.tagValue"]'), 'sentinel-edited');
            click(button('common.save')!);
            await flush();
        },
        type: async () => { await typeInto(input('input[aria-label="s3.tagValue"]'), 'sentinel-edited'); },
    },
    {
        name: 'GitHubReleaseBrowser (create release, class A and C)',
        render: onClose => <GitHubReleaseBrowser isOpen onClose={onClose} />,
        answers: { github_list_releases: { releases: [], count: 0 } },
        held: 'github_create_release',
        backdrop: siblingBackdrop,
        start: async () => {
            click(button('github.createRelease')!);
            await typeInto(input('input[placeholder="v1.0.0"]'), 'v9.9.9-sentinel');
            click([...document.querySelectorAll('button')].filter(b => b.textContent?.includes('github.createRelease')).pop()!);
            await flush();
        },
        type: async () => {
            click(button('github.createRelease')!);
            await typeInto(input('input[placeholder="v1.0.0"]'), 'v9.9.9-sentinel');
        },
    },
];

const setup = (c: Case) => {
    beforeEach(() => {
        host = document.createElement('div');
        document.body.appendChild(host);
        root = createRoot(host);
        release = null;
        tauri.invoke.mockImplementation((cmd: string) => {
            if (cmd === c.held) return new Promise(resolve => { release = () => resolve(undefined); });
            return Promise.resolve(cmd in c.answers ? c.answers[cmd] : undefined);
        });
    });
    afterEach(async () => {
        await act(async () => { release?.(); });
        act(() => root.unmount());
        host.remove();
        vi.clearAllMocks();
    });
};

const mount = async (c: Case, onClose: () => void) => {
    act(() => root.render(c.render(onClose)));
    await flush();
};

/**
 * Discussion #1527: a stray click on the backdrop closed these modals while
 * a request they had sent was running, or dropped what had been typed. The
 * backdrop is now inert while either holds, and the X asks first.
 */
describe.each(CASES)('$name', (c) => {
    setup(c);

    it('ignores a backdrop click while its operation runs, and the X asks before closing', async () => {
        const onClose = vi.fn();
        await mount(c, onClose);
        await c.start();
        expect(tauri.invoke).toHaveBeenCalledWith(c.held, expect.anything());
        const callsBefore = tauri.invoke.mock.calls.length;

        click(c.backdrop());
        expect(onClose).not.toHaveBeenCalled();
        expect(button('modalGuard.closeAnyway')).toBeUndefined();
        expect(tauri.invoke.mock.calls.length).toBe(callsBefore);
        // A footer Cancel would close past the guard: it waits for the request.
        const cancel = button('common.cancel');
        if (cancel) expect(cancel.disabled).toBe(true);

        click(closeX());
        expect(onClose).not.toHaveBeenCalled();
        expect(document.body.textContent).toContain('modalGuard.busyBodyKeepsRunning');
        click(button('modalGuard.closeAnyway')!);
        expect(onClose).toHaveBeenCalledTimes(1);
        expect(tauri.invoke.mock.calls.length).toBe(callsBefore);
    });

    it('still closes from the backdrop when idle with nothing typed', async () => {
        const onClose = vi.fn();
        await mount(c, onClose);
        click(c.backdrop());
        expect(onClose).toHaveBeenCalledTimes(1);
        expect(button('modalGuard.discardAndClose')).toBeUndefined();
        expect(button('modalGuard.closeAnyway')).toBeUndefined();
    });
});

describe.each(CASES.filter(c => c.type))('$name with typed input', (c) => {
    setup(c);

    it('keeps the backdrop inert and the X offers to discard', async () => {
        const onClose = vi.fn();
        await mount(c, onClose);
        await c.type!();

        click(c.backdrop());
        expect(onClose).not.toHaveBeenCalled();
        expect(button('modalGuard.discardAndClose')).toBeUndefined();
        expect(tauri.invoke).not.toHaveBeenCalledWith(c.held, expect.anything());

        click(closeX());
        expect(onClose).not.toHaveBeenCalled();
        click(button('modalGuard.keepEditing')!);
        expect(onClose).not.toHaveBeenCalled();
        expect(button('modalGuard.discardAndClose')).toBeUndefined();

        click(closeX());
        click(button('modalGuard.discardAndClose')!);
        expect(onClose).toHaveBeenCalledTimes(1);
        expect(tauri.invoke).not.toHaveBeenCalledWith(c.held, expect.anything());
    });
});

describe('GitHubReleaseBrowser create form (Discussion #1527)', () => {
    const c = CASES.find(x => x.name.startsWith('GitHubReleaseBrowser'))!;
    setup(c);

    it('keeps a release form being typed when ESC is pressed', async () => {
        const onClose = vi.fn();
        await mount(c, onClose);
        await c.type!();
        act(() => { window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true })); });
        expect(input('input[placeholder="v1.0.0"]')?.value).toBe('v9.9.9-sentinel');
        expect(onClose).not.toHaveBeenCalled();
    });
});
