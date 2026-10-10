// @vitest-environment jsdom
import { act, createElement } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(async () => ['Generated-Pw-1']) }));
vi.mock('../../i18n', () => ({ useTranslation: () => (k: string) => k }));

import { InlinePasswordGenerator } from './InlinePasswordGenerator';

let root: Root;
let host: HTMLDivElement;
let generated: string[];

beforeEach(async () => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    host = document.createElement('div');
    document.body.append(host);
    generated = [];
    root = createRoot(host);
    await act(async () => root.render(
        createElement('div', null,
            createElement('input', { type: 'password', 'data-field': true }),
            createElement(InlinePasswordGenerator, { onGenerated: (v: string) => generated.push(v) }),
        ),
    ));
});
afterEach(async () => {
    await act(async () => root.unmount());
    host.remove();
});

it('stays out of the Tab order but names its keyboard shortcut', () => {
    const button = host.querySelector('button')!;
    expect(button.tabIndex).toBe(-1);
    expect(button.getAttribute('aria-keyshortcuts')).toBe('Alt+G');
    expect(button.title).toContain('Alt+G');
});

it('generates with Alt+G from the field it sits in', async () => {
    const field = host.querySelector('input')!;
    await act(async () => {
        field.dispatchEvent(new KeyboardEvent('keydown', { key: 'g', altKey: true, bubbles: true }));
    });
    expect(generated).toEqual(['Generated-Pw-1']);
});

it('leaves a plain g, and Ctrl+Alt+G, to the field', async () => {
    const field = host.querySelector('input')!;
    await act(async () => {
        field.dispatchEvent(new KeyboardEvent('keydown', { key: 'g', bubbles: true }));
        field.dispatchEvent(new KeyboardEvent('keydown', { key: 'g', altKey: true, ctrlKey: true, bubbles: true }));
    });
    expect(generated).toEqual([]);
});
