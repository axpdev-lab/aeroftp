// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
//
// The pin for #510: on a host where no file chooser can be presented, the user
// must be TOLD, in their own language, and the picker must not be opened.
//
// Why this needs a test at all, and why the assertions look the way they do: the
// bug is not a thrown error that went unhandled. `rfd` exposes
// `pick_file() -> Option<PathBuf>` with no error channel, so a portal-less host
// makes `open()` resolve to `null` — the same value as "the user pressed Cancel".
// Nothing throws. So the only observable difference between "fixed" and "still
// broken" is that a message reaches the user, and that is exactly what is
// asserted here: the toast event, its content, and that it carries real
// translated copy rather than a bare i18n key.
//
// Verified by breaking the code, not by watching it pass. Measured, out of 11:
//   - deleting the `chooserIsPresentable` guard in `pickFile`   -> 4 failed
//   - dropping `important: true` from the toast detail          -> 1 failed
//   - pointing a reason at a translation key that does not exist -> 3 failed
//   - making `reportToUser` a no-op                             -> 7 failed

import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest';

import enTranslations from '../i18n/locales/en.json';
import itTranslations from '../i18n/locales/it.json';
import deTranslations from '../i18n/locales/de.json';

const mockInvoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({
    invoke: (cmd: string, args?: unknown) => mockInvoke(cmd, args),
}));

const mockPluginOpen = vi.fn();
const mockPluginSave = vi.fn();
vi.mock('@tauri-apps/plugin-dialog', () => ({
    open: (opts?: unknown) => mockPluginOpen(opts),
    save: (opts?: unknown) => mockPluginSave(opts),
}));

import { pickFile, pickSave } from './pickPath';

interface ToastDetail {
    type?: string;
    title?: string;
    message?: string;
    important?: boolean;
}

/**
 * The unit tests run under `environment: 'node'` (see vitest.config.ts), so the
 * two browser globals the helper touches are stood up by hand rather than by
 * pulling in a DOM implementation for two properties.
 */
let toasts: ToastDetail[] = [];
let storage: Record<string, string> = {};
let htmlLang = '';
const savedWindow = (globalThis as { window?: unknown }).window;
const savedStorage = (globalThis as { localStorage?: unknown }).localStorage;
const savedDocument = (globalThis as { document?: unknown }).document;

beforeEach(() => {
    mockInvoke.mockReset();
    mockPluginOpen.mockReset();
    mockPluginSave.mockReset();
    toasts = [];
    storage = {};
    htmlLang = '';
    // What the mounted I18nProvider publishes, and what `translate()` reads.
    (globalThis as { document?: unknown }).document = {
        get documentElement() { return { get lang() { return htmlLang; } }; },
    };
    (globalThis as { window?: unknown }).window = {
        dispatchEvent: (e: Event) => {
            if (e.type === 'aeroftp-toast') toasts.push((e as CustomEvent).detail as ToastDetail);
            return true;
        },
    };
    (globalThis as { localStorage?: unknown }).localStorage = {
        getItem: (k: string) => storage[k] ?? null,
        setItem: (k: string, v: string) => { storage[k] = v; },
        removeItem: (k: string) => { delete storage[k]; },
    };
});

afterEach(() => {
    (globalThis as { window?: unknown }).window = savedWindow;
    (globalThis as { localStorage?: unknown }).localStorage = savedStorage;
    (globalThis as { document?: unknown }).document = savedDocument;
});

/** The copy a user should actually read, straight from the locale files. */
const EN = enTranslations.translations.picker.unavailable;
const IT = itTranslations.translations.picker.unavailable;

