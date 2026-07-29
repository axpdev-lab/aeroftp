// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * The file and folder pickers, with the one thing the plugin cannot tell us
 * added back: whether a chooser could be presented at all.
 *
 * Why this module exists rather than a `try`/`catch` at each call site — this is
 * measured by the #464 harness, not assumed:
 *
 * - `lib.rs` forces `GTK_USE_PORTAL=1`, because the in-process `GtkFileChooser`
 *   corrupts the GLib heap under WebKitGTK. The portal is therefore the only
 *   safe chooser on Linux, not merely the preferred one.
 * - On a host with no `xdg-desktop-portal`, GTK logs `Can't open portal file
 *   chooser: ...Error.ServiceUnknown` and presents nothing. It does not fall
 *   back.
 * - `rfd`, which `tauri-plugin-dialog` uses, exposes
 *   `pick_file() -> Option<PathBuf>`: no error channel at all. So `open()` is
 *   **not rejected** — it resolves to `null`, the exact value that also means
 *   "the user pressed Cancel".
 *
 * That last point is what makes a `try`/`catch` useless here: nothing throws, so
 * there is nothing to catch, and `if (!filePath) return;` at a call site is not
 * forgetful code — it genuinely cannot tell the two cases apart. The distinction
 * has to be recreated *before* the dialog is invoked, which is what the
 * `chooser_unavailable` command does.
 *
 * A refusing portal is the other case, and there an error does exist, so it is
 * caught and reported here too rather than left to 34 separate call sites.
 *
 * The signatures mirror `@tauri-apps/plugin-dialog` exactly, so migrating a call
 * site is an import swap and nothing else.
 */

import { invoke } from '@tauri-apps/api/core';
import {
    open as pluginOpen,
    save as pluginSave,
    type OpenDialogOptions,
    type OpenDialogReturn,
    type SaveDialogOptions,
} from '@tauri-apps/plugin-dialog';
import { translate } from '../i18n';

/**
 * Machine-readable reasons `chooser_unavailable` can return. The backend keeps
 * these stable precisely so the user-facing copy can live here and be
 * translated.
 */
const REASON_KEYS: Record<string, string> = {
    'portal-missing': 'picker.unavailable.portalMissing',
};

/** Fallback copy key for a reason a newer backend might invent. */
const UNKNOWN_REASON_KEY = 'picker.unavailable.unknown';

/**
 * Tell the user, through the app's own UI.
 *
 * `important: true` on purpose: a picker that opens nothing after a click is the
 * textbook "direct user-action failure" that must surface even for a user who
 * turned ambient toast notifications off, because otherwise the click looks like
 * it silently did nothing — which is the entire bug.
 *
 * The message is rendered by the WebView, not by GTK. That matters on the host
 * this fixes: a GTK dialog is the code path we are avoiding, and on a portal-less
 * session it is not a reliable way to reach the user either.
 */
function reportToUser(messageKey: string): void {
    try {
        window.dispatchEvent(
            new CustomEvent('aeroftp-toast', {
                detail: {
                    type: 'error',
                    title: translate('picker.unavailable.title'),
                    message: translate(messageKey),
                    important: true,
                    duration: 12000,
                },
            }),
        );
    } catch (err) {
        // A window torn down mid-gesture, or a non-DOM host. Log rather than
        // return quietly: silence is the defect this module exists to remove, so
        // failing to deliver the message must itself leave a trace.
        console.warn('[picker] could not surface the chooser message', err);
    }
}

/**
 * `null` when a chooser can be presented, otherwise the reason it cannot.
 *
 * Deliberately not cached. A negative cached across the moment the user installs
 * the portal would nag them forever; a positive cached across a portal crash
 * would bring the silence back. The check is a local D-Bus round trip in front
 * of a user gesture, and the backend caps it at one second, so paying it every
 * time is cheaper than either wrong answer. A failure to ask at all is reported
 * by the backend as "no problem", so this never blocks a picker on its own
 * account.
 */
async function chooserUnavailableReason(): Promise<string | null> {
    try {
        return (await invoke<string | null>('chooser_unavailable')) ?? null;
    } catch {
        // The command is missing or the IPC failed. Never block the picker on
        // this check: a false "your chooser is broken" is worse than the silence
        // it exists to fix.
        return null;
    }
}

/**
 * `true` when the picker may be opened; otherwise the user has been told why not.
 */
async function chooserIsPresentable(): Promise<boolean> {
    const reason = await chooserUnavailableReason();
    if (reason === null) return true;
    reportToUser(REASON_KEYS[reason] ?? UNKNOWN_REASON_KEY);
    return false;
}

/**
 * Same contract as the plugin's `open`, plus: when no chooser can be presented,
 * the user is told why and this resolves to `null` without opening anything.
 *
 * Covers folder pickers too, `{ directory: true }` and all, because the plugin's
 * `open` does: the name follows the function it wraps rather than inventing a
 * second one for the same call.
 *
 * Call sites keep their existing `if (!selected) return;` — a cancel and a
 * refusal both still mean "no selection", which is correct. The difference is
 * that the refusal is no longer silent.
 */
export async function pickFile<T extends OpenDialogOptions>(
    options?: T,
): Promise<OpenDialogReturn<T>> {
    if (!(await chooserIsPresentable())) {
        return null as OpenDialogReturn<T>;
    }
    try {
        return await pluginOpen<T>(options);
    } catch (err) {
        // A portal that refuses *does* produce an error, unlike a portal that is
        // absent. Report it rather than let it escape into a call site that may
        // have no handler.
        reportToUser(UNKNOWN_REASON_KEY);
        console.error('[picker] the file chooser refused to open', err);
        return null as OpenDialogReturn<T>;
    }
}

/**
 * Same contract as the plugin's `save`, with the same guarantee as `pickFile`.
 */
export async function pickSave(options?: SaveDialogOptions): Promise<string | null> {
    if (!(await chooserIsPresentable())) return null;
    try {
        return await pluginSave(options);
    } catch (err) {
        reportToUser(UNKNOWN_REASON_KEY);
        console.error('[picker] the save chooser refused to open', err);
        return null;
    }
}
