// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { act, createElement } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { FilenNotesPanel } from './FilenNotesPanel';

const invoke = vi.hoisted(() => vi.fn());
vi.mock('@tauri-apps/api/core', () => ({ invoke }));
vi.mock('../hooks/useHumanizedLog', () => ({ useHumanizedLog: () => ({ logRaw: () => 'log', updateEntry: () => undefined }) }));
vi.mock('../i18n', async () => {
    const { translations } = (await import('../i18n/locales/en.json')).default as { translations: Record<string, unknown> };
    const t = (key: string) => {
        const value = key.split('.').reduce<unknown>((node, part) => (node as Record<string, unknown> | undefined)?.[part], translations);
        return typeof value === 'string' ? value : key;
    };
    return { useTranslation: () => t };
});

const note = (uuid: string, title: string, noteType: string) => ({
    uuid, title, preview: '', noteType, favorite: false, pinned: false, trash: false, archive: false,
    createdTimestamp: 1, editedTimestamp: 1, tags: [], participants: [],
});
const NOTES = [note('a', 'Alpha', 'text'), note('b', 'Beta', 'code')];

/** A Filen backend with two notes; `overrides` replaces single commands. */
const server = (overrides: Record<string, (args: Record<string, unknown>) => unknown> = {}) =>
    invoke.mockImplementation(async (command: string, args: Record<string, unknown>) => {
        if (overrides[command]) return overrides[command](args);
        if (command === 'filen_notes_list') return NOTES;
        if (command === 'filen_notes_tags_list') return [];
        if (command === 'filen_notes_get_content') {
            return { content: '', preview: '', noteType: NOTES.find(n => n.uuid === args.uuid)!.noteType, editedTimestamp: 1, editorId: 1 };
        }
        return undefined;
    });

let root: Root;
let host: HTMLDivElement;
const render = async () => { await act(async () => root.render(createElement(FilenNotesPanel, { isOpen: true, onClose: () => undefined }))); };
const open = async (title: string) => {
    const row = Array.from(host.querySelectorAll('span')).find(e => e.textContent === title)!;
    await act(async () => row.click());
};
const input = async (element: HTMLInputElement | HTMLTextAreaElement, value: string) => {
    await act(async () => {
        const prototype = element instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
        Object.getOwnPropertyDescriptor(prototype, 'value')!.set!.call(element, value);
        element.dispatchEvent(new Event('input', { bubbles: true }));
    });
};
const choose = async (select: HTMLSelectElement, value: string) => {
    await act(async () => {
        select.value = value;
        select.dispatchEvent(new Event('change', { bubbles: true }));
    });
};
const press = async (target: EventTarget, key: string, init: KeyboardEventInit = {}) => {
    await act(async () => { target.dispatchEvent(new KeyboardEvent('keydown', { key, bubbles: true, ...init })); });
};
const typeSelect = () => host.querySelector('select[aria-label="Note type"]') as HTMLSelectElement;
const calls = (command: string) => invoke.mock.calls.filter(([c]) => c === command);

beforeEach(() => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    invoke.mockReset();
    host = document.createElement('div'); document.body.append(host); root = createRoot(host);
});
afterEach(async () => {
    await act(async () => root.unmount()); host.remove();
    vi.useRealTimers();
});

describe('Filen notes panel: note type and tag requests', () => {
    it('saves a pending edit before the type change, so the old type cannot land after it', async () => {
        vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] });
        server();
        await render(); await open('Alpha');
        await input(host.querySelector('textarea')!, 'typed before the change');
        await choose(typeSelect(), 'md');
        await act(async () => { vi.advanceTimersByTime(2000); });
        const changedAt = invoke.mock.calls.findIndex(([c]) => c === 'filen_notes_change_type');
        expect(changedAt).toBeGreaterThan(-1);
        const staleSaves = invoke.mock.calls.slice(changedAt + 1).filter(([c, a]) => c === 'filen_notes_edit_content' && a.noteType !== 'md');
        expect(staleSaves).toEqual([]);
        expect(calls('filen_notes_edit_content').map(([, a]) => a.content)).toContain('typed before the change');
    });

    it('applies a type change result only to the note it was made on', async () => {
        let refuse!: (reason: unknown) => void;
        server({ filen_notes_change_type: () => new Promise((_, reject) => { refuse = reject; }) });
        await render(); await open('Alpha');
        await choose(typeSelect(), 'md');
        await press(window, 'Escape');
        await open('Beta');
        expect(typeSelect().value).toBe('code');
        await act(async () => refuse(new Error('type change refused')));
        expect(typeSelect().value).toBe('code');
    });

    it('creates a tag once when Enter is pressed again while the request is pending', async () => {
        let created!: (uuid: string) => void;
        server({ filen_notes_tags_create: () => new Promise(resolve => { created = resolve; }) });
        await render(); await open('Alpha');
        await choose(host.querySelector('select[aria-label="Add tag"]') as HTMLSelectElement, '__new__');
        const name = host.querySelector('input[aria-label="Tag name"]') as HTMLInputElement;
        await input(name, 'work');
        await press(name, 'Enter');
        await press(name, 'Enter');
        await act(async () => created('t1'));
        expect(calls('filen_notes_tags_create')).toHaveLength(1);
    });

    it('keeps the type selector disabled until the change settles', async () => {
        // A second change while the first is pending could end with the server
        // on one type and the editor on another when the first one fails.
        let settle!: () => void;
        server({ filen_notes_change_type: () => new Promise<void>(resolve => { settle = resolve; }) });
        await render(); await open('Alpha');
        await choose(typeSelect(), 'md');
        expect(typeSelect().disabled).toBe(true);
        await act(async () => settle());
        expect(typeSelect().disabled).toBe(false);
    });

    it('ignores the Enter that confirms an IME composition in a tag name', async () => {
        server();
        await render(); await open('Alpha');
        await choose(host.querySelector('select[aria-label="Add tag"]') as HTMLSelectElement, '__new__');
        const name = host.querySelector('input[aria-label="Tag name"]') as HTMLInputElement;
        await input(name, 'trav');
        await press(name, 'Enter', { isComposing: true });
        expect(calls('filen_notes_tags_create')).toHaveLength(0);
    });
});
