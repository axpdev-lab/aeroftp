// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

/** How a changed-target refusal of the backend starts (`TARGET_CHANGED_MARKER` in Rust). */
export const TARGET_CHANGED_MARKER = 'CONNECTION_CHANGED:';

/** Words an operation or the window puts in front of an error: `upload failed: `, `Error: `. */
const WRAPPERS_ONLY = /^\s*(?:[A-Za-z0-9 ]+: )*$/;

/**
 * Whether a rendered error is the backend's refusal for a changed target.
 * Twin of `is_target_changed_text` (src-tauri/src/providers/types.rs); both
 * run src/shared/target-changed-cases.json.
 *
 * The marker counts where the message starts, or behind wrappers only
 * (`upload failed: CONNECTION_CHANGED: ...`). A marker later in the text is
 * part of something the error names, such as a remote path, and is not the
 * refusal. The caller's own path, and each of its components that holds the
 * marker, is taken out first, so a file named after the marker is not read
 * as the refusal either. A path or component that is the marker and nothing
 * else is left in: taking it out would take the refusal's first word with it.
 */
export const isTargetChangedRefusal = (error: unknown, filePath?: string | null): boolean => {
    let text = String(error);
    if (filePath) {
        for (const part of [filePath, ...filePath.split(/[\\/]/)]) {
            if (part.includes(TARGET_CHANGED_MARKER) && part.trim() !== TARGET_CHANGED_MARKER) text = text.split(part).join('');
        }
    }
    const at = text.indexOf(TARGET_CHANGED_MARKER);
    return at >= 0 && WRAPPERS_ONLY.test(text.slice(0, at));
};