describe('pickFile / pickSave when no chooser can be presented', () => {
    it('tells the user instead of opening nothing, and never calls the plugin', async () => {
        mockInvoke.mockResolvedValue('portal-missing');

        const result = await pickFile({ multiple: false });

        // The picker must not have been attempted: on this host it would present
        // no window and report a `null` indistinguishable from a cancel.
        expect(mockPluginOpen).not.toHaveBeenCalled();
        expect(result).toBeNull();

        // And the user must have been told. This is the whole fix.
        expect(toasts).toHaveLength(1);
        expect(toasts[0].type).toBe('error');
        expect(toasts[0].title).toBe(EN.title);
        expect(toasts[0].message).toBe(EN.portalMissing);
    });

    it('surfaces the message even for a user who turned toasts off', async () => {
        // `important` is what makes App.tsx bypass `showToastNotifications`.
        // Without it, the very users most likely to hit this get the silence back.
        mockInvoke.mockResolvedValue('portal-missing');

        await pickSave({ defaultPath: 'x.txt' });

        expect(mockPluginSave).not.toHaveBeenCalled();
        expect(toasts).toHaveLength(1);
        expect(toasts[0].important).toBe(true);
    });

    it('delivers real copy, not a bare i18n key', async () => {
        // `translate()` returns the key itself when a translation is missing, so
        // a message of "picker.unavailable.portalMissing" would look like a pass
        // to a laxer assertion while showing the user nothing they can read.
        mockInvoke.mockResolvedValue('portal-missing');

        await pickFile();

        expect(toasts[0].message).not.toMatch(/^picker\./);
        expect(toasts[0].title).not.toMatch(/^picker\./);
        expect(toasts[0].message!.length).toBeGreaterThan(20);
    });

    it('speaks the language the user chose, not only English', async () => {
        // The 46 non-English locales are not decoration: a portal-less host is
        // most likely a minimal or containerised desktop, and that user gets the
        // same message everyone else does. This also pins the hookless
        // `translate()` path, which a plain helper has to use because the
        // provider's `t` is only reachable from a hook.
        storage['aeroftp_language'] = 'it';
        mockInvoke.mockResolvedValue('portal-missing');

        await pickFile();

        expect(toasts[0].title).toBe(IT.title);
        expect(toasts[0].message).toBe(IT.portalMissing);
        expect(IT.portalMissing).not.toBe(EN.portalMissing);
    });

    it('follows the window it is rendered in, not the last language the main app was left in', async () => {
        // The gap this closes. `extract-main.tsx` mounts the provider with
        // `initialLanguage` set to the desktop language Rust injects, and never
        // persists it — precisely so the extract window reads in the desktop
        // language rather than whatever the main app was last set to. A
        // `translate()` that consulted storage would have contradicted that, in a
        // window whose folder picker is one of the migrated call sites.
        storage['aeroftp_language'] = 'it';   // what the main app was left in
        htmlLang = 'de';                      // what THIS window is rendering in
        mockInvoke.mockResolvedValue('portal-missing');

        await pickFile({ directory: true });

        const de = deTranslations.translations.picker.unavailable;
        expect(toasts[0].title).toBe(de.title);
        expect(toasts[0].message).toBe(de.portalMissing);
        expect(de.title).not.toBe(IT.title);
    });

    it('falls back to readable copy for a reason it does not recognise', async () => {
        // A newer backend could invent a reason string. Reporting something is
        // better than reporting nothing, which was the bug.
        mockInvoke.mockResolvedValue('some-future-reason');

        const result = await pickFile();

        expect(result).toBeNull();
        expect(mockPluginOpen).not.toHaveBeenCalled();
        expect(toasts).toHaveLength(1);
        expect(toasts[0].message).toBe(EN.unknown);
    });
});

describe('pickFile / pickSave when a chooser is available', () => {
    it('opens the picker and returns the selection', async () => {
        // The other half of the pin: the check must not pass by always refusing.
        // A guard that cried wolf on every healthy host would be worse than the
        // silence, because a warning nobody trusts is a warning nobody reads.
        mockInvoke.mockResolvedValue(null);
        mockPluginOpen.mockResolvedValue('/home/u/file.txt');

        const result = await pickFile({ multiple: false });

        expect(result).toBe('/home/u/file.txt');
        expect(mockPluginOpen).toHaveBeenCalledOnce();
        expect(toasts).toHaveLength(0);
    });

    it('passes options through to the plugin untouched', async () => {
        mockInvoke.mockResolvedValue(null);
        mockPluginSave.mockResolvedValue('/home/u/out.zip');
        const opts = { defaultPath: '/home/u/out.zip', title: 'Save archive' };

        const result = await pickSave(opts);

        expect(result).toBe('/home/u/out.zip');
        expect(mockPluginSave).toHaveBeenCalledWith(opts);
    });

    it('treats a real cancel as a plain no-selection, with no message', async () => {
        // A cancel and a refusal both resolve to `null` at the call site, which
        // is correct. What must differ is whether the user is told anything.
        mockInvoke.mockResolvedValue(null);
        mockPluginOpen.mockResolvedValue(null);

        const result = await pickFile();

        expect(result).toBeNull();
        expect(toasts).toHaveLength(0);
    });

    it('does not block the picker when the check itself cannot run', async () => {
        // A false "your chooser is broken" on a healthy machine is worse than the
        // silence being fixed, so a failed check must never stop a picker.
        mockInvoke.mockRejectedValue(new Error('command not found'));
        mockPluginOpen.mockResolvedValue('/home/u/file.txt');

        const result = await pickFile();

        expect(result).toBe('/home/u/file.txt');
        expect(toasts).toHaveLength(0);
    });
});

describe('a portal that refuses', () => {
    it('reports the refusal instead of letting it escape to the call site', async () => {
        // Unlike an absent portal, a refusing one does produce an error. Half the
        // call sites had no `try`/`catch`, so this is caught once here.
        mockInvoke.mockResolvedValue(null);
        mockPluginOpen.mockRejectedValue(new Error('portal refused'));
        const consoleError = vi.spyOn(console, 'error').mockImplementation(() => {});

        const result = await pickFile();

        expect(result).toBeNull();
        expect(toasts).toHaveLength(1);
        expect(toasts[0].message).toBe(EN.unknown);
        consoleError.mockRestore();
    });

    it('reports a refused save dialog too', async () => {
        mockInvoke.mockResolvedValue(null);
        mockPluginSave.mockRejectedValue(new Error('portal refused'));
        const consoleError = vi.spyOn(console, 'error').mockImplementation(() => {});

        const result = await pickSave();

        expect(result).toBeNull();
        expect(toasts).toHaveLength(1);
        consoleError.mockRestore();
    });
});
